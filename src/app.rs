//! Window, surface and the run loop.

use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use crate::cli::Cli;
use crate::feedback::Feedback;
use crate::gpu::Gpu;
use crate::input::Source;
use crate::keys::{action_for, Action};
use crate::midi::{Map, Midi};
use crate::overlay::Overlay;
use crate::params::{Focus, Params};
use crate::present::Present;

/// Borderless rather than exclusive: the instrument renders at its own
/// resolution and lets the compositor scale, so taking a video mode from the
/// display would buy nothing and cost a mode switch on every toggle.
fn borderless(fullscreen: bool) -> Option<winit::window::Fullscreen> {
    fullscreen.then_some(winit::window::Fullscreen::Borderless(None))
}

/// One pass every sixtieth of a second. The instrument evolves one pass per
/// frame, so the frame rate *is* the tempo: a rate that is not sixty plays
/// the piece at the wrong speed.
const BEAT: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// The deadline after `due`, for a pass that went out at `now`.
///
/// Deadlines are absolute — each is the last one plus a beat, never "now
/// plus a beat" — so a compositor grid faster than sixty still leaves sixty
/// passes in a second rather than one per slot. Taking that grid for the
/// tempo is what played the piece a fifth fast under the TV's nested
/// gamescope, which hands out about 72 Hz (#11 — measured; the same chain in
/// Immediate runs 2400 fps, so nothing here is ever short of time).
///
/// Deadlines `now` has overtaken are dropped rather than owed: a machine
/// that could not keep up must not then run the passes it missed back to
/// back, which is a lurch in the tempo rather than a repair of one.
fn next_due(due: Instant, now: Instant) -> Instant {
    let mut due = due + BEAT;
    while due <= now {
        due += BEAT;
    }
    due
}

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    feedback: Feedback,
    present: Present,
    /// The controls overlay, built once from the map in force — the map
    /// cannot change while the instrument runs, so neither can this.
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
    /// The camera and monitor the knobs act on.
    focus: Focus,
    /// The running external inputs, in `params.inputs` order, which is the
    /// order `Feedback::write_input` indexes them by.
    sources: Vec<Source>,
    /// Where the preset slots are kept.
    slots: std::path::PathBuf,
    /// The control surface, connected or not — it is looked for while the
    /// instrument runs rather than at startup, so plugging one in mid-piece
    /// is the whole of setting it up.
    midi: Midi,
    /// Whether shift is down, which only the slot keys read.
    shift: bool,
    /// How big every monitor is — see [`crate::cli::DEFAULT_RESOLUTION`], and
    /// note that the window has nothing to do with it.
    resolution: (u32, u32),
    /// Whether the window covers the display. Kept here rather than asked of
    /// the window, because it is also what the window is *created* with.
    fullscreen: bool,
    /// Whether the controls overlay is showing. Off at startup: the overlay
    /// is help, and help is what the cycle button and backquote are for.
    overlay_shown: bool,
    /// Passes since the last rate line, and when that was. A deadline can
    /// only hold the loop back, never push it, so a line under sixty is the
    /// whole reason to print one.
    frames: u32,
    metered: Instant,
    /// When the next pass falls due — see [`next_due`], which is where the
    /// tempo is kept rather than in the display.
    due: Instant,
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
    let slots = crate::slots::default_dir();
    log::info!("preset slots: {}", slots.display());
    // Read before the window opens, like the inputs and for the same reason:
    // a map that will not load is a terminal error, not a surface that turns
    // out to be playing the wrong knobs once there is light on the glass.
    let map = Map::load(&slots)?;
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
            slots,
            midi,
            shift: false,
            resolution: cli.resolution,
            fullscreen: cli.fullscreen,
            overlay_shown: false,
            frames: 0,
            metered: Instant::now(),
            due: Instant::now(),
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
        // Fifo, so a frame reaches the glass whole and every backend can
        // present at all — `get_default_config` takes whatever the adapter
        // lists first. It is not the clock, though: [`next_due`] is, because
        // the vertical blank Fifo stands in for is the compositor's to
        // invent, and the TV's nested gamescope invents a grid of about
        // 72 Hz (#11).
        config.present_mode = wgpu::PresentMode::Fifo;
        // Focused under that same gamescope, a presented buffer comes back
        // one composite hop late (app → Xwayland → gamescope 4K → GNOME),
        // and at the default latency of two the acquire blocks for a whole
        // slot: every other pass lands a slot late and the tempo sits at 40
        // instead of 60 (GPU and compositor were both measured idle-fast;
        // the main thread spent the gap in DRM syncobj waits). A third frame
        // in flight absorbs the chain's round trip; the extra 16.7 ms to the
        // screen is invisible in an instrument whose knobs are the input.
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
        let overlay = Overlay::new(&gpu.device, &gpu.queue, format, map);

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

    /// Whether a pass ran. A surface with no texture to give is the one way
    /// a redraw evolves nothing, and the caller counts passes rather than
    /// attempts so that a stale surface reads as the rate it really is.
    fn render(
        &mut self,
        gpu: &Gpu,
        params: &Params,
        sources: &mut [Source],
        overlay_shown: bool,
    ) -> bool {
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
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Before the cameras read the bank, not after.
        for (i, source) in sources.iter_mut().enumerate() {
            if let Some(frame) = source.frame() {
                self.feedback.write_input(&gpu.queue, i, frame);
            }
        }

        self.feedback.step(&gpu.device, &gpu.queue, params);
        self.present.draw(
            &gpu.device,
            &gpu.queue,
            &target,
            (self.config.width, self.config.height),
            &self.feedback,
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

    /// Take `params` as the live graph, rebuilding whatever no longer serves
    /// it: the inputs are reopened when they changed, and the bank and its
    /// presenter are rebuilt when the layer counts moved — which is also when
    /// the inputs that stayed are asked to upload again. A slot stores the
    /// whole panel, so a recall reconstructs the rig rather than refusing a
    /// graph shaped differently from what is playing. A same-shape adopt
    /// still touches neither, so the loops keep running: what it changes is
    /// the knobs the next pass reads, not the light already on the glass.
    ///
    /// Fallible because a graph can ask for more bank than the cap allows,
    /// or for an input that will not open — and everything fallible is done
    /// before anything is torn down, so an error leaves the running rig
    /// exactly as it was.
    ///
    /// The one way `self.params` is replaced, because the focus was walked
    /// on the old one and a graph with fewer cameras would leave it pointing
    /// at nothing — every read of it, the readout included, indexes straight
    /// in. Two callers, one of which used to forget.
    fn adopt(&mut self, params: Params) -> Result<(), String> {
        crate::feedback::bank_fits(&params, self.resolution)?;
        // The layer counts are baked into the bank's textures, so a graph
        // that moved either one gets a new bank below.
        let rebank = params.monitors.len() != self.params.monitors.len()
            || params.inputs.len() != self.params.inputs.len();
        let sources = (params.inputs != self.params.inputs)
            .then(|| {
                params
                    .inputs
                    .iter()
                    .map(|input| Source::open(input, self.resolution))
                    .collect::<Result<Vec<Source>, String>>()
            })
            .transpose()?;
        match sources {
            Some(sources) => self.sources = sources,
            // The inputs are the same ones, but a rebuilt bank is a blank
            // one and a still pattern uploads exactly once — so the sources
            // that stayed have to hand their frames over again.
            None if rebank => self.sources.iter_mut().for_each(Source::replay),
            None => {}
        }
        // Blanked, as any bank is at creation, which is what a rig with
        // different monitors means anyway.
        if let Some(live) = self.live.as_mut() {
            if rebank {
                let (width, height) = self.resolution;
                live.feedback = Feedback::new(&self.gpu.device, width, height, &params);
                live.present = Present::new(&self.gpu.device, &live.feedback, live.config.format);
            }
        }
        self.focus = self.focus.clamped(&params);
        self.params = params;
        // The whole panel just moved without a fader moving with it.
        self.midi.release();
        Ok(())
    }

    /// Rebuild the rig from a slot. The only refusals are a slot that will
    /// not read and a graph the instrument would refuse at startup; either
    /// way the running rig plays on untouched.
    fn recall(&mut self, slot: usize) {
        let params = match crate::slots::recall(&self.slots, slot) {
            Ok(params) => params,
            Err(why) => return log::error!("slot {}: {why}", slot + 1),
        };
        match self.adopt(params) {
            Ok(()) => log::info!("slot {}: {}", slot + 1, self.params.describe(self.focus)),
            Err(why) => log::error!("slot {}: {why}", slot + 1),
        }
    }

    /// Point the camera knobs at one camera by its place in the graph. A
    /// select row is wider than most graphs, so a press past the end does
    /// nothing: sliding to the last camera instead would make every button
    /// past the end the same button.
    fn focus_camera(&mut self, camera: usize) {
        if camera < self.params.cameras.len() {
            self.refocus(Focus {
                camera,
                ..self.focus
            });
        }
    }

    /// Point the knobs at another node. The one way `self.focus` moves, for
    /// the same reason [`App::adopt`] is the one way `params` is replaced: a
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
        }
        log::info!("{}", self.params.describe(self.focus));
    }

    /// One line a second on how the tempo is going, counting the passes that
    /// ran rather than the redraws attempted. The instrument is deployed
    /// fullscreen on a display, so the log is the only place a number can be
    /// read at all — and a rate that has fallen under sixty is the first
    /// thing to know, whether a graph is too much for the machine or a
    /// display path is holding the piece back.
    fn meter(&mut self, ran: bool) {
        self.frames += u32::from(ran);
        let elapsed = self.metered.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let fps = self.frames as f64 / elapsed.as_secs_f64();
        log::info!("{fps:.0} fps ({:.1} ms/frame)", 1e3 / fps);
        self.frames = 0;
        self.metered = Instant::now();
    }

    /// One action, from wherever it came. The keyboard and the control
    /// surface both land here and nowhere else, so a binding cannot mean one
    /// thing under a finger and another under a fader.
    fn apply(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::Nudge(knob, delta) => {
                self.params.nudge(knob, delta, self.focus);
                log::info!("{}", self.params.describe(self.focus));
            }
            Action::Set(knob, value) => {
                self.params.set(knob, value, self.focus);
                log::info!("{}", self.params.describe(self.focus));
            }
            Action::NextCamera => {
                let camera = (self.focus.camera + 1) % self.params.cameras.len();
                self.refocus(Focus {
                    camera,
                    ..self.focus
                });
            }
            Action::FocusCamera(camera) => self.focus_camera(camera),
            Action::NextMonitor => {
                let monitor = (self.focus.monitor + 1) % self.params.monitors.len();
                self.refocus(Focus {
                    monitor,
                    ..self.focus
                });
            }
            Action::Store(slot) => match crate::slots::store(&self.slots, slot, &self.params) {
                Ok(path) => log::info!("slot {}: wrote {}", slot + 1, path.display()),
                Err(why) => log::error!("slot {}: {why}", slot + 1),
            },
            Action::Recall(slot) => self.recall(slot),
            Action::Reset => match self.adopt(self.initial.clone()) {
                Ok(()) => log::info!("reset: {}", self.params.describe(self.focus)),
                Err(why) => log::error!("reset: {why}"),
            },
            Action::Clear => {
                if let Some(live) = self.live.as_mut() {
                    live.feedback.clear(&self.gpu.device, &self.gpu.queue);
                    log::info!("cleared");
                }
            }
            Action::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                if let Some(live) = self.live.as_ref() {
                    live.window.set_fullscreen(borderless(self.fullscreen));
                    live.window.set_cursor_visible(!self.fullscreen);
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
            Action::Quit => event_loop.exit(),
        }
    }
}

