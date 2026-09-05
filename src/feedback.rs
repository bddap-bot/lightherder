//! The feedback graph: N monitors, M cameras, and the wiring between them.
//!
//! The monitors are the layers of one texture array, a ring of their past
//! frames deep enough for a pass to read every monitor's previous frame — or
//! one as many frames back as a camera's delay — while writing the next.
//! The external inputs are further layers of the same array, written rather
//! than rendered — so what a monitor's pass samples is one layer index and
//! the shader never learns which kind it got.
//! Everything between a monitor and what feeds it — the routing matrix, each
//! camera's beam splitter, each camera's gain — flattens on the CPU into a
//! list of *taps*: (source layer, sampling affine, weight).
//! Sampling is linear, so a camera looking through a splitter at a blend of
//! monitors is exactly the weighted sum of its per-monitor samples; no
//! intermediate blend texture exists because none is needed. An input is one
//! tap of its own: the switcher hands its layer straight to the monitor,
//! there being no camera between the two to frame or colour it.

use bytemuck::Zeroable;

use crate::affine::{flip_uv, sample_transform, Framing};
use crate::params::{Camera, Key, Params};

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
/// pixels because it is the layers that do the multiplying: the rig's five
/// monitors two frames deep, plus the seed, are 1.3 GiB of half-float at
/// 3840x2160, a frame of delay is another copy of every monitor on top, and
/// a card asked for more than it has fails inside the driver rather than at
/// the command line.
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
    Shape::of(params).layers() as u64 * size.0 as u64 * size.1 as u64 * monitor_texel_bytes() as u64
}

/// Whether that fits in [`MAX_BANK_BYTES`] and in one texture array, with
/// the figures in the refusal: a resolution is chosen at the command line
/// and the layer count comes from the graph and its reach, so no one
/// of them alone is what went wrong.
pub fn bank_fits(params: &Params, size: (u32, u32)) -> Result<(), String> {
    let shape = Shape::of(params);
    let what = format!(
        "{} bank layers ({} frames of {} monitors, and the seed)",
        shape.layers(),
        shape.history,
        shape.monitors
    );
    // The limit every WebGPU implementation grants, since the browser build
    // asks for no more than that.
    let deepest = wgpu::Limits::default().max_texture_array_layers as usize;
    if shape.layers() > deepest {
        return Err(format!(
            "{what} is deeper than the {deepest} layers a texture array holds"
        ));
    }
    let bytes = bank_bytes(params, size);
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

/// How the bank's layers are laid out: [`Params::history`] slabs of the
/// monitors as a ring, then the seed's own layer. The one place a layer
/// index comes from, whether a tap reads it, the seed's frame is written to
/// it or a pass draws on it — three formulas for one layout would be three
/// ways to read the wrong frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shape {
    history: usize,
    monitors: usize,
}

impl Shape {
    pub(crate) fn of(params: &Params) -> Shape {
        Shape {
            history: params.history(),
            monitors: params.monitors.len(),
        }
    }

    pub(crate) fn layers(self) -> usize {
        self.history * self.monitors + 1
    }

    fn monitor(self, slab: usize, m: usize) -> usize {
        debug_assert!(slab < self.history && m < self.monitors);
        slab * self.monitors + m
    }

    /// The seed: past the whole ring, since it is never delayed — nothing
    /// stands between the switcher and the outside light it was handed.
    fn seed(self) -> usize {
        self.history * self.monitors
    }

    /// The slab holding the frame `back` passes before the newest, which
    /// sits in slab `newest`: counting back round the ring. The slab after
    /// the newest is the one being written, which no count may reach.
    fn back(self, newest: usize, back: u32) -> usize {
        let back = back as usize;
        assert!(
            back + 2 <= self.history,
            "{back} frames back in a ring of {}",
            self.history
        );
        (newest + self.history - back) % self.history
    }

    /// The slab `camera` reads: the ring moves on one slab a pass and so
    /// does the look-back, so a delayed cable hands on a frame that many
    /// passes old without a copy being kept to hold it.
    fn read(self, newest: usize, camera: &Camera) -> usize {
        self.back(newest, camera.delay)
    }
}

