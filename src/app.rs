//! Window, surface and the run loop.

use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use crate::capture::Capture;
use crate::cli::Cli;
use crate::feedback::Feedback;
use crate::gpu::Gpu;
use crate::input::Source;
use crate::keys::{action_for, Action, Edge};
use crate::midi::{Map, Midi};
use crate::overlay::Overlay;
use crate::params::{Focus, Knob, Params, Seed};
use crate::present::Present;
use crate::tempo::Tempo;

/// Borderless rather than exclusive: the instrument renders at its own
/// resolution and lets the compositor scale, so taking a video mode from the
/// display would buy nothing and cost a mode switch on every toggle.
fn borderless(fullscreen: bool) -> Option<winit::window::Fullscreen> {
    fullscreen.then_some(winit::window::Fullscreen::Borderless(None))
}

/// Close a capture and say where it went, which is the only report a
/// performer on a fullscreen display gets of one.
fn finished(capture: Capture) {
    match capture.finish() {
        Ok(path) => log::info!("recorded {}", path.display()),
        Err(why) => log::error!("capture: {why}"),
    }
}

/// Whether the instrument goes on after an action. Named rather than a
/// `bool`, because the one caller that reads it is a line about ending the
/// run loop and `if self.act(a) { exit }` would not say which way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Play,
    Stop,
}

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    feedback: Feedback,
    present: Present,
    overlay: Overlay,
}

pub struct App {
    /// Opened before the run loop starts, because on the web nothing may
    /// block and `resumed` is not a place to wait for an adapter.
    gpu: Gpu,
    params: Params,
    /// What Reset restores: the graph as it was loaded, not the single
    /// preset's knobs.
    initial: Params,
    /// The camera, the monitor and the input the knobs act on.
    focus: Focus,
    /// The running external inputs, in `params.inputs` order, which is the
    /// order `Feedback::write_input` indexes them by.
    sources: Vec<Source>,
    /// The control surface, connected or not — it is looked for while the
    /// instrument runs rather than at startup, so plugging one in mid-piece
    /// is the whole of setting it up.
    midi: Midi,
    shift: bool,
    /// The last knob that moved, which is the one [`Action::ResetLastKnob`] puts
    /// back. `None` until something is turned — on a panel nothing has
    /// touched there is no "that one" to mean.
    last_knob: Option<Knob>,
    /// How big every monitor is — see [`crate::cli::DEFAULT_RESOLUTION`], and
    /// note that the window has nothing to do with it.
    resolution: (u32, u32),
    /// Whether the window covers the display. Kept here rather than asked of
    /// the window, because it is also what the window is *created* with.
    fullscreen: bool,
    /// Whether the controls overlay is showing. Off at startup: the overlay
    /// is help, and help is what the cycle button and backquote are for.
    overlay_shown: bool,
    /// Whether the display shows the focused monitor alone rather than the
    /// tiled bank. Which monitor is not kept here — that is the focus, and
    /// two indices for one question is one of them going stale.
    solo: bool,
    /// Passes and presents since the last rate line, and when that was. Two
    /// counts because they are two clocks — see [`App::meter`], where the
    /// difference between them is the whole of what the line says.
    passes: u32,
    presents: u32,
    metered: Instant,
    /// The tempo, which is where the rate of the piece is kept — not in the
    /// display, whose grid is the compositor's to invent.
    tempo: Tempo,
    /// Whether the last frame went out, and so whether there is a present
    /// left to pace the loop. One pacer at a time: while frames are landing
    /// the swapchain's blank is the clock, and only when they stop is the
    /// tempo's deadline armed to keep the piece going without one.
    paced: bool,
    /// Whether the compositor says nothing can see the window. Nothing is
    /// drawn while it does — see the redraw, where the piece goes on being
    /// played and only the picture waits.
    covered: bool,
    /// The recording, while a hand is on the button that started it. A still
    /// is taken and finished inside the press that asked for it, so nothing
    /// of it is kept here.
    capture: Option<Capture>,
    live: Option<Live>,
    /// Where [`App::give_up`] parks a refusal, since nothing may return out
    /// of `resumed`. Not on the web, which has no `start` left waiting.
    #[cfg(not(target_arch = "wasm32"))]
    failed: Option<String>,
}

/// The window the instrument opens in. On the web it is the page's own
/// canvas — the one the stylesheet has already stretched over the viewport,
/// so "fullscreen" there is the page rather than anything winit does. That
/// canvas has to be found, which is the whole of why this is fallible; a
/// terminal's window is described without asking anything of anyone.
#[cfg(target_arch = "wasm32")]
fn attributes(_fullscreen: bool) -> Result<winit::window::WindowAttributes, String> {
    use winit::platform::web::WindowAttributesExtWebSys;
    Ok(Window::default_attributes().with_canvas(Some(crate::web::canvas()?)))
}

#[cfg(not(target_arch = "wasm32"))]
fn attributes(fullscreen: bool) -> Result<winit::window::WindowAttributes, String> {
    Ok(Window::default_attributes()
        .with_title("lightherder")
        .with_fullscreen(borderless(fullscreen)))
}

