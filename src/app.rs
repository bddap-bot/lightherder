//! Window, surface and the run loop.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use crate::feedback::Feedback;
use crate::params::{action_for, Action, Params};
use crate::present::Present;

/// The monitor is a fixed size, independent of the window. Resizing the window
/// then rescales the view instead of scrambling the loop's state, and the
/// framing numbers keep meaning the same thing.
pub const MONITOR_SIZE: (u32, u32) = (1920, 1080);

pub const KEY_HELP: &str = "\
keys:
  - / =        zoom out / in
  , / .        rotate one way / the other
  arrows       pan
  [ / ]        loop gain, all channels
  1 2 3 4 5 6  loop gain per channel (r-, r+, g-, g+, b-, b+)
  ; / '        seed brightness
  space        blank the monitor
  r            reset
  esc          quit";

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    feedback: Feedback,
    present: Present,
    clear_next_frame: bool,
}

#[derive(Default)]
pub struct App {
    params: Params,
    live: Option<Live>,
}

pub fn run() -> Result<(), winit::error::EventLoopError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut App::default())
}

impl Live {
    async fn new(event_loop: &ActiveEventLoop) -> Live {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("lightherder"))
                .expect("create window"),
        );

        // The display handle lets backends that need one (GLES) come up; the
        // window handles for the surface itself travel with `create_surface`.
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );
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

        let feedback = Feedback::new(&device, MONITOR_SIZE.0, MONITOR_SIZE.1);
        let present = Present::new(&device, feedback.layout(), format);

        Live {
            window,
            surface,
            device,
            queue,
            config,
            feedback,
            present,
            clear_next_frame: true,
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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        if std::mem::take(&mut self.clear_next_frame) {
            self.feedback.clear(&mut encoder);
        }
        self.feedback.step(&self.queue, &mut encoder, params);
        self.present
            .draw(&mut encoder, &target, self.feedback.bind_group());

        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        // The one place this program blocks. A browser build cannot, and
        // replaces this with a spawn that hands the ready `Live` back through
        // a user event; nothing else in the render path needs to change.
        let live = pollster::block_on(Live::new(event_loop));
        log::info!("monitor {}x{}", MONITOR_SIZE.0, MONITOR_SIZE.1);
        log::info!("{}", self.params.describe());
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
                        self.params.nudge(knob, delta);
                        log::info!("{}", self.params.describe());
                    }
                    Some(Action::Reset) => {
                        self.params = Params::default();
                        log::info!("reset: {}", self.params.describe());
                    }
                    Some(Action::Clear) => {
                        live.clear_next_frame = true;
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