/// The edges of monitor `m`'s pass while the newest frame sits in ring slab
/// `newest`: what the light came through, the source layer, and the share of
/// it this monitor shows times the share of that the glass passes.
///
/// A camera fans out over its beam splitter, since a camera watching two
/// monitors is two taps. The seed is exactly one tap and carries no camera.
/// A camera's taps read the ring as far back as its delay; the seed's reads
/// the layer [`Feedback::write_seed`] wrote its frame to.
pub(crate) fn taps_of(
    params: &Params,
    m: usize,
    newest: usize,
) -> impl Iterator<Item = (Through, usize, f32)> + '_ {
    let shape = Shape::of(params);
    let feed = params.rig.feed(m);
    let through_cameras = params
        .cameras
        .iter()
        .enumerate()
        .filter(move |(c, _)| feed.cameras[*c] > 0.0)
        .flat_map(move |(c, camera)| {
            camera
                .look
                .iter()
                .enumerate()
                .filter(|(_, look)| **look > 0.0)
                .map(move |(src, look)| {
                    let layer = shape.monitor(shape.read(newest, camera), src);
                    (Through::Camera(c), layer, feed.cameras[c] * look)
                })
        });
    let straight_in = (feed.seed > 0.0)
        .then(move || (Through::Seed, shape.seed(), feed.seed))
        .into_iter();
    through_cameras.chain(straight_in)
}

/// What a tap came through: one of the graph's cameras, or an input on its
/// way in past the switcher. Which, not merely whether — an input carries a
/// key of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Through {
    Camera(usize),
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
    /// rgb: routing weight x splitter weight x camera gain, per channel.
    /// a: the source monitor's layer index.
    weight: [f32; 4],
    /// The switcher's keyer on the way in: x luma threshold, y softness, zw
    /// padding. Every tap through a camera is unkeyed — the rig keys on the
    /// switcher — so this is the seed's tap and nothing else.
    key: [f32; 4],
}

