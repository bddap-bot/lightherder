use crate::lamps::{lamp, Lamplight};
use crate::midi::{spot, Spot, BUTTONS, FADERS, TRANSPORT};
use crate::params::{Flow, Focus, Knob, Node, Params};
use crate::present::{mark_thickness, Rect};
use crate::rig::{CAMERAS, MONITORS};

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
const PANEL_W: i32 = STRIPS_X + 8 * STRIP_W + PAD;
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

const LEGEND_W: i32 = 64;
const LEGEND_BOX_H: i32 = 16;
const LEGEND_GAP: i32 = 6;
const LEGEND_ROWS: usize = CAMERAS + 1;
const LEGEND_H: i32 = LEGEND_ROWS as i32 * (LEGEND_BOX_H + LEGEND_GAP) - LEGEND_GAP;

const MAX_ARROWS: usize = 40;
const _: () = assert!(
    MAX_ARROWS >= 2 * CAMERAS * MONITORS + MONITORS,
    "shaders/overlay.wgsl spells this number too"
);

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
pub struct Readout {
    values: [f32; Knob::ALL.len()],
    fractions: [f32; Knob::ALL.len()],
    lit: Lamplight,
}

impl Readout {
    pub(crate) fn of(params: &Params, focus: Focus, lit: Lamplight) -> Readout {
        let values = Knob::ALL.map(|knob| params.knob(knob, focus));
        let fractions = Knob::ALL.map(|knob| knob.limit(params).fraction(params.knob(knob, focus)));
        Readout {
            values,
            fractions,
            lit,
        }
    }

    pub(crate) fn reads(&self, knob: Knob) -> String {
        knob.readout(self.values[knob as usize])
    }

    fn fraction(&self, knob: Knob) -> f32 {
        self.fractions[knob as usize]
    }

    fn lit(&self, cc: u8) -> bool {
        self.lit & lamp(cc) != 0
    }
}

fn strip_x(i: u8) -> i32 {
    STRIPS_X + i as i32 * STRIP_W
}

fn strip_chrome(c: &mut Canvas, i: u8) {
    let x = strip_x(i);
    rotary(c, x + STRIP_W / 2, DIM, 0.5);
    for y in ROWS_Y {
        c.frame(x + 2, y, SQUARE, SQUARE, DIM);
    }
    fader_track(c, i, DIM, 0.5);
}

fn track_x(i: u8) -> i32 {
    strip_x(i) + STRIP_W - 14
}

fn thumb_y(fraction: f32) -> i32 {
    let (top, bottom) = (ROWS_Y[0], ROWS_Y[2] + SQUARE);
    let travel = bottom - top - THUMB_H - 2;
    bottom - 1 - THUMB_H - (fraction * travel as f32).round() as i32
}

fn fader_track(c: &mut Canvas, i: u8, colour: [u8; 4], fraction: f32) {
    let x = track_x(i);
    c.frame(x, ROWS_Y[0], 12, ROWS_Y[2] + SQUARE - ROWS_Y[0], colour);
    c.fill(x - 1, thumb_y(fraction), 14, THUMB_H, colour);
}

fn rotary(c: &mut Canvas, cx: i32, colour: [u8; 4], fraction: f32) {
    c.ring(cx, ROTARY_Y, ROTARY_R, colour);
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
    Button(u8, String),
}