/// Hand the run loop over. Native gives it the thread, which it keeps until
/// the instrument is closed; the browser owns its own loop, so there it is
/// handed to the page and this returns at once.
#[cfg(target_arch = "wasm32")]
fn start(event_loop: EventLoop<()>, app: App) -> Result<(), Box<dyn std::error::Error>> {
    winit::platform::web::EventLoopExtWebSys::spawn_app(event_loop, app);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn start(event_loop: EventLoop<()>, mut app: App) -> Result<(), Box<dyn std::error::Error>> {
    let ran = event_loop.run_app(&mut app);
    // A window that never opened is a failed run, not a short one — and its
    // reason comes first: a loop already asked to exit can fail on the way
    // out with something that says less than why it was asked.
    match app.failed {
        Some(why) => Err(why.into()),
        None => Ok(ran?),
    }
}

/// `params` is the loaded graph, already validated by `config::load`; `cli`
/// says how big its monitors are and whether the window covers the display.
///
/// The inputs are opened before the window is, so a file that is not there or
/// a device that will not open says so on the terminal instead of behind a
/// black layer of a running instrument.
///
/// Async because opening a GPU is, and because the one caller that cannot
/// block on it — the browser — is the reason the adapter is opened out here
/// instead of inside `resumed`.
pub async fn run(params: Params, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    crate::feedback::bank_fits(&params, cli.resolution)?;
    let sources = params
        .inputs
        .iter()
        .map(|input| Source::open(input, cli.resolution))
        .collect::<Result<Vec<Source>, String>>()?;
    for input in &params.inputs {
        log::info!("input: {input}");
    }
    // Read before the window opens, like the inputs and for the same reason:
    // a map that will not load is a terminal error, not a surface that turns
    // out to be playing the wrong knobs once there is light on the glass.
    let map_path = crate::midi::map_path();
    log::info!("surface map: {}", map_path.display());
    let map = Map::load(&map_path)?;
    // The controls, off the map that is about to be played rather than off a
    // second read of the same file. Fullscreen this scrolls past behind the
    // instrument; it is here for the terminal it was started from, which is
    // where the log lands too.
    print!("{}{}", crate::keys::help(), map.card());
    log::info!("surface: waiting for {}", map.device);
    let midi = Midi::new(map)?;
    let event_loop = EventLoop::new()?;
    // Through the event loop's own display connection rather than a window's:
    // the adapter is chosen before there is a window, and wgpu forbids a
    // surface created against a different one later.
    let gpu = Gpu::open(Some(event_loop.owned_display_handle()), "lightherder").await?;
    start(
        event_loop,
        App {
            gpu,
            initial: params.clone(),
            params,
            focus: Focus::default(),
            sources,
            midi,
            shift: false,
            last_knob: None,
            resolution: cli.resolution,
            fullscreen: cli.fullscreen,
            overlay_shown: false,
            solo: false,
            passes: 0,
            presents: 0,
            metered: Instant::now(),
            tempo: Tempo::new(cli.rate),
            paced: false,
            covered: false,
            capture: None,
            live: None,
            #[cfg(not(target_arch = "wasm32"))]
            failed: None,
        },
    )
}

impl Live {
    /// Fallible rather than panicking, because a panic is the wrong shape in
    /// both places this runs: a backtrace where the performer was owed the
    /// one line every other refusal gets, and on the web a console message
    /// no visitor will read, behind a window that has already opened. What
    /// comes back instead goes to [`App::give_up`].
    fn new(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        params: &Params,
        map: &Map,
        resolution: (u32, u32),
        fullscreen: bool,
    ) -> Result<Live, String> {
        let window = Arc::new(
            event_loop
                .create_window(attributes(fullscreen)?)
                .map_err(|e| format!("no window to draw in: {e}"))?,
        );
        window.set_cursor_visible(!fullscreen);

        // On the web this is `canvas.getContext("webgpu")`, which a browser
        // that answered `navigator.gpu` and handed over an adapter can still
        // refuse — the realistic way the instrument fails in a tab.
        let surface = gpu
            .instance
            .create_surface(window.clone())
            .map_err(|e| format!("nothing can be drawn on this window: {e}"))?;
        let size = window.inner_size();
        // `None` means the adapter cannot present here — which is a thing
        // that can happen at all because of how it was chosen, and [`Gpu::open`]
        // says why. A hybrid machine whose display hangs off the integrated
        // GPU is the one that meets it.
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| {
                format!(
                    "{} cannot draw to this display — set WGPU_POWER_PREF=low \
                     to open the integrated adapter instead",
                    gpu.adapter.get_info().name,
                )
            })?;
        // Fifo, so a frame reaches the glass whole: a torn frame is a wrong
        // frame in a piece whose look is the whole point, and Immediate buys
        // nothing here now that the display no longer keeps the tempo (#16).
        // It is not the clock — [`Tempo`] is — because the vertical blank
        // Fifo stands in for is the compositor's to invent, and the TV's
        // nested gamescope invents a grid of about 72 Hz (#11).
        config.present_mode = wgpu::PresentMode::Fifo;
        // Focused under that same gamescope a presented buffer comes back one
        // composite hop late (app → Xwayland → gamescope 4K → GNOME), and at
        // the default latency of two the acquire blocks for a whole slot: the
        // piece is shown 40 times a second instead of 60 (#11 — measured; GPU
        // and compositor were both idle-fast, the main thread sat in DRM
        // syncobj waits). A third frame in flight absorbs the round trip, and
        // the extra 16.7 ms to the screen is invisible in an instrument whose
        // knobs are the input. It buys presents and not passes, so a chain it
        // does not suit costs smoothness rather than the piece.
        config.desired_maximum_frame_latency = 3;
        let format = config.format;
        surface.configure(&gpu.device, &config);
        log::info!(
            "window {}x{} {}, presenting {:?} at {format:?}",
            config.width,
            config.height,
            if fullscreen {
                "(covering the display)"
            } else {
                "(windowed)"
            },
            config.present_mode,
        );

        // wgpu zero-initialises textures, so the monitors start black without
        // an explicit clear.
        let feedback = Feedback::new(&gpu.device, resolution.0, resolution.1, params);
        let present = Present::new(&gpu.device, &feedback, format);
        let overlay = Overlay::new(&gpu.device, &gpu.queue, format, map, params);

        Ok(Live {
            window,
            surface,
            config,
            feedback,
            present,
            overlay,
        })
    }

    fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        log::info!("window {width}x{height}");
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&gpu.device, &self.config);
    }

    /// One pass of the instrument: the light on the switcher, and then every
    /// monitor stepped from the bank the last pass left.
    ///
    /// It touches no surface, which is what lets several run to one present
    /// — the display's grid is not the tempo — and is also why the bench can
    /// time exactly this work with no window at all.
    fn pass(&mut self, gpu: &Gpu, params: &Params, sources: &mut [Source]) {
        // Before the cameras read the bank, not after. Once a pass rather than
        // once a present, because a generator that keeps no time of its own —
        // `lavfi` — runs at the rate its frames are collected: light entering
        // the graph follows the piece's clock rather than the display's.
        for (i, source) in sources.iter_mut().enumerate() {
            if let Some(frame) = source.frame() {
                self.feedback.write_input(&gpu.queue, i, frame);
            }
        }
        self.feedback.step(&gpu.device, &gpu.queue, params);
    }

    /// Put what the bank holds on the glass. Whether it went out: a surface
    /// with no texture to give is the one way a present does nothing, and
    /// the caller counts the ones that landed so that a stale surface reads
    /// as the rate it really is.
    fn show(&mut self, gpu: &Gpu, solo: Option<usize>, overlay_shown: bool) -> bool {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            // Suboptimal still hands back a usable texture, and the next
            // resize reconfigures the surface anyway.
            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
            // The surface goes stale on resize, on a monitor change and on
            // compositor restarts. Reconfiguring and skipping one frame is the
            // whole recovery.
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                return false;
            }
            other => {
                log::warn!("dropped a frame: {other:?}");
                return false;
            }
        };
        self.present.draw(
            &gpu.device,
            &gpu.queue,
            &frame.texture,
            &self.feedback,
            solo,
            overlay_shown.then_some(&self.overlay),
        );
        gpu.queue.present(frame);
        true
    }
}

