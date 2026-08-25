//! Window, surface and the run loop.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use crate::feedback::Feedback;
use crate::keys::{action_for, Action};
use crate::params::{Focus, Params};
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
    live: Option<Live>,
}

/// `params` is the loaded graph, already validated by `config::load`.
pub fn run(params: Params) -> Result<(), winit::error::EventLoopError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut App {
        initial: params.clone(),
        params,
        focus: Focus::default(),
        live: None,
    })
}

impl Live {
    async fn new(event_loop: &ActiveEventLoop, monitors: usize) -> Live {
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
        let feedback = Feedback::new(&device, MONITOR_SIZE.0, MONITOR_SIZE.1, monitors);
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

    fn render(&mut self, params: &Params) {
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
    fn describe(&self) -> String {
        self.params.describe(self.focus)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        // The one place this program blocks.
        let live = pollster::block_on(Live::new(event_loop, self.params.monitors.len()));
        log::info!(
            "{} monitors of {}x{}, {} cameras",
            self.params.monitors.len(),
            MONITOR_SIZE.0,
            MONITOR_SIZE.1,
            self.params.cameras.len()
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
            WindowEvent::Resized(size) => live.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                live.render(&self.params);
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
                match action_for(code) {
                    Some(Action::Nudge(knob, delta)) => {
                        self.params.nudge(knob, delta, self.focus);
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
                    Some(Action::Reset) => {
                        self.params = self.initial.clone();
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
