//! The feedback graph: N monitors, M cameras, and the wiring between them.
//!
//! The monitors are the layers of one texture array, two frames of each: a
//! pass reads every monitor's previous frame while writing the next. The
//! external input is a further layer of the same array, written rather than
//! rendered, and past it lie the delay units' lines, each a ring of the
//! pictures its camera took — so what a monitor's pass samples is one layer
//! index and the shader never learns which kind it got.
//! Everything between a monitor and what feeds it — the routing matrix, each
//! camera's beam splitter, each camera's gain — flattens on the CPU into a
//! list of *taps*: (source layer, sampling affine, weight).
//! Sampling is linear, so a camera looking through a splitter at a blend of
//! monitors is exactly the weighted sum of its per-monitor samples; no
//! intermediate blend texture exists because none is needed. An input is one
//! tap of its own: the switcher hands its layer straight to the monitor,
//! there being no camera between the two to frame or colour it.
//!
//! A delay unit is the one place a camera's picture is kept: it has to
//! outlast the monitors it was taken of.

use bytemuck::Zeroable;

use crate::affine::{flip_uv, sample_transform, Affine2, Framing};
use crate::params::Params;
use crate::rig::UNITS;

/// Half-float so the loop keeps headroom above 1.0 and does not quantise to
/// bands after a few dozen passes.
const MONITOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Twice display white, where every monitor's video amplifier runs out of
/// rails. The knee is at half of it, so it lands exactly on 1.0: nothing a
/// monitor can actually show is touched, and the reserve above white — which
/// the half-float bank exists to keep — compresses onto 2.0 rather than
/// running. A real amplifier always has rails and no knob on the rig turns
/// them, so this is a constant of the instrument.
pub const HEADROOM: f32 = 2.0;

/// Most taps one monitor can be fed by, and so the length of the shader's
/// uniform array. Every camera through every monitor its glass could see,
/// plus the seed: the rig cannot reach it — its cameras see two monitors
/// each and the seed is one tap, so six is what a pass is actually handed —
/// but it is the bound a look matrix cannot cross, so nothing has to check.
pub const MAX_TAPS: usize = crate::rig::CAMERAS * crate::rig::MONITORS + 1;

const _: () = assert!(
    MAX_TAPS == 16,
    "shaders/feedback.wgsl spells this number too, and wgpu catches only a \
     shader that wants MORE than the binding holds — grown here alone, the \
     taps past the array's end read whatever the implementation clamps to"
);

/// The most GPU memory a bank may ask for. A cap in bytes rather than in
/// pixels because it is the layers that do the multiplying — two frames of
/// every monitor, the seed, and a picture per delay unit per frame of reach
/// — and a card asked for more than it has fails inside the driver rather
/// than at the command line.
pub const MAX_BANK_BYTES: u64 = 2 << 30;

/// One texel of [`MONITOR_FORMAT`], in bytes. Asked of the format rather than
/// written down beside it, since a second copy of it is a second thing to
/// change.
fn monitor_texel_bytes() -> u32 {
    MONITOR_FORMAT
        .block_copy_size(None)
        .expect("a colour format copies a whole texel at a time")
}

/// What the bank costs `params` at monitors of `size`.
pub fn bank_bytes(params: &Params, size: (u32, u32)) -> u64 {
    Shape::of(params).bytes(size)
}

/// Whether that fits in [`MAX_BANK_BYTES`] and in one texture array, with
/// the figures in the refusal: a resolution is chosen at the command line
/// and the layer count comes from the graph and its reach, so no one
/// of them alone is what went wrong.
pub fn bank_fits(params: &Params, size: (u32, u32)) -> Result<(), String> {
    Shape::of(params).fits(size)
}

/// The deepest the delay units can reach on monitors of `size`: the
/// graph's own reach, or as far short of it as a bank that fits.
pub fn reach(params: &Params, size: (u32, u32)) -> u32 {
    let shape = Shape::of(params);
    (0..=params.reach)
        .rev()
        .find(|reach| {
            Shape {
                reach: *reach as usize,
                ..shape
            }
            .fits(size)
            .is_ok()
        })
        .unwrap_or(0)
}

