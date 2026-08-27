//! Window, surface and the run loop.

use std::sync::Arc;

use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
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
use crate::params::{Focus, Knob, Params};
use crate::present::Present;

/// Borderless rather than exclusive: the instrument renders at its own
/// resolution and lets the compositor scale, so taking a video mode from the
/// display would buy nothing and cost a mode switch on every toggle.
fn borderless(fullscreen: bool) -> Option<winit::window::Fullscreen> {
    fullscreen.then_some(winit::window::Fullscreen::Borderless(None))
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
    /// Which knob the automation keys act on: the last one turned by hand.
    /// The *node* they act on is `focus`, the same one every other key
    /// follows — so `p` and `a` reach the same monitor, always. The readout
    /// names the knob, since nothing on screen otherwise would.
    touched: Knob,
    /// Where the automation's clock is read from. Not reset by Reset: a knob
    /// jumping back to where it was is what Reset is for, and a phase jumping
    /// with it would show up as a lurch in every LFO at once.
    started: Instant,
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
    /// Frames since the last rate line, and when that was. The loop is paced
    /// by the vertical blank, so this reads the refresh rate until a frame
    /// stops fitting inside one — which is the whole reason to print it.
    frames: u32,
    metered: Instant,
    live: Option<Live>,
}

/// The window the instrument opens in. On the web it is the page's own
/// canvas — the one the stylesheet has already stretched over the viewport,
/// so "fullscreen" there is the page rather than anything winit does.
#[cfg(target_arch = "wasm32")]
fn attributes(_fullscreen: bool) -> winit::window::WindowAttributes {
    use winit::platform::web::WindowAttributesExtWebSys;
    Window::default_attributes().with_canvas(Some(crate::web::canvas()))
}

#[cfg(not(target_arch = "wasm32"))]
fn attributes(fullscreen: bool) -> winit::window::WindowAttributes {
    Window::default_attributes()
        .with_title("lightherder")
        .with_fullscreen(borderless(fullscreen))
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
    Ok(event_loop.run_app(&mut app)?)
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
    event_loop.set_control_flow(ControlFlow::Poll);
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
            // So `p` does something worth seeing before any knob has been turned.
            touched: Knob::Rotation,
            started: Instant::now(),
            sources,
            slots,
            midi,
            shift: false,
            resolution: cli.resolution,
            fullscreen: cli.fullscreen,
            overlay_shown: false,
            frames: 0,
            metered: Instant::now(),
            live: None,
        },
    )
}

