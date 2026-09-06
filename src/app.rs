//! Window, surface and the run loop.

use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::capture::Capture;
use crate::cli::Cli;
use crate::clock::Clock;
use crate::command::{Action, Edge};
use crate::feedback::Feedback;
use crate::gpu::Gpu;
use crate::input::Source;
use crate::midi::{Midi, Shown};
use crate::overlay::{Overlay, Readout};
use crate::params::{Focus, Knob, Node, Params};
use crate::present::{Present, View};

/// Close a capture and say where it went, which is the only report a
/// performer on a fullscreen display gets of one.
fn finished(capture: Capture) {
    match capture.finish() {
        Ok(path) => log::info!("captured {}", path.display()),
        Err(why) => log::error!("capture: {why}"),
    }
}

/// One beat of the clock, before the pass it falls on. Not said on the log:
/// at a period of one it would be a line a pass.
fn beat(played: &mut u64, params: &mut Params) {
    *played += 1;
    params.rig.beat(*played);
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
    /// What Reset restores: the rig as it was built.
    initial: Params,
    /// The camera, the monitor and the switcher the knobs act on.
    focus: Focus,
    /// The seed, running.
    source: Source,
    /// The control surface, connected or not — it is looked for while the
    /// instrument runs rather than at startup, so plugging one in mid-piece
    /// is the whole of setting it up.
    midi: Midi,
    /// The last knob that moved, which is the one [`Action::ResetLastKnob`] puts
    /// back. `None` until something is turned — on a panel nothing has
    /// touched there is no "that one" to mean.
    last_knob: Option<Knob>,
    /// How big every monitor is — see [`crate::cli::DEFAULT_RESOLUTION`], and
    /// note that the window has nothing to do with it.
    resolution: (u32, u32),
    fullscreen: bool,
    /// Whether the controls overlay is showing. Off at startup: the overlay
    /// is help, and help is what the cycle button is for.
    overlay_shown: bool,
    /// Whether the display shows the focused monitor alone rather than the
    /// tiled bank. Which monitor is not kept here — that is the focus, and
    /// two indices for one question is one of them going stale.
    solo: bool,
    /// The switcher the held cut is standing on, for as long as the hand is
    /// on the button. Named rather than taken from the focus at release, so
    /// a select pressed mid-hold cannot hand the release to another one.
    cut: Option<usize>,
    /// Passes since the run began, or the last reset: the grid the period
    /// mode beats on.
    played: u64,
    /// Passes and presents since the last rate line, and when that was. Two
    /// counts because they are two clocks — see [`App::meter`], where the
    /// difference between them is the whole of what the line says.
    passes: u32,
    presents: u32,
    metered: Instant,
    /// The pass clock — sixty a second on the wall clock, not the display's
    /// grid, which is the compositor's to invent.
    clock: Clock,
    /// Whether the last frame went out, and so whether there is a present
    /// left to pace the loop. One pacer at a time: while frames are landing
    /// the swapchain's blank is the pacer, and only when they stop is the
    /// clock's deadline armed to keep the piece going without one.
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
    // Borderless rather than exclusive: the instrument renders at its own
    // resolution and lets the compositor scale, so taking a video mode from
    // the display would buy nothing.
    Ok(Window::default_attributes()
        .with_title("lightherder")
        .with_fullscreen(fullscreen.then_some(winit::window::Fullscreen::Borderless(None))))
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

/// `params` is the rig; `cli` says how big its monitors are and whether the
/// window covers the display.
///
/// The seed is opened before the window is, so a device that will not open
/// says so on the terminal instead of behind a black layer of a running
/// instrument.
///
/// Async because opening a GPU is, and because the one caller that cannot
/// block on it — the browser — is the reason the adapter is opened out here
/// instead of inside `resumed`.
pub async fn run(params: Params, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    #[cfg(not(target_arch = "wasm32"))]
    crate::halt::on_signal(event_loop.create_proxy())?;
    crate::feedback::bank_fits(&params, cli.resolution)?;
    log::info!("seed: {}", params.input.source);
    let source = Source::open(&params.input.source, cli.resolution).await?;
    log::info!("surface: waiting for {}", crate::midi::DEVICE);
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
            source,
            midi: Midi::default(),
            last_knob: None,
            resolution: cli.resolution,
            fullscreen: cli.fullscreen,
            overlay_shown: false,
            solo: false,
            cut: None,
            played: 0,
            passes: 0,
            presents: 0,
            metered: Instant::now(),
            clock: Clock::new(crate::clock::RATE),
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
        // nothing here now that the display no longer keeps the pass clock
        // (#16). It is not the clock — [`Clock`] is — because the vertical blank
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
        let overlay = Overlay::new(&gpu.device, &gpu.queue, format);

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
    /// — the display's grid is not the clock — and is also why the bench can
    /// time exactly this work with no window at all.
    fn pass(&mut self, gpu: &Gpu, params: &Params, source: &mut Source) {
        // Before the cameras read the bank, not after. Once a pass rather than
        // once a present, because a generator that keeps no time of its own —
        // `lavfi` — runs at the rate its frames are collected: light entering
        // the graph follows the piece's clock rather than the display's.
        if let Some(frame) = source.frame() {
            self.feedback.write_seed(&gpu.queue, frame);
        }
        self.feedback.step(&gpu.device, &gpu.queue, params);
    }

    /// Put what the bank holds on the glass. Whether it went out: a surface
    /// with no texture to give is the one way a present does nothing, and
    /// the caller counts the ones that landed so that a stale surface reads
    /// as the rate it really is.
    fn show(&mut self, gpu: &Gpu, params: &Params, view: View, overlay: bool) -> bool {
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
            view,
            overlay.then_some((&self.overlay, params)),
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

    /// The rig is untouched: the graph's shape is the launch configuration's
    /// and nothing on the surface can move it, so the loops keep running and
    /// only the knobs the next pass reads have moved.
    fn reset(&mut self) {
        self.params = self.initial.clone();
        self.played = 0;
        // The column the cut took was a column of the panel that is gone,
        // and so was the knob "the last knob turned" names.
        self.cut = None;
        self.last_knob = None;
        self.midi.forgive(Knob::ALL);
        log::info!("reset: {}", self.params.describe(self.focus));
    }

    #[cfg(test)]
    fn beat(&mut self) {
        beat(&mut self.played, &mut self.params);
    }

    fn shown(&self) -> Shown {
        Shown {
            flipped: self.params.monitors[self.focus.monitor].flip,
            program: self.params.rig.on_program(self.focus.monitor),
            overlay: self.overlay_shown,
            solo: self.solo,
        }
    }

    /// What the display shows, read off the focus: a solo is of the focused
    /// monitor and nothing else, so no second index is kept that could
    /// disagree with it.
    fn view(&self) -> View {
        match self.solo {
            true => View::Solo(self.focus.monitor),
            false => View::Bank {
                focus: Some(self.focus.monitor),
            },
        }
    }

    /// Point the knobs at another node. The one way `self.focus` moves.
    ///
    /// The select row invites a press on the node already under the knobs,
    /// which moves nothing. The readout still prints —
    /// on a one-node graph that press is the only way to ask what the knobs
    /// are on, and the log line is the only place the answer appears.
    fn refocus(&mut self, node: Node, index: usize) {
        // The index and not the value: two monitors may sit on the same hue,
        // and a rewind after a select between them owes the monitor the hand
        // was on rather than the one it is on now. Cameras A and 3 share a
        // shaft, so a select between those two clears a name that had not
        // moved — over-eager, which costs a rewind that says so, where the
        // other way round costs the wrong node put back.
        let moved_under = |knob: Knob| knob.node() == node && index != self.focus.at(node);
        if self.last_knob.is_some_and(moved_under) {
            self.last_knob = None;
        }
        self.midi
            .forgive(Knob::ALL.into_iter().filter(|knob| moved_under(*knob)));
        self.focus = self.focus.with(node, index);
        log::info!("{}", self.params.describe(self.focus));
    }

    /// One line a second on how the two clocks are going. The instrument is
    /// deployed fullscreen on a display, so the log is the only place a
    /// number can be read at all — and read together these two say which
    /// thing is short. Passes under sixty is the machine or the graph: the
    /// piece is playing slow. Presents under the passes is only the display
    /// path, which is allowed to hand out fewer frames than the piece has —
    /// that is what the clock being kept here rather than in the swapchain
    /// is for (#16).
    fn meter(&mut self, passes: u32, shown: bool) {
        self.passes += passes;
        self.presents += u32::from(shown);
        let elapsed = self.metered.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        log::info!(
            "sim {:.0} Hz, present {:.0} Hz",
            self.passes as f64 / seconds,
            self.presents as f64 / seconds,
        );
        self.passes = 0;
        self.presents = 0;
        self.metered = Instant::now();
    }

    /// Put the last knob that moved back to its identity, and nothing else.
    fn reset_knob(&mut self) {
        let Some(knob) = self.last_knob else {
            // Not silent: the button did nothing, and the one place a
            // performer can find out why is the line the rest of the panel
            // reports on.
            return log::info!("no knob has been turned yet");
        };
        self.params.reset(knob, self.focus);
        self.midi.forgive([knob]);
        log::info!(
            "{} reset: {}",
            knob.name(),
            self.params.describe(self.focus)
        );
    }

    /// One action off the control surface, which is the only place one comes
    /// from — so a binding cannot mean one thing here and another there.
    ///
    /// Nothing it can do ends the run loop: the instrument is quit the way
    /// any window is, by the window manager, and a slipped finger on a
    /// control surface must not be able to.
    fn act(&mut self, action: Action) {
        match action {
            Action::Turn(knob, delta) => {
                self.params.nudge(knob, delta, self.focus);
                self.last_knob = Some(knob);
                log::info!("{}", self.params.describe(self.focus));
            }
            Action::Focus(node, index) => self.refocus(node, index),
            Action::Reset => self.reset(),
            Action::ResetLastKnob => self.reset_knob(),
            Action::Clear => {
                if let Some(live) = self.live.as_mut() {
                    live.feedback.clear(&self.gpu.device, &self.gpu.queue);
                    log::info!("cleared");
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
            Action::Cut(edge) => {
                let moved = match (edge, self.cut.take()) {
                    (Edge::Down, None) => {
                        self.params.rig.flip(self.focus.switcher);
                        self.cut = Some(self.focus.switcher);
                        true
                    }
                    (Edge::Up, Some(held)) => {
                        self.params.rig.flip(held);
                        true
                    }
                    (_, held) => {
                        self.cut = held;
                        false
                    }
                };
                if moved {
                    log::info!("{}", self.params.describe(self.focus));
                }
            }
            Action::Reverse => {
                self.params.rig.flip(self.focus.switcher);
                log::info!("{}", self.params.describe(self.focus));
            }
            Action::Select => {
                let monitor = self.focus.monitor;
                match self.params.rig.select(monitor) {
                    true => log::info!("{}", self.params.describe(self.focus)),
                    false => log::info!("monitor {} has no select", monitor + 1),
                }
            }
            Action::Flip(axis) => {
                let monitor = &mut self.params.monitors[self.focus.monitor];
                monitor.flip(axis);
                log::info!(
                    "monitor {} flipped {:?}",
                    self.focus.monitor + 1,
                    monitor.flip
                );
            }
        }
    }

    fn readout(&self) -> Readout {
        Readout::of(
            &self.params,
            self.focus,
            self.midi.precision(),
            self.midi.wanted(self.focus, self.shown()),
        )
    }

    fn grab(&mut self, capture: &mut Capture) -> Result<(), String> {
        let view = match self.view() {
            View::Bank { .. } => View::Bank { focus: None },
            solo => solo,
        };
        let readout = self.overlay_shown.then(|| self.readout());
        let live = self
            .live
            .as_mut()
            .ok_or_else(|| "there is no picture yet".to_string())?;
        if let Some(readout) = readout {
            live.overlay
                .show(&self.gpu.device, &self.gpu.queue, readout);
        }
        capture.frame(
            &self.gpu.device,
            &self.gpu.queue,
            &live.present,
            &live.feedback,
            view,
            self.overlay_shown.then_some((&live.overlay, &self.params)),
        )
    }

    /// How big the display is and what format it is in, which is the whole
    /// of what a capture needs to be built against — and `None` before there
    /// is a display at all.
    fn glass(&self) -> Option<((u32, u32), wgpu::TextureFormat)> {
        let live = self.live.as_ref()?;
        Some(((live.config.width, live.config.height), live.config.format))
    }

    fn screencap(&mut self) {
        let Some((size, format)) = self.glass() else {
            return log::info!("nothing on the glass to capture yet");
        };
        match Capture::still(&self.gpu.device, &crate::capture::dir(), size, format) {
            Ok(mut capture) => match self.grab(&mut capture) {
                Ok(()) => finished(capture),
                Err(why) => log::error!("capture: {why}"),
            },
            Err(why) => log::error!("capture: {why}"),
        }
    }

    /// Start recording the display. Nothing on a press that repeats, since
    /// the recording running is what the press asked for and starting a
    /// second would drop the first mid-file.
    fn record(&mut self) {
        if self.capture.is_some() {
            return;
        }
        let Some((size, format)) = self.glass() else {
            return log::info!("nothing on the glass to record yet");
        };
        match Capture::video(&self.gpu.device, &crate::capture::dir(), size, format) {
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
    /// snapshot of the whole batch. A reset and a fader inside one frame is a
    /// real two-handed gesture, and resolved against a snapshot the fader
    /// would be dragging a knob back out of the panel the button just put
    /// back.
    ///
    /// Then the panel is written — see [`Midi::show`] for why every redraw
    /// and not each of the several places the focus moves. A method of its
    /// own rather than the body of the redraw arm, because this half of a
    /// frame needs no window, and so can be played by a test.
    fn surface_frame(&mut self) {
        for message in self.midi.poll() {
            if let Some(action) = self.midi.action_for(message, &self.params) {
                self.act(action);
            }
        }
        self.midi.show(self.focus, self.shown());
    }
}

impl ApplicationHandler for App {
    /// The clock's wake-up, for when no frame is going out to ask for the
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
            Some(_) if !self.paced => ControlFlow::WaitUntil(self.clock.due()),
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
            self.resolution,
            self.fullscreen,
        ) {
            Ok(live) => live,
            Err(why) => return self.give_up(event_loop, why),
        };
        log::info!(
            "{} monitors of {}x{}, {} cameras, one seed",
            self.params.monitors.len(),
            self.resolution.0,
            self.resolution.1,
            self.params.cameras.len(),
        );
        log::info!("{}", self.params.describe(self.focus));
        live.window.request_redraw();
        // The clock and the meter both start from the first frame, not from
        // before the adapter, the device and the pipelines were built — half
        // a second of startup inside the first window would owe the piece
        // passes it never missed, and would report a rate the instrument never
        // ran at. That first line is what a deploy is read off.
        self.clock = Clock::new(crate::clock::RATE);
        self.metered = Instant::now();
        self.passes = 0;
        self.presents = 0;
        self.live = Some(live);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: ()) {
        event_loop.exit();
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
            WindowEvent::Resized(size) => {
                if let Some(live) = self.live.as_mut() {
                    live.resize(&self.gpu, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                self.surface_frame();
                // Read before the window is taken, which is the whole of why
                // it is up here: the view is the focus's and the focus is not
                // the window's.
                let view = self.view();
                let overlay = self.overlay_shown;
                let readout = overlay.then(|| self.readout());
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // Whatever the clock owes, and then the frame either way: a
                // pass is the piece's clock and the blank is the display's,
                // so a beat that has not fallen due yet is no reason to leave
                // an expose, a resize or the overlay unanswered.
                let passes = self.clock.take_due(Instant::now());
                for _ in 0..passes {
                    beat(&mut self.played, &mut self.params);
                    live.pass(&self.gpu, &self.params, &mut self.source);
                }
                // Nothing is drawn to a window nothing can see. The
                // compositor either hands out frames that wait on no blank at
                // all, which the chain below would spin on, or stops handing
                // them out and leaves a second per frame inside the acquire.
                // The piece plays on through either; only the picture waits.
                if let Some(readout) = readout {
                    live.overlay
                        .show(&self.gpu.device, &self.gpu.queue, readout);
                }
                let shown = !self.covered && live.show(&self.gpu, &self.params, view, overlay);
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
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::affine::Axis;
    use crate::config;
    use crate::midi::TestSurface;
    use crate::params::Node;

    /// An instrument playing `params`, headless: no window has opened, so
    /// `live` is `None` the way it is before `resumed`. `None` when the
    /// machine has no adapter, said on stderr the way tests/feedback_gpu.rs
    /// skips.
    fn playing(params: Params) -> Option<App> {
        // One device for the whole module, like tests/feedback_gpu.rs keeps
        // one: a test apiece opened its own, and a headless Vulkan stack
        // does not survive that many being created and dropped at once —
        // it takes the binary down with a SIGSEGV once there are enough of
        // them.
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
        // A drawn pattern, not the rig's seed: a suite that demanded
        // /dev/video0 would be testing the machine it runs on. Nothing here
        // reads a pixel of it.
        let mut params = params;
        params.input.source = crate::input::Input::Pattern(crate::input::Pattern::Bars);
        let source = pollster::block_on(Source::open(&params.input.source, resolution)).unwrap();
        Some(App {
            gpu,
            initial: params.clone(),
            focus: Focus::default(),
            source,
            midi: Midi::default(),
            params,
            last_knob: None,
            resolution,
            fullscreen: false,
            overlay_shown: false,
            solo: false,
            cut: None,
            played: 0,
            passes: 0,
            presents: 0,
            metered: Instant::now(),
            clock: Clock::new(crate::clock::RATE),
            paced: false,
            covered: false,
            capture: None,
            live: None,
            failed: None,
        })
    }

    #[test]
    fn a_flip_mirrors_the_focused_monitor_and_again_puts_it_back() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let started = app.params.clone();
        app.act(Action::Focus(Node::Monitor, 1));
        app.act(Action::Flip(Axis::X));
        assert_eq!(app.shown().flipped, [true, false]);
        let mut want = started.clone();
        want.monitors[1].flip[0] = true;
        assert_eq!(app.params, want, "only the focused monitor, only that axis");
        app.act(Action::Flip(Axis::Y));
        assert_eq!(app.shown().flipped, [true, true]);
        app.act(Action::Flip(Axis::X));
        app.act(Action::Flip(Axis::Y));
        assert_eq!(app.params, started);
    }

    #[test]
    fn a_reset_puts_the_panel_back_on_the_graph_the_instrument_started_on() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let started = app.params.clone();
        turn(&mut app, Knob::Zoom, 0.5);
        app.act(Action::Focus(Node::Monitor, 0));
        turn(&mut app, Knob::Contrast, 0.5);
        assert_ne!(app.params, started);
        // The bars are handed over exactly once, so a rig rebuilt or replayed
        // under the reset would have them pending again.
        assert!(app.source.frame().is_some(), "nothing to upload");

        app.act(Action::Reset);
        assert_eq!(app.params, started);
        assert!(
            app.source.frame().is_none(),
            "the rig was rebuilt under the reset"
        );
    }

    #[test]
    fn one_knob_goes_back_and_the_rest_of_the_panel_stays() {
        // The whole point of the button: Stop already puts everything back,
        // and what a hand mid-piece wants is the one knob it just pushed too
        // far.
        let Some(mut app) = playing(off_identity()) else {
            return;
        };
        app.focus = Focus {
            camera: 1,
            monitor: 1,
            switcher: 0,
        };
        let before = app.params.clone();
        assert_ne!(before.knob(Knob::Zoom, app.focus), Knob::Zoom.identity());
        turn(&mut app, Knob::Saturation, 1.0);
        turn(&mut app, Knob::Zoom, 0.5);
        assert_ne!(app.params, before);

        // Only the last one turned, and only on the focused node — the other
        // camera's zoom is a different number in the same field.
        app.act(Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Zoom, app.focus),
            Knob::Zoom.identity()
        );
        assert_eq!(
            app.params.knob(Knob::Saturation, app.focus),
            before.knob(Knob::Saturation, app.focus) + 1.0,
            "the saturation went back too"
        );

        // Idempotent: a second press is the same knob at the same place, not
        // the one before it.
        app.act(Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Zoom, app.focus),
            Knob::Zoom.identity()
        );
        assert_eq!(
            app.params.knob(Knob::Saturation, app.focus),
            before.knob(Knob::Saturation, app.focus) + 1.0
        );

        app.act(Action::Turn(Knob::Contrast, 1.0));
        app.act(Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Contrast, app.focus),
            Knob::Contrast.identity()
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
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let mut surface = plugged(&mut app);

        // One row per kind: camera 1 is S1, monitor 1 is M1 and switcher 1 is
        // R1, which are controls 32, 48 and 64.
        app.surface_frame();
        assert!(
            surface
                .wire
                .panel_becomes(lamp(32) | lamp(48) | lamp(64) | lamp(71)),
            "the focus the instrument started on never reached the surface"
        );
        // Solo 2 selects camera 2 — pressed on the surface rather than acted
        // on directly, so what is asserted is that the frame read it at all.
        surface.press(33);
        app.surface_frame();
        assert_eq!(app.focus.camera, 1, "the press was never played");
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71)),
            "the lamp did not follow the focus its own frame moved"
        );
        // Record is held rather than pressed, and its lamp is lit for as
        // long as the finger is.
        surface.press(45);
        app.surface_frame();
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71) | lamp(45)),
            "the record button never lit under the finger"
        );
        surface.release(45);
        app.surface_frame();
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71)),
            "the record button stayed lit after the finger left"
        );
        // The cut is the other held button, on marker prev.
        surface.press(61);
        app.surface_frame();
        assert!(app.cut.is_some(), "the press was never played");
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71) | lamp(61)),
            "the cut button never lit under the finger"
        );
        surface.release(61);
        app.surface_frame();
        assert!(app.cut.is_none(), "the release was never played");
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71)),
            "the cut button stayed lit after the finger left"
        );
        // Help and solo are the two lamps whose state lives in the instrument
        // rather than in the surface; this frame is where it crosses over.
        surface.press(46);
        surface.release(46);
        app.surface_frame();
        assert!(app.overlay_shown, "the press was never played");
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71) | lamp(46)),
            "the help lamp never lit for the overlay"
        );
        surface.press(44);
        surface.release(44);
        app.surface_frame();
        assert!(app.solo, "the press was never played");
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71) | lamp(46) | lamp(44)),
            "the solo lamp never lit"
        );
        surface.press(46);
        surface.release(46);
        surface.press(44);
        surface.release(44);
        app.surface_frame();
        assert!(
            surface
                .wire
                .panel_becomes(lamp(33) | lamp(48) | lamp(64) | lamp(71)),
            "a latch let go kept its lamp"
        );
    }

    fn off_identity() -> Params {
        let mut params = config::instrument();
        params.shafts[1].zoom = 0.9;
        params
    }

    fn turn(app: &mut App, knob: Knob, delta: f32) {
        app.act(Action::Turn(knob, delta));
    }

    fn plugged(app: &mut App) -> TestSurface {
        let mut surface = app.midi.plug_in_a_test_surface();
        surface.wire.handshake(0);
        surface
    }

    fn surface(app: &mut App, board: &TestSurface, control: u8, value: u8) {
        board.send(control, value);
        app.surface_frame();
    }

    #[test]
    fn the_knob_a_reset_means_does_not_outlive_the_panel_it_was_turned_on() {
        // "The knob that hand was just on" stops being true the moment the
        // panel moves without the hands. A focus change lands the knobs on
        // another node and a reset puts the whole panel back, so a rewind
        // after either would reset a knob nobody has touched.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };

        turn(&mut app, Knob::Zoom, 0.5);
        app.act(Action::Focus(Node::Camera, 1));
        let moved = app.params.clone();
        app.act(Action::ResetLastKnob);
        assert_eq!(app.params, moved, "a focus change left the knob named");

        turn(&mut app, Knob::Zoom, 0.5);
        app.act(Action::Reset);
        let reset = app.params.clone();
        app.act(Action::ResetLastKnob);
        assert_eq!(app.params, reset, "a reset left the knob named");

        // A knob turned *after* the move is named again, so this clears the
        // name rather than disabling the button.
        turn(&mut app, Knob::Contrast, 0.5);
        app.act(Action::ResetLastKnob);
        assert_eq!(
            app.params.knob(Knob::Contrast, app.focus),
            Knob::Contrast.identity()
        );
    }

    #[test]
    fn a_select_on_a_node_the_last_knob_does_not_read_leaves_it_named() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        turn(&mut app, Knob::Switcher, 0.005);
        app.act(Action::Focus(Node::Camera, 1));
        assert_eq!(
            app.last_knob,
            Some(Knob::Switcher),
            "the crossfade is not the camera's"
        );
        app.act(Action::Focus(Node::Monitor, 1));
        assert_eq!(app.last_knob, Some(Knob::Switcher), "nor the monitor's");
        app.act(Action::Focus(Node::Switcher, 1));
        assert_eq!(app.last_knob, None, "the switcher moved out from under it");

        turn(&mut app, Knob::Zoom, 0.5);
        app.act(Action::Focus(Node::Monitor, 0));
        app.act(Action::Focus(Node::Switcher, 0));
        assert_eq!(app.last_knob, Some(Knob::Zoom), "zoom reads neither");
        app.act(Action::Focus(Node::Camera, 0));
        assert_eq!(app.last_knob, None);
    }

    #[test]
    fn a_change_of_focus_leaves_the_faders_turning_from_where_they_stand() {
        // The select row moves the knobs to another node; the faders stay
        // where the hands left them, and turn that node's knobs on from
        // there by how far they move — nothing on either node jumps.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let board = plugged(&mut app);
        surface(&mut app, &board, 3, 0);
        surface(&mut app, &board, 3, 127);
        assert_eq!(app.params.monitors[0].colour.contrast, 2.0);
        app.act(Action::Focus(Node::Monitor, 1));
        surface(&mut app, &board, 3, 100);
        assert!(
            (app.params.monitors[1].colour.contrast - (1.0 - 27.0 / 127.0)).abs() < 1e-6,
            "{}",
            app.params.monitors[1].colour.contrast
        );
        assert_eq!(app.params.monitors[0].colour.contrast, 2.0);
    }

    #[test]
    fn a_cut_throws_the_switcher_and_the_release_puts_it_back() {
        // The crossfade is fader 8, control 7. A cut runs it to the other end
        // of its travel and the release runs it back; the fader, wherever it
        // stands, turns it by how far it moves.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let board = plugged(&mut app);
        surface(&mut app, &board, 7, 0);
        surface(&mut app, &board, 7, 64);
        let held = app.params.knob(Knob::Switcher, app.focus);
        app.act(Action::Cut(Edge::Down));
        let thrown = app.params.knob(Knob::Switcher, app.focus);
        assert!((thrown - (1.0 - held)).abs() < 1e-6, "{thrown} of {held}");
        // A second press with the pedal already down is not a second cut.
        app.act(Action::Cut(Edge::Down));
        assert_eq!(app.params.knob(Knob::Switcher, app.focus), thrown);
        app.act(Action::Cut(Edge::Up));
        assert!((app.params.knob(Knob::Switcher, app.focus) - held).abs() < 1e-6);
    }

    #[test]
    fn a_reset_before_any_knob_has_turned_does_nothing() {
        // There is no "that one" to mean yet, and the panel must not take a
        // guess at which knob was meant.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let before = app.params.clone();
        app.act(Action::ResetLastKnob);
        assert_eq!(app.params, before);
    }

    #[test]
    fn a_monitor_select_moves_the_monitor_and_not_the_camera() {
        // The rig has more monitors than cameras, so a select that wrote the
        // wrong side of the focus could not land at all: monitor 5 exists and
        // camera 5 does not.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        app.act(Action::Focus(Node::Monitor, 4));
        assert_eq!(app.focus.monitor, 4);
        assert_eq!(app.focus.camera, 0, "the other hand moved");
    }

    #[test]
    fn a_held_cut_belongs_to_the_switcher_it_was_taken_from() {
        // The whole graph is compared before and after: a cut that leaked
        // into another switcher, or a release that put back the wrong one,
        // would both show here.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        app.act(Action::Focus(Node::Switcher, 1));
        let before = app.params.clone();
        app.act(Action::Cut(Edge::Down));
        assert_ne!(app.params.rig.switchers[1], before.rig.switchers[1]);
        assert_eq!(app.params.rig.switchers[0], before.rig.switchers[0]);
        // A second down with the first still held takes nothing.
        app.act(Action::Cut(Edge::Down));
        // The focus moving under a held cut moves nothing: the release owes
        // the throw to the switcher it was taken from.
        app.act(Action::Focus(Node::Switcher, 0));
        app.act(Action::Cut(Edge::Up));
        assert_eq!(app.params, before);
        // Letting go of nothing is nothing.
        app.act(Action::Cut(Edge::Up));
        assert_eq!(app.params, before);
    }

    #[test]
    fn a_reversal_runs_the_switcher_to_the_other_end_of_its_travel() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        app.act(Action::Focus(Node::Switcher, 2));
        let before = app.params.clone();
        app.act(Action::Reverse);
        assert_eq!(
            app.params.rig.switchers[2],
            1.0 - before.rig.switchers[2],
            "In1 and In2 did not trade"
        );
        assert_eq!(
            app.params.rig.switchers[0], before.rig.switchers[0],
            "the other switchers are their own"
        );
        // Pressed again, it is the reverse of the reverse.
        app.act(Action::Reverse);
        assert_eq!(app.params, before);
    }

    #[test]
    fn the_select_puts_the_focused_monitor_on_its_program_or_its_own_camera() {
        // The other half of the routing state, and the only control that
        // moves it. On Direct a monitor shows its own camera whatever the
        // switchers say; on Program it shows the crossfade. The rotating
        // monitor has neither, and the button is dead on it.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        app.act(Action::Focus(Node::Monitor, 0));
        app.params.rig.switchers[0] = 1.0;
        assert!(app.shown().program, "the rig starts on its programs");
        assert_eq!(app.params.route(0, 1), 1.0, "switcher A is at camera B");

        app.act(Action::Select);
        assert!(!app.shown().program);
        assert_eq!(app.params.route(0, 0), 1.0, "direct is the monitor's own");
        assert_eq!(app.params.route(0, 1), 0.0);
        // Only the focused one: structure A's other monitor is still on the
        // program, so it is still showing camera B.
        assert_eq!(app.params.route(1, 1), 1.0, "monitor 2 went with it");

        app.act(Action::Select);
        assert!(app.shown().program, "the latch did not come back");
        assert_eq!(app.params.route(0, 1), 1.0);

        // The rotating monitor shows camera B and has no select to press.
        app.act(Action::Focus(Node::Monitor, 4));
        let before = app.params.clone();
        app.act(Action::Select);
        assert_eq!(app.params, before, "the rotating monitor grew a select");
        assert!(!app.shown().program);
    }

    #[test]
    fn a_beat_belongs_to_the_switchers_whichever_monitor_the_focus_is_on() {
        // The periods live on the four switchers and the rig has five
        // monitors, so a beat that read the focus's monitor index would fall
        // off the end the moment the last monitor was selected.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        app.act(Action::Focus(Node::Monitor, 4));
        app.params.rig.periods[3] = 1;
        let before = app.params.rig.switchers[3];
        app.beat();
        assert_eq!(app.params.rig.switchers[3], 1.0 - before);
    }

    #[test]
    fn the_period_beats_the_focused_switcher_and_a_reset_stops_it() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let board = plugged(&mut app);
        let started = app.params.clone();
        app.act(Action::Focus(Node::Switcher, 1));
        surface(&mut app, &board, 6, 0);
        surface(&mut app, &board, 6, 4);
        assert_eq!(app.params.rig.periods[1], 2);
        surface(&mut app, &board, 7, 127);
        surface(&mut app, &board, 7, 76);
        let stood = app.params.knob(Knob::Switcher, app.focus);
        app.beat();
        assert_eq!(
            app.params.rig.switchers[1], stood,
            "the first pass is not a beat of two"
        );
        app.beat();
        assert_eq!(app.params.rig.switchers[1], 1.0 - stood);
        assert_eq!(
            app.params.rig.switchers[0], started.rig.switchers[0],
            "the other switcher is its own"
        );
        app.beat();
        app.beat();
        assert!((app.params.rig.switchers[1] - stood).abs() < 1e-6);
        // The beats moved the crossfade under the fader and back; the fader
        // turns it on from there, by how far it moved and nothing more.
        surface(&mut app, &board, 7, 90);
        assert!(
            (app.params.rig.switchers[1] - (stood + 14.0 / 127.0 / 4.0)).abs() < 1e-3,
            "{}",
            app.params.rig.switchers[1]
        );

        app.act(Action::Reset);
        assert_eq!(app.params, started);
        for _ in 0..6 {
            app.beat();
        }
        assert_eq!(app.params, started, "a reset panel went on beating");
        // And the grid restarted with the panel: a period of three dialled
        // now beats on the third pass from here.
        app.params.rig.periods[1] = 3;
        let stood = app.params.rig.switchers[1];
        app.beat();
        app.beat();
        assert_eq!(
            app.params.rig.switchers[1], stood,
            "the grid ran on through the reset"
        );
        app.beat();
        assert_ne!(app.params.rig.switchers[1], stood);
    }

    #[test]
    fn a_reset_under_a_held_cut_wins_over_the_release() {
        // The throw the cut owes back is a throw of a panel the reset has
        // replaced, so putting it back would undo the reset one switcher at
        // a time.
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        turn(&mut app, Knob::Switcher, -0.5);
        assert_ne!(app.params, app.initial);
        app.act(Action::Cut(Edge::Down));
        app.act(Action::Reset);
        assert_eq!(app.params, app.initial);
        app.act(Action::Cut(Edge::Up));
        assert_eq!(app.params, app.initial);
    }
    const PROOF_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    fn overlaid(app: &App, size: (u32, u32)) -> (Feedback, Present, Overlay) {
        let (device, queue) = (&app.gpu.device, &app.gpu.queue);
        let mut source = pollster::block_on(Source::open(
            &crate::input::Input::Pattern(crate::input::Pattern::Bars),
            size,
        ))
        .unwrap();
        let mut feedback = Feedback::new(device, size.0, size.1, &app.params);
        feedback.write_seed(queue, source.frame().unwrap());
        for _ in 0..3 {
            feedback.step(device, queue, &app.params);
        }
        let present = Present::new(device, &feedback, PROOF_FORMAT);
        let mut overlay = Overlay::new(device, queue, PROOF_FORMAT);
        overlay.show(device, queue, app.readout());
        (feedback, present, overlay)
    }

    #[test]
    fn the_overlay_draws_its_panel_sources_and_arrows_on_the_gpu() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        app.overlay_shown = true;
        let (feedback, present, overlay) = overlaid(&app, (64, 64));
        let (device, queue) = (&app.gpu.device, &app.gpu.queue);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay test"),
            size: wgpu::Extent3d {
                width: 192,
                height: 128,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PROOF_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        for view in [View::Bank { focus: Some(0) }, View::Solo(0)] {
            present.draw(
                device,
                queue,
                &target,
                &feedback,
                view,
                Some((&overlay, &app.params)),
            );
        }
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the overlay's passes ran");
    }

    fn proof(app: &App, name: &str, view: View) {
        let Some(dir) = std::env::var_os("LIGHTHERDER_PROOF_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let (device, queue) = (&app.gpu.device, &app.gpu.queue);
        let (feedback, present, overlay) = overlaid(app, (640, 360));
        let mut capture = Capture::still(device, &dir, (1920, 1080), PROOF_FORMAT).unwrap();
        capture
            .frame(
                device,
                queue,
                &present,
                &feedback,
                view,
                Some((&overlay, &app.params)),
            )
            .unwrap();
        let path = capture.finish().unwrap();
        std::fs::rename(&path, dir.join(format!("{name}.png"))).unwrap();
    }

    #[test]
    fn the_overlay_reads_the_knob_the_program_holds_and_not_where_the_fader_stands() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let board = plugged(&mut app);
        let fader = 7;
        let reads = |app: &App| app.readout().reads(Knob::Switcher);
        assert_eq!(reads(&app), "1.000");

        surface(&mut app, &board, fader, 127);
        surface(&mut app, &board, fader, 64);
        let moved = app.params.knob(Knob::Switcher, app.focus);
        assert!((moved - (1.0 - 63.0 / 127.0 / 4.0)).abs() < 1e-5, "{moved}");
        assert_eq!(reads(&app), Knob::Switcher.reads(moved));

        app.act(Action::Focus(Node::Switcher, 1));
        assert_eq!(app.midi.standing(fader), Some(64));
        assert_eq!(reads(&app), "1.000", "a page flip: the fader stands at 64");
        app.act(Action::Focus(Node::Switcher, 0));
        assert_eq!(reads(&app), Knob::Switcher.reads(moved));

        surface(&mut app, &board, fader, 20);
        app.act(Action::ResetLastKnob);
        assert_eq!(app.midi.standing(fader), Some(20));
        assert_eq!(app.params.knob(Knob::Switcher, app.focus), 1.0);
        assert_eq!(reads(&app), "1.000", "a rewind: the fader stands at 20");
        app.overlay_shown = true;
        proof(&app, "overlay-true-value", View::Bank { focus: None });
        proof(&app, "overlay-solo", View::Solo(0));
    }

    #[test]
    fn the_overlay_shows_the_dataflow_of_the_graph_being_played() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        use crate::params::{End, Flow};
        let camera_b_on_the_b_pair = |app: &App| {
            app.params
                .flows()
                .filter(|f| f.from == End::Camera(1) && matches!(f.to, End::Monitor(2 | 3)))
                .collect::<Vec<Flow>>()
        };
        assert_eq!(camera_b_on_the_b_pair(&app), []);
        app.act(Action::Focus(Node::Switcher, 1));
        turn(&mut app, Knob::Switcher, -0.5);
        let fed = camera_b_on_the_b_pair(&app);
        assert_eq!(fed.len(), 2, "{fed:?}");
        assert!(fed.iter().all(|f| (f.share - 0.5).abs() < 1e-6), "{fed:?}");
        app.act(Action::Focus(Node::Monitor, 2));
        app.act(Action::Flip(crate::affine::Axis::X));
        app.overlay_shown = true;
        proof(&app, "overlay-arrows", View::Bank { focus: None });
    }

    #[test]
    fn rotaries_3_and_4_turn_what_the_overlay_reads() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let board = plugged(&mut app);
        app.overlay_shown = true;
        for (rotary, knob, name) in [
            (18, Knob::Delay, "delay"),
            (19, Knob::FrameRate, "frame-rate"),
        ] {
            let before = app.readout().reads(knob);
            proof(&app, &format!("{name}-before"), View::Solo(0));
            surface(&mut app, &board, rotary, 20);
            assert_eq!(app.readout().reads(knob), before, "the first word places");
            surface(&mut app, &board, rotary, 127);
            let after = app.readout().reads(knob);
            assert_ne!(after, before, "{knob:?}");
            proof(&app, &format!("{name}-after"), View::Solo(0));
        }
        assert_eq!(app.readout().reads(Knob::Delay), "2");
        assert_eq!(app.readout().reads(Knob::FrameRate), "24");
    }

    #[test]
    fn a_select_and_a_reset_forgive_the_half_step_a_count_knob_was_owed() {
        let Some(mut app) = playing(config::instrument()) else {
            return;
        };
        let board = plugged(&mut app);
        let delays = |app: &App| app.params.cameras.map(|cam| cam.delay);
        surface(&mut app, &board, 18, 20);
        surface(&mut app, &board, 18, 50);
        assert_eq!(delays(&app), [0, 0, 0]);
        app.act(Action::Focus(Node::Camera, 1));
        surface(&mut app, &board, 18, 60);
        assert_eq!(
            delays(&app),
            [0, 0, 0],
            "camera 2 is not paid camera 1's debt"
        );
        surface(&mut app, &board, 18, 80);
        assert_eq!(delays(&app), [0, 0, 0]);
        app.act(Action::Reset);
        surface(&mut app, &board, 18, 90);
        assert_eq!(delays(&app), [0, 0, 0], "a reset owes nothing");
        surface(&mut app, &board, 18, 127);
        assert_eq!(delays(&app), [0, 1, 0]);
        surface(&mut app, &board, 18, 100);
        surface(&mut app, &board, 18, 110);
        assert_eq!(delays(&app), [0, 0, 0]);
        app.act(Action::ResetLastKnob);
        surface(&mut app, &board, 18, 113);
        assert_eq!(delays(&app), [0, 0, 0], "a rewind owes nothing");
    }
}