impl ApplicationHandler for App {
    /// The tempo's own wake-up: past the first frame this is where every
    /// redraw is asked for, so what decides when a pass happens is the
    /// deadline — not the end of the last pass, and not the swapchain.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            if let Some(live) = self.live.as_ref() {
                live.window.request_redraw();
            }
        }
    }

    /// Waking early — a key, a fader, a resize — leaves the deadline where
    /// it was; with no window there is no pass to be on time for, and a
    /// deadline already gone by would spin the loop rather than wait in it.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(match self.live {
            Some(_) => ControlFlow::WaitUntil(self.due),
            None => ControlFlow::Wait,
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
        // The rate is counted from the first frame, not from before the
        // adapter, the device and the pipelines were built — half a second of
        // startup inside the first window would report a rate the instrument
        // never ran at, and that first line is what a deploy is read off.
        self.metered = Instant::now();
        self.frames = 0;
        self.live = Some(live);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.shift = modifiers.state().shift_key(),
            WindowEvent::Resized(size) => {
                if let Some(live) = self.live.as_mut() {
                    live.resize(&self.gpu, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                // The surface is read once a frame, and each message is
                // turned into an action against the panel the message
                // before it left — not against a snapshot of the whole
                // batch. A slot button and a fader inside one frame is a
                // real two-handed gesture, and resolved against a
                // snapshot the fader would be dragging a knob back out
                // of the preset the button just recalled.
                for message in self.midi.poll() {
                    let Some(action) = self.midi.action_for(message, &self.params, self.focus)
                    else {
                        continue;
                    };
                    self.apply(action, event_loop);
                }
                // And the panel is written every redraw — see [`Midi::show`]
                // for why every redraw and not each place the focus moves.
                self.midi.show(self.focus.camera);
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // A redraw the windowing system asked for on its own — an
                // X11 expose, a `WM_PAINT` — arrives whenever it likes, and
                // a pass *is* the tempo, so drawing one early plays the piece
                // fast. The frame it wanted goes out with the next
                // deadline's, at most a beat away.
                if Instant::now() < self.due {
                    return;
                }
                let ran = live.render(
                    &self.gpu,
                    &self.params,
                    &mut self.sources,
                    self.overlay_shown,
                );
                // Dated from after the pass, not before it: a pass that
                // overran has already spent the deadlines it went past, and
                // dating the next one from before would run one of them at
                // once.
                self.due = next_due(self.due, Instant::now());
                self.meter(ran);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                // Repeats are wanted: holding a key sweeps its knob.
                if let Some(action) = action_for(code, self.shift) {
                    self.apply(action, event_loop);
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

    /// A directory of this test's own, like the slots tests keep.
    fn scratch(what: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lightherder-app-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// An instrument playing `params`, headless: no window has opened, so
    /// `live` is `None` the way it is before `resumed` — which is also the
    /// state the bank rebuild has to be right about. `None` when the machine
    /// has no adapter, said on stderr the way tests/feedback_gpu.rs skips.
    fn playing(params: Params, slots: std::path::PathBuf) -> Option<App> {
        let gpu = match pollster::block_on(Gpu::open(None, "app test")) {
            Ok(gpu) => gpu,
            Err(why) => {
                use std::io::Write;
                writeln!(std::io::stderr(), "skipping an app test: {why}").unwrap();
                return None;
            }
        };
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
            slots: slots.clone(),
            // No file in a scratch directory, so this is the factory map.
            midi: Midi::new(Map::load(&slots).unwrap()).unwrap(),
            shift: false,
            resolution,
            fullscreen: false,
            overlay_shown: false,
            frames: 0,
            metered: Instant::now(),
            due: Instant::now(),
            live: None,
            failed: None,
        })
    }

    #[test]
    fn an_on_time_pass_owes_one_beat() {
        let start = Instant::now();
        assert_eq!(next_due(start, start), start + BEAT);
        assert_eq!(next_due(start, start + BEAT / 2), start + BEAT);
        // Landing exactly on the deadline is on time, not late: the beat
        // after it is a whole beat away, not due at once.
        assert_eq!(next_due(start, start + BEAT), start + BEAT * 2);
    }

    #[test]
    fn a_stall_drops_the_passes_it_missed() {
        let start = Instant::now();
        // Three deadlines went by inside one pass. What follows is the next
        // pass, not the three missed ones run back to back.
        let stalled = start + BEAT * 3 + BEAT / 2;
        assert_eq!(next_due(start, stalled), start + BEAT * 4);
    }

    #[test]
    fn a_faster_grid_still_gets_sixty_passes_a_second() {
        // The bug this pacing exists for (#11): a compositor grid faster
        // than sixty — the TV's nested gamescope hands out about 72 Hz —
        // must still leave sixty passes in a second, not one per slot.
        let start = Instant::now();
        let slot = Duration::from_nanos(1_000_000_000 / 72);
        let second = Duration::from_secs(1);
        let mut due = start;
        let mut now = start;
        let mut passes = 0u32;
        loop {
            // A pass waits for its deadline, then for the grid slot the
            // swapchain will hand it — the two waits this loop lives with.
            let waited = due.max(now);
            let slots = (waited - start).as_nanos().div_ceil(slot.as_nanos()) as u32;
            now = start + slot * slots;
            if now - start >= second {
                break;
            }
            passes += 1;
            due = next_due(due, now);
        }
        assert_eq!(passes, 60);
    }

    #[test]
    fn a_recall_rebuilds_the_rig_across_graph_shapes() {
        // The couch flow of issue #10: the Play button launches the default
        // rig, and a slot holds the webcam rig — one more input, one more
        // camera. `external` is that rig with the capture device swapped
        // for the bars, so the test runs on machines with no webcam.
        let dir = scratch("cross-shape");
        let stored = config::external();
        crate::slots::store(&dir, 0, &stored).unwrap();
        crate::slots::store(&dir, 1, &config::single()).unwrap();
        // The same graph with a second monitor, for the sideways recall below.
        let mut wider = stored.clone();
        wider.monitors.push(wider.monitors[0].clone());
        wider.routing = vec![vec![1.0, 1.0]; 2];
        for camera in &mut wider.cameras {
            camera.look.push(0.0);
        }
        crate::slots::store(&dir, 2, &wider).unwrap();
        let Some(mut app) = playing(config::single(), dir.clone()) else {
            return;
        };
        assert!(app.sources.is_empty());

        app.recall(0);
        assert_eq!(app.params, stored);
        assert_eq!(app.sources.len(), stored.inputs.len());

        // Sideways, to the same rig with a second monitor: the inputs did not
        // change but the bank is rebuilt, and a new bank is blank. A still
        // pattern hands its frame over exactly once, so a source carried
        // across that swap would leave its layer black for the rest of the
        // run — the reopen has to follow the bank, not just the inputs.
        assert!(app.sources[0].frame().is_some(), "nothing to upload");
        assert!(app.sources[0].frame().is_none(), "uploaded twice");
        app.recall(2);
        assert_eq!(app.params.inputs, stored.inputs, "the same bars");
        assert!(
            app.sources[0].frame().is_some(),
            "the rebuilt bank never got the bars"
        );

        // And back down: the focus walked onto the second camera, which the
        // recalled graph does not have, so the recall has to land it inside.
        app.focus = Focus {
            camera: 1,
            monitor: 0,
        };
        app.recall(1);
        assert_eq!(app.params, config::single());
        assert!(app.sources.is_empty());
        assert_eq!(app.focus, Focus::default());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_camera_the_graph_does_not_have_is_a_button_that_does_nothing() {
        // The select row is eight wide and every shipped graph is shallower,
        // so most of a set is played with some of it pointing past the end.
        // No slot is written, so nothing makes the directory and there is
        // nothing to take away afterwards.
        let Some(mut app) = playing(config::crossed(), scratch("select-past-the-end")) else {
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
    fn a_slot_that_will_not_read_leaves_the_rig_playing() {
        let dir = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(crate::slots::path(&dir, 0), "not toml [").unwrap();
        let Some(mut app) = playing(config::external(), dir.clone()) else {
            return;
        };
        let before = app.params.clone();
        app.recall(0); // corrupt
        app.recall(1); // empty
        assert_eq!(app.params, before);
        assert_eq!(app.sources.len(), before.inputs.len());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
