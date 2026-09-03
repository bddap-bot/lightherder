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

use crate::affine::{sample_transform, Framing};
use crate::params::{Camera, Character, Key, Params};

/// Half-float so the loop keeps headroom above 1.0 and does not quantise to
/// bands after a few dozen passes.
const MONITOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Radius of the white blob, in screen units where the monitor is 1.0 tall.
/// The blob and not the seed: a camera-seeded monitor has a seed and no
/// spot, so the geometry belongs to the one variant that draws one.
const BLOB_RADIUS: f32 = 0.06;

/// Where the blob sits, in the same screen units. Off-centre on purpose: a
/// radially symmetric spot at the centre is a fixed point of rotation, so a
/// centred one would make the rotation knob do nothing visible.
const BLOB_CENTRE: [f32; 2] = [0.25, 0.0];

/// Most taps one monitor can be fed by. Sized for comfort: all-to-all with
/// every camera the board can select is five taps, so this leaves room for
/// each to look through a splitter several ways on top. `config::validate`
/// holds the line.
pub const MAX_TAPS: usize = 32;

/// The most GPU memory a bank may ask for. A cap in bytes rather than in
/// pixels because it is the layers that do the multiplying: the largest
/// undelayed graph `config::validate` allows — eight monitors and four
/// inputs — is 1.2 GiB of half-float at 3840x2160 and four times that at
/// 7680x4320, a frame of delay is another copy of every monitor on top, and
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
        "{} bank layers ({} frames of {} monitors, and {} inputs)",
        shape.layers(),
        shape.history,
        shape.monitors,
        shape.inputs
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
/// monitors as a ring, then the inputs. The one place a layer index comes
/// from, whether a tap reads it, an input's frame is written to it or a pass
/// draws on it — three formulas for one layout would be three ways to read
/// the wrong frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shape {
    history: usize,
    monitors: usize,
    inputs: usize,
}

impl Shape {
    pub(crate) fn of(params: &Params) -> Shape {
        Shape {
            history: params.history(),
            monitors: params.monitors.len(),
            inputs: params.inputs.len(),
        }
    }

    pub(crate) fn layers(self) -> usize {
        self.history * self.monitors + self.inputs
    }

    fn monitor(self, slab: usize, m: usize) -> usize {
        debug_assert!(slab < self.history && m < self.monitors);
        slab * self.monitors + m
    }

    /// Input `i`: past the whole ring, since an input is never delayed —
    /// nothing stands between the switcher and the outside light it was
    /// handed.
    fn input(self, i: usize) -> usize {
        debug_assert!(i < self.inputs);
        self.history * self.monitors + i
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

    /// The slab `camera` reads on pass number `pass`. The ring moves on one
    /// slab a pass and so does the look-back, so the slab stays put for
    /// `divider` passes and then jumps: no frame is copied to hold it.
    fn read(self, newest: usize, camera: &Camera, pass: u64) -> usize {
        let phase = (pass % camera.divider as u64) as u32;
        self.back(newest, camera.delay + phase)
    }
}

/// The edges of monitor `m`'s pass while the newest frame sits in ring slab
/// `newest`, on pass number `pass`: (the camera the light came through if
/// it came through one, source layer, routing weight times splitter
/// weight). This is where the switcher's two kinds of column meet the one
/// index space the bank is laid out in — a camera's taps read the ring as
/// far back as its delay and its hold, an input's the layer
/// [`Feedback::write_input`] writes its frame to.
///
/// A routed camera fans out over its beam splitter, since a camera watching
/// two monitors is two taps. A patched input is exactly one tap and carries
/// no camera.
///
/// The one definition of which edges become taps, so that what
/// [`Feedback::step`] writes and what [`reachable_taps`] bounds cannot drift
/// apart on the zero-weight rule.
pub(crate) fn taps_of(
    params: &Params,
    m: usize,
    newest: usize,
    pass: u64,
) -> impl Iterator<Item = (Through, usize, f32)> + '_ {
    let shape = Shape::of(params);
    let through_cameras = params.routing[m]
        .iter()
        .zip(&params.cameras)
        .enumerate()
        .filter(|(_, (route, _))| **route > 0.0)
        .flat_map(move |(c, (route, camera))| {
            camera
                .look
                .iter()
                .enumerate()
                .filter(|(_, look)| **look > 0.0)
                .map(move |(src, look)| {
                    let layer = shape.monitor(shape.read(newest, camera, pass), src);
                    (Through::Camera(c), layer, route * look)
                })
        });
    let straight_in = params
        .inputs
        .iter()
        .enumerate()
        .map(move |(i, plug)| (i, plug.into[m]))
        .filter(|(_, route)| *route > 0.0)
        .map(move |(i, route)| (Through::Input(i), shape.input(i), route));
    through_cameras.chain(straight_in)
}

