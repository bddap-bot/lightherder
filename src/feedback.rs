//! The feedback graph: N monitors, M cameras, and the wiring between them.
//!
//! The monitors are the layers of one texture array, double-buffered so a
//! pass can read every monitor's previous frame while writing one's next.
//! Everything between a monitor and the cameras feeding it — the routing
//! matrix, each camera's beam splitter, each camera's gain — flattens on the
//! CPU into a list of *taps*: (source layer, sampling affine, weight).
//! Sampling is linear, so a camera looking through a splitter at a blend of
//! monitors is exactly the weighted sum of its per-monitor samples; no
//! intermediate blend texture exists because none is needed.

use bytemuck::Zeroable;

use crate::affine::sample_transform;
use crate::params::Params;

/// Half-float so the loop keeps headroom above 1.0 and does not quantise to
/// bands after a few dozen passes.
const MONITOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Radius of the seed spot, in screen units where the monitor is 1.0 tall.
const SEED_RADIUS: f32 = 0.06;

/// Where the seed sits, in the same screen units. Off-centre on purpose: a
/// radially symmetric spot at the centre is a fixed point of rotation, so a
/// centred seed would make the rotation knob do nothing visible.
const SEED_CENTRE: [f32; 2] = [0.25, 0.0];

/// Most taps one monitor can be fed by. Sized for comfort: all-to-all with
/// eight cameras is eight taps, so this leaves room for every camera to look
/// through a four-way splitter on top. `config::validate` holds the line.
pub const MAX_TAPS: usize = 32;

/// The edges of monitor `m`'s pass: (camera, source monitor, routing weight
/// times splitter weight). The one definition of which edges become taps —
/// `config::validate` counts these against [`MAX_TAPS`] and [`Feedback::step`]
/// writes them, so the two cannot drift apart on the zero-weight rule. Note
/// for the increment that makes routing or look weights live-mutable:
/// validate-at-load stops bounding the tap count the moment a zero weight can
/// be swept positive mid-performance, and the bound must move to the nudge.
pub(crate) fn taps_of(params: &Params, m: usize) -> impl Iterator<Item = (usize, usize, f32)> + '_ {
    params.routing[m]
        .iter()
        .zip(&params.cameras)
        .enumerate()
        .filter(|(_, (route, _))| **route > 0.0)
        .flat_map(|(c, (route, camera))| {
            camera
                .look
                .iter()
                .enumerate()
                .filter(|(_, look)| **look > 0.0)
                .map(move |(src, look)| (c, src, route * look))
        })
}

/// One flattened edge of the graph, mirrored in `shaders/feedback.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Tap {
    /// Rows of the sampling affine. The fourth components are padding to the
    /// 16-byte stride a uniform array demands; there is no missing term.
    row0: [f32; 4],
    row1: [f32; 4],
    /// rgb: routing weight x splitter weight x camera gain, per channel.
    /// a: the source monitor's layer index.
    weight: [f32; 4],
}

/// Per-monitor uniforms, mirrored by hand in `shaders/feedback.wgsl`, which
/// documents what each lane carries. The sizes are held together by
/// `min_binding_size` below.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// xy: seed centre in uv. zw: seed radii in uv, already aspect-corrected.
    seed: [f32; 4],
    /// Columns, each padded to 16 bytes: that is what a WGSL `mat3x3<f32>`
    /// is, and it is column-major where [`Colour::chroma_matrix`] is not.
    chroma: [[f32; 4]; 3],
    /// x: brightness. y: contrast. z: gamma. w: seed brightness.
    levels: [f32; 4],
    /// x: tap count. y: this monitor's own layer, for the present pass.
    info: [f32; 4],
    taps: [Tap; MAX_TAPS],
}

/// Uniform slots per monitor sit this far apart: WebGPU's guaranteed
/// dynamic-offset alignment.
const UNIFORM_STRIDE: u64 = (std::mem::size_of::<Uniforms>() as u64).next_multiple_of(256);

pub struct Feedback {
    width: u32,
    height: u32,
    monitors: usize,
    /// Render targets, one per monitor layer of the two banks — two because
    /// a pass cannot sample the bank it is writing: `layer_views[bank][monitor]`.
    layer_views: [Vec<wgpu::TextureView>; 2],
    bind_groups: [wgpu::BindGroup; 2],
    front: usize,
    uniforms: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
    pipeline: wgpu::RenderPipeline,
}