impl Live {
    fn new(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        params: &Params,
        map: &Map,
        resolution: (u32, u32),
        fullscreen: bool,
    ) -> Live {
        let window = Arc::new(
            event_loop
                .create_window(attributes(fullscreen))
                .expect("create window"),
        );
        window.set_cursor_visible(!fullscreen);

        let surface = gpu
            .instance
            .create_surface(window.clone())
            .expect("create surface");
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .expect("adapter cannot draw to this surface");
        // The vertical blank is this instrument's clock, and not for smoothness:
        // the loop evolves one pass per frame, so the frame rate is a tempo. A
        // camera that pulls back 0.6% and turns 0.05 rad per pass draws its
        // spiral in a second at sixty and in a twenty-fifth of one at fifteen
        // hundred. `get_default_config` takes whatever mode the adapter lists
        // first — Mailbox on this hardware, which ran the analog preset at
        // 1500 fps on a 60 Hz display — so the mode is pinned rather than
        // taken. Fifo is the one every backend must support.
        config.present_mode = wgpu::PresentMode::Fifo;
        // Focused under the TV's nested gamescope, a presented buffer comes
        // back one composite hop late (app → Xwayland → gamescope 4K →
        // GNOME), and at the default latency of two the acquire blocks past
        // the vblank: every other frame lands a slot late and the tempo sits
        // at 40 instead of 60 (#11 — GPU and compositor were both measured
        // idle-fast; the main thread spent the gap in DRM syncobj waits). A
        // third frame in flight absorbs the chain's round trip; the extra
        // 16.7 ms to the screen is invisible in an instrument whose knobs
        // are the input.
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

        Live {
            window,
            surface,
            config,
            feedback,
            present,
            overlay,
        }
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

    fn render(&mut self, gpu: &Gpu, params: &Params, sources: &mut [Source], overlay_shown: bool) {
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
                return;
            }
            other => {
                log::warn!("dropped a frame: {other:?}");
                return;
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Before the cameras read the bank, not after.
        for (i, source) in sources.iter_mut().enumerate() {
            if let Some(frame) = source.frame() {
                self.feedback.write_input(&gpu.queue, i, &frame);
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
    }
}

impl App {
    /// Take `params` as the live graph, rebuilding whatever no longer serves
    /// it: the inputs are reopened when they changed, and the bank and its
    /// presenter are rebuilt when the layer counts moved — a slot stores the
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
        let sources = (params.inputs != self.params.inputs)
            .then(|| {
                params
                    .inputs
                    .iter()
                    .map(|input| Source::open(input, self.resolution))
                    .collect::<Result<Vec<Source>, String>>()
            })
            .transpose()?;
        if let Some(sources) = sources {
            self.sources = sources;
        }
        // The layer counts are baked into the bank's textures, so a graph
        // that changed either gets a new bank — blanked, as any bank is at
        // creation, which is what a rig with different monitors means anyway.
        if let Some(live) = self.live.as_mut() {
            if params.monitors.len() != self.params.monitors.len()
                || params.inputs.len() != self.params.inputs.len()
            {
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
            Ok(()) => log::info!("slot {}: {}", slot + 1, self.describe()),
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
        log::info!("{}", self.describe());
    }

    fn describe(&self) -> String {
        format!(
            "{}\nmotion keys on: {}",
            self.params.describe(self.focus),
            self.touched.name()
        )
    }

    /// One line a second on how the frame is going. The instrument is
    /// deployed fullscreen on a display, so the log is the only place a
    /// number can be read at all — and a rate that has left sixty is the
    /// first thing to know when a graph is too much for the machine.
    fn meter(&mut self) {
        self.frames += 1;
        let elapsed = self.metered.elapsed();
        if elapsed < std::time::Duration::from_secs(1) {
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
                self.touched = knob;
                log::info!("{}", self.describe());
            }
            Action::Set(knob, value) => {
                self.params.set(knob, value, self.focus);
                self.touched = knob;
                log::info!("{}", self.describe());
            }
            Action::Motion => {
                let now = self.started.elapsed().as_secs_f64();
                self.params.motion_cycle(self.touched, self.focus, now);
                log::info!("{}", self.describe());
            }
            Action::MotionRate(steps) => {
                let now = self.started.elapsed().as_secs_f64();
                self.params
                    .motion_rate(self.touched, self.focus, steps, now);
                log::info!("{}", self.describe());
            }
            Action::MotionDepth(steps) => {
                self.params.motion_depth(self.touched, self.focus, steps);
                log::info!("{}", self.describe());
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
                Ok(()) => log::info!("reset: {}", self.describe()),
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
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        let live = Live::new(
            event_loop,
            &self.gpu,
            &self.params,
            self.midi.map(),
            self.resolution,
            self.fullscreen,
        );
        log::info!(
            "{} monitors of {}x{}, {} cameras, {} inputs",
            self.params.monitors.len(),
            self.resolution.0,
            self.resolution.1,
            self.params.cameras.len(),
            self.params.inputs.len(),
        );
        log::info!("{}", self.describe());
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
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // The automation is read here and nowhere else: the knobs the
                // GPU is handed are the stored ones offset by whatever is
                // driving them at this instant, and `self.params` — what the
                // keys turn and what a preset slot saves — is untouched.
                let now = self.started.elapsed().as_secs_f64();
                live.render(
                    &self.gpu,
                    &self.params.modulated(now),
                    &mut self.sources,
                    self.overlay_shown,
                );
                live.window.request_redraw();
                self.meter();
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
            touched: Knob::Rotation,
            started: Instant::now(),
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
            live: None,
        })
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
        let Some(mut app) = playing(config::single(), dir.clone()) else {
            return;
        };
        assert!(app.sources.is_empty());

        app.recall(0);
        assert_eq!(app.params, stored);
        assert_eq!(app.sources.len(), stored.inputs.len());

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