/// The most taps any one monitor's pass can ever be given.
///
/// [`taps_of`] drops a column whose routing weight is zero, and a crosspoint
/// can be raised mid-performance — so the count a file loads with is not a
/// bound on the count the shader will be handed. This is that count with
/// every crosspoint treated as live, which is what [`config::validate`] holds
/// against [`MAX_TAPS`]: each camera contributes the monitors its splitter
/// can see, and each input the one tap it is. Every input, not the ones sent
/// somewhere on disk — a send is a crosspoint the panel turns, so a row of
/// zeroes at load is no promise about the tap count a second later. The look
/// weights are the other way about: no knob turns one, so a monitor a camera
/// cannot see stays uncounted.
pub(crate) fn reachable_taps(params: &Params) -> usize {
    let through_cameras: usize = params
        .cameras
        .iter()
        .map(|camera| camera.look.iter().filter(|look| **look > 0.0).count())
        .sum();
    through_cameras + params.inputs.len()
}

/// What a tap came through: one of the graph's cameras, or an input on its
/// way in past the switcher. Which, not merely whether — an input carries a
/// key of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Through {
    Camera(usize),
    Input(usize),
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
    /// The camera's keyer: x luma threshold, y softness, z chroma tolerance,
    /// w padding.
    key: [f32; 4],
    /// xyz: the RGB row measuring the key colour in a sample — zeroed when
    /// the tolerance stands at its off rail, so the default keys nothing
    /// however bright the loop runs. See `params::key_weights`.
    keyvec: [f32; 4],
}

