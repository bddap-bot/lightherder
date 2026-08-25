//! The feedback loop itself: one monitor, one camera pointed at it.
//!
//! Later increments turn this into a graph of many monitors and cameras with a
//! routing matrix between them. The shape that survives is the one here: a
//! monitor is a pair of textures, and a camera is a pass that reads one and
//! writes the other.

use crate::affine::sample_transform;
use crate::params::{Params, SEED_RADIUS};

/// Half-float so the loop keeps headroom above 1.0 and does not quantise to
/// bands after a few dozen passes.
pub const MONITOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    row0: [f32; 4],
    row1: [f32; 4],
    gain: [f32; 4],
    seed: [f32; 4],
}

pub struct Feedback {
    width: u32,
    height: u32,
    /// The monitor. Two textures because a pass cannot sample the target it is
    /// writing; the camera reads `[front]` and writes `[1 - front]`.
    views: [wgpu::TextureView; 2],
    bind_groups: [wgpu::BindGroup; 2],
    front: usize,
    uniforms: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl Feedback {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Feedback {
        assert!(width > 0 && height > 0, "monitor must have a size");
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/feedback.wgsl"));

        let textures: Vec<wgpu::Texture> = (0..2)
            .map(|i| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("monitor {i}")),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: MONITOR_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
            })
            .collect();
        let views: [wgpu::TextureView; 2] = std::array::from_fn(|i| {
            textures[i].create_view(&wgpu::TextureViewDescriptor::default())
        });

        // Linear filtering is what makes the loop smooth rather than a stack of
        // hard-edged copies, and clamping keeps out-of-range samples cheap to
        // detect in the shader.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("camera"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_groups: [wgpu::BindGroup; 2] = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("camera reading monitor {i}")),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&views[i]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        });

        let pipeline = crate::fullscreen_pipeline(
            device,
            &shader,
            &layout,
            "fs_camera",
            MONITOR_FORMAT,
            "camera",
        );

        Feedback {
            width,
            height,
            views,
            bind_groups,
            front: 0,
            uniforms,
            layout,
            pipeline,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// The monitor as it currently stands, for anyone who wants to look at it.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.views[self.front]
    }

    /// Binds the current monitor as a sampled texture, for a pipeline built
    /// against [`Feedback::layout`].
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_groups[self.front]
    }

    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// Blank both halves of the monitor, restarting the loop from the seed.
    pub fn clear(&self, encoder: &mut wgpu::CommandEncoder) {
        for view in &self.views {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear monitor"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
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
        }
    }

    /// One trip round the loop: the camera reads the monitor and redraws it.
    pub fn step(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &Params,
    ) {
        let aspect = self.aspect();
        let rows = sample_transform(&params.framing, aspect).rows();
        let uniforms = Uniforms {
            row0: [rows[0][0], rows[0][1], rows[0][2], 0.0],
            row1: [rows[1][0], rows[1][1], rows[1][2], 0.0],
            gain: [
                params.decay[0],
                params.decay[1],
                params.decay[2],
                params.seed_gain,
            ],
            // The seed is round on screen, so its uv radius is narrower on the
            // axis the monitor is wider on.
            seed: [0.5, 0.5, SEED_RADIUS / aspect, SEED_RADIUS],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let back = 1 - self.front;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("camera"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.views[back],
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
            pass.set_bind_group(0, &self.bind_groups[self.front], &[]);
            pass.draw(0..3, 0..1);
        }
        self.front = back;
    }
}
