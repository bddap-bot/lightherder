//! Copying the monitor to whatever is watching it — a window, or a texture a
//! test can read back.

use crate::feedback::Feedback;

pub struct Present {
    pipeline: wgpu::RenderPipeline,
}

impl Present {
    /// `format` is the target's, which for a surface is the surface's choice
    /// rather than ours.
    pub fn new(device: &wgpu::Device, monitor: &Feedback, format: wgpu::TextureFormat) -> Present {
        let pipeline = crate::fullscreen_pipeline(
            device,
            monitor.shader(),
            monitor.layout(),
            "fs_present",
            format,
            "present",
        );
        Present { pipeline }
    }

    /// Draws the monitor into the largest centred rectangle of `target` that
    /// keeps the monitor's aspect ratio; the rest of the target stays black.
    /// Stretching instead would undo the aspect correction the sampling
    /// transform and the seed spot both go to trouble to maintain.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        monitor: &Feedback,
    ) {
        let Some((x, y, width, height)) = fit(target_size, monitor.aspect()) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("present"),
        });
        {
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
            pass.set_viewport(x, y, width, height, 0.0, 1.0);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, monitor.bind_group(), &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

/// The centred `(x, y, width, height)` of aspect `aspect` inside `target`, or
/// `None` when the target is too small to hold a viewport at all.
fn fit(target: (u32, u32), aspect: f32) -> Option<(f32, f32, f32, f32)> {
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let height = (tw / aspect).min(th);
    let width = height * aspect;
    if width < 1.0 || height < 1.0 {
        return None;
    }
    Some(((tw - width) / 2.0, (th - height) / 2.0, width, height))
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn a_matching_target_is_filled_edge_to_edge() {
        assert_eq!(
            fit((1920, 1080), 16.0 / 9.0),
            Some((0.0, 0.0, 1920.0, 1080.0))
        );
    }

    #[test]
    fn a_tall_target_gets_bars_above_and_below() {
        let (x, y, w, h) = fit((1600, 1200), 16.0 / 9.0).unwrap();
        assert_eq!((x, w), (0.0, 1600.0));
        assert!((h - 900.0).abs() < 1e-3, "height {h}");
        assert!((y - 150.0).abs() < 1e-3, "y {y}");
    }

    #[test]
    fn a_wide_target_gets_bars_left_and_right() {
        let (x, y, w, h) = fit((3000, 1080), 16.0 / 9.0).unwrap();
        assert_eq!((y, h), (0.0, 1080.0));
        assert!((w - 1920.0).abs() < 1e-3, "width {w}");
        assert!((x - 540.0).abs() < 1e-3, "x {x}");
    }

    #[test]
    fn a_collapsed_target_draws_nothing() {
        assert_eq!(fit((0, 0), 16.0 / 9.0), None);
        assert_eq!(fit((1, 1000), 16.0 / 9.0), None);
    }
}