/// Per-monitor uniforms, flipped by hand in `shaders/feedback.wgsl`, which
/// documents what each lane carries. The sizes are held together by
/// `min_binding_size` below.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// xy: blob centre in uv. zw: blob radii in uv, already aspect-corrected.
    blob: [f32; 4],
    /// Columns, each padded to 16 bytes: that is what a WGSL `mat3x3<f32>`
    /// is, and it is column-major where [`Colour::chroma_matrix`] is not.
    chroma: [[f32; 4]; 3],
    /// x: brightness. y: contrast. z: gamma. w: the white blob's brightness.
    levels: [f32; 4],
    /// x: tap count. y: this monitor's own layer, for the present pass.
    /// zw: where the bank splits round the slab this pass writes — the
    /// first layer past the lower view, and the first layer of the upper
    /// one.
    info: [f32; 4],
    /// The analog stage's per-monitor lanes, which are not [`Character`]'s
    /// fields — those are per camera and ride the taps. x: grain amplitude,
    /// summed over the cameras the switcher routes here. y: the amplifier's
    /// headroom. z: a frame counter, so the grain moves. w: the unsharp
    /// mask, [`Monitor::sharpness`].
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
    /// Passes taken. The grain reads it to move, and a divided camera to
    /// know where in its hold it stands — so a wrap would be a hold cut
    /// short, and it does not wrap.
    pass: u64,
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
        assert!(monitors > 0, "a graph with no monitors draws nothing");
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
                // What an input's frames are written through.
                | wgpu::TextureUsages::COPY_DST,
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
            pass: 0,
            uniforms,
            scratch: Vec::new(),
            layout,
            shader,
            pipeline,
        }
    }

    /// Where the white blob lands, in uv — the same spot on every monitor
    /// that has one. A blob-seeded loop is driven from here, so anything
    /// measuring the instrument needs to know it.
    pub fn blob_uv(&self) -> [f32; 2] {
        crate::affine::screen_to_uv(self.aspect()).apply(BLOB_CENTRE)
    }

    pub(crate) fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    pub(crate) fn monitors(&self) -> usize {
        self.shape.monitors
    }

    /// The size of every layer of the bank, and so the size an input's frames
    /// have to arrive at.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Puts one external frame — tightly packed RGBA8 — on input `i`'s source
    /// layer, where the cameras looking at it will find it.
    pub fn write_input(&mut self, queue: &wgpu::Queue, i: usize, rgba8: &[u8]) {
        assert!(i < self.shape.inputs, "input {i} of {}", self.shape.inputs);
        assert_eq!(
            rgba8.len(),
            crate::input::frame_bytes(self.size()),
            "input {i} handed over a short frame"
        );
        to_half(rgba8, &mut self.scratch);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.ring,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: self.shape.input(i) as u32,
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
            "the graph's monitors, inputs and reach are baked into the bank at creation"
        );
        // Everything else the tap flattening assumes — row lengths, weight
        // signs, the tap cap — is the loader's contract, re-asserted here so
        // a hand-built Params that skipped `config::load` fails loudly
        // instead of sampling the wrong layer. One compare per value the
        // graph holds, and no allocation on the success path, which is what
        // makes it affordable every frame.
        if let Err(why) = crate::config::validate(params) {
            panic!("unvalidated params reached the GPU: {why}");
        }
        let aspect = self.aspect();
        let blob = self.blob_uv();

        // Framings move every frame; the affine per camera is the same for
        // all of its taps, so it is worked out once.
        let framings: Vec<[[f32; 3]; 2]> = params
            .cameras
            .iter()
            .map(|camera| sample_transform(&camera.framing, aspect).rows())
            .collect();
        // What a tap with no camera samples through. An input is plugged
        // into the switcher, so nothing frames it: it arrives square on and
        // fills the monitor, which is the identity framing carried through
        // the same transform every camera's is.
        let square_on = sample_transform(&Framing::identity(), aspect).rows();
        // The slab after the newest holds the one frame in the ring older
        // than any delay reaches, so it is the one a pass may draw on.
        let next = (self.newest + 1) % self.shape.history;
        let (split, above) = (
            self.shape.monitor(next, 0),
            self.shape.monitor(next, 0) + self.shape.monitors,
        );

        for (m, monitor) in params.monitors.iter().enumerate() {
            let mut taps = [Tap::zeroed(); MAX_TAPS];
            let mut count = 0usize;
            for (through, src, w) in taps_of(params, m, self.newest, self.pass) {
                // There is no camera between the switcher and an input, so
                // every stage a camera would have takes its identity and the
                // layer arrives as itself. Its key is the switcher's own,
                // which is where the rig keys at all.
                let (rows, gain, character, key) = match through {
                    Through::Camera(c) => {
                        let camera = &params.cameras[c];
                        (&framings[c], camera.gain, camera.character, camera.key)
                    }
                    Through::Input(i) => {
                        (&square_on, [1.0; 3], Character::CLEAN, params.inputs[i].key)
                    }
                };
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
                let halo = step(character.bloom_radius);
                let bleed = step(character.chroma_bleed);
                // The chroma key is disarmed outright at its rail rather
                // than out-thresholded: the smoothstep alone would hold off
                // frames, but a loop signal can run past white and project
                // past any finite tolerance.
                let keyvec = if key.tolerance >= Key::TOLERANT {
                    [0.0; 3]
                } else {
                    crate::params::key_weights(key.hue)
                };
                taps[count] = Tap {
                    row0: [rows[0][0], rows[0][1], rows[0][2], 0.0],
                    row1: [rows[1][0], rows[1][1], rows[1][2], 0.0],
                    weight: [w * gain[0], w * gain[1], w * gain[2], src as f32],
                    halo,
                    bleed: [bleed[0], bleed[1], character.bloom, 0.0],
                    key: [key.threshold, key.softness, key.tolerance, 0.0],
                    keyvec: [keyvec[0], keyvec[1], keyvec[2], 0.0],
                };
                count += 1;
            }

            // Every camera the switcher routes here contributes its own
            // grain, scaled by how much of it this monitor is shown. Its
            // splitter does not come into it: the grain is the sensor's and
            // the cable's, added after the glass. Summed in quadrature,
            // because two sensors are two independent noise sources and
            // adding their amplitudes would overstate the pair by 40%. The
            // sends are not in the sum: an input arrives already a signal,
            // down no cable of this graph's, and grains nothing.
            let grain: f32 = params.routing[m]
                .iter()
                .zip(&params.cameras)
                .map(|(route, camera)| (route * camera.character.noise).powi(2))
                .sum::<f32>()
                .sqrt();

            let chroma = monitor.colour.chroma_matrix();
            let uniforms = Uniforms {
                // The blob is round on screen, so its uv radius is narrower
                // on the axis the monitor is wider on.
                blob: [blob[0], blob[1], BLOB_RADIUS / aspect, BLOB_RADIUS],
                chroma: std::array::from_fn(|col| {
                    [chroma[0][col], chroma[1][col], chroma[2][col], 0.0]
                }),
                levels: [
                    monitor.colour.brightness,
                    monitor.colour.contrast,
                    monitor.colour.gamma,
                    monitor.seed.brightness(),
                ],
                info: [count as f32, (split + m) as f32, split as f32, above as f32],
                analog: [
                    grain,
                    monitor.headroom,
                    // Past 2^24 an f32 skips integers. This copy is the grain's
                    // alone, so its wrap costs a repeat nobody will see.
                    (self.pass % (1 << 24)) as f32,
                    monitor.sharpness,
                ],
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
        for (m, view) in self.layer_views[next].iter().enumerate() {
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
        self.pass += 1;
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
        // 1920x1080 layer of Rgba16Float is 8 bytes a texel, and an
        // undelayed graph is two copies of every monitor, so a pass can read
        // every layer while writing one.
        let layers = |p: &Params| Shape::of(p).layers();
        let mut single = crate::config::single();
        assert_eq!(layers(&single), 2);
        assert_eq!(bank_bytes(&single, (1920, 1080)), 2 * 1920 * 1080 * 8);
        // Four monitors, and the layers are what multiply.
        let insanity = crate::config::insanity();
        assert_eq!(layers(&insanity), 8);
        assert_eq!(bank_bytes(&insanity, (3840, 2160)), 4 * 2 * 3840 * 2160 * 8);
        // A frame of delay is another slab of every monitor in the ring, and
        // an input is one layer past the ring, delayed or not.
        single.delay = 3;
        assert_eq!(single.history(), 5);
        single.cameras[0].divider = 3;
        assert_eq!(single.history(), 7);
        assert_eq!(layers(&single), 7);
        single.cameras[0].divider = 1;
        assert_eq!(layers(&single), 5);
        assert_eq!(bank_bytes(&single, (1920, 1080)), 5 * 1920 * 1080 * 8);
        single.inputs = vec![crate::params::Plug {
            source: crate::input::Input::Pattern(crate::input::Pattern::Bars),
            key: Key::OFF,
            into: vec![0.0],
        }];
        assert_eq!(layers(&single), 6);
        assert_eq!(bank_bytes(&single, (1920, 1080)), 6 * 1920 * 1080 * 8);
    }

    #[test]
    fn a_bank_past_the_cap_is_refused_and_the_4k_deployment_is_not() {
        // The resolution this instrument is deployed at, on the largest
        // graph that ships: it fits, or the cap would be refusing the thing
        // it was written to allow.
        let insanity = crate::config::insanity();
        assert!(bank_fits(&insanity, (3840, 2160)).is_ok());
        // And the largest graph `config::validate` permits at all.
        let mut most = insanity.clone();
        most.monitors = vec![most.monitors[0].clone(); crate::config::MAX_MONITORS];
        most.inputs = vec![
            crate::params::Plug {
                source: crate::input::Input::Pattern(crate::input::Pattern::Bars),
                key: Key::OFF,
                into: vec![0.0; crate::config::MAX_MONITORS],
            };
            crate::config::MAX_INPUTS
        ];
        assert_eq!(Shape::of(&most).layers(), 20);
        assert!(bank_fits(&most, (3840, 2160)).is_ok());
        // Four times the texels each, on twenty of them, is past it — and the
        // refusal says both halves of why, since neither the graph nor the
        // resolution alone is what went wrong.
        let why = bank_fits(&most, (7680, 4320)).unwrap_err();
        assert!(
            why.contains("20 bank layers") && why.contains("7680x4320"),
            "{why}"
        );
        // The ring counts: ten frames deep, eighty-four layers: past the cap by bytes, and the
        // refusal says how deep the ring is, which is what the file can
        // change.
        most.delay = 8;
        let why = bank_fits(&most, (3840, 2160)).unwrap_err();
        assert!(
            why.contains("84 bank layers (10 frames of 8 monitors, and 4 inputs)")
                && why.contains("2.0 GiB"),
            "{why}"
        );
        // The full delay is 260 layers, more than a texture array holds
        // however small the monitors — refused by depth, not by bytes.
        most.delay = crate::params::Params::MAX_DELAY;
        let why = bank_fits(&most, (640, 480)).unwrap_err();
        assert!(
            why.contains("260 bank layers") && why.contains("256 layers"),
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