/// How many frames of every monitor the bank keeps: the one a pass reads
/// and the one it writes.
const RING: usize = 2;

/// How the bank's layers are laid out: [`RING`] slabs of the monitors,
/// then the seed's own layer, then each delay unit's line of
/// [`Params::reach`] pictures. The one place a layer index comes from,
/// whether a tap reads it, the seed's frame is written to it or a pass
/// draws on it — a formula apiece would be a way apiece to read the wrong
/// frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shape {
    monitors: usize,
    reach: usize,
}

impl Shape {
    pub(crate) fn of(params: &Params) -> Shape {
        Shape {
            monitors: params.monitors.len(),
            reach: params.reach as usize,
        }
    }

    pub(crate) fn layers(self) -> usize {
        self.lines() + UNITS * self.reach
    }

    fn bytes(self, size: (u32, u32)) -> u64 {
        self.layers() as u64 * size.0 as u64 * size.1 as u64 * monitor_texel_bytes() as u64
    }

    fn fits(self, size: (u32, u32)) -> Result<(), String> {
        let what = format!(
            "{} bank layers ({RING} frames of {} monitors, the seed, and {} frames of {UNITS} delay units)",
            self.layers(),
            self.monitors,
            self.reach,
        );
        // The limit every WebGPU implementation grants, since the browser
        // build asks for no more than that.
        let deepest = wgpu::Limits::default().max_texture_array_layers as usize;
        if self.layers() > deepest {
            return Err(format!(
                "{what} is deeper than the {deepest} layers a texture array holds"
            ));
        }
        let bytes = self.bytes(size);
        if bytes > MAX_BANK_BYTES {
            return Err(format!(
                "{what} at {}x{} is {:.1} GiB of bank, past the {:.1} GiB cap",
                size.0,
                size.1,
                bytes as f64 / (1u64 << 30) as f64,
                MAX_BANK_BYTES as f64 / (1u64 << 30) as f64,
            ));
        }
        Ok(())
    }

    fn monitor(self, slab: usize, m: usize) -> usize {
        debug_assert!(slab < RING && m < self.monitors);
        slab * self.monitors + m
    }

    fn seed(self) -> usize {
        RING * self.monitors
    }

    /// The first layer of the lines: a recording binds only the layers below.
    fn lines(self) -> usize {
        self.seed() + 1
    }

    /// The layer camera `camera`'s unit records pass `pass`'s picture on.
    fn line(self, camera: usize, pass: u64) -> usize {
        debug_assert!(camera < UNITS && self.reach > 0);
        self.lines() + camera * self.reach + (pass % self.reach as u64) as usize
    }

    fn late(self, camera: usize, pass: u64, late: u32) -> usize {
        assert!(
            (1..=self.reach).contains(&(late as usize)),
            "{late} passes late on a line {} long",
            self.reach
        );
        self.line(camera, pass + (self.reach - late as usize) as u64)
    }

    /// The cameras whose units keep a line: none, with no reach.
    fn recorded(self) -> std::ops::Range<usize> {
        0..if self.reach > 0 { UNITS } else { 0 }
    }

    /// The slab holding the newest frame as pass `pass` begins.
    fn newest(self, pass: u64) -> usize {
        (pass % RING as u64) as usize
    }
}

/// One edge of a pass: what the light came through, the source layer, and
/// the share of it the monitor shows times the share of that the glass
/// passes — where the seed's key has passed, and where it has cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Edge {
    pub(crate) through: Through,
    pub(crate) layer: usize,
    pub(crate) passed: f32,
    pub(crate) cut: f32,
}

