//! The adapter and the device: what every way of running this instrument
//! needs, opened once and in one place.
//!
//! A window and the off-screen bench differ in what they draw *to*, not in
//! how they get a GPU — so the choosing of an adapter and the asking for a
//! device is written here rather than twice.

/// A GPU, opened. `instance` is kept because surfaces are created from it and
/// a surface may not outlive it.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// `display` is the platform connection any surface will be created
    /// against — winit's `OwnedDisplayHandle` when a window is coming, `None`
    /// off screen. It is asked for here rather than at `create_surface`
    /// because GLES on Wayland needs it before any surface exists, and
    /// wgpu forbids handing `create_surface` a different one afterwards.
    ///
    /// No `compatible_surface`: the adapter is chosen before the window
    /// exists, which is what lets the browser — where nothing may block —
    /// open the GPU and then hand a ready run loop to the page. A machine
    /// whose high-performance adapter cannot reach the display it opens on
    /// would notice at `get_default_config`, and says so there.
    pub async fn open(
        display: Option<winit::event_loop::OwnedDisplayHandle>,
        label: &str,
    ) -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: crate::BACKENDS,
            display: display.map(|handle| Box::new(handle) as _),
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let name = adapter.get_info().name.clone();
        log::info!("adapter: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some(label),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("adapter {name} refused a device: {e}"))?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
        })
    }
}
