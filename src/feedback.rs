//! The feedback graph: N monitors, M cameras, and the wiring between them.
//!
//! The monitors are the layers of one texture array, double-buffered so a
//! pass can read every monitor's previous frame while writing one's next.
//! The external inputs are further layers of the same array, written rather
//! than rendered — so "what a camera is looking at" is one layer index and
//! the shader never learns which kind it got.
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

/// One texel of [`MONITOR_FORMAT`], in bytes. Beside the format because an
/// upload has to lay its rows out at exactly this stride.
const MONITOR_TEXEL: u32 = 8;

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

/// The most taps any one monitor's pass can ever be given.
///
/// [`taps_of`] drops a camera whose routing weight is zero, and `Knob::Route`
/// can raise one of those mid-performance — so the count a file loads with is
/// not a bound on the count the shader will be handed. This is that count with
/// every crosspoint treated as live, which is what [`config::validate`] holds
/// against [`MAX_TAPS`]. The look weights are not a knob, so a source a camera
/// cannot see stays uncounted.
pub(crate) fn reachable_taps(params: &Params) -> usize {
    params
        .cameras
        .iter()
        .map(|camera| camera.look.iter().filter(|look| **look > 0.0).count())
        .sum()
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
    /// The lens: xy and zw are the source-uv steps for one bloom radius
    /// across and down the camera's image. Worked out here rather than in
    /// the shader because the tap's affine already carries the camera's zoom
    /// and turn, and a lens's halo is round in the camera's image — not in
    /// the monitor's, which the camera may be viewing at any angle.
    halo: [f32; 4],
    /// xy: the source-uv step for one chroma-bleed offset along the
    /// scanline, mapped the same way. zw: the fraction of the light the lens
    /// scatters, and padding.
    bleed: [f32; 4],
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
    /// The analog stage's per-monitor lanes, which are not [`Character`]'s
    /// fields — those are per camera and ride the taps. x: grain amplitude,
    /// summed over the cameras the switcher routes here. y: the amplifier's
    /// headroom. z: a frame counter, so the grain moves.
    analog: [f32; 4],
    /// NTSC luma, from [`crate::params::luma_row`]. Passed rather than
    /// written into the shader so there is one copy of it in the crate.
    luma: [f32; 4],
    taps: [Tap; MAX_TAPS],
}

/// Uniform slots per monitor sit this far apart: WebGPU's guaranteed
/// dynamic-offset alignment.
const UNIFORM_STRIDE: u64 = (std::mem::size_of::<Uniforms>() as u64).next_multiple_of(256);

pub struct Feedback {
    width: u32,
    height: u32,
    monitors: usize,
    inputs: usize,
    /// The two banks themselves, kept because an external input is written
    /// into their layers rather than rendered into them.
    textures: [wgpu::Texture; 2],
    /// Render targets, one per monitor layer of the two banks — two because
    /// a pass cannot sample the bank it is writing: `layer_views[bank][monitor]`.
    /// Monitors only: an input layer is never drawn to, and never blanked.
    layer_views: [Vec<wgpu::TextureView>; 2],
    bind_groups: [wgpu::BindGroup; 2],
    front: usize,
    /// Seeds the grain, and only that. Wrapped well inside the integers f32
    /// holds exactly, since that is what carries it to the shader.
    frame: u32,
    uniforms: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
    pipeline: wgpu::RenderPipeline,
}