/// The edges of monitor `m`'s pass `pass`.
///
/// A camera on time fans out over its beam splitter, since a camera
/// watching two monitors is two taps; a late one is one tap on the picture
/// its unit recorded, the glass already in it. The seed is exactly one tap
/// and carries no camera; it reads the layer [`Feedback::write_seed`] wrote
/// its frame to.
pub(crate) fn taps_of(params: &Params, m: usize, pass: u64) -> impl Iterator<Item = Edge> + '_ {
    let shape = Shape::of(params);
    let newest = shape.newest(pass);
    let feed = params.rig.feed(m);
    let through_cameras = (0..params.cameras.len())
        .filter(move |c| feed.cut(*c) > 0.0)
        .flat_map(move |c| {
            let (passed, cut) = (feed.cameras[c], feed.cut(c));
            let late = params.rig.late(c, m);
            let recorded = (late > 0).then(|| Edge {
                through: Through::Line(c),
                layer: shape.late(c, pass, late),
                passed,
                cut,
            });
            let live = (late == 0).then(|| looks(params, c, newest, passed, cut));
            recorded.into_iter().chain(live.into_iter().flatten())
        });
    let straight_in = (feed.seed > 0.0)
        .then(move || Edge {
            through: Through::Seed,
            layer: shape.seed(),
            passed: feed.seed,
            cut: 0.0,
        })
        .into_iter();
    through_cameras.chain(straight_in)
}

fn looks(
    params: &Params,
    c: usize,
    slab: usize,
    passed: f32,
    cut: f32,
) -> impl Iterator<Item = Edge> + '_ {
    let shape = Shape::of(params);
    params.cameras[c]
        .look
        .iter()
        .enumerate()
        .filter(|(_, look)| **look > 0.0)
        .map(move |(src, look)| Edge {
            through: Through::Camera(c),
            layer: shape.monitor(slab, src),
            passed: passed * look,
            cut: cut * look,
        })
}

/// What a tap came through: one of the graph's cameras, live or off its
/// unit's line, or the seed on its way in past the switcher, which is what
/// the key is judged on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Through {
    Camera(usize),
    Line(usize),
    Seed,
}

/// One flattened edge of the graph, flipped in `shaders/feedback.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Tap {
    /// Rows of the sampling affine. The fourth components are padding to the
    /// 16-byte stride a uniform array demands; there is no missing term.
    row0: [f32; 4],
    row1: [f32; 4],
    /// rgb: routing weight x splitter weight x camera gain, per channel,
    /// where the seed's key passes. a: the source layer index.
    passes: [f32; 4],
    /// rgb: the same where the key cuts — the seed's tap goes to nothing and
    /// the taps it was keyed over stand whole. a: padding.
    cuts: [f32; 4],
}

impl Tap {
    fn new(sampled: Affine2, passed: [f32; 3], cut: [f32; 3], layer: usize) -> Tap {
        let rows = sampled.rows();
        Tap {
            row0: [rows[0][0], rows[0][1], rows[0][2], 0.0],
            row1: [rows[1][0], rows[1][1], rows[1][2], 0.0],
            passes: [passed[0], passed[1], passed[2], layer as f32],
            cuts: [cut[0], cut[1], cut[2], 0.0],
        }
    }
}

/// Per-pass uniforms, flipped by hand in `shaders/feedback.wgsl`, which
/// documents what each lane carries. The sizes are held together by
/// `min_binding_size` below.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// Columns, each padded to 16 bytes: that is what a WGSL `mat3x3<f32>`
    /// is, and it is column-major where [`Colour::chroma_matrix`] is not.
    chroma: [[f32; 4]; 3],
    /// x: brightness. y: contrast. zw: padding.
    levels: [f32; 4],
    /// x: tap count. y: this monitor's own layer, for the present pass.
    /// zw: where the bank splits round what this pass writes — the first
    /// layer past the lower view, and the first layer of the upper one.
    info: [f32; 4],
    /// x: the unsharp mask, [`crate::params::Monitor::sharpness`]. yzw:
    /// padding.
    analog: [f32; 4],
    /// NTSC luma, from [`crate::params::luma_row`]. Passed rather than
    /// written into the shader so there is one copy of it in the crate.
    luma: [f32; 4],
    /// The switcher's keyer, judged on the seed: x luma threshold, y
    /// softness, z the seed's tap index, or -1 when this monitor carries no
    /// seed and the key passes everything. w: padding.
    key: [f32; 4],
    taps: [Tap; MAX_TAPS],
}

/// Uniform slots sit this far apart: WebGPU's guaranteed dynamic-offset
/// alignment.
const UNIFORM_STRIDE: u64 = (std::mem::size_of::<Uniforms>() as u64).next_multiple_of(256);