impl Feedback {
    pub fn new(device: &wgpu::Device, width: u32, height: u32, monitors: usize) -> Feedback {
        assert!(width > 0 && height > 0, "monitors must have a size");
        assert!(monitors > 0, "a graph with no monitors draws nothing");
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/feedback.wgsl"));

        let textures: Vec<wgpu::Texture> = (0..2)
            .map(|i| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("monitor bank {i}")),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: monitors as u32,
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
        // A one-monitor graph still binds as an array: `create_view` defaults
        // the dimension to D2 for a single layer, which would not match the
        // shader's `texture_2d_array`.
        let array_views: [wgpu::TextureView; 2] = std::array::from_fn(|i| {
            textures[i].create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            })
        });
        let layer_views: [Vec<wgpu::TextureView>; 2] = std::array::from_fn(|i| {
            (0..monitors as u32)
                .map(|layer| {
                    textures[i].create_view(&wgpu::TextureViewDescriptor {
                        label: Some(&format!("monitor {layer} of bank {i}")),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_array_layer: layer,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect()
        });

        // Linear filtering is what makes the loop smooth rather than a stack of
        // hard-edged copies. The address mode barely matters: WebGPU offers no
        // clamp-to-border, so the shader supplies the black outside the
        // monitor itself.
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
            label: Some("per-monitor uniforms"),
            size: UNIFORM_STRIDE * monitors as u64,
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
                        // One buffer, one slot per monitor: the offset picks
                        // the monitor.
                        has_dynamic_offset: true,
                        // Makes wgpu check this struct's size against the one
                        // the shader declares, at pipeline creation: a member
                        // added to one side and not the other fails loudly
                        // instead of silently misreading every lane after it.
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<Uniforms>() as u64
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
                label: Some(&format!("cameras reading bank {i}")),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniforms,
                            offset: 0,
                            size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&array_views[i]),
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
            monitors,
            layer_views,
            bind_groups,
            front: 0,
            uniforms,
            layout,
            shader,
            pipeline,
        }
    }

    /// Where the seed spot lands, in uv — the same spot on every monitor.
    /// The loop is driven from here, so anything measuring the instrument
    /// needs to know it.
    pub fn seed_uv(&self) -> [f32; 2] {
        crate::affine::screen_to_uv(self.aspect()).apply(SEED_CENTRE)
    }

    pub(crate) fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    pub(crate) fn monitors(&self) -> usize {
        self.monitors
    }

    /// The dynamic offset that binds monitor `m`'s uniform slot.
    pub(crate) fn uniform_offset(&self, m: usize) -> u32 {
        (UNIFORM_STRIDE * m as u64) as u32
    }

    /// Binds the monitor bank as it currently stands, for a pipeline built
    /// against [`Feedback::layout`].
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_groups[self.front]
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn shader(&self) -> &wgpu::ShaderModule {
        &self.shader
    }

    /// Blank every monitor, restarting the loops from the seeds alone. Both
    /// banks, so no stale frame comes back round on the next swap.
    pub fn clear(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        for views in &self.layer_views {
            for view in views {
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
        queue.submit([encoder.finish()]);
    }

    /// One trip round every loop at once: each camera reads the monitors as
    /// they stand, and every monitor is redrawn from its taps — the
    /// simultaneous capture a rig of real cameras performs. Self-contained,
    /// so no caller threads an encoder — and so no caller can batch two
    /// steps behind one write of the uniform buffer.
    pub fn step(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, params: &Params) {
        assert_eq!(
            params.monitors.len(),
            self.monitors,
            "the graph's monitor count is baked into the textures at creation"
        );
        // Everything else the tap flattening assumes — row lengths, weight
        // signs, the tap cap — is the loader's contract, re-asserted here so
        // a hand-built Params that skipped `config::load` fails loudly
        // instead of sampling the wrong layer. Cheap: a few dozen float
        // compares per frame, no allocation on the success path.
        if let Err(why) = crate::config::validate(params) {
            panic!("unvalidated params reached the GPU: {why}");
        }
        let aspect = self.aspect();
        let seed = self.seed_uv();

        // Framings move every frame; the affine per camera is the same for
        // all of its taps, so it is worked out once.
        let framings: Vec<[[f32; 3]; 2]> = params
            .cameras
            .iter()
            .map(|camera| sample_transform(&camera.framing, aspect).rows())
            .collect();

        for (m, monitor) in params.monitors.iter().enumerate() {
            let mut taps = [Tap::zeroed(); MAX_TAPS];
            let mut count = 0usize;
            for (c, src, w) in taps_of(params, m) {
                let (rows, gain) = (&framings[c], params.cameras[c].gain);
                taps[count] = Tap {
                    row0: [rows[0][0], rows[0][1], rows[0][2], 0.0],
                    row1: [rows[1][0], rows[1][1], rows[1][2], 0.0],
                    weight: [w * gain[0], w * gain[1], w * gain[2], src as f32],
                };
                count += 1;
            }

            let chroma = monitor.colour.chroma_matrix();
            let uniforms = Uniforms {
                // The seed is round on screen, so its uv radius is narrower
                // on the axis the monitor is wider on.
                seed: [seed[0], seed[1], SEED_RADIUS / aspect, SEED_RADIUS],
                chroma: std::array::from_fn(|col| {
                    [chroma[0][col], chroma[1][col], chroma[2][col], 0.0]
                }),
                levels: [
                    monitor.colour.brightness,
                    monitor.colour.contrast,
                    monitor.colour.gamma,
                    monitor.seed_brightness,
                ],
                info: [count as f32, m as f32, 0.0, 0.0],
                taps,
            };
            queue.write_buffer(
                &self.uniforms,
                UNIFORM_STRIDE * m as u64,
                bytemuck::bytes_of(&uniforms),
            );
        }

        let back = 1 - self.front;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("step"),
        });
        for m in 0..self.monitors {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("camera"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.layer_views[back][m],
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
            pass.set_bind_group(0, &self.bind_groups[self.front], &[self.uniform_offset(m)]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        self.front = back;
    }
}
