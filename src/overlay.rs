use crate::lamps::{lamp, Lamplight};
use crate::midi::{spot, Spot, BUTTONS, FADERS, TRANSPORT};
use crate::params::{End, Flow, Focus, Knob, Node, Params};
use crate::present::{mark_thickness, Bank, Rect};
use crate::rig::CAMERAS;

struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

const BACK: [u8; 4] = [0, 0, 0, 200];
const DIM: [u8; 4] = [255, 255, 255, 70];
const LIT: [u8; 4] = [255, 255, 255, 255];

const GLYPH: i32 = 8;
const STRIP_W: i32 = 84;
const BUTTON_W: i32 = 60;
const BUTTON_H: i32 = 16;
const BUTTON_PITCH: i32 = 64;
const PAD: i32 = 10;
const GROUP_LIFT: i32 = GLYPH + 2;
const TRANSPORT_W: i32 = 5 * BUTTON_PITCH - (BUTTON_PITCH - BUTTON_W);
const STRIPS_X: i32 = PAD + TRANSPORT_W + 16;
const STRIPS: u8 = crate::midi::STRIPS as u8;
const PANEL_W: i32 = STRIPS_X + STRIPS as i32 * STRIP_W + PAD;
const ROWS_Y: [i32; 3] = [66, 84, 102];
const SQUARE: i32 = 14;
const PANEL_H: i32 = 152;
const ROTARY_Y: i32 = 24;
const ROTARY_R: i32 = 12;
const ROTARY_CAPTION_Y: i32 = 42;
const ROTARY_VALUE_Y: i32 = 52;
const FADER_CAPTION_Y: i32 = 124;
const FADER_VALUE_Y: i32 = 134;
const THUMB_H: i32 = 3;
const ROTARY_SWEEP: f32 = 1.5 * std::f32::consts::PI;

const SOURCE_W: i32 = 64;
const SOURCE_H: i32 = 16;
const SOURCES: usize = CAMERAS + 1;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Seat {
    col: f32,
    row: f32,
}

const SEATS: [Seat; SOURCES] = [
    Seat { col: 0.5, row: 1.0 },
    Seat {
        col: 1.25,
        row: 1.0,
    },
    Seat { col: 2.5, row: 1.0 },
    Seat {
        col: 1.75,
        row: 1.0,
    },
];

const MAX_ARROWS: usize = 40;
const _: () = assert!(MAX_ARROWS.is_multiple_of(4));
const _: () = assert!(MAX_ARROWS >= 2 * CAMERAS * crate::rig::MONITORS + crate::rig::MONITORS);