impl App {
    /// Refuse, from inside the run loop, and stop it — there is nothing to
    /// draw. Where the reason goes is the one thing the two hosts do not
    /// share: the terminal's `start` is still on the stack waiting to return
    /// it, and the browser's returned the moment it was handed the loop, so
    /// there the page is written directly — the same place every refusal
    /// before the loop is said, see [`crate::web::complain`].
    fn give_up(&mut self, event_loop: &ActiveEventLoop, why: String) {
        #[cfg(target_arch = "wasm32")]
        crate::web::complain(&why);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.failed = Some(why);
        }
        event_loop.exit();
    }

    /// The rig is untouched: the graph's shape is the launch
    /// configuration's and nothing at the keys can move it, so the loops
    /// keep running and only the knobs the next pass reads have moved.
    fn reset(&mut self) {
        self.params = self.initial.clone();
        // The whole panel just moved without a fader moving with it — and
        // without a hand moving with it either, so the knob "the last knob
        // turned" names was turned on a panel that is gone.
        self.midi.release();
        self.last_knob = None;
        log::info!("reset: {}", self.params.describe(self.focus));
    }

    /// Point the camera knobs at one camera by its place in the graph. A
    /// select row is wider than most graphs, so a press past the end does
    /// nothing: sliding to the last camera instead would make every button
    /// past the end the same button. It says so rather than going quiet —
    /// the button really is dead on this graph, and a performer who cannot
    /// read the panel can at least read the log afterwards.
    fn focus_camera(&mut self, camera: usize) {
        match camera < self.params.cameras.len() {
            true => self.refocus(Focus {
                camera,
                ..self.focus
            }),
            false => log::info!(
                "no camera {}: the graph has {}",
                camera + 1,
                self.params.cameras.len()
            ),
        }
    }

    /// The same for the monitor the faders turn, and past the end for the
    /// same reason.
    fn focus_monitor(&mut self, monitor: usize) {
        match monitor < self.params.monitors.len() {
            true => self.refocus(Focus {
                monitor,
                ..self.focus
            }),
            false => log::info!(
                "no monitor {}: the graph has {}",
                monitor + 1,
                self.params.monitors.len()
            ),
        }
    }

    /// What lights the monitor the faders are on — the one fact about the
    /// graph the panel's lamps read, since the focus alone cannot say it.
    fn seed(&self) -> Seed {
        self.params.monitors[self.focus.monitor].seed
    }

    /// The monitor the display is showing on its own, if any. The solo keeps
    /// no index: which monitor is the focus's business, and a second one
    /// would be a focus that can disagree with the focus.
    fn soloed(&self) -> Option<usize> {
        self.solo.then_some(self.focus.monitor)
    }

    /// Point the knobs at another node. The one way `self.focus` moves: a
    /// fader that has caught a knob is holding *that* node's knob, and the
    /// new node's is somewhere else entirely — without letting go, the next
    /// rotary touched throws it to wherever the fader is standing.
    ///
    /// A focus that is not moving costs no grips: the select row invites a
    /// press on the node already under the knobs, and a key held down
    /// repeats. The readout still prints — on a one-node graph that press is
    /// the only way to ask what the knobs are on, and the log line is the
    /// only place the answer appears.
    fn refocus(&mut self, focus: Focus) {
        if focus != self.focus {
            self.focus = focus;
            self.midi.release();
            // And with the faders' grips, the name of the knob the hands
            // were on: it was a knob of the node they have just left, and
            // putting "the last knob turned" back on the node they landed on
            // would reset one nobody has touched.
            self.last_knob = None;
        }
        log::info!("{}", self.params.describe(self.focus));
    }

    /// One line a second on how the two clocks are going. The instrument is
    /// deployed fullscreen on a display, so the log is the only place a
    /// number can be read at all — and read together these two say which
    /// thing is short. Passes under the tempo is the machine or the graph:
    /// the piece is playing slow. Presents under the passes is only the
    /// display path, which is allowed to hand out fewer frames than the
    /// piece has — that is what the tempo being kept here rather than in the
    /// swapchain is for (#16).
    fn meter(&mut self, passes: u32, shown: bool) {
        self.passes += passes;
        self.presents += u32::from(shown);
        let elapsed = self.metered.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        log::info!(
            "sim {:.0} Hz of {:.0}, present {:.0} Hz",
            self.passes as f64 / seconds,
            self.tempo.rate(),
            self.presents as f64 / seconds,
        );
        self.passes = 0;
        self.presents = 0;
        self.metered = Instant::now();
    }

    /// A turn of `knob`, and what the panel owes it. A knob whose graph has
    /// no node for it — a send with no input — moved nothing, so it must not
    /// become the knob backspace takes back: that button would then reset a
    /// knob the hand never turned, and report a panel it did not change. Said
    /// out loud rather than passed over, the way a select past the end of the
    /// graph is.
    fn turned(&mut self, knob: Knob, turn: impl FnOnce(&mut Params, Focus) -> bool) {
        match turn(&mut self.params, self.focus) {
            true => {
                self.last_knob = Some(knob);
                log::info!("{}", self.params.describe(self.focus));
            }
            false => log::info!("no {}: the graph has no inputs", knob.name()),
        }
    }

    /// Put the last knob that moved back to its identity, and nothing else.
    ///
    /// Only that knob's own faders let go — [`Midi::release_knob`] rather
    /// than the whole panel's release, which is what a reset or a refocus
    /// owes: one knob moved without its fader, so charging every other fader
    /// a pickup sweep for it would make a single-knob reset more expensive
    /// on the hands than the whole-panel one.
    fn reset_knob(&mut self) {
        let Some(knob) = self.last_knob else {
            // Not silent: the button did nothing, and the one place a
            // performer can find out why is the line the rest of the panel
            // reports on.
            return log::info!("no knob has been turned yet");
        };
        self.params.reset(knob, self.focus);
        self.midi.release_knob(knob);
        log::info!(
            "{} reset: {}",
            knob.name(),
            self.params.describe(self.focus)
        );
    }

    /// One action, from wherever it came. The keyboard and the control
    /// surface both land here and nowhere else, so a binding cannot mean one
    /// thing under a finger and another under a fader.
    /// Whether the action wants the run loop stopped — the one thing an
    /// action can ask for that this cannot do itself, since the loop is only
    /// in scope where the events arrive. Returned rather than performed, so
    /// there is no action this refuses to take and the tests can play the
    /// whole vocabulary with no window system under them.
    #[must_use]
    fn act(&mut self, action: Action) -> Flow {
        match action {
            Action::Nudge(knob, delta) => self.turned(knob, |p, f| p.nudge(knob, delta, f)),
            Action::Set(knob, value) => self.turned(knob, |p, f| p.set(knob, value, f)),
            Action::NextInput => match self.params.inputs.is_empty() {
                // Said out loud, the way a select past the end of the graph
                // is: a key that quietly does nothing is a key a performer
                // reads as broken.
                true => log::info!("no inputs: nothing is plugged into the switcher"),
                false => {
                    let input = (self.focus.input + 1) % self.params.inputs.len();
                    self.refocus(Focus {
                        input,
                        ..self.focus
                    });
                }
            },
            Action::FocusCamera(camera) => self.focus_camera(camera),
            Action::FocusMonitor(monitor) => self.focus_monitor(monitor),
            Action::Reset => self.reset(),
            Action::ResetLastKnob => self.reset_knob(),
            // Toggled first and reported after. `log::info!` does not
            // evaluate its arguments when the level is off, so a mode change
            // written inside one is a mode change that happens only while
            // somebody is watching the log.
            Action::Fine => {
                let on = self.midi.toggle_fine();
                log::info!("fine {}", if on { "on" } else { "off" });
            }
            // The focused monitor's, because the seed is the monitor's: the
            // faders' index of the focus is the one that names it, exactly
            // as the front panel beside it does.
            Action::Seed => {
                let seed = &mut self.params.monitors[self.focus.monitor].seed;
                *seed = seed.toggled();
                log::info!("{}", self.params.describe(self.focus));
            }
            Action::Clear => {
                if let Some(live) = self.live.as_mut() {
                    live.feedback.clear(&self.gpu.device, &self.gpu.queue);
                    log::info!("cleared");
                }
            }
            // Said out loud, because the tempo has nothing on the glass to
            // show it: the piece looks the same played fast or slow, and the
            // rate line a second later is the only other place it appears.
            Action::Tempo(step) => {
                self.tempo.step(step, Instant::now());
                // A tenth, because a press at the bottom of the range moves
                // the rate by less than a whole pass a second and a readout
                // that did not move would read as a dead key.
                log::info!("sim {:.1} Hz", self.tempo.rate());
            }
            Action::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                if let Some(live) = self.live.as_ref() {
                    live.window.set_fullscreen(borderless(self.fullscreen));
                    live.window.set_cursor_visible(!self.fullscreen);
                }
            }
            Action::Solo => {
                self.solo = !self.solo;
                match self.solo {
                    true => log::info!("solo monitor {}", self.focus.monitor + 1),
                    false => log::info!("tiled bank"),
                }
            }
            Action::Overlay => {
                self.overlay_shown = !self.overlay_shown;
                log::info!(
                    "overlay {}",
                    if self.overlay_shown {
                        "shown"
                    } else {
                        "hidden"
                    }
                );
            }
            Action::Screencap => self.screencap(),
            Action::Record(Edge::Down) => self.record(),
            Action::Record(Edge::Up) => self.stop_recording(),
            Action::Quit => return Flow::Stop,
        }
        Flow::Play
    }

    /// Draw the display into `capture` and hand it whatever falls due.
    ///
    /// The capture's own pass rather than the surface's: what the glass gets
    /// is a swapchain texture, which is not a copy source on every backend
    /// and is a different size on every window. Solo and the overlay are the
    /// display's, so a capture is framed the way the display is.
    fn grab(&self, capture: &mut Capture) -> Result<(), String> {
        let solo = self.soloed();
        let live = self
            .live
            .as_ref()
            .ok_or_else(|| "there is no picture yet".to_string())?;
        capture.frame(
            &self.gpu.device,
            &self.gpu.queue,
            &live.present,
            &live.feedback,
            solo,
            self.overlay_shown.then_some(&live.overlay),
        )
    }

    /// What the display is showing, in a file.
    fn screencap(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return log::info!("nothing on the glass to capture yet");
        };
        let size = (live.config.width, live.config.height);
        let shot = Capture::still(
            &self.gpu.device,
            &crate::capture::dir(),
            size,
            live.config.format,
        )
        .and_then(|mut capture| self.grab(&mut capture).map(|()| capture))
        .and_then(Capture::finish);
        match shot {
            Ok(path) => log::info!("captured {}", path.display()),
            Err(why) => log::error!("capture: {why}"),
        }
    }

    /// Start recording the display. Nothing on a press that repeats — a held
    /// key sends one — since the recording running is what the press asked
    /// for and starting a second would drop the first mid-file.
    fn record(&mut self) {
        if self.capture.is_some() {
            return;
        }
        let Some(live) = self.live.as_ref() else {
            return log::info!("nothing on the glass to record yet");
        };
        let size = (live.config.width, live.config.height);
        match Capture::video(
            &self.gpu.device,
            &crate::capture::dir(),
            size,
            live.config.format,
        ) {
            Ok(capture) => {
                log::info!("recording");
                self.capture = Some(capture);
            }
            Err(why) => log::error!("capture: {why}"),
        }
    }

    /// Close the recording, if there is one. A release with nothing running
    /// is quiet: the press that would have started it has already said why
    /// it did not.
    fn stop_recording(&mut self) {
        if let Some(capture) = self.capture.take() {
            finished(capture);
        }
    }

    /// The recording's share of a frame. Taken out and put back rather than
    /// borrowed in place, so the one that fails can be closed here and said
    /// out loud instead of going quiet with the file half written.
    fn record_frame(&mut self) {
        let Some(mut capture) = self.capture.take() else {
            return;
        };
        match self.grab(&mut capture) {
            Ok(()) => self.capture = Some(capture),
            Err(why) => {
                log::error!("recording: {why}");
                finished(capture);
            }
        }
    }

    /// The surface's whole part in one frame, and the only place the lamps
    /// are written from.
    ///
    /// The surface is read once a frame, and each message is turned into an
    /// action against the panel the message before it left — not against a
    /// snapshot of the whole batch. A Stop and a fader inside one frame is a
    /// real two-handed gesture, and resolved against a snapshot the fader
    /// would be dragging a knob back out of the panel the button just put
    /// back. Every message is played even after one has asked to stop: the
    /// run loop ends after this frame, not inside it.
    ///
    /// Then the panel is written — see [`Midi::show`] for why every redraw
    /// and not each of the several places the focus moves. A method of its
    /// own rather than the body of the redraw arm, because this half of a
    /// frame needs no window, and so can be played by a test.
    fn surface_frame(&mut self) -> Flow {
        let mut flow = Flow::Play;
        for message in self.midi.poll() {
            let Some(action) = self.midi.action_for(message, &self.params, self.focus) else {
                continue;
            };
            if self.act(action) == Flow::Stop {
                flow = Flow::Stop;
            }
        }
        self.midi
            .show(self.focus, self.seed(), self.overlay_shown, self.solo);
        flow
    }
}

