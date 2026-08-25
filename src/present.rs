//! Copying the monitor to whatever is watching it — a window, or a texture a
//! test can read back.

pub struct Present {
    pipeline: wgpu::RenderPipeline,
}

impl Present {
    /// `layout` must be a [`crate::feedback::Feedback`] bind group layout;
    /// `format` is the target's format, which for a surface is decided by the
    /// surface rather than by us.
    pub fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Present {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/feedback.wgsl"));
        let pipeline =
            crate::fullscreen_pipeline(device, &shader, layout, "fs_present", format, "present");
        Present { pipeline }
    }

    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        source: &wgpu::BindGroup,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, source, &[]);
        pass.draw(0..3, 0..1);
    }
}