struct Canvas {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Canvas {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend(BACK);
        }
        Canvas {
            width,
            height,
            pixels,
        }
    }

    fn set(&mut self, x: i32, y: i32, colour: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let at = ((y * self.width + x) * 4) as usize;
        self.pixels[at..at + 4].copy_from_slice(&colour);
    }

    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, colour: [u8; 4]) {
        for dy in 0..h {
            for dx in 0..w {
                self.set(x + dx, y + dy, colour);
            }
        }
    }

    fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, colour: [u8; 4]) {
        for dx in 0..w {
            self.set(x + dx, y, colour);
            self.set(x + dx, y + h - 1, colour);
        }
        for dy in 0..h {
            self.set(x, y + dy, colour);
            self.set(x + w - 1, y + dy, colour);
        }
    }

    fn ring(&mut self, cx: i32, cy: i32, r: i32, colour: [u8; 4]) {
        for y in -r..=r {
            for x in -r..=r {
                let d = ((x * x + y * y) as f32).sqrt() - r as f32;
                if d.abs() <= 0.5 {
                    self.set(cx + x, cy + y, colour);
                }
            }
        }
    }

    fn line(&mut self, from: (i32, i32), to: (i32, i32), colour: [u8; 4]) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let steps = dx.abs().max(dy.abs()).max(1);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.set(
                from.0 + (dx as f32 * t).round() as i32,
                from.1 + (dy as f32 * t).round() as i32,
                colour,
            );
        }
    }

    fn text(&mut self, x: i32, y: i32, text: &str, max_x: i32, colour: [u8; 4]) {
        for (i, ch) in text.chars().enumerate() {
            let at = x + i as i32 * GLYPH;
            if at + GLYPH > max_x {
                return;
            }
            let glyph = font8x8::legacy::BASIC_LEGACY
                .get(ch as usize)
                .unwrap_or(&[0; 8]);
            for (dy, row) in glyph.iter().enumerate() {
                for dx in 0..8 {
                    if row & (1 << dx) != 0 {
                        self.set(at + dx, y + dy as i32, colour);
                    }
                }
            }
        }
    }

    fn text_centred(&mut self, cx: i32, y: i32, text: &str, colour: [u8; 4]) {
        let w = text.chars().count() as i32 * GLYPH;
        self.text(cx - w / 2, y, text, self.width, colour);
    }

    fn raster(self) -> Raster {
        Raster {
            width: self.width as u32,
            height: self.height as u32,
            pixels: self.pixels,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Reading {
    value: f32,
    fraction: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readout {
    knobs: [Reading; Knob::ALL.len()],
    lamps: Lamplight,
}

impl Readout {
    pub(crate) fn of(params: &Params, focus: Focus, lamps: Lamplight) -> Readout {
        let knobs = Knob::ALL.map(|knob| {
            let value = params.knob(knob, focus);
            Reading {
                value,
                fraction: knob.limit(params).fraction(value),
            }
        });
        Readout { knobs, lamps }
    }

    pub(crate) fn reads(&self, knob: Knob) -> String {
        knob.reads(self.knobs[knob as usize].value)
    }

    fn fraction(&self, knob: Knob) -> f32 {
        self.knobs[knob as usize].fraction
    }

    fn lit(&self, cc: u8) -> bool {
        self.lamps & lamp(cc) != 0
    }
}

fn strip_x(i: u8) -> i32 {
    STRIPS_X + i as i32 * STRIP_W
}

fn track_x(i: u8) -> i32 {
    strip_x(i) + STRIP_W - 14
}

fn strip_chrome(c: &mut Canvas, i: u8) {
    let x = strip_x(i);
    c.ring(x + STRIP_W / 2, ROTARY_Y, ROTARY_R, DIM);
    for y in ROWS_Y {
        c.frame(x + 2, y, SQUARE, SQUARE, DIM);
    }
    c.frame(
        track_x(i),
        ROWS_Y[0],
        12,
        ROWS_Y[2] + SQUARE - ROWS_Y[0],
        DIM,
    );
}

fn thumb_y(fraction: f32) -> i32 {
    let (top, bottom) = (ROWS_Y[0], ROWS_Y[2] + SQUARE);
    let travel = bottom - top - THUMB_H - 2;
    bottom - 1 - THUMB_H - (fraction * travel as f32).round() as i32
}

fn thumb(c: &mut Canvas, i: u8, colour: [u8; 4], fraction: f32) {
    c.fill(track_x(i) - 1, thumb_y(fraction), 14, THUMB_H, colour);
}

fn needle(c: &mut Canvas, i: u8, colour: [u8; 4], fraction: f32) {
    let cx = strip_x(i) + STRIP_W / 2;
    let angle = (fraction - 0.5) * ROTARY_SWEEP;
    let reach = (ROTARY_R - 2) as f32;
    let tip = (
        cx + (angle.sin() * reach).round() as i32,
        ROTARY_Y - (angle.cos() * reach).round() as i32,
    );
    c.line((cx, ROTARY_Y), tip, colour);
}

fn button_x(col: u8) -> i32 {
    PAD + col as i32 * BUTTON_PITCH
}

fn transport_button(c: &mut Canvas, row: u8, col: u8, colour: [u8; 4]) {
    c.frame(
        button_x(col),
        ROWS_Y[row as usize],
        BUTTON_W,
        BUTTON_H,
        colour,
    );
}

fn group_labels(c: &mut Canvas) {
    for (i, t) in TRANSPORT.iter().enumerate() {
        let Some(name) = t.group else { continue };
        if TRANSPORT[..i].iter().any(|e| e.group == Some(name)) {
            continue;
        }
        let (lo, hi) = TRANSPORT
            .iter()
            .filter(|e| e.group == Some(name))
            .fold((t.col, t.col), |(lo, hi), e| (lo.min(e.col), hi.max(e.col)));
        c.text_centred(
            (button_x(lo) + button_x(hi) + BUTTON_W) / 2,
            ROWS_Y[t.row as usize] - GROUP_LIFT,
            name,
            DIM,
        );
    }
}

#[derive(Clone, Debug)]
enum Control {
    Knob(Knob),
    Button(String),
}

fn place(c: &mut Canvas, cc: u8, control: &Control, readout: &Readout) {
    let spot = spot(cc).expect("every bound control is on the panel");
    let beside = |c: &mut Canvas, i: u8, row: usize, label: &str| {
        let x = strip_x(i);
        match readout.lit(cc) {
            true => c.fill(x + 2, ROWS_Y[row], SQUARE, SQUARE, LIT),
            false => c.frame(x + 2, ROWS_Y[row], SQUARE, SQUARE, LIT),
        }
        c.text(x + SQUARE + 4, ROWS_Y[row] + 3, label, track_x(i) - 1, LIT);
    };
    match (spot, control) {
        (Spot::Fader(i), Control::Knob(knob)) => {
            c.frame(
                track_x(i),
                ROWS_Y[0],
                12,
                ROWS_Y[2] + SQUARE - ROWS_Y[0],
                LIT,
            );
            thumb(c, i, LIT, readout.fraction(*knob));
            let cx = strip_x(i) + STRIP_W / 2;
            c.text_centred(cx, FADER_CAPTION_Y, knob.name(), LIT);
            c.text_centred(cx, FADER_VALUE_Y, &readout.reads(*knob), LIT);
        }
        (Spot::Rotary(i), Control::Knob(knob)) => {
            let cx = strip_x(i) + STRIP_W / 2;
            c.ring(cx, ROTARY_Y, ROTARY_R, LIT);
            needle(c, i, LIT, readout.fraction(*knob));
            c.text_centred(cx, ROTARY_CAPTION_Y, knob.name(), LIT);
            c.text_centred(cx, ROTARY_VALUE_Y, &readout.reads(*knob), LIT);
        }
        (Spot::S(i), Control::Button(label)) => beside(c, i, 0, label),
        (Spot::M(i), Control::Button(label)) => beside(c, i, 1, label),
        (Spot::R(i), Control::Button(label)) => beside(c, i, 2, label),
        (Spot::Transport(t), Control::Button(label)) => {
            let (x, y) = (button_x(t.col), ROWS_Y[t.row as usize]);
            let ink = match readout.lit(cc) {
                true => {
                    c.fill(x, y, BUTTON_W, BUTTON_H, LIT);
                    BACK
                }
                false => {
                    transport_button(c, t.row, t.col, LIT);
                    LIT
                }
            };
            c.text(x + 2, y + 4, label, x + BUTTON_W - 1, ink);
        }
        (Spot::Fader(_) | Spot::Rotary(_), Control::Button(_))
        | (Spot::S(_) | Spot::M(_) | Spot::R(_) | Spot::Transport(_), Control::Knob(_)) => {
            unreachable!("a knob sits on a fader or rotary and a button on a button")
        }
    }
}

fn controls() -> impl Iterator<Item = (u8, Control)> {
    let faders = FADERS.iter().map(|f| (f.cc, Control::Knob(f.knob)));
    let buttons = BUTTONS
        .iter()
        .map(|b| (b.cc, Control::Button(b.action.caption())));
    faders.chain(buttons)
}

fn dead_indicators(c: &mut Canvas) {
    let bound = |wanted: Spot| {
        FADERS.iter().any(|f| match (spot(f.cc), wanted) {
            (Some(Spot::Fader(a)), Spot::Fader(b)) | (Some(Spot::Rotary(a)), Spot::Rotary(b)) => {
                a == b
            }
            _ => false,
        })
    };
    for i in 0..STRIPS {
        if !bound(Spot::Fader(i)) {
            thumb(c, i, DIM, 0.5);
        }
        if !bound(Spot::Rotary(i)) {
            needle(c, i, DIM, 0.5);
        }
    }
}

fn rasterize(readout: &Readout) -> Raster {
    let mut c = Canvas::new(PANEL_W, PANEL_H);
    for i in 0..STRIPS {
        strip_chrome(&mut c, i);
    }
    dead_indicators(&mut c);
    for t in TRANSPORT {
        transport_button(&mut c, t.row, t.col, DIM);
    }
    group_labels(&mut c);
    for (cc, control) in controls() {
        place(&mut c, cc, &control, readout);
    }
    c.raster()
}

fn source_name(i: usize) -> String {
    if i < CAMERAS {
        format!("{} {}", Node::Camera.short(), i + 1)
    } else {
        "seed".to_string()
    }
}

fn source(i: usize) -> Raster {
    let mut c = Canvas::new(SOURCE_W, SOURCE_H);
    c.frame(0, 0, SOURCE_W, SOURCE_H, LIT);
    c.text_centred(SOURCE_W / 2, 4, &source_name(i), LIT);
    c.raster()
}

fn seats(bank: &Bank) -> Option<[Rect; SOURCES]> {
    let size = (SOURCE_W as u32, SOURCE_H as u32);
    let scale = scale_into(size, (bank.cell.0 * 0.3, bank.cell.1 * 0.15))?;
    let (w, h) = (SOURCE_W as f32 * scale, SOURCE_H as f32 * scale);
    Some(std::array::from_fn(|i| Rect {
        x: SEATS[i].col * bank.cell.0 - w / 2.0,
        y: SEATS[i].row * bank.cell.1 - h / 2.0,
        w,
        h,
    }))
}

fn scale_into(size: (u32, u32), room: (f32, f32)) -> Option<f32> {
    let scale = (room.0 / size.0 as f32).min(room.1 / size.1 as f32);
    let scale = if scale >= 1.0 { scale.floor() } else { scale };
    (scale > 0.0).then_some(scale)
}

const MARGIN: f32 = 24.0;

fn panel_placement(size: (u32, u32), target: (u32, u32)) -> Option<Rect> {
    let scale = scale_into(size, (target.0 as f32 * 0.9, target.1 as f32 * 0.9))?;
    let (w, h) = (size.0 as f32 * scale, size.1 as f32 * scale);
    Some(Rect {
        x: (target.0 as f32 - w - MARGIN).max(0.0),
        y: (target.1 as f32 - h - MARGIN).max(0.0),
        w,
        h,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Arrow {
    from: [f32; 2],
    to: [f32; 2],
    share: f32,
}

fn centre(r: Rect) -> [f32; 2] {
    [r.x + r.w / 2.0, r.y + r.h / 2.0]
}

fn border(r: Rect, along: [f32; 2], past: f32) -> [f32; 2] {
    let c = centre(r);
    let reach = |half: f32, d: f32| {
        if d.abs() < 1e-6 {
            f32::INFINITY
        } else {
            half / d.abs()
        }
    };
    let t = reach(r.w / 2.0, along[0]).min(reach(r.h / 2.0, along[1])) + past;
    [c[0] + along[0] * t, c[1] + along[1] * t]
}

fn arrow(from: Rect, to: Rect, share: f32, line: f32) -> Option<Arrow> {
    let (a, b) = (centre(from), centre(to));
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy);
    if len < 1e-3 {
        return None;
    }
    let d = [dx / len, dy / len];
    let side = [-d[1] * 2.0 * line, d[0] * 2.0 * line];
    let start = border(from, d, 3.0 * line);
    let end = border(to, [-d[0], -d[1]], 3.0 * line);
    let shifted = |p: [f32; 2]| [p[0] + side[0], p[1] + side[1]];
    Some(Arrow {
        from: shifted(start),
        to: shifted(end),
        share,
    })
}

fn arrows(
    flows: impl Iterator<Item = Flow>,
    tiles: &[Rect],
    sources: &[Rect; SOURCES],
    line: f32,
) -> Vec<Arrow> {
    let rect = |end: End| match end {
        End::Monitor(m) => tiles.get(m).copied(),
        End::Camera(c) => sources.get(c).copied(),
        End::Seed => Some(sources[CAMERAS]),
    };
    flows
        .filter_map(|f| arrow(rect(f.from)?, rect(f.to)?, f.share, line))
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ArrowsUniform {
    count: [u32; 4],
    line: [f32; 4],
    segments: [[f32; 4]; MAX_ARROWS],
    shares: [[f32; 4]; MAX_ARROWS / 4],
}

impl ArrowsUniform {
    fn of(arrows: &[Arrow], line: f32) -> ArrowsUniform {
        let mut uniform = ArrowsUniform {
            count: [arrows.len().min(MAX_ARROWS) as u32, 0, 0, 0],
            line: [line, 0.0, 0.0, 0.0],
            segments: [[0.0; 4]; MAX_ARROWS],
            shares: [[0.0; 4]; MAX_ARROWS / 4],
        };
        for (i, a) in arrows.iter().take(MAX_ARROWS).enumerate() {
            uniform.segments[i] = [a.from[0], a.from[1], a.to[0], a.to[1]];
            uniform.shares[i / 4][i % 4] = 0.35 + 0.65 * a.share.clamp(0.0, 1.0);
        }
        uniform
    }
}

struct Image {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, raster: &Raster) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &raster.pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(raster.width * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: raster.width,
            height: raster.height,
            depth_or_array_layers: 1,
        },
    );
}

impl Image {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        raster: &Raster,
    ) -> Image {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay"),
            size: wgpu::Extent3d {
                width: raster.width,
                height: raster.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write(queue, &texture, raster);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Image {
            texture,
            bind_group,
            size: (raster.width, raster.height),
        }
    }

    fn blit(&self, pass: &mut wgpu::RenderPass, pipeline: &wgpu::RenderPipeline, at: Rect) {
        pass.set_viewport(at.x, at.y, at.w, at.h, 0.0, 1.0);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

pub struct Overlay {
    blit: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    panel: Option<(Readout, Image)>,
    sources: [Image; SOURCES],
    arrows: wgpu::RenderPipeline,
    arrows_uniform: wgpu::Buffer,
    arrows_bind: wgpu::BindGroup,
}

impl Overlay {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Overlay {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("overlay"),
            ..Default::default()
        });
        let sources =
            std::array::from_fn(|i| Image::new(device, queue, &layout, &sampler, &source(i)));
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/overlay.wgsl"));
        let blit = crate::fullscreen_pipeline(
            device,
            &shader,
            &layout,
            "fs_overlay",
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            "overlay",
        );

        let arrows_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("arrows"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<ArrowsUniform>() as u64
                    ),
                },
                count: None,
            }],
        });
        let arrows_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("arrows"),
            size: std::mem::size_of::<ArrowsUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let arrows_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("arrows"),
            layout: &arrows_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: arrows_uniform.as_entire_binding(),
            }],
        });
        let arrows = crate::fullscreen_pipeline(
            device,
            &shader,
            &arrows_layout,
            "fs_arrows",
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            "arrows",
        );
        Overlay {
            blit,
            layout,
            sampler,
            panel: None,
            sources,
            arrows,
            arrows_uniform,
            arrows_bind,
        }
    }

    pub(crate) fn show(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, readout: Readout) {
        match &mut self.panel {
            Some((shown, _)) if *shown == readout => {}
            Some((shown, image)) => {
                write(queue, &image.texture, &rasterize(&readout));
                *shown = readout;
            }
            None => {
                let image = Image::new(
                    device,
                    queue,
                    &self.layout,
                    &self.sampler,
                    &rasterize(&readout),
                );
                self.panel = Some((readout, image));
            }
        }
    }

    pub(crate) fn draw(
        &self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass,
        target_size: (u32, u32),
        params: &Params,
        bank: Option<&Bank>,
    ) {
        let Some((_, panel)) = &self.panel else {
            return;
        };
        let Some(at) = panel_placement(panel.size, target_size) else {
            return;
        };
        if let Some((bank, boxes)) = bank.and_then(|b| Some((b, seats(b)?))) {
            let line = mark_thickness(target_size.1);
            let arrows = arrows(params.flows(), &bank.tiles, &boxes, line);
            queue.write_buffer(
                &self.arrows_uniform,
                0,
                bytemuck::bytes_of(&ArrowsUniform::of(&arrows, line)),
            );
            pass.set_viewport(
                0.0,
                0.0,
                target_size.0 as f32,
                target_size.1 as f32,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.arrows);
            pass.set_bind_group(0, &self.arrows_bind, &[]);
            pass.draw(0..3, 0..1);
            for (image, r) in self.sources.iter().zip(boxes) {
                image.blit(pass, &self.blit, r);
            }
        }
        panel.blit(pass, &self.blit, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::midi::CLUTCH;
    use crate::params::Node;
    use crate::rig::MONITORS;

    fn at_rest() -> Readout {
        Readout::of(&crate::config::instrument(), Focus::default(), 0)
    }

    fn lit_texels(r: &Raster) -> usize {
        r.pixels.chunks(4).filter(|p| *p == LIT).count()
    }

    fn box_of(pixels: &[u8], width: i32, x: i32, y: i32, w: i32, h: i32) -> Vec<[u8; 4]> {
        let mut out = Vec::new();
        for row in y..y + h {
            for col in x..x + w {
                let at = ((row * width + col) * 4) as usize;
                out.push(pixels[at..at + 4].try_into().expect("four channels"));
            }
        }
        out
    }

    fn marked_texels(r: &Raster, x: i32, y: i32, w: i32, h: i32) -> usize {
        box_of(&r.pixels, r.width as i32, x, y, w, h)
            .iter()
            .filter(|p| **p != BACK)
            .count()
    }

    #[test]
    fn the_left_cluster_is_arranged_the_way_the_surface_is() {
        let raster = rasterize(&at_rest());
        assert_eq!(button_x(0), PAD);
        assert!(button_x(4) + BUTTON_W < STRIPS_X);
        assert_eq!(
            marked_texels(&raster, button_x(1), ROWS_Y[1], BUTTON_W, BUTTON_H),
            0,
        );
        for col in [0, 2, 3, 4] {
            assert!(
                marked_texels(&raster, button_x(col), ROWS_Y[1], BUTTON_W, BUTTON_H) > 0,
                "middle row column {col}",
            );
        }
        let says = |cx: i32, row: usize, word: &str| {
            let mut want = Canvas::new(PANEL_W, PANEL_H);
            want.text_centred(cx, ROWS_Y[row] - GROUP_LIFT, word, DIM);
            let w = word.chars().count() as i32 * GLYPH;
            let (x, y) = (cx - w / 2 - GLYPH, ROWS_Y[row] - GROUP_LIFT);
            assert_eq!(
                box_of(
                    &raster.pixels,
                    raster.width as i32,
                    x,
                    y,
                    w + 2 * GLYPH,
                    GLYPH
                ),
                box_of(&want.pixels, want.width, x, y, w + 2 * GLYPH, GLYPH),
                "{word}",
            );
        };
        says((button_x(0) + button_x(1) + BUTTON_W) / 2, 0, "TRACK");
        says((button_x(2) + button_x(4) + BUTTON_W) / 2, 1, "MARKER");
    }

    #[test]
    fn the_panel_is_drawn_and_captioned() {
        let raster = rasterize(&at_rest());
        assert_eq!(
            (raster.width, raster.height),
            (PANEL_W as u32, PANEL_H as u32)
        );
        assert!(lit_texels(&raster) > 1000, "{}", lit_texels(&raster));
    }

    #[test]
    fn every_caption_lands_on_its_own_control() {
        let readout = at_rest();
        let raster = rasterize(&readout);
        for (cc, control) in controls() {
            let mut want = Canvas::new(PANEL_W, PANEL_H);
            place(&mut want, cc, &control, &readout);
            let lit: Vec<(i32, i32)> = (0..PANEL_H)
                .flat_map(|y| (0..PANEL_W).map(move |x| (x, y)))
                .filter(|(x, y)| {
                    let at = ((y * PANEL_W + x) * 4) as usize;
                    want.pixels[at..at + 4] == LIT
                })
                .collect();
            assert!(
                lit.len() > 20,
                "cc {cc}: {control:?} drew {} texels",
                lit.len()
            );
            for (x, y) in lit {
                let at = ((y * PANEL_W + x) * 4) as usize;
                assert_eq!(
                    raster.pixels[at..at + 4],
                    LIT,
                    "cc {cc}: {control:?} at {x},{y}"
                );
            }
        }
    }

    fn row_index(node: Node) -> usize {
        match spot(crate::midi::row_of(node)).expect("a select row is a block of spots") {
            Spot::S(_) => 0,
            Spot::M(_) => 1,
            Spot::R(_) => 2,
            other => panic!("a select row landed on {other:?}"),
        }
    }

    fn band(pixels: &[u8], width: i32, node: Node, i: u8) -> Vec<[u8; 4]> {
        let w = track_x(i) - strip_x(i) - 1;
        box_of(
            pixels,
            width,
            strip_x(i),
            ROWS_Y[row_index(node)],
            w,
            SQUARE,
        )
    }

    fn band_of(node: Node, i: u8, draw: impl Fn(&mut Canvas)) -> Vec<[u8; 4]> {
        let mut c = Canvas::new(PANEL_W, PANEL_H);
        draw(&mut c);
        band(&c.pixels, c.width, node, i)
    }

    fn asleep(node: Node, i: u8) -> Vec<[u8; 4]> {
        band_of(node, i, |c| strip_chrome(c, i))
    }

    fn captioned(node: Node, i: u8, caption: &str) -> Vec<[u8; 4]> {
        band_of(node, i, |c| {
            strip_chrome(c, i);
            place(
                c,
                crate::midi::row_of(node) + i,
                &Control::Button(caption.to_string()),
                &at_rest(),
            );
        })
    }

    #[test]
    fn a_select_row_is_drawn_for_its_own_kind_and_stops_where_the_graph_does() {
        let raster = rasterize(&at_rest());
        for node in Node::ALL {
            for i in 0..STRIPS {
                let bound = BUTTONS
                    .iter()
                    .find(|b| b.cc == crate::midi::row_of(node) + i);
                let want = match bound {
                    Some(b) => captioned(node, i, &b.action.caption()),
                    None => asleep(node, i),
                };
                assert_eq!(
                    band(&raster.pixels, raster.width as i32, node, i),
                    want,
                    "{} {}",
                    node.short(),
                    i + 1
                );
            }
        }
    }

    #[test]
    fn a_readout_is_indexed_the_way_the_knobs_are_listed() {
        for (i, knob) in Knob::ALL.iter().enumerate() {
            assert_eq!(*knob as usize, i, "{knob:?}");
        }
    }

    fn strip_of(knob: Knob) -> u8 {
        let cc = FADERS.iter().find(|f| f.knob == knob).unwrap().cc;
        match spot(cc).unwrap() {
            Spot::Fader(i) | Spot::Rotary(i) => i,
            other => panic!("{knob:?} sits on {other:?}"),
        }
    }

    #[test]
    fn a_knob_is_drawn_where_the_program_holds_it_and_nothing_else_moves() {
        let mut params = crate::config::instrument();
        let focus = Focus::default();
        let rest = rasterize(&Readout::of(&params, focus, 0));
        params.rig.switchers[0] = 0.0;
        let moved = Readout::of(&params, focus, 0);
        assert_eq!(moved.reads(Knob::Switcher), "0.000");
        assert_eq!(at_rest().reads(Knob::Switcher), "1.000");
        let thrown = rasterize(&moved);
        let strip = strip_of(Knob::Switcher);
        let (x0, x1) = (strip_x(strip), strip_x(strip) + STRIP_W);
        for y in 0..PANEL_H {
            for x in 0..PANEL_W {
                let at = ((y * PANEL_W + x) * 4) as usize;
                if (x0..x1).contains(&x) {
                    continue;
                }
                assert_eq!(
                    rest.pixels[at..at + 4],
                    thrown.pixels[at..at + 4],
                    "a switcher moved changed {x},{y}"
                );
            }
        }
        let thumb = |r: &Raster, y: i32| marked_texels(r, track_x(strip) - 1, y, 14, THUMB_H);
        assert_eq!(thumb(&rest, thumb_y(1.0)), 14 * THUMB_H as usize);
        assert_eq!(thumb(&thrown, thumb_y(0.0)), 14 * THUMB_H as usize);
        assert!(thumb_y(0.0) > thumb_y(0.5) && thumb_y(0.5) > thumb_y(1.0));
        assert!(thumb_y(1.0) > ROWS_Y[0] && thumb_y(0.0) + THUMB_H < ROWS_Y[2] + SQUARE);
    }

    #[test]
    fn one_thumb_per_fader_and_one_needle_per_rotary() {
        let raster = rasterize(&at_rest());
        let track = |i: u8| {
            let x = track_x(i) + 1;
            (ROWS_Y[0] + 1..ROWS_Y[2] + SQUARE - 1)
                .filter(|y| marked_texels(&raster, x, *y, 1, 1) > 0)
                .count()
        };
        let needles = |i: u8| {
            let cx = strip_x(i) + STRIP_W / 2;
            let r = ROTARY_R - 3;
            let ring = (0..360).step_by(3).map(|deg| {
                let a = (deg as f32).to_radians();
                (
                    cx + (a.sin() * r as f32).round() as i32,
                    ROTARY_Y - (a.cos() * r as f32).round() as i32,
                )
            });
            let samples: Vec<bool> = ring
                .map(|(x, y)| marked_texels(&raster, x, y, 1, 1) > 0)
                .collect();
            let mut on = *samples.last().unwrap();
            let mut hits = 0;
            for hit in samples {
                hits += usize::from(hit && !on);
                on = hit;
            }
            hits
        };
        for i in 0..STRIPS {
            assert_eq!(track(i), THUMB_H as usize, "fader {}", i + 1);
            assert_eq!(needles(i), 1, "rotary {}", i + 1);
        }
    }

    #[test]
    fn a_lit_lamp_fills_its_button_on_the_panel() {
        let params = crate::config::instrument();
        let dark = rasterize(&Readout::of(&params, Focus::default(), 0));
        let lit = rasterize(&Readout::of(
            &params,
            Focus::default(),
            lamp(46) | lamp(CLUTCH),
        ));
        let inside = |r: &Raster, x: i32, y: i32, w: i32, h: i32| {
            box_of(&r.pixels, r.width as i32, x + 1, y + 1, w - 2, h - 2)
                .iter()
                .filter(|p| **p == LIT)
                .count()
        };
        let Spot::Transport(help) = spot(46).unwrap() else {
            panic!("help is on the transport")
        };
        let (x, y) = (button_x(help.col), ROWS_Y[help.row as usize]);
        assert!(
            inside(&lit, x, y, BUTTON_W, BUTTON_H) > inside(&dark, x, y, BUTTON_W, BUTTON_H) + 400
        );
        let Spot::S(i) = spot(CLUTCH).unwrap() else {
            panic!("the clutch is on the S row")
        };
        let (x, y) = (strip_x(i) + 2, ROWS_Y[0]);
        assert_eq!(
            inside(&lit, x, y, SQUARE, SQUARE),
            ((SQUARE - 2) * (SQUARE - 2)) as usize
        );
        assert_eq!(inside(&dark, x, y, SQUARE, SQUARE), 0);
    }

    fn tiled() -> Bank {
        crate::present::bank((1920, 1080), 16.0 / 9.0, MONITORS).unwrap()
    }

    fn inside(p: [f32; 2], r: Rect, slack: f32) -> bool {
        p[0] >= r.x - slack
            && p[0] <= r.x + r.w + slack
            && p[1] >= r.y - slack
            && p[1] <= r.y + r.h + slack
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    #[test]
    fn the_arrows_run_from_each_flows_source_to_its_sink() {
        let params = crate::config::instrument();
        let flows: Vec<Flow> = params.flows().collect();
        let bank = tiled();
        let boxes = seats(&bank).unwrap();
        let arrows = arrows(flows.iter().copied(), &bank.tiles, &boxes, 2.0);
        assert_eq!(arrows.len(), flows.len());
        let looks = flows
            .iter()
            .filter(|f| matches!(f.to, End::Camera(_)))
            .count();
        let feeds = flows
            .iter()
            .filter(|f| matches!(f.from, End::Camera(_)))
            .count();
        let seeds = flows.iter().filter(|f| f.from == End::Seed).count();
        assert_eq!((looks, feeds, seeds), (5, 3, 2), "{flows:?}");
        for (flow, arrow) in flows.iter().zip(&arrows) {
            let rect = |end: End| match end {
                End::Monitor(m) => bank.tiles[m],
                End::Camera(c) => boxes[c],
                End::Seed => boxes[CAMERAS],
            };
            let (from, to) = (rect(flow.from), rect(flow.to));
            assert!(
                inside(arrow.from, from, 12.0) && !inside(arrow.from, from, -4.0),
                "{flow:?} starts at {:?}",
                arrow.from
            );
            assert!(
                inside(arrow.to, to, 12.0) && !inside(arrow.to, to, -4.0),
                "{flow:?} ends at {:?}",
                arrow.to
            );
        }
    }

    fn crosses(a: &Arrow, b: &Arrow) -> bool {
        let side = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
            (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
        };
        let (s1, s2) = (side(a.from, a.to, b.from), side(a.from, a.to, b.to));
        let (s3, s4) = (side(b.from, b.to, a.from), side(b.from, b.to, a.to));
        s1 * s2 < 0.0 && s3 * s4 < 0.0
    }

    fn crossings(arrows: &[Arrow]) -> usize {
        arrows
            .iter()
            .enumerate()
            .flat_map(|(i, a)| arrows[i + 1..].iter().map(move |b| (a, b)))
            .filter(|(a, b)| crosses(a, b))
            .count()
    }

    fn length(arrows: &[Arrow], unit: f32) -> f32 {
        arrows
            .iter()
            .map(|a| (a.to[0] - a.from[0]).hypot(a.to[1] - a.from[1]) / unit)
            .sum()
    }

    #[test]
    fn every_source_sits_beside_what_it_talks_to_so_the_arrows_are_short() {
        let params = crate::config::instrument();
        let bank = tiled();
        let boxes = seats(&bank).unwrap();
        for (i, b) in boxes.iter().enumerate() {
            for t in &bank.tiles {
                assert!(!overlaps(*b, *t), "{} sits on a tile", source_name(i));
            }
            for (j, o) in boxes.iter().enumerate().skip(i + 1) {
                assert!(
                    !overlaps(*b, *o),
                    "{} sits on {}",
                    source_name(i),
                    source_name(j)
                );
            }
        }
        let arrows = arrows(params.flows(), &bank.tiles, &boxes, 2.0);
        let (length, crossings) = (length(&arrows, bank.cell.0), crossings(&arrows));
        println!("identity dataflow: {length:.2} tile widths of arrow, {crossings} crossings");
        assert!(length < 2.5, "{length:.2} tile widths of arrow");
        assert_eq!(crossings, 0);
        for flow in params.flows() {
            let (source, m) = match (flow.from, flow.to) {
                (End::Monitor(m), End::Camera(c)) => (c, m),
                (End::Seed, End::Monitor(m)) => (CAMERAS, m),
                _ => continue,
            };
            let (col, _) = crate::present::cell_of(MONITORS, m);
            assert_eq!(
                SEATS[source].col.floor() as u32,
                col,
                "{} is not seated in the column of monitor {}",
                source_name(source),
                m + 1
            );
        }
        for (a, flow) in arrows.iter().zip(params.flows()) {
            for (i, b) in boxes.iter().enumerate() {
                let end = |e: End| {
                    matches!(e, End::Camera(c) if c == i) || (e == End::Seed && i == CAMERAS)
                };
                if end(flow.from) || end(flow.to) {
                    continue;
                }
                let steps = 64;
                let clear = (0..=steps).all(|k| {
                    let t = k as f32 / steps as f32;
                    let p = [
                        a.from[0] + (a.to[0] - a.from[0]) * t,
                        a.from[1] + (a.to[1] - a.from[1]) * t,
                    ];
                    !inside(p, *b, 2.0)
                });
                assert!(clear, "{flow:?} runs through {}", source_name(i));
            }
        }
    }

    #[test]
    fn a_flow_to_a_tile_the_bank_lacks_draws_nothing() {
        let bank = tiled();
        let boxes = seats(&bank).unwrap();
        let flows = [Flow {
            from: End::Seed,
            to: End::Monitor(3),
            share: 1.0,
        }];
        assert!(arrows(flows.iter().copied(), &bank.tiles[..2], &boxes, 2.0).is_empty());
        assert_eq!(
            arrows(flows.iter().copied(), &bank.tiles, &boxes, 2.0).len(),
            1
        );
    }

    #[test]
    fn the_shader_holds_as_many_arrows_as_the_uniform_carries() {
        let wgsl = include_str!("shaders/overlay.wgsl");
        assert!(wgsl.contains(&format!("segments: array<vec4<f32>, {MAX_ARROWS}>,")));
        assert!(wgsl.contains(&format!("shares: array<vec4<f32>, {}>,", MAX_ARROWS / 4)));
    }

    #[test]
    fn opposite_arrows_between_the_same_pair_do_not_overlap() {
        let (a, b) = (
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            Rect {
                x: 300.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
        );
        let there = arrow(a, b, 1.0, 2.0).unwrap();
        let back = arrow(b, a, 1.0, 2.0).unwrap();
        assert!((there.from[1] - back.to[1]).abs() >= 4.0);
        assert!(there.from[0] > 100.0 && there.to[0] < 300.0);
        assert!(back.from[0] < 300.0 && back.to[0] > 100.0);
    }
}
