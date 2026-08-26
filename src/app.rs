//! Window, surface and the run loop.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use crate::cli::Cli;
use crate::feedback::Feedback;
use crate::input::Source;
use crate::keys::{action_for, Action};
use crate::midi::{Map, Midi};
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
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    feedback: Feedback,
    present: Present,
}

pub struct App {
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
    started: std::time::Instant,
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
    /// Frames since the last rate line, and when that was. The loop is paced
    /// by the vertical blank, so this reads the refresh rate until a frame
    /// stops fitting inside one — which is the whole reason to print it.
    frames: u32,
    metered: std::time::Instant,
    live: Option<Live>,
}

/// `params` is the loaded graph, already validated by `config::load`; `cli`
/// says how big its monitors are and whether the window covers the display.
///
/// The inputs are opened before the window is, so a file that is not there or
/// a device that will not open says so on the terminal instead of behind a
/// black layer of a running instrument.
pub fn run(params: Params, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(event_loop.run_app(&mut App {
        initial: params.clone(),
        params,
        focus: Focus::default(),
        // So `p` does something worth seeing before any knob has been turned.
        touched: Knob::Rotation,
        started: std::time::Instant::now(),
        sources,
        slots,
        midi,
        shift: false,
        resolution: cli.resolution,
        fullscreen: cli.fullscreen,
        frames: 0,
        metered: std::time::Instant::now(),
        live: None,
    })?)
}

impl Live {
    async fn new(
        event_loop: &ActiveEventLoop,
        params: &Params,
        resolution: (u32, u32),
        fullscreen: bool,
    ) -> Live {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("lightherder")
                        .with_fullscreen(borderless(fullscreen)),
                )
                .expect("create window"),
        );
        window.set_cursor_visible(!fullscreen);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: crate::BACKENDS,
            // Some backends need a display handle before any surface exists.
            ..wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone()))
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("no GPU adapter can draw to this window");
        log::info!("adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lightherder"),
                ..Default::default()
            })
            .await
            .expect("request device");

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
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
        let format = config.format;
        surface.configure(&device, &config);
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
        let feedback = Feedback::new(&device, resolution.0, resolution.1, params);
        let present = Present::new(&device, &feedback, format);

        Live {
            window,
            surface,
            device,
            queue,
            config,
            feedback,
            present,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self, params: &Params, sources: &mut [Source]) {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            // Suboptimal still hands back a usable texture, and the next
            // resize reconfigures the surface anyway.
            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
            // The surface goes stale on resize, on a monitor change and on
            // compositor restarts. Reconfiguring and skipping one frame is the
            // whole recovery.
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
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
                self.feedback.write_input(&self.queue, i, &frame);
            }
        }

        self.feedback.step(&self.device, &self.queue, params);
        self.present.draw(
            &self.device,
            &self.queue,
            &target,
            (self.config.width, self.config.height),
            &self.feedback,
        );
        self.queue.present(frame);
    }
}

impl App {
    /// Take `params` as the live graph. The one way `self.params` is
    /// replaced, because the focus was walked on the old one and a graph with
    /// fewer cameras would leave it pointing at nothing — every read of it,
    /// the readout included, indexes straight in. Two callers, one of which
    /// used to forget.
    fn adopt(&mut self, params: Params) {
        self.focus = self.focus.clamped(&params);
        self.params = params;
        // The whole panel just moved without a fader moving with it, so every
        // fader has to find its knob again. Otherwise the first one brushed
        // afterwards throws its knob back to where the fader was standing,
        // which is the recall undone one knob at a time.
        self.midi.release();
    }

    /// Point the knobs at another node. The one way `self.focus` moves, for
    /// the same reason [`App::adopt`] is the one way `params` is replaced: a
    /// fader that has caught a knob is holding *that* node's knob, and the
    /// new node's is somewhere else entirely — without letting go, the next
    /// rotary touched throws it to wherever the fader is standing.
    fn refocus(&mut self, focus: Focus) {
        self.focus = focus;
        self.midi.release();
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
        self.metered = std::time::Instant::now();
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
            Action::Recall(slot) => match crate::slots::recall(&self.slots, slot) {
                Err(why) => log::error!("slot {}: {why}", slot + 1),
                Ok(params) if !self.params.same_bank_as(&params) => log::error!(
                    "slot {} is a different instrument: {} monitors and {} inputs, \
                     against {} and {} running. Start it with that file instead.",
                    slot + 1,
                    params.monitors.len(),
                    params.inputs.len(),
                    self.params.monitors.len(),
                    self.params.inputs.len(),
                ),
                Ok(params) => {
                    // The loops keep running: the bank is untouched, so what
                    // a recall changes is the knobs the next pass reads, not
                    // the light already on the glass.
                    self.adopt(params);
                    log::info!("slot {}: {}", slot + 1, self.describe());
                }
            },
            Action::Reset => {
                self.adopt(self.initial.clone());
                log::info!("reset: {}", self.describe());
            }
            Action::Clear => {
                if let Some(live) = self.live.as_mut() {
                    live.feedback.clear(&live.device, &live.queue);
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
            Action::Quit => event_loop.exit(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        // The one place this program blocks.
        let live = pollster::block_on(Live::new(
            event_loop,
            &self.params,
            self.resolution,
            self.fullscreen,
        ));
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
        self.metered = std::time::Instant::now();
        self.frames = 0;
        self.live = Some(live);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.shift = modifiers.state().shift_key(),
            WindowEvent::Resized(size) => {
                if let Some(live) = self.live.as_mut() {
                    live.resize(size.width, size.height);
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
                live.render(&self.params.modulated(now), &mut self.sources);
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