pub struct Feedback {
    width: u32,
    height: u32,
    shape: Shape,
    /// The bank. Kept because an external input is written into its layers
    /// rather than rendered into them.
    ring: wgpu::Texture,
    /// Render targets, `layer_views[slab][monitor]`. Monitors only: an input
    /// layer is never drawn to, and never blanked.
    layer_views: Vec<Vec<wgpu::TextureView>>,
    line_views: Vec<wgpu::TextureView>,
    /// One per slab, for the passes that write that slab: the bank bound as
    /// the layers below it and the layers above it, since a pass may not
    /// sample the layers it is drawing to and a view is one run of layers.
    /// A side with no layers binds the other side again — never sampled,
    /// because no tap's layer falls on it.
    writing: Vec<wgpu::BindGroup>,
    /// The whole bank, for the passes that write none of it.
    whole: wgpu::BindGroup,
    /// Every layer below the lines, for the recordings.
    recording: wgpu::BindGroup,
    /// Passes stepped so far: the clock a router output's
    /// [`crate::params::Cadence`] runs on.
    frame: u64,
    uniforms: wgpu::Buffer,
    /// One frame in the bank's format, reused by every
    /// [`Feedback::write_input`]. An external input hands over a frame every
    /// frame it has one, so at 1920x1080 allocating this rather than keeping
    /// it would be sixteen megabytes a frame per input. Grown on first use
    /// and never again — a graph with no inputs never grows it at all.
    scratch: Vec<u16>,
    layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
    pipeline: wgpu::RenderPipeline,
    recorder: wgpu::RenderPipeline,
}

