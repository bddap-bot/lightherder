//! Window, surface and the run loop.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use crate::feedback::Feedback;
use crate::input::Source;
use crate::keys::{action_for, Action};
use crate::params::{Focus, Knob, Params};
use crate::present::Present;

/// Every monitor is a fixed size, independent of the window. Resizing the
/// window then rescales the view instead of scrambling the loops' state, and
/// the framing numbers keep meaning the same thing.
pub const MONITOR_SIZE: (u32, u32) = (1920, 1080);

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
    /// Whether shift is down, which only the slot keys read.
    shift: bool,
    live: Option<Live>,
}

/// `params` is the loaded graph, already validated by `config::load`.
///
/// The inputs are opened before the window is, so a file that is not there or
/// a device that will not open says so on the terminal instead of behind a
/// black layer of a running instrument.
pub fn run(params: Params) -> Result<(), Box<dyn std::error::Error>> {
    let sources = params
        .inputs
        .iter()
        .map(|input| Source::open(input, MONITOR_SIZE))
        .collect::<Result<Vec<Source>, String>>()?;
    for input in &params.inputs {
        log::info!("input: {input}");
    }
    let slots = crate::slots::default_dir();
    log::info!("preset slots: {}", slots.display());
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
        shift: false,
        live: None,
    })?)
}

impl Live {
    async fn new(event_loop: &ActiveEventLoop, params: &Params) -> Live {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("lightherder"))
                .expect("create window"),
        );

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
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("adapter cannot draw to this surface");
        let format = config.format;
        surface.configure(&device, &config);

        // wgpu zero-initialises textures, so the monitors start black without
        // an explicit clear.
        let feedback = Feedback::new(&device, MONITOR_SIZE.0, MONITOR_SIZE.1, params);
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
    }

    fn describe(&self) -> String {
        format!(
            "{}\nmotion keys on: {}",
            self.params.describe(self.focus),
            self.touched.name()
        )
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        // The one place this program blocks.
        let live = pollster::block_on(Live::new(event_loop, &self.params));
        log::info!(
            "{} monitors of {}x{}, {} cameras, {} inputs",
            self.params.monitors.len(),
            MONITOR_SIZE.0,
            MONITOR_SIZE.1,
            self.params.cameras.len(),
            self.params.inputs.len(),
        );
        log::info!("{}", self.describe());
        live.window.request_redraw();
        self.live = Some(live);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.shift = modifiers.state().shift_key(),
            WindowEvent::Resized(size) => live.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                // The automation is read here and nowhere else: the knobs the
                // GPU is handed are the stored ones offset by whatever is
                // driving them at this instant, and `self.params` — what the
                // keys turn and what a preset slot saves — is untouched.
                let now = self.started.elapsed().as_secs_f64();
                live.render(&self.params.modulated(now), &mut self.sources);
                live.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                // Repeats are wanted: holding a key sweeps its knob.
                match action_for(code, self.shift) {
                    Some(Action::Nudge(knob, delta)) => {
                        self.params.nudge(knob, delta, self.focus);
                        self.touched = knob;
                        log::info!("{}", self.describe());
                    }
                    Some(Action::Motion) => {
                        let now = self.started.elapsed().as_secs_f64();
                        self.params.motion_cycle(self.touched, self.focus, now);
                        log::info!("{}", self.describe());
                    }
                    Some(Action::MotionRate(steps)) => {
                        let now = self.started.elapsed().as_secs_f64();
                        self.params
                            .motion_rate(self.touched, self.focus, steps, now);
                        log::info!("{}", self.describe());
                    }
                    Some(Action::MotionDepth(steps)) => {
                        self.params.motion_depth(self.touched, self.focus, steps);
                        log::info!("{}", self.describe());
                    }
                    Some(Action::NextCamera) => {
                        self.focus.camera = (self.focus.camera + 1) % self.params.cameras.len();
                        log::info!("{}", self.describe());
                    }
                    Some(Action::NextMonitor) => {
                        self.focus.monitor = (self.focus.monitor + 1) % self.params.monitors.len();
                        log::info!("{}", self.describe());
                    }
                    Some(Action::Store(slot)) => {
                        match crate::slots::store(&self.slots, slot, &self.params) {
                            Ok(path) => log::info!("slot {}: wrote {}", slot + 1, path.display()),
                            Err(why) => log::error!("slot {}: {why}", slot + 1),
                        }
                    }
                    Some(Action::Recall(slot)) => match crate::slots::recall(&self.slots, slot) {
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
                            // The loops keep running: the bank is untouched,
                            // so what a recall changes is the knobs the next
                            // pass reads, not the light already on the glass.
                            self.adopt(params);
                            log::info!("slot {}: {}", slot + 1, self.describe());
                        }
                    },
                    Some(Action::Reset) => {
                        self.adopt(self.initial.clone());
                        log::info!("reset: {}", self.describe());
                    }
                    Some(Action::Clear) => {
                        live.feedback.clear(&live.device, &live.queue);
                        log::info!("cleared");
                    }
                    Some(Action::Quit) => event_loop.exit(),
                    None => {}
                }
            }
            _ => {}
        }
    }
}
