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
    /// open the GPU and then hand a ready run loop to the page. Nothing has
    /// ruled out an adapter that cannot reach the display, then: a machine
    /// where the fastest card is the wrong one notices at
    /// `get_default_config`, and the refusal there names `WGPU_POWER_PREF`
    /// — which is why it is read here.
    pub async fn open(
        display: Option<winit::event_loop::OwnedDisplayHandle>,
        label: &str,
    ) -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: crate::BACKENDS,
            display: display.map(|handle| Box::new(handle) as _),
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        // Asked twice on purpose. `var_os` answers only whether someone set
        // it — which `from_env`, whose `None` is both "unset" and "unknown
        // spelling", cannot be asked — and a value it does not recognise is
        // refused rather than ignored: whoever sets this was sent here by a
        // message naming it, and a typo that quietly reselected the fastest
        // card would answer them with that same message again. The spellings
        // themselves are still read in exactly one place.
        let power_preference = match std::env::var_os("WGPU_POWER_PREF") {
            None => wgpu::PowerPreference::HighPerformance,
            Some(asked) => wgpu::PowerPreference::from_env().ok_or_else(|| {
                format!(
                    "WGPU_POWER_PREF={:?} is not low, high or none",
                    asked.to_string_lossy(),
                )
            })?,
        };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference,
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