impl Feedback {
    /// Sized for `params`, which is the graph it will be stepped with: the
    /// two counts are baked into the textures here, so taking them from the
    /// graph itself is the only way they cannot be swapped or drift.
    pub fn new(device: &wgpu::Device, width: u32, height: u32, params: &Params) -> Feedback {
        assert!(width > 0 && height > 0, "monitors must have a size");
        let (monitors, inputs) = (params.monitors.len(), params.inputs.len());
        assert!(monitors > 0, "a graph with no monitors draws nothing");
        let layers = params.sources();

        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/feedback.wgsl"));

        let textures: [wgpu::Texture; 2] = std::array::from_fn(|i| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("source bank {i}")),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: layers as u32,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: MONITOR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    // What an input's frames are written through.
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
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
                        // Checked against the size the shader declares, at
                        // pipeline creation — but only in one direction:
                        // wgpu rejects a binding SMALLER than the shader
                        // wants, so this catches a member added to the WGSL
                        // and forgotten here. The other way round is on the
                        // reviewer, and is why every lane above is named on
                        // both sides.
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
            inputs,
            textures,
            layer_views,
            bind_groups,
            front: 0,
            frame: 0,
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

    /// The size of every layer of the bank, and so the size an input.s frames
    /// have to arrive at.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Puts one external frame — tightly packed RGBA8 — on input `i`'s source
    /// layer, where the cameras looking at it will find it.
    ///
    /// Both banks, because a camera reads whichever is current and an input
    /// layer is never rendered into, so there is no swap to carry it across.
    pub fn write_input(&self, queue: &wgpu::Queue, i: usize, rgba8: &[u8]) {
        assert!(i < self.inputs, "input {i} of {}", self.inputs);
        let expected = crate::input::frame_bytes(self.size());
        assert_eq!(rgba8.len(), expected, "input {i} handed over a short frame");
        let halves = to_half(rgba8);
        for texture in &self.textures {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: (self.monitors + i) as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&halves),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.width * MONITOR_TEXEL),
                    rows_per_image: Some(self.height),
                },
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
        }
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
        assert_eq!(
            params.inputs.len(),
            self.inputs,
            "the graph's input count is baked into the textures at creation"
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
                let camera = &params.cameras[c];
                let (rows, gain) = (&framings[c], camera.gain);
                // A step of `r` screen units across and up the camera's
                // image, carried through the tap's affine into the source
                // it samples. Screen units are height-normalised, so the
                // horizontal one is narrower by the aspect — the same
                // correction the seed spot makes to stay round.
                let step = |r: f32| {
                    let (dx, dy) = (r / aspect, r);
                    [
                        rows[0][0] * dx,
                        rows[1][0] * dx,
                        rows[0][1] * dy,
                        rows[1][1] * dy,
                    ]
                };
                // Only the scanline direction is kept for the bleed:
                // composite band-limits chroma in time, and time along a
                // scanline is across the camera's image.
                let halo = step(camera.character.bloom_radius);
                let bleed = step(camera.character.chroma_bleed);
                taps[count] = Tap {
                    row0: [rows[0][0], rows[0][1], rows[0][2], 0.0],
                    row1: [rows[1][0], rows[1][1], rows[1][2], 0.0],
                    weight: [w * gain[0], w * gain[1], w * gain[2], src as f32],
                    halo,
                    bleed: [bleed[0], bleed[1], camera.character.bloom, 0.0],
                };
                count += 1;
            }

            // Every camera the switcher routes here contributes its own
            // grain, scaled by how much of it this monitor is shown. Its
            // splitter does not come into it: the grain is the sensor's and
            // the cable's, added after the glass. Summed in quadrature,
            // because two sensors are two independent noise sources and
            // adding their amplitudes would overstate the pair by 40%.
            let grain: f32 = params.routing[m]
                .iter()
                .zip(&params.cameras)
                .map(|(route, camera)| (route * camera.character.noise).powi(2))
                .sum::<f32>()
                .sqrt();

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
                analog: [grain, monitor.headroom, self.frame as f32, 0.0],
                luma: {
                    let l = crate::params::luma_row();
                    [l[0], l[1], l[2], 0.0]
                },
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
        // Exact in f32 for two days at 60 Hz, and the grain is the only
        // thing that reads it, so the wrap costs a repeat nobody will see.
        self.frame = (self.frame + 1) % (1 << 24);
    }
}

