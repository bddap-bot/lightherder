//! The on-screen controls overlay: the surface drawn as the panel under the
//! performer's hands so a glance maps hand to screen — a fader row, the
//! rotaries above it, the S/M/R grid and the transport strip, each control
//! captioned with what it does in two words at most.
//!
//! Drawn from [`FADERS`] and [`BUTTONS`], never from a picture kept beside
//! them. The image is rasterized once on the CPU into a texture, and the
//! present pass blits it over a corner: a dozen captions do not justify a
//! text-shaping stack or a second render architecture, and a texture built
//! at startup works the same in a browser as on the deployed display.

use crate::midi::{spot, Spot, BUTTONS, FADERS, TRANSPORT};

/// The image, `width * height` RGBA texels, row 0 at the top.
struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// The panel's backdrop: dark but not opaque, so the picture keeps playing
/// underneath the help rather than stopping for it.
const BACK: [u8; 4] = [0, 0, 0, 200];
/// Everything that is not a binding: the panel's own chrome and printing,
/// and the dead controls. Those are still drawn — the panel should look like
/// the device, absences included — but visibly asleep.
const DIM: [u8; 4] = [255, 255, 255, 70];
const LIT: [u8; 4] = [255, 255, 255, 255];

/// Every measure below is in texels of the raster, which the blit then
/// scales whole; the font is 8x8, so 8 is the unit everything else is
/// spaced around.
const GLYPH: i32 = 8;
/// One channel strip's width. "temperature", eleven glyphs, overhangs it by
/// two texels either side and still clears both neighbours' captions.
const STRIP_W: i32 = 84;
/// A transport button and the pitch between them. Seven glyphs and a texel
/// each side. A caption past seven clips.
const BUTTON_W: i32 = 60;
const BUTTON_H: i32 = 16;
const BUTTON_PITCH: i32 = 64;
const PAD: i32 = 10;
/// How far above its row's buttons a printed group label sits: a glyph and
/// two texels of air, the room the device leaves for the same words.
const GROUP_LIFT: i32 = GLYPH + 2;
/// The transport strip, whose widest row is five buttons: the last of them
/// ends at its own width, not at a whole pitch.
const TRANSPORT_W: i32 = 5 * BUTTON_PITCH - (BUTTON_PITCH - BUTTON_W);
const STRIPS_X: i32 = PAD + TRANSPORT_W + 16;
const PANEL_W: i32 = STRIPS_X + 8 * STRIP_W + PAD;
/// The three button rows, shared by the S/M/R grid and the transport strip
/// so the two halves of the panel read as one device.
const ROWS_Y: [i32; 3] = [66, 84, 102];
const SQUARE: i32 = 14;
const PANEL_H: i32 = 152;