/// Per-monitor uniforms, flipped by hand in `shaders/feedback.wgsl`, which
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
    /// zw: where the bank splits round the slab this pass writes — the
    /// first layer past the lower view, and the first layer of the upper
    /// one.
    info: [f32; 4],
    /// x: the unsharp mask, [`crate::params::Monitor::sharpness`]. yzw:
    /// padding.
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
    shape: Shape,
    /// The bank. Kept because an external input is written into its layers
    /// rather than rendered into them.
    ring: wgpu::Texture,
    /// Render targets, `layer_views[slab][monitor]`. Monitors only: an input
    /// layer is never drawn to, and never blanked.
    layer_views: Vec<Vec<wgpu::TextureView>>,
    /// One per slab, for the passes that write that slab: the bank bound as
    /// the layers below it and the layers above it, since a pass may not
    /// sample the layers it is drawing to and a view is one run of layers.
    /// A side with no layers binds the other side again — never sampled,
    /// because no tap's layer falls on it.
    writing: Vec<wgpu::BindGroup>,
    /// The whole bank, for the passes that write none of it.
    whole: wgpu::BindGroup,
    /// The ring slab holding the newest frame — the one the present pass
    /// shows and an undelayed camera reads.
    newest: usize,
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
        let layer_views: Vec<Vec<wgpu::TextureView>> = (0..shape.history)
            .map(|slab| {
                (0..monitors)
                    .map(|m| {
                        ring.create_view(&wgpu::TextureViewDescriptor {
                            label: Some(&format!("monitor {m} of slab {slab}")),
                            dimension: Some(wgpu::TextureViewDimension::D2),
                            base_array_layer: shape.monitor(slab, m) as u32,
                            array_layer_count: Some(1),
                            ..Default::default()
                        })
                    })
                    .collect()
            })
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
        let writing = (0..shape.history)
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

        Feedback {
            width,
            height,
            shape,
            ring,
            layer_views,
            writing,
            whole,
            // The ring is zero-initialised, so every slab is a black frame
            // and which one is newest does not matter yet.
            newest: 0,
            frame: 0,
            uniforms,
            scratch: Vec::new(),
            layout,
            shader,
            pipeline,
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

    /// The dynamic offset that binds monitor `m`'s uniform slot.
    pub(crate) fn uniform_offset(&self, m: usize) -> u32 {
        (UNIFORM_STRIDE * m as u64) as u32
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
    /// whole ring, so no stale frame comes back round a delay later.
    pub fn clear(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        for view in self.layer_views.iter().flatten() {
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

        let framing = sample_transform(&params.framing, aspect);
        // What the seed's tap samples through. It is plugged into the
        // switcher, so nothing frames it: it arrives square on and fills the
        // monitor, which is the identity framing carried through the same
        // transform every camera's is.
        let square_on = sample_transform(&Framing::identity(), aspect);
        // The slab after the newest holds the one frame in the ring older
        // than any delay reaches, so it is the one a pass may draw on.
        let next = (self.newest + 1) % self.shape.history;
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
            for (through, src, w) in taps_of(params, m, self.newest) {
                // There is no camera between the switcher and the seed, so
                // every stage a camera would have takes its identity and the
                // layer arrives as itself. Its key is the switcher's own,
                // which is where this rig keys at all.
                let (sampled, gain, key) = match through {
                    Through::Camera(c) => (&framing, params.cameras[c].gain, Key::OFF),
                    Through::Seed => (&square_on, [1.0; 3], params.input.key),
                };
                let rows = mirror.then(sampled).rows();
                taps[count] = Tap {
                    row0: [rows[0][0], rows[0][1], rows[0][2], 0.0],
                    row1: [rows[1][0], rows[1][1], rows[1][2], 0.0],
                    weight: [w * gain[0], w * gain[1], w * gain[2], src as f32],
                    key: [key.threshold, key.softness, 0.0, 0.0],
                };
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
                taps,
            };
            queue.write_buffer(
                &self.uniforms,
                UNIFORM_STRIDE * m as u64,
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
                layer(self.newest),
                layer(next),
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        for (m, view) in self.layer_views[next].iter().enumerate() {
            if !refreshes(m) {
                continue;
            }
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.writing[next], &[self.uniform_offset(m)]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        self.newest = next;
        self.frame += 1;
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
    fn a_bank_is_a_ring_of_every_monitor_and_the_inputs() {
        // Worked out from the format rather than from `bank_bytes`: one
        // 1920x1080 layer of Rgba16Float is 8 bytes a texel, and the rig is
        // five monitors two frames deep — a pass reads every layer while
        // writing one — plus one layer for the seed.
        let layers = |p: &Params| Shape::of(p).layers();
        let mut rig = crate::config::instrument();
        rig.delay = 0;
        assert_eq!(rig.history(), 2);
        assert_eq!(layers(&rig), 2 * 5 + 1);
        assert_eq!(bank_bytes(&rig, (1920, 1080)), 11 * 1920 * 1080 * 8);
        // A frame of delay is another slab of every monitor in the ring, and
        // an input is one layer past the ring, delayed or not.
        rig.delay = 3;
        assert_eq!(rig.history(), 5);
        assert_eq!(layers(&rig), 5 * 5 + 1);
        assert_eq!(bank_bytes(&rig, (1920, 1080)), 26 * 1920 * 1080 * 8);
    }

    #[test]
    fn a_bank_past_the_cap_is_refused_and_the_4k_deployment_is_not() {
        // The resolution this instrument is deployed at: it fits, or the cap
        // would be refusing the thing it was written to allow.
        let rig = crate::config::instrument();
        assert!(bank_fits(&rig, (3840, 2160)).is_ok());
        // And the deepest ring the delay units can buy, which is past the cap
        // even at 1080 — the refusal says both halves of why, since neither
        // the graph nor the resolution alone is what went wrong.
        let mut most = rig.clone();
        most.delay = crate::params::Params::MAX_DELAY;
        assert_eq!(Shape::of(&most).layers(), 161);
        let why = bank_fits(&most, (1920, 1080)).unwrap_err();
        assert!(
            why.contains("161 bank layers") && why.contains("1920x1080"),
            "{why}"
        );
        // Eight frames of reach fit at 1080.
        most.delay = 8;
        assert_eq!(Shape::of(&most).layers(), 51);
        assert!(bank_fits(&most, (1920, 1080)).is_ok());
        // At 4K the same ring is past the cap by bytes, and the refusal says
        // how deep the ring is, which is what the delay can change.
        let why = bank_fits(&most, (3840, 2160)).unwrap_err();
        assert!(
            why.contains("51 bank layers (10 frames of 5 monitors, and the seed)")
                && why.contains("2.0 GiB"),
            "{why}"
        );
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