impl Feedback {
    /// Sized for `params`, which is the graph it will be stepped with: the
    /// two counts are baked into the textures here, so taking them from the
    /// graph itself is the only way they cannot be swapped or drift.
    pub fn new(device: &wgpu::Device, width: u32, height: u32, params: &Params) -> Feedback {
        assert!(width > 0 && height > 0, "monitors must have a size");
        let shape = Shape::of(params);
        let monitors = shape.monitors;
        let layers = shape.layers();

        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/feedback.wgsl"));

        let ring = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("source bank"),
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
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        // A run of layers of the bank, bound as an array even when it is one
        // layer long: `create_view` defaults the dimension to D2 for a single
        // layer, which would not match the shader's `texture_2d_array`.
        let run = |from: usize, to: usize| {
            ring.create_view(&wgpu::TextureViewDescriptor {
                label: Some(&format!("bank layers {from}..{to}")),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_array_layer: from as u32,
                array_layer_count: Some((to - from) as u32),
                ..Default::default()
            })
        };
        let target = |layer: usize, label: String| {
            ring.create_view(&wgpu::TextureViewDescriptor {
                label: Some(&label),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        };
        let layer_views: Vec<Vec<wgpu::TextureView>> = (0..RING)
            .map(|slab| {
                (0..monitors)
                    .map(|m| {
                        target(
                            shape.monitor(slab, m),
                            format!("monitor {m} of slab {slab}"),
                        )
                    })
                    .collect()
            })
            .collect();
        let line_views: Vec<wgpu::TextureView> = (shape.lines()..layers)
            .map(|layer| target(layer, format!("delay line layer {layer}")))
            .collect();

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
            label: Some("per-pass uniforms"),
            size: UNIFORM_STRIDE * (monitors + UNITS) as u64,
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind = |label: &str, lower: &wgpu::TextureView, upper: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
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
                        resource: wgpu::BindingResource::TextureView(lower),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(upper),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let everything = run(0, layers);
        let whole = bind("reading the whole bank", &everything, &everything);
        let below_the_lines = run(0, shape.lines());
        let recording = bind(
            "reading below the lines",
            &below_the_lines,
            &below_the_lines,
        );
        let writing = (0..RING)
            .map(|slab| {
                let (split, above) = (shape.monitor(slab, 0), shape.monitor(slab, 0) + monitors);
                let lower = (split > 0).then(|| run(0, split));
                let upper = (above < layers).then(|| run(above, layers));
                let (lower, upper) = match (&lower, &upper) {
                    (Some(lower), Some(upper)) => (lower, upper),
                    (Some(lower), None) => (lower, lower),
                    (None, Some(upper)) => (upper, upper),
                    (None, None) => unreachable!("a ring has at least two slabs"),
                };
                bind(&format!("reading round slab {slab}"), lower, upper)
            })
            .collect();

        let pipeline = crate::fullscreen_pipeline(
            device,
            &shader,
            &layout,
            "fs_camera",
            MONITOR_FORMAT,
            None,
            "camera",
        );
        let recorder = crate::fullscreen_pipeline(
            device,
            &shader,
            &layout,
            "fs_record",
            MONITOR_FORMAT,
            None,
            "delay unit",
        );

        Feedback {
            width,
            height,
            shape,
            ring,
            layer_views,
            line_views,
            writing,
            whole,
            recording,
            frame: 0,
            uniforms,
            scratch: Vec::new(),
            layout,
            shader,
            pipeline,
            recorder,
        }
    }

    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    pub(crate) fn monitors(&self) -> usize {
        self.shape.monitors
    }

    /// The size of every layer of the bank, and so the size the seed's frames
    /// have to arrive at.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Puts one external frame — tightly packed RGBA8 — on the seed's layer,
    /// where the switcher will find it.
    pub fn write_seed(&mut self, queue: &wgpu::Queue, rgba8: &[u8]) {
        assert_eq!(
            rgba8.len(),
            crate::input::frame_bytes(self.size()),
            "the seed handed over a short frame"
        );
        to_half(rgba8, &mut self.scratch);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.ring,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: self.shape.seed() as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&self.scratch),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * monitor_texel_bytes()),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub(crate) fn uniform_offset(&self, slot: usize) -> u32 {
        (UNIFORM_STRIDE * slot as u64) as u32
    }

    /// Binds the whole monitor bank, for a pipeline built against
    /// [`Feedback::layout`] that draws to none of it.
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.whole
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn shader(&self) -> &wgpu::ShaderModule {
        &self.shader
    }

    /// Blank every monitor, restarting the loops from the seeds alone. The
    /// delay lines too, so no stale picture comes back round a delay later.
    pub fn clear(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        for view in self.layer_views.iter().flatten().chain(&self.line_views) {
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
        queue.submit([encoder.finish()]);
    }

    /// One trip round every loop at once: each camera reads the monitors as
    /// they stand, and every monitor is redrawn from its taps — the
    /// simultaneous capture a rig of real cameras performs. Self-contained,
    /// so no caller threads an encoder — and so no caller can batch two
    /// steps behind one write of the uniform buffer.
    pub fn step(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, params: &Params) {
        assert_eq!(
            Shape::of(params),
            self.shape,
            "the graph's monitors and reach are baked into the bank at creation"
        );
        // Everything the tap flattening assumes — the counts, the splitter
        // weights, every knob inside its rails — re-asserted here, so a
        // Params a knob has poisoned fails loudly instead of feeding a NaN
        // into a loop it can never leave. One compare per value the graph
        // holds, and no allocation on the success path, which is what makes
        // it affordable every frame.
        if let Err(why) = crate::config::validate(params) {
            panic!("unvalidated params reached the GPU: {why}");
        }
        let aspect = self.aspect();

        let framings = params.shafts.map(|shaft| sample_transform(&shaft, aspect));
        // What the seed's tap samples through. It is plugged into the
        // switcher, so nothing frames it: it arrives square on and fills the
        // monitor, which is the identity framing carried through the same
        // transform every camera's is.
        let square_on = sample_transform(&Framing::identity(), aspect);
        let (newest, next) = (
            self.shape.newest(self.frame),
            self.shape.newest(self.frame + 1),
        );
        let (split, above) = (
            self.shape.monitor(next, 0),
            self.shape.monitor(next, 0) + self.shape.monitors,
        );

        for (m, monitor) in params.monitors.iter().enumerate() {
            // The router output's mirror, applied to the texel being written
            // rather than to any one source: what it flips is the whole
            // picture this monitor is handed, which is what a flip on an
            // output is.
            let mirror = flip_uv(monitor.flip);
            let mut taps = [Tap::zeroed(); MAX_TAPS];
            let mut count = 0usize;
            let mut seed = -1.0;
            for edge in taps_of(params, m, self.frame) {
                let (sampled, gain) = match edge.through {
                    Through::Camera(c) => (
                        mirror.then(&framings[crate::rig::SHAFT_OF[c]]),
                        params.cameras[c].gain,
                    ),
                    // Framed when it was recorded: all that is left is the
                    // cable and the router output.
                    Through::Line(c) => (mirror, params.cameras[c].gain),
                    // There is no camera between the switcher and the seed,
                    // so every stage a camera would have takes its identity
                    // and the layer arrives as itself.
                    Through::Seed => {
                        seed = count as f32;
                        (mirror.then(&square_on), [1.0; 3])
                    }
                };
                let scaled = |share: f32| gain.map(|g| share * g);
                taps[count] = Tap::new(sampled, scaled(edge.passed), scaled(edge.cut), edge.layer);
                count += 1;
            }

            let chroma = monitor.colour.chroma_matrix();
            let uniforms = Uniforms {
                chroma: std::array::from_fn(|col| {
                    [chroma[0][col], chroma[1][col], chroma[2][col], 0.0]
                }),
                levels: [
                    monitor.colour.brightness,
                    monitor.colour.contrast,
                    HEADROOM,
                    0.0,
                ],
                info: [count as f32, (split + m) as f32, split as f32, above as f32],
                analog: [monitor.sharpness, 0.0, 0.0, 0.0],
                luma: {
                    let l = crate::params::luma_row();
                    [l[0], l[1], l[2], 0.0]
                },
                key: [
                    params.input.key.threshold,
                    params.input.key.softness,
                    seed,
                    0.0,
                ],
                taps,
            };
            queue.write_buffer(
                &self.uniforms,
                UNIFORM_STRIDE * m as u64,
                bytemuck::bytes_of(&uniforms),
            );
        }
        for camera in self.shape.recorded() {
            let framing = framings[crate::rig::SHAFT_OF[camera]];
            let mut uniforms = Uniforms::zeroed();
            let mut count = 0usize;
            for edge in looks(params, camera, newest, 1.0, 1.0) {
                uniforms.taps[count] =
                    Tap::new(framing, [edge.passed; 3], [edge.cut; 3], edge.layer);
                count += 1;
            }
            let lines = self.shape.lines() as f32;
            uniforms.info = [count as f32, 0.0, lines, lines];
            uniforms.key[2] = -1.0;
            queue.write_buffer(
                &self.uniforms,
                UNIFORM_STRIDE * self.recording_slot(camera) as u64,
                bytemuck::bytes_of(&uniforms),
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("step"),
        });
        let refreshes = |m: usize| params.monitors[m].cadence.refreshes(self.frame);
        // A router output holding its frame: the monitor's face does not
        // change, so its last frame is carried forward in the ring as it is
        // — a redraw would put it through the front panel again.
        for m in (0..self.shape.monitors).filter(|m| !refreshes(*m)) {
            let layer = |slab: usize| wgpu::TexelCopyTextureInfo {
                texture: &self.ring,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: self.shape.monitor(slab, m) as u32,
                },
                aspect: wgpu::TextureAspect::All,
            };
            encoder.copy_texture_to_texture(
                layer(newest),
                layer(next),
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let draw = |encoder: &mut wgpu::CommandEncoder,
                    view: &wgpu::TextureView,
                    pipeline: &wgpu::RenderPipeline,
                    reading: &wgpu::BindGroup,
                    slot: usize| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("camera"),
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, reading, &[self.uniform_offset(slot)]);
            pass.draw(0..3, 0..1);
        };
        for (m, view) in self.layer_views[next].iter().enumerate() {
            if refreshes(m) {
                draw(&mut encoder, view, &self.pipeline, &self.writing[next], m);
            }
        }
        // After every monitor's pass, which may still read the picture this
        // one records over, and off the slab those passes read.
        for camera in self.shape.recorded() {
            let view = &self.line_views[self.shape.line(camera, self.frame) - self.shape.lines()];
            draw(
                &mut encoder,
                view,
                &self.recorder,
                &self.recording,
                self.recording_slot(camera),
            );
        }
        queue.submit([encoder.finish()]);
        self.frame += 1;
    }

    fn recording_slot(&self, camera: usize) -> usize {
        self.shape.monitors + camera
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

/// A tightly packed RGBA8 frame in the bank's format, refilling `halves`
/// rather than returning a new one: the capacity survives, so an input that
/// hands over a frame every frame allocates on its first only. Refilled and
/// not written in place, because a `halves` too short for `rgba8` would
/// otherwise leave the tail of the last frame on the layer.
fn to_half(rgba8: &[u8], halves: &mut Vec<u16>) {
    // No transfer curve on the way in. The bank holds whatever a monitor is
    // displaying, in the same convention as the rest of the instrument: the
    // phosphor gamma is a knob on the front panel, applied on the way out of
    // a pass, not an encoding this stage is entitled to undo.
    let table = half_table();
    halves.clear();
    halves.extend(rgba8.iter().map(|v| table[*v as usize]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bank_is_two_frames_of_every_monitor_the_seed_and_a_line_per_unit() {
        // Worked out from the format rather than from `bank_bytes`: one
        // 1920x1080 layer of Rgba16Float is 8 bytes a texel, and the rig is
        // five monitors two frames deep — a pass reads one while it writes
        // the other — plus one layer for the seed.
        let layers = |p: &Params| Shape::of(p).layers();
        let mut rig = crate::config::instrument();
        rig.reach = 0;
        assert_eq!(layers(&rig), 2 * 5 + 1);
        assert_eq!(bank_bytes(&rig, (1920, 1080)), 11 * 1920 * 1080 * 8);
        // A frame of reach is another picture on each unit's line, and the
        // seed stays where it was.
        rig.reach = 3;
        assert_eq!(layers(&rig), 2 * 5 + 1 + 2 * 3);
        assert_eq!(bank_bytes(&rig, (1920, 1080)), 17 * 1920 * 1080 * 8);
        let shape = Shape::of(&rig);
        assert_eq!(shape.seed(), 10);
        // Each line is a ring of its own past the seed, unit B's after A's.
        assert_eq!(
            [0, 1, 2, 3].map(|pass| shape.line(0, pass)),
            [11, 12, 13, 11]
        );
        assert_eq!(
            [0, 1, 2, 3].map(|pass| shape.line(1, pass)),
            [14, 15, 16, 14]
        );
        // A picture `late` passes back is the one that pass recorded, and
        // the longest delay reads the layer this pass records over.
        assert_eq!(shape.late(0, 5, 1), shape.line(0, 4));
        assert_eq!(shape.late(1, 5, 3), shape.line(1, 2));
        assert_eq!(shape.late(1, 5, 3), shape.line(1, 5));
        assert_eq!(shape.late(0, 0, 3), shape.line(0, 0));
    }

    #[test]
    fn a_bank_past_the_cap_is_refused_and_the_units_reach_as_far_as_it_holds() {
        // The rig's units reach the original's thirty frames, and at the
        // resolution the instrument is deployed at the bank holds them all.
        let rig = crate::config::instrument();
        assert_eq!(rig.reach, Params::MAX_DELAY);
        assert!(bank_fits(&rig, (1920, 1080)).is_ok());
        assert_eq!(reach(&rig, (1920, 1080)), Params::MAX_DELAY);
        // At 4K the same lines are past the cap by bytes, and the refusal
        // says both halves of why, since neither the graph nor the
        // resolution alone is what went wrong.
        let why = bank_fits(&rig, (3840, 2160)).unwrap_err();
        assert!(
            why.contains(
                "71 bank layers (2 frames of 5 monitors, the seed, and 30 frames of 2 delay units)"
            ) && why.contains("3840x2160")
                && why.contains("2.0 GiB cap"),
            "{why}"
        );
        // Ten frames a unit is as far as a 4K bank goes.
        assert_eq!(reach(&rig, (3840, 2160)), 10);
        let at = |reach: u32| Params {
            reach,
            ..rig.clone()
        };
        assert!(bank_fits(&at(10), (3840, 2160)).is_ok());
        assert!(bank_fits(&at(11), (3840, 2160)).is_err());
        // A bank that does not fit with no reach at all has nothing to give
        // up, and is refused as it stands.
        assert_eq!(reach(&rig, (7680, 7680)), 0);
        assert!(bank_fits(&at(0), (7680, 7680)).is_err());
    }

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
}