/// An RGBA image being drawn. Every mark goes through [`Canvas::set`], which
/// drops texels outside the image — so a caption longer than the room it was
/// given clips instead of corrupting a neighbour's row.
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

    /// A one-texel ring: enough circle for a rotary at this scale.
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

    /// `text` left-aligned at `(x, y)`, stopping at `max_x`: a caption that
    /// outgrows its control clips rather than writing over the next one.
    fn text(&mut self, x: i32, y: i32, text: &str, max_x: i32, colour: [u8; 4]) {
        for (i, ch) in text.chars().enumerate() {
            let at = x + i as i32 * GLYPH;
            if at + GLYPH > max_x {
                return;
            }
            // The font is ASCII; anything past it draws as a blank, which a
            // caption from this crate's own tables never contains.
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
}

fn strip_x(i: u8) -> i32 {
    STRIPS_X + i as i32 * STRIP_W
}

/// One channel strip's furniture, all of it asleep: the rotary, the three
/// buttons, the fader track. Bound controls are drawn over this, lit.
fn strip_chrome(c: &mut Canvas, i: u8) {
    let x = strip_x(i);
    c.ring(x + STRIP_W / 2, 24, 12, DIM);
    for y in ROWS_Y {
        c.frame(x + 2, y, SQUARE, SQUARE, DIM);
    }
    fader_track(c, i, DIM);
}

fn track_x(i: u8) -> i32 {
    strip_x(i) + STRIP_W - 14
}

/// The fader: a vertical slot spanning the three button rows, with a thumb.
fn fader_track(c: &mut Canvas, i: u8, colour: [u8; 4]) {
    let x = track_x(i);
    c.frame(x, ROWS_Y[0], 12, ROWS_Y[2] + SQUARE - ROWS_Y[0], colour);
    c.fill(x - 1, 88, 14, 3, colour);
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

/// The words the silkscreen prints above the grouped buttons — TRACK over
/// the pair, MARKER over the three that sit apart from cycle — each centred
/// over the columns [`TRANSPORT`] gives its own. They are printing on the
/// surface rather than controls, so they are drawn as chrome.
fn group_labels(c: &mut Canvas) {
    for (i, t) in TRANSPORT.iter().enumerate() {
        let Some(name) = t.group else { continue };
        // Only the group's first button draws it; the rest would print the
        // same word over the same columns.
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

/// Light the control at `spot` and caption it. Faders and rotaries take the
/// strip's own caption places — under the fader, under the rotary — and the
/// buttons are captioned beside or inside themselves.
fn place(c: &mut Canvas, spot: Spot, label: &str) {
    let beside = |c: &mut Canvas, i: u8, row: usize, label: &str| {
        let x = strip_x(i);
        c.frame(x + 2, ROWS_Y[row], SQUARE, SQUARE, LIT);
        // Stop where the fader track starts: its caption may not eat the
        // neighbour it shares the strip with.
        c.text(x + SQUARE + 4, ROWS_Y[row] + 3, label, track_x(i) - 1, LIT);
    };
    match spot {
        Spot::Fader(i) => {
            fader_track(c, i, LIT);
            c.text_centred(strip_x(i) + STRIP_W / 2, 124, label, LIT);
        }
        Spot::Rotary(i) => {
            let cx = strip_x(i) + STRIP_W / 2;
            c.ring(cx, 24, 12, LIT);
            c.fill(cx, 14, 1, 6, LIT);
            c.text_centred(cx, 42, label, LIT);
        }
        Spot::S(i) => beside(c, i, 0, label),
        Spot::M(i) => beside(c, i, 1, label),
        Spot::R(i) => beside(c, i, 2, label),
        Spot::Transport(t) => {
            transport_button(c, t.row, t.col, LIT);
            let x = button_x(t.col);
            // Two texels in from the frame, and stopped at the far one.
            c.text(
                x + 2,
                ROWS_Y[t.row as usize] + 4,
                label,
                x + BUTTON_W - 1,
                LIT,
            );
        }
    }
}

/// Every bound control and its caption. What the panel draws dim is what is
/// dead.
fn labels() -> impl Iterator<Item = (u8, String)> {
    let faders = FADERS.iter().map(|f| (f.cc, f.knob.name().to_string()));
    let buttons = BUTTONS.iter().map(|b| (b.cc, b.action.caption()));
    faders.chain(buttons)
}

fn rasterize() -> Raster {
    let mut c = Canvas::new(PANEL_W, PANEL_H);
    for i in 0..8 {
        strip_chrome(&mut c, i);
    }
    for t in TRANSPORT {
        transport_button(&mut c, t.row, t.col, DIM);
    }
    group_labels(&mut c);
    for (cc, label) in labels() {
        let spot = spot(cc).expect("every bound control is on the panel");
        place(&mut c, spot, &label);
    }
    Raster {
        width: c.width as u32,
        height: c.height as u32,
        pixels: c.pixels,
    }
}

/// The raster on the GPU, ready for the present pass to composite: a
/// texture, and a pipeline that blends it over whatever is already in the
/// target.
pub struct Overlay {
    pipeline: wgpu::RenderPipeline,
    /// Drawn at startup, so showing it mid-piece costs a frame nothing.
    panel: Image,
}

struct Image {
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

/// How far the panel stands off the target's corner, in target texels.
const MARGIN: f32 = 24.0;

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, raster: &Raster) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: raster.width,
        height: raster.height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("overlay"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // sRGB because the raster's texels are display colours, and the
        // blend wants them decoded the same way the monitors are.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
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
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

impl Overlay {
    /// `format` is the present target's, because the overlay draws into the
    /// present pass's own attachment.
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
        // Nearest: the blit scales by a whole number wherever it fits, and
        // 8x8 glyphs read better as texels than smeared across them.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("overlay"),
            ..Default::default()
        });
        let panel = {
            let raster = rasterize();
            let view = upload(device, queue, &raster);
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("overlay"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            Image {
                bind_group,
                size: (raster.width, raster.height),
            }
        };
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/overlay.wgsl"));
        let pipeline = crate::fullscreen_pipeline(
            device,
            &shader,
            &layout,
            "fs_overlay",
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            "overlay",
        );
        Overlay { pipeline, panel }
    }

    /// Blit the panel into the bottom-right of a pass already drawing
    /// to a target of `target_size`. Scaled by a whole number where one fits
    /// inside ninety percent of the target — texel-crisp — and shrunk to fit
    /// where none does.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass, target_size: (u32, u32)) {
        let image = &self.panel;
        let (w, h) = (image.size.0 as f32, image.size.1 as f32);
        let room = (
            target_size.0 as f32 * 0.9 / w,
            target_size.1 as f32 * 0.9 / h,
        );
        let scale = room.0.min(room.1);
        let scale = if scale >= 1.0 { scale.floor() } else { scale };
        if scale <= 0.0 {
            return;
        }
        let (w, h) = (w * scale, h * scale);
        pass.set_viewport(
            (target_size.0 as f32 - w - MARGIN).max(0.0),
            (target_size.1 as f32 - h - MARGIN).max(0.0),
            w,
            h,
            0.0,
            1.0,
        );
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &image.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::params::Node;

    fn lit_texels(r: &Raster) -> usize {
        r.pixels.chunks(4).filter(|p| *p == LIT).count()
    }

    /// The texels of a box, so a claim about what the picture puts
    /// somewhere is checked against the thing itself rather than against
    /// "something is drawn near here".
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
        let raster = rasterize();
        // Every expectation below is in the strip's own texels, so the
        // strip's own place has to be claimed outright: it starts at the
        // panel's edge and its widest row stops short of the channel
        // strips, which it would otherwise draw over.
        assert_eq!(button_x(0), PAD);
        assert!(button_x(4) + BUTTON_W < STRIPS_X);
        // The gap. A marker button drawn beside cycle would frame here.
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
        // TRACK over the pair and MARKER over the three, each against the
        // word drawn on its own in a band a glyph wider either side: a
        // label shifted, doubled, misspelt, or lit as though something were
        // bound to it all differ from that.
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
        let raster = rasterize();
        assert_eq!(
            (raster.width, raster.height),
            (PANEL_W as u32, PANEL_H as u32)
        );
        // Captions are the lit texels; a panel with none is chrome around
        // nothing. The exact count is the drawing's business, but the panel
        // captions every strip, which is thousands of texels.
        assert!(lit_texels(&raster) > 1000, "{}", lit_texels(&raster));
    }

    #[test]
    fn every_caption_lands_on_its_own_control() {
        // Each binding's caption drawn alone, against the same texels of the
        // whole panel: a caption that moved or doubled differs.
        let raster = rasterize();
        for (cc, label) in labels() {
            let spot = spot(cc).unwrap();
            let mut want = Canvas::new(PANEL_W, PANEL_H);
            place(&mut want, spot, &label);
            let lit: Vec<(i32, i32)> = (0..PANEL_H)
                .flat_map(|y| (0..PANEL_W).map(move |x| (x, y)))
                .filter(|(x, y)| {
                    let at = ((y * PANEL_W + x) * 4) as usize;
                    want.pixels[at..at + 4] == LIT
                })
                .collect();
            assert!(
                lit.len() > 20,
                "cc {cc}: {label:?} drew {} texels",
                lit.len()
            );
            for (x, y) in lit {
                let at = ((y * PANEL_W + x) * 4) as usize;
                assert_eq!(
                    raster.pixels[at..at + 4],
                    LIT,
                    "cc {cc}: {label:?} at {x},{y}"
                );
            }
        }
    }

    /// Where a kind of node's button `i` sits, off the panel's own table
    /// rather than a second copy of it here.
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

    /// Exactly the texels a binding on that node lights, and nothing a
    /// neighbour can reach. Stopping a texel short of the fader's track
    /// leaves out its thumb, which crosses the middle row.
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

    /// The same band of a panel carrying nothing but `draw`.
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
            place(c, spot_of(node, i), caption);
        })
    }

    #[test]
    fn a_select_row_is_drawn_for_its_own_kind_and_stops_where_the_graph_does() {
        // The rig's three counts differ, so a row drawn from another kind's
        // would read wrong on at least one of them. Past the choice the strip
        // is bare chrome, which is what a dead button looks like.
        let raster = rasterize();
        for node in Node::ALL {
            for i in 0..crate::midi::STRIPS as u8 {
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
}