/// One 8-bit channel as the bits of the half float the bank stores, for all
/// 256 of them. Built once: an input frame is millions of these, and the
/// domain is small enough to be a table rather than arithmetic.
fn half_table() -> &'static [u16; 256] {
    static TABLE: std::sync::OnceLock<[u16; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|v| half_bits(v as f32 / 255.0)))
}

/// `x` as IEEE binary16, for `0 <= x <= 1` and nothing else — which is the
/// whole domain of an 8-bit channel. Every value `v/255` above zero lands
/// between 2^-8 and 2^0, well inside the exponents binary16 keeps normal, so
/// there is no subnormal or overflow case here to get wrong. Ties round up
/// rather than to even: a half step of the coarsest value in range is 1/2048,
/// two orders below anything the loop's arithmetic distinguishes.
fn half_bits(x: f32) -> u16 {
    debug_assert!((0.0..=1.0).contains(&x), "{x} is outside an 8-bit channel");
    if x == 0.0 {
        return 0;
    }
    let bits = x.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    // A mantissa carry lands in the exponent, which is what the addition of
    // the rounding bit does for free — the two fields are adjacent.
    ((exponent as u16) << 10 | (mantissa >> 13) as u16) + ((mantissa >> 12) & 1) as u16
}

/// A tightly packed RGBA8 frame in the bank's format.
fn to_half(rgba8: &[u8]) -> Vec<u16> {
    // No transfer curve on the way in. The bank holds whatever a monitor is
    // displaying, in the same convention as the rest of the instrument: the
    // phosphor gamma is a knob on the front panel, applied on the way out of
    // a pass, not an encoding this stage is entitled to undo.
    let table = half_table();
    rgba8.iter().map(|v| table[*v as usize]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// binary16 back to f32, written from the format rather than from
    /// [`half_bits`], so the two are independent.
    fn from_half(h: u16) -> f32 {
        let sign = if h >> 15 == 1 { -1.0 } else { 1.0 };
        let exponent = ((h >> 10) & 0x1f) as i32;
        let mantissa = (h & 0x3ff) as f32 / 1024.0;
        match exponent {
            0 => sign * mantissa * 2f32.powi(-14),
            31 => f32::INFINITY,
            e => sign * (1.0 + mantissa) * 2f32.powi(e - 15),
        }
    }

    #[test]
    fn every_channel_value_lands_on_the_nearest_half() {
        // Exhaustive, because the domain is 256 values: nothing here is
        // sampled or assumed. Nearest rather than within-a-tolerance, because
        // a tolerance of one ulp is met by truncating — which is what the
        // rounding term exists not to do, and inside a loop that feeds itself
        // a bias that always rounds down is a ratchet.
        for v in 0u16..=255 {
            let want = v as f32 / 255.0;
            let bits = half_table()[v as usize];
            let error = (from_half(bits) - want).abs();
            for neighbour in [bits.wrapping_sub(1), bits + 1] {
                let theirs = (from_half(neighbour) - want).abs();
                assert!(error <= theirs, "{v}: {bits:#06x} is not the nearest half");
            }
        }
    }

    #[test]
    fn the_ends_of_the_scale_are_exact() {
        // Black and white are the two values a rounding error would be
        // visible on: a white that came back as 0.9995 would darken the loop
        // one step per pass, which is the failure the colour stage went to
        // trouble over.
        assert_eq!(half_table()[0], 0x0000);
        assert_eq!(half_table()[255], 0x3c00);
        assert_eq!(from_half(half_table()[255]), 1.0);
    }

    #[test]
    fn a_frame_converts_channel_for_channel() {
        let rgba8 = [0u8, 128, 255, 255, 255, 0, 128, 255];
        let halves = to_half(&rgba8);
        assert_eq!(halves.len(), rgba8.len());
        for (h, v) in halves.iter().zip(rgba8) {
            assert_eq!(*h, half_table()[v as usize]);
        }
    }
}