impl ApplicationHandler for App {
    /// The tempo's wake-up, for when no frame is going out to ask for the
    /// next one: a surface gone stale, or a window covered up. The piece goes
    /// on playing without a picture — it is not the picture.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            if let Some(live) = self.live.as_ref() {
                live.window.request_redraw();
            }
        }
    }

    /// The deadline is armed only when the presents that would otherwise pace
    /// the loop have stopped. Arming it under a live redraw chain would be a
    /// second clock on top of the swapchain's: on the web, where the chain is
    /// `requestAnimationFrame` and the deadline is a zero-delay task, that is
    /// a spin between one frame and the next rather than a wait.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(match self.live {
            Some(_) if !self.paced => ControlFlow::WaitUntil(self.tempo.due()),
            _ => ControlFlow::Wait,
        });
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        let live = match Live::new(
            event_loop,
            &self.gpu,
            &self.params,
            self.midi.map(),
            self.resolution,
            self.fullscreen,
        ) {
            Ok(live) => live,
            Err(why) => return self.give_up(event_loop, why),
        };
        log::info!(
            "{} monitors of {}x{}, {} cameras, {} inputs",
            self.params.monitors.len(),
            self.resolution.0,
            self.resolution.1,
            self.params.cameras.len(),
            self.params.inputs.len(),
        );
        log::info!("{}", self.params.describe(self.focus));
        live.window.request_redraw();
        // The tempo and the rate line both start from the first frame, not
        // from before the adapter, the device and the pipelines were built —
        // half a second of startup inside the first window would owe the piece
        // passes it never missed, and would report a rate the instrument never
        // ran at. That first line is what a deploy is read off.
        self.tempo.start();
        self.metered = Instant::now();
        self.passes = 0;
        self.presents = 0;
        self.live = Some(live);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Occluded(covered) => {
                self.covered = covered;
                self.paced = false;
                // Uncovered, nothing is left to ask for the frame that shows
                // where the piece got to while it was hidden.
                if let (false, Some(live)) = (covered, self.live.as_ref()) {
                    live.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.shift = modifiers.state().shift_key(),
            WindowEvent::Resized(size) => {
                if let Some(live) = self.live.as_mut() {
                    live.resize(&self.gpu, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if self.surface_frame() == Flow::Stop {
                    event_loop.exit();
                }
                // Read before the window is taken, which is the whole of why
                // it is up here: the solo is the focus's and the focus is not
                // the window's.
                let solo = self.soloed();
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // Whatever the tempo owes, and then the frame either way: a
                // pass is the piece's clock and the blank is the display's,
                // so a beat that has not fallen due yet is no reason to leave
                // an expose, a resize or the overlay unanswered.
                let passes = self.tempo.take_due(Instant::now());
                for _ in 0..passes {
                    live.pass(&self.gpu, &self.params, &mut self.sources);
                }
                // Nothing is drawn to a window nothing can see. The
                // compositor either hands out frames that wait on no blank at
                // all, which the chain below would spin on, or stops handing
                // them out and leaves a second per frame inside the acquire.
                // The piece plays on through either; only the picture waits.
                let shown = !self.covered && live.show(&self.gpu, solo, self.overlay_shown);
                // Under Fifo the present waits for the blank, so a frame that
                // went out is the one thing that can ask for the next at the
                // display's rate. One that did not paces nothing.
                self.paced = shown;
                if shown {
                    live.window.request_redraw();
                }
                self.meter(passes, shown);
                // After the picture and off the same frame: a recording is
                // the display, and it goes on through a window nothing can
                // see — the piece is not the picture.
                self.record_frame();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                // Repeats are wanted: holding a key sweeps its knob.
                let Some(action) = action_for(code, self.shift) else {
                    return;
                };
                let action = match event.state {
                    ElementState::Pressed => action,
                    // A release reaches only the controls that are held
                    // rather than pressed, which is the surface's answer
                    // too — see [`crate::keys::released`].
                    ElementState::Released => match crate::keys::released(action) {
                        Some(action) => action,
                        None => return,
                    },
                };
                if self.act(action) == Flow::Stop {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::tempo::Step;

    /// An instrument playing `params`, headless: no window has opened, so
    /// `live` is `None` the way it is before `resumed`. `None` when the
    /// machine has no adapter, said on stderr the way tests/feedback_gpu.rs
    /// skips.
    fn playing(params: Params) -> Option<App> {
        // One device for the whole module, like tests/feedback_gpu.rs keeps
        // one: a test apiece opened its own, and a headless Vulkan stack
        // does not survive that many being created and dropped at once —
        // it takes the binary down with a SIGSEGV once there are enough of
        // them. None of these tests renders, so there is nothing for them
        // to share wrongly.
        static GPU: std::sync::OnceLock<Option<Gpu>> = std::sync::OnceLock::new();
        let gpu = GPU
            .get_or_init(|| match pollster::block_on(Gpu::open(None, "app test")) {
                Ok(gpu) => Some(gpu),
                Err(why) => {
                    use std::io::Write;
                    writeln!(std::io::stderr(), "skipping an app test: {why}").unwrap();
                    None
                }
            })
            .clone()?;
        let resolution = (64, 64);
        let sources = params
            .inputs
            .iter()
            .map(|input| Source::open(input, resolution))
            .collect::<Result<Vec<Source>, String>>()
            .unwrap();
        Some(App {
            gpu,
            initial: params.clone(),
            params,
            focus: Focus::default(),
            sources,
            midi: Midi::new(Map::nano_kontrol2()).unwrap(),
            shift: false,
            last_knob: None,
            resolution,
            fullscreen: false,
            overlay_shown: false,
            solo: false,
            passes: 0,
            presents: 0,
            metered: Instant::now(),
            tempo: Tempo::new(crate::tempo::DEFAULT_RATE),
            paced: false,
            covered: false,
            capture: None,
            live: None,
            failed: None,
        })
    }

    #[test]
    fn a_reset_puts_the_panel_back_on_the_graph_the_instrument_started_on() {
        let Some(mut app) = playing(config::external()) else {
            return;
        };
        let started = app.params.clone();
        play(&mut app, Action::Nudge(Knob::Zoom, 0.5));
        play(&mut app, Action::FocusMonitor(0));
        play(&mut app, Action::Nudge(Knob::Gamma, 0.5));
        assert_ne!(app.params, started);
        // The bars are handed over exactly once, so a rig rebuilt or replayed
        // under the reset would have them pending again.
        assert!(app.sources[0].frame().is_some(), "nothing to upload");

        play(&mut app, Action::Reset);
        assert_eq!(app.params, started);
        assert!(
            app.sources[0].frame().is_none(),
            "the rig was rebuilt under the reset"
        );
    }

    #[test]
    fn the_seed_button_swaps_one_monitor_s_rig_and_leaves_the_rest() {
        // Two monitors, both lamp-lit, so "it toggled" and "it toggled the
        // one under the faders" are different observations.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        app.focus_monitor(1);
        assert_eq!(app.params.monitors[1].seed, Seed::BLOB);

        play(&mut app, Action::Seed);
        assert_eq!(app.params.monitors[1].seed, Seed::Dark);
        assert_eq!(app.params.monitors[0].seed, Seed::BLOB, "both went");
        // What the panel reads, which is the focused monitor's and follows
        // the focus rather than the press.
        assert_eq!(app.seed(), Seed::Dark);
        app.focus_monitor(0);
        assert_eq!(app.seed(), Seed::BLOB);

        // And back, on the key a hand actually presses.
        app.focus_monitor(1);
        let Some(action) = crate::keys::action_for(winit::keyboard::KeyCode::Semicolon, false)
        else {
            panic!("; should do something")
        };
        play(&mut app, action);
        assert_eq!(app.params.monitors[1].seed, Seed::BLOB);
    }

    #[test]
    fn a_camera_the_graph_does_not_have_is_a_button_that_does_nothing() {
        // The select row is eight wide and every shipped graph is shallower,
        // so most of a set is played with some of it pointing past the end.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        assert_eq!(app.params.cameras.len(), 2);

        app.focus_camera(1);
        assert_eq!(app.focus.camera, 1);
        // Past the end the focus stays where the hand left it, rather than
        // sliding to the last camera — which would make six buttons one.
        app.focus_camera(7);
        assert_eq!(app.focus.camera, 1);
    }

    #[test]
    fn a_send_on_a_graph_with_no_inputs_is_a_knob_that_holds_nothing() {
        // The send's keys are on the board whatever graph is playing, and
        // most graphs have no input under them. A turn that moved nothing
        // must not become the knob backspace takes back — that button would
        // then put a knob the hand never turned back to its identity, and
        // report a panel it did not change.
        let Some(mut app) = playing(config::single()) else {
            return;
        };
        assert!(app.params.inputs.is_empty());

        play(&mut app, Action::Nudge(Knob::Zoom, 0.1));
        let zoom = app.params.knob(Knob::Zoom, app.focus);
        play(&mut app, Action::Nudge(Knob::Send, -0.005));
        assert_eq!(app.last_knob, Some(Knob::Zoom), "the send took the button");
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Zoom, app.focus),
            Knob::Zoom.identity()
        );
        assert_ne!(zoom, Knob::Zoom.identity(), "the zoom was never off it");

        // And on a graph that has one, the send is a knob like any other.
        let Some(mut app) = playing(config::external()) else {
            return;
        };
        let sent = app.params.knob(Knob::Send, app.focus);
        play(&mut app, Action::Nudge(Knob::Send, 0.005));
        assert!(app.params.knob(Knob::Send, app.focus) > sent);
        assert_eq!(app.last_knob, Some(Knob::Send));
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Send, app.focus),
            Knob::Send.identity()
        );
    }

    #[test]
    fn the_input_focus_walks_round_the_switcher_and_the_readout_says_where() {
        let mut three = config::external();
        three.inputs = vec![config::external().inputs[0].clone(); 3];
        three.routing_inputs = vec![vec![0.014], vec![0.0], vec![0.0]];
        let Some(mut app) = playing(three) else {
            return;
        };

        play(&mut app, Action::NextInput);
        play(&mut app, Action::NextInput);
        assert_eq!(app.focus.input, 2);
        assert!(app.params.describe(app.focus).contains("input 3/3"));
        play(&mut app, Action::NextInput);
        assert_eq!(app.focus.input, 0);
        assert!(app.params.describe(app.focus).contains("input 1/3"));

        let Some(app) = playing(config::single()) else {
            return;
        };
        assert!(!app.params.describe(app.focus).contains("send"));
    }

    #[test]
    fn one_knob_goes_back_and_the_rest_of_the_panel_stays() {
        // The whole point of the button: Stop already puts everything back,
        // and what a hand mid-piece wants is the one knob it just pushed too
        // far.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        app.focus = Focus {
            camera: 1,
            monitor: 1,
            input: 0,
        };
        let before = app.params.clone();
        // Zoom, because the preset loads it at 0.994 rather than at 1.0: a
        // reset that put back *what was loaded* instead of the identity
        // would land on the wrong number, and there is nowhere else in this
        // test that difference shows.
        assert_ne!(before.knob(Knob::Zoom, app.focus), Knob::Zoom.identity());
        play(&mut app, Action::Nudge(Knob::Saturation, 1.0));
        play(&mut app, Action::Nudge(Knob::Zoom, 0.5));
        assert_ne!(app.params, before);

        // Only the last one turned, and only on the focused node — the other
        // camera's zoom is a different number in the same field.
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Zoom, app.focus),
            Knob::Zoom.identity()
        );
        assert_eq!(
            app.params.knob(Knob::Saturation, app.focus),
            before.knob(Knob::Saturation, app.focus) + 1.0,
            "the saturation went back too"
        );
        assert_eq!(app.params.cameras[0], before.cameras[0], "camera 1 moved");

        // Idempotent: a second press is the same knob at the same place, not
        // the one before it.
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Zoom, app.focus),
            Knob::Zoom.identity()
        );
        assert_eq!(
            app.params.knob(Knob::Saturation, app.focus),
            before.knob(Knob::Saturation, app.focus) + 1.0
        );

        // And a fader takes over from where the reset left the knob, not
        // from wherever that fader happens to be standing.
        play(&mut app, Action::Set(Knob::Gamma, 2.0));
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Gamma, app.focus),
            Knob::Gamma.identity()
        );
    }

    #[test]
    fn a_frame_plays_what_the_surface_said_and_lights_what_it_left() {
        // [`App::surface_frame`] is the whole surface's part in a frame, and
        // the instrument's only call to [`Midi::show`] — so this is what
        // stands between either half and being deleted without a test
        // noticing. Both halves in one frame, because they are one: the
        // panel written is the panel that frame's own messages left, so a
        // lamp and the press it answers land together rather than a frame
        // apart. Real lamps on a real file descriptor; the device node is
        // the only thing stood in for.
        use crate::lamps::lamp;
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        // The seed carries a lamp of its own, and which monitors are seeded
        // is the graph's business rather than this test's — said outright,
        // so the panels below are exactly the lamps the focus is.
        app.params.monitors[0].seed = Seed::Dark;
        // The factory layout binds nothing to quit; a performer's map may,
        // and whether the run loop goes on is what a frame's answer *is*.
        let mut map = Map::nano_kontrol2();
        map.button.push(crate::midi::Button {
            cc: 61,
            key: "esc".into(),
        });
        app.midi = Midi::new(map).unwrap();
        let mut surface = app.midi.plug_in_a_test_surface();
        surface.wire.handshake(0);

        // The Solo row splits down the middle, four nodes to a hand: camera
        // 1 is Solo 1 and monitor 1 is Solo 5, which are controls 32 and 36.
        assert_eq!(app.surface_frame(), Flow::Play);
        assert!(
            surface.wire.panel_becomes(lamp(32) | lamp(36)),
            "the focus the instrument started on never reached the surface"
        );
        // Solo 2 selects camera 2 — pressed on the surface rather than acted
        // on directly, so what is asserted is that the frame read it at all.
        surface.press(33);
        assert_eq!(app.surface_frame(), Flow::Play);
        assert_eq!(app.focus.camera, 1, "the press was never played");
        assert!(
            surface.wire.panel_becomes(lamp(33) | lamp(36)),
            "the lamp did not follow the focus its own frame moved"
        );
        // Record is held rather than pressed, and its lamp is lit for as
        // long as the finger is — on a fullscreen display that light is the
        // whole of what says a recording is running.
        surface.press(45);
        assert_eq!(app.surface_frame(), Flow::Play);
        assert!(
            surface.wire.panel_becomes(lamp(33) | lamp(36) | lamp(45)),
            "the record button never lit under the finger"
        );
        surface.release(45);
        assert_eq!(app.surface_frame(), Flow::Play);
        assert!(
            surface.wire.panel_becomes(lamp(33) | lamp(36)),
            "the record button stayed lit after the finger left"
        );
        // And the press that ends the run loop ends it, which only the
        // frame's answer carries.
        surface.press(61);
        assert_eq!(app.surface_frame(), Flow::Stop);
    }

    /// One action, asserting the instrument plays on. Nothing these tests do
    /// is meant to end the run loop, and `act`'s answer is the only thing
    /// that would say otherwise — so it is asserted rather than discarded.
    fn play(app: &mut App, action: Action) {
        assert_eq!(app.act(action), Flow::Play, "{action:?} ended the run loop");
    }

    /// One message off the surface, played the way the redraw plays it:
    /// resolved against the panel as it stands, then applied to it.
    fn surface(app: &mut App, control: u8, value: u8) {
        let change = crate::midi::change(control, value);
        if let Some(action) = app.midi.action_for(change, &app.params, app.focus) {
            play(app, action);
        }
    }

    #[test]
    fn a_reset_takes_the_knob_out_of_the_hands_of_the_fader_holding_it() {
        let Some(mut app) = playing(config::single()) else {
            return;
        };
        // Gamma is fader 6 and contrast fader 5. Sweep each up from the
        // bottom past where its knob stands, which catches it, and leave
        // both faders at the top with both knobs driven to 4. Contrast
        // second, so it is the one the reset means.
        for control in [5, 4] {
            surface(&mut app, control, 0);
            surface(&mut app, control, 127);
        }
        assert_eq!(app.params.knob(Knob::Gamma, app.focus), 4.0);
        assert_eq!(app.params.knob(Knob::Contrast, app.focus), 4.0);
        // Walk gamma away from its fader with the keys, so that whether its
        // fader is still holding it is a question the next touch answers
        // out loud: a grip that survived puts it back at the top, and one
        // that was let go leaves it where the keys left it.
        play(&mut app, Action::Nudge(Knob::Gamma, -2.0));
        // Contrast last, so it is the knob the reset means.
        surface(&mut app, 4, 126);
        surface(&mut app, 4, 127);

        play(&mut app, Action::ResetLastKnob);
        let identity = Knob::Contrast.identity();
        assert_eq!(app.params.knob(Knob::Contrast, app.focus), identity);
        // Contrast's fader is still at the top while its knob is back at
        // 1.0. One that kept its grip throws it straight back on the next
        // touch — the reset undone by a hand nowhere near it.
        surface(&mut app, 4, 126);
        assert_eq!(app.params.knob(Knob::Contrast, app.focus), identity);
        // And *only* that knob's grip went: gamma's fader is still holding
        // it and takes it back to the top at a touch. A whole-panel release
        // here would leave gamma at 2.0, charging a sweep it does not owe.
        surface(&mut app, 5, 126);
        assert!(
            (app.params.knob(Knob::Gamma, app.focus) - 4.0).abs() < 0.05,
            "gamma let go: {}",
            app.params.knob(Knob::Gamma, app.focus)
        );
        // Contrast takes its fader again by sweeping back down to it.
        surface(&mut app, 4, 20);
        assert_ne!(app.params.knob(Knob::Contrast, app.focus), identity);
    }

    #[test]
    fn the_knob_a_reset_means_does_not_outlive_the_panel_it_was_turned_on() {
        // "The knob that hand was just on" stops being true the moment the
        // panel moves without the hands. A focus change lands the knobs on
        // another node and a reset puts the whole panel back, so a rewind
        // after either would reset a knob nobody has touched.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };

        play(&mut app, Action::Nudge(Knob::Zoom, 0.5));
        play(&mut app, Action::FocusCamera(1));
        let moved = app.params.clone();
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(app.params, moved, "a focus change left the knob named");

        play(&mut app, Action::Nudge(Knob::Zoom, 0.5));
        play(&mut app, Action::Reset);
        let reset = app.params.clone();
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(app.params, reset, "a reset left the knob named");

        // A knob turned *after* the move is named again, so this clears the
        // name rather than disabling the button.
        play(&mut app, Action::Nudge(Knob::Gamma, 0.5));
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Gamma, app.focus),
            Knob::Gamma.identity()
        );
    }

    #[test]
    fn a_reset_before_any_knob_has_turned_does_nothing() {
        // There is no "that one" to mean yet, and the panel must not take a
        // guess at which knob was meant.
        let Some(mut app) = playing(config::single()) else {
            return;
        };
        let before = app.params.clone();
        play(&mut app, Action::ResetLastKnob);
        assert_eq!(app.params, before);
    }

    #[test]
    fn the_fine_key_is_what_puts_the_surface_in_fine_mode() {
        // The key, not the method: `Action::Fine` is the whole of what the
        // tab key and the track-prev button do, and a mode nothing reaches
        // through the action is a mode the instrument has not got.
        let Some(mut app) = playing(config::single()) else {
            return;
        };
        let contrast = |app: &App| app.params.knob(Knob::Contrast, app.focus);
        let identity = Knob::Contrast.identity();
        // Fader 5 sits at the bottom while contrast stands a quarter up.
        // Coarse, it does nothing until it sweeps to the knob.
        surface(&mut app, 4, 0);
        surface(&mut app, 4, 1);
        assert_eq!(contrast(&app), identity);

        play(&mut app, Action::Fine);
        surface(&mut app, 4, 2);
        let moved = contrast(&app);
        assert_ne!(moved, identity, "fine mode never came on");
        assert!(
            (moved - identity).abs() < 0.01,
            "that was a coarse set, not a fine nudge: {moved}"
        );

        // And back off again: the same fader, still nowhere near the knob,
        // goes back to doing nothing.
        play(&mut app, Action::Fine);
        surface(&mut app, 4, 3);
        assert_eq!(contrast(&app), moved);
    }

    #[test]
    fn a_change_of_focus_takes_the_faders_off_the_node_they_were_on() {
        // The select row moves the knobs to another node; the faders stay
        // where the hands left them. Without the release the next touch
        // throws that node's knob to a position that stood for the old one.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        // Catch contrast on monitor 1 and drive it to the top of its travel.
        surface(&mut app, 4, 0);
        surface(&mut app, 4, 127);
        assert_eq!(app.params.monitors[0].colour.contrast, 4.0);

        play(&mut app, Action::FocusMonitor(1));
        surface(&mut app, 4, 126);
        assert_eq!(
            app.params.monitors[1].colour.contrast,
            Knob::Contrast.identity(),
            "the fader kept its grip across the focus"
        );
    }

    #[test]
    fn the_monitor_half_of_the_select_row_is_bounded_by_the_monitors() {
        // A square graph cannot tell the two counts apart, so the guard is
        // checked on one that has more monitors than cameras: monitor 3
        // exists and camera 3 does not.
        let mut wider = config::crossed();
        wider.monitors.push(wider.monitors[0].clone());
        wider.routing = vec![vec![1.0, 0.0]; 3];
        for camera in &mut wider.cameras {
            camera.look.push(0.0);
        }
        let Some(mut app) = playing(wider) else {
            return;
        };
        assert_eq!(app.params.cameras.len(), 2);
        assert_eq!(app.params.monitors.len(), 3);
        play(&mut app, Action::FocusMonitor(2));
        assert_eq!(
            app.focus.monitor, 2,
            "monitor 3 is a monitor this graph has"
        );
        play(&mut app, Action::FocusMonitor(3));
        assert_eq!(app.focus.monitor, 2);
    }

    #[test]
    fn a_solo_shows_whichever_monitor_the_focus_is_on() {
        // The solo carries no monitor of its own, so the keys that walk the
        // focus walk what is on the glass — and a bank that is not soloed
        // shows the lot however far the focus moves.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        assert_eq!(app.soloed(), None);
        play(&mut app, Action::FocusMonitor(1));
        assert_eq!(app.soloed(), None);
        play(&mut app, Action::Solo);
        assert_eq!(app.soloed(), Some(1));
        play(&mut app, Action::FocusMonitor(0));
        assert_eq!(app.soloed(), Some(0));
        play(&mut app, Action::Solo);
        assert_eq!(app.soloed(), None);
    }

    #[test]
    fn quit_is_the_one_action_that_stops_the_loop() {
        // The whole vocabulary is playable without a window system now that
        // the run loop is asked for rather than reached for, so the one
        // action that ends it can be checked alongside the ones that do not.
        let Some(mut app) = playing(config::single()) else {
            return;
        };
        assert_eq!(app.act(Action::Quit), Flow::Stop);
        for action in [
            Action::Clear,
            Action::Overlay,
            Action::Solo,
            Action::Fine,
            Action::Reset,
            Action::Tempo(Step::Faster),
        ] {
            assert_eq!(app.act(action), Flow::Play, "{action:?}");
        }
    }

    #[test]
    fn the_tempo_keys_move_the_rate_the_way_they_are_labelled() {
        // The half the tempo tests cannot reach: which key carries which
        // step. A table that hands the faster ratio to the slower key is a
        // wiring mistake no arithmetic test would see.
        let Some(mut app) = playing(config::single()) else {
            return;
        };
        let started = app.tempo.rate();
        play(
            &mut app,
            action_for(winit::keyboard::KeyCode::Digit8, false).unwrap(),
        );
        assert!(
            app.tempo.rate() > started,
            "{} is not faster",
            app.tempo.rate()
        );
        play(
            &mut app,
            action_for(winit::keyboard::KeyCode::Digit7, false).unwrap(),
        );
        assert!(
            (app.tempo.rate() - started).abs() < 1e-3,
            "a press each way left {}",
            app.tempo.rate()
        );
    }

    #[test]
    fn the_select_row_reaches_both_halves_of_the_focus() {
        // The right half of the row points the faders at a monitor, which
        // before this had only the `m` step. Past the end it does nothing,
        // for the same reason the camera half does.
        let Some(mut app) = playing(config::crossed()) else {
            return;
        };
        assert_eq!(app.params.monitors.len(), 2);
        play(&mut app, Action::FocusMonitor(1));
        assert_eq!(app.focus.monitor, 1);
        assert_eq!(app.focus.camera, 0, "the other hand moved");
        play(&mut app, Action::FocusMonitor(7));
        assert_eq!(app.focus.monitor, 1);
    }
}