fn place(c: &mut Canvas, spot: Spot, control: &Control, readout: &Readout) {
    let beside = |c: &mut Canvas, i: u8, row: usize, cc: u8, label: &str| {
        let x = strip_x(i);
        match readout.lit(cc) {
            true => c.fill(x + 2, ROWS_Y[row], SQUARE, SQUARE, LIT),
            false => c.frame(x + 2, ROWS_Y[row], SQUARE, SQUARE, LIT),
        }
        c.text(x + SQUARE + 4, ROWS_Y[row] + 3, label, track_x(i) - 1, LIT);
    };
    match (spot, control) {
        (Spot::Fader(i), Control::Knob(knob)) => {
            fader_track(c, i, LIT, readout.fraction(*knob));
            let cx = strip_x(i) + STRIP_W / 2;
            c.text_centred(cx, FADER_CAPTION_Y, knob.name(), LIT);
            c.text_centred(cx, FADER_VALUE_Y, &readout.reads(*knob), LIT);
        }
        (Spot::Rotary(i), Control::Knob(knob)) => {
            let cx = strip_x(i) + STRIP_W / 2;
            rotary(c, cx, LIT, readout.fraction(*knob));
            c.text_centred(cx, ROTARY_CAPTION_Y, knob.name(), LIT);
            c.text_centred(cx, ROTARY_VALUE_Y, &readout.reads(*knob), LIT);
        }
        (Spot::S(i), Control::Button(cc, label)) => beside(c, i, 0, *cc, label),
        (Spot::M(i), Control::Button(cc, label)) => beside(c, i, 1, *cc, label),
        (Spot::R(i), Control::Button(cc, label)) => beside(c, i, 2, *cc, label),
        (Spot::Transport(t), Control::Button(cc, label)) => {
            let (x, y) = (button_x(t.col), ROWS_Y[t.row as usize]);
            let ink = match readout.lit(*cc) {
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
        (Spot::Fader(_) | Spot::Rotary(_), Control::Button(..))
        | (Spot::S(_) | Spot::M(_) | Spot::R(_) | Spot::Transport(_), Control::Knob(_)) => {
            unreachable!("a knob sits on a fader or rotary and a button on a button")
        }
    }
}

fn controls() -> impl Iterator<Item = (u8, Control)> {
    let faders = FADERS.iter().map(|f| (f.cc, Control::Knob(f.knob)));
    let buttons = BUTTONS
        .iter()
        .map(|b| (b.cc, Control::Button(b.cc, b.action.caption())));
    faders.chain(buttons)
}

fn rasterize(readout: &Readout) -> Raster {
    let mut c = Canvas::new(PANEL_W, PANEL_H);
    for i in 0..8 {
        strip_chrome(&mut c, i);
    }
    for t in TRANSPORT {
        transport_button(&mut c, t.row, t.col, DIM);
    }
    group_labels(&mut c);
    for (cc, control) in controls() {
        let spot = spot(cc).expect("every bound control is on the panel");
        place(&mut c, spot, &control, readout);
    }
    c.raster()
}

fn legend_names() -> [String; LEGEND_ROWS] {
    std::array::from_fn(|i| match i < CAMERAS {
        true => format!("{} {}", Node::Camera.short(), i + 1),
        false => "seed".to_string(),
    })
}

fn legend_box(i: usize) -> Rect {
    Rect {
        x: 0.0,
        y: (i as i32 * (LEGEND_BOX_H + LEGEND_GAP)) as f32,
        w: LEGEND_W as f32,
        h: LEGEND_BOX_H as f32,
    }
}

fn legend() -> Raster {
    let mut c = Canvas::new(LEGEND_W, LEGEND_H);
    for (i, name) in legend_names().iter().enumerate() {
        let b = legend_box(i);
        c.frame(b.x as i32, b.y as i32, b.w as i32, b.h as i32, LIT);
        c.text_centred(LEGEND_W / 2, b.y as i32 + 4, name, LIT);
    }
    c.raster()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    x: f32,
    y: f32,
    scale: f32,
}

impl Placement {
    fn of(&self, r: Rect) -> Rect {
        Rect {
            x: self.x + r.x * self.scale,
            y: self.y + r.y * self.scale,
            w: r.w * self.scale,
            h: r.h * self.scale,
        }
    }
}

fn scale_into(size: (u32, u32), room: (f32, f32)) -> Option<f32> {
    let scale = (room.0 / size.0 as f32).min(room.1 / size.1 as f32);
    let scale = if scale >= 1.0 { scale.floor() } else { scale };
    (scale > 0.0).then_some(scale)
}

const MARGIN: f32 = 24.0;

fn panel_placement(size: (u32, u32), target: (u32, u32)) -> Option<Placement> {
    let scale = scale_into(size, (target.0 as f32 * 0.9, target.1 as f32 * 0.9))?;
    Some(Placement {
        x: (target.0 as f32 - size.0 as f32 * scale - MARGIN).max(0.0),
        y: (target.1 as f32 - size.1 as f32 * scale - MARGIN).max(0.0),
        scale,
    })
}

fn legend_placement(size: (u32, u32), cell: Rect, panel_top: f32) -> Option<Placement> {
    let room = Rect {
        h: (panel_top.min(cell.y + cell.h) - cell.y - MARGIN).max(0.0),
        ..cell
    };
    let scale = scale_into(size, (room.w * 0.8, room.h * 0.8))?;
    Some(Placement {
        x: room.x + (room.w - size.0 as f32 * scale) / 2.0,
        y: room.y + (room.h - size.1 as f32 * scale) / 2.0,
        scale,
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

fn arrows(flows: &[Flow], tiles: &[Rect], boxes: &[Rect; LEGEND_ROWS], line: f32) -> Vec<Arrow> {
    flows
        .iter()
        .filter_map(|flow| match *flow {
            Flow::Look {
                camera,
                monitor,
                share,
            } => arrow(tiles[monitor], boxes[camera], share, line),
            Flow::Feed {
                camera,
                monitor,
                share,
            } => arrow(boxes[camera], tiles[monitor], share, line),
            Flow::Seed { monitor, share } => arrow(boxes[CAMERAS], tiles[monitor], share, line),
        })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ArrowsUniform {
    count: [u32; 4],
    line: [f32; 4],
    segments: [[f32; 4]; MAX_ARROWS],
    shares: [[f32; 4]; MAX_ARROWS],
}

impl ArrowsUniform {
    fn of(arrows: &[Arrow], line: f32) -> ArrowsUniform {
        let mut uniform = ArrowsUniform {
            count: [arrows.len().min(MAX_ARROWS) as u32, 0, 0, 0],
            line: [line, 6.0 * line, 3.0 * line, 0.0],
            segments: [[0.0; 4]; MAX_ARROWS],
            shares: [[0.0; 4]; MAX_ARROWS],
        };
        for (i, a) in arrows.iter().take(MAX_ARROWS).enumerate() {
            uniform.segments[i] = [a.from[0], a.from[1], a.to[0], a.to[1]];
            uniform.shares[i] = [0.35 + 0.65 * a.share.clamp(0.0, 1.0), 0.0, 0.0, 0.0];
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

    fn blit(&self, pass: &mut wgpu::RenderPass, pipeline: &wgpu::RenderPipeline, at: Placement) {
        pass.set_viewport(
            at.x,
            at.y,
            self.size.0 as f32 * at.scale,
            self.size.1 as f32 * at.scale,
            0.0,
            1.0,
        );
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

pub struct Overlay {
    blit: wgpu::RenderPipeline,
    panel: Image,
    legend: Image,
    arrows: wgpu::RenderPipeline,
    arrows_uniform: wgpu::Buffer,
    arrows_bind: wgpu::BindGroup,
    flows: Vec<Flow>,
    shown: Option<Readout>,
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
        let panel = Image::new(
            device,
            queue,
            &layout,
            &sampler,
            &Canvas::new(PANEL_W, PANEL_H).raster(),
        );
        let legend = Image::new(device, queue, &layout, &sampler, &legend());
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
            panel,
            legend,
            arrows,
            arrows_uniform,
            arrows_bind,
            flows: Vec::new(),
            shown: None,
        }
    }

    pub(crate) fn show(&mut self, queue: &wgpu::Queue, params: &Params, readout: Readout) {
        self.flows = params.flows().collect();
        if self.shown != Some(readout) {
            write(queue, &self.panel.texture, &rasterize(&readout));
            self.shown = Some(readout);
        }
    }

    pub(crate) fn draw(
        &self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass,
        target_size: (u32, u32),
        cells: &[Rect],
    ) {
        let Some(panel) = panel_placement(self.panel.size, target_size) else {
            return;
        };
        let spare = cells.get(MONITORS).copied();
        let legend = spare.and_then(|cell| legend_placement(self.legend.size, cell, panel.y));
        if let Some(at) = legend {
            let line = mark_thickness(target_size.1);
            let boxes: [Rect; LEGEND_ROWS] = std::array::from_fn(|i| at.of(legend_box(i)));
            let arrows = arrows(&self.flows, &cells[..MONITORS], &boxes, line);
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
            self.legend.blit(pass, &self.blit, at);
        }
        self.panel.blit(pass, &self.blit, panel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::midi::CLUTCH;
    use crate::params::Node;

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
            let spot = spot(cc).unwrap();
            let mut want = Canvas::new(PANEL_W, PANEL_H);
            place(&mut want, spot, &control, &readout);
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

    fn spot_of(node: Node, i: u8) -> Spot {
        spot(crate::midi::row_of(node) + i).expect("a select row is a block of spots")
    }

    fn row_index(node: Node) -> usize {
        match spot_of(node, 0) {
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

    fn captioned(node: Node, i: u8, cc: u8, caption: &str) -> Vec<[u8; 4]> {
        band_of(node, i, |c| {
            strip_chrome(c, i);
            place(
                c,
                spot_of(node, i),
                &Control::Button(cc, caption.to_string()),
                &at_rest(),
            );
        })
    }

    #[test]
    fn a_select_row_is_drawn_for_its_own_kind_and_stops_where_the_graph_does() {
        let raster = rasterize(&at_rest());
        for node in Node::ALL {
            for i in 0..crate::midi::STRIPS as u8 {
                let bound = BUTTONS
                    .iter()
                    .find(|b| b.cc == crate::midi::row_of(node) + i);
                let want = match bound {
                    Some(b) => captioned(node, i, b.cc, &b.action.caption()),
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

    fn cells(n: usize) -> Vec<Rect> {
        let (cols, _) = crate::present::grid(MONITORS);
        (0..n)
            .map(|i| Rect {
                x: (i as u32 % cols * 640) as f32,
                y: (i as u32 / cols * 360) as f32,
                w: 640.0,
                h: 360.0,
            })
            .collect()
    }

    fn inside(p: [f32; 2], r: Rect, slack: f32) -> bool {
        p[0] >= r.x - slack
            && p[0] <= r.x + r.w + slack
            && p[1] >= r.y - slack
            && p[1] <= r.y + r.h + slack
    }

    #[test]
    fn the_arrows_are_the_flows_between_the_tiles_and_the_legend() {
        let params = crate::config::instrument();
        let flows: Vec<Flow> = params.flows().collect();
        let (cols, rows) = crate::present::grid(MONITORS);
        let cells = cells((cols * rows) as usize);
        assert!(
            cells.len() > MONITORS,
            "the bank has no spare cell for the cameras"
        );
        let legend =
            legend_placement((LEGEND_W as u32, LEGEND_H as u32), cells[MONITORS], 700.0).unwrap();
        let boxes: [Rect; LEGEND_ROWS] = std::array::from_fn(|i| legend.of(legend_box(i)));
        for b in boxes {
            assert!(
                inside([b.x, b.y], cells[MONITORS], 0.0) && b.y + b.h < 700.0,
                "{b:?}"
            );
        }
        let arrows = arrows(&flows, &cells[..MONITORS], &boxes, 2.0);
        assert_eq!(arrows.len(), flows.len());
        assert_eq!(arrows.len(), 10);
        for (flow, arrow) in flows.iter().zip(&arrows) {
            let (from, to) = match *flow {
                Flow::Look {
                    camera, monitor, ..
                } => (cells[monitor], boxes[camera]),
                Flow::Feed {
                    camera, monitor, ..
                } => (boxes[camera], cells[monitor]),
                Flow::Seed { monitor, .. } => (boxes[CAMERAS], cells[monitor]),
            };
            assert!(
                inside(arrow.from, from, 12.0),
                "{flow:?} starts at {:?}",
                arrow.from
            );
            assert!(
                inside(arrow.to, to, 12.0),
                "{flow:?} ends at {:?}",
                arrow.to
            );
            assert!(
                !inside(arrow.from, from, -4.0),
                "{flow:?} starts inside its tile"
            );
            assert!(!inside(arrow.to, to, -4.0), "{flow:?} ends inside its tile");
        }
        let seeds = flows
            .iter()
            .filter(|f| matches!(f, Flow::Seed { .. }))
            .count();
        assert_eq!(seeds, 2, "the identity graph seeds the B pair");
    }

    #[test]
    fn a_solo_has_no_spare_cell_and_so_no_arrows() {
        assert!(cells(1).get(MONITORS).is_none());
        assert!(cells(0).get(MONITORS).is_none());
        let uniform = ArrowsUniform::of(&[], 2.0);
        assert_eq!(uniform.count[0], 0);
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
