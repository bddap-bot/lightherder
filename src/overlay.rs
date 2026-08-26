//! The on-screen controls overlay: the surface as it is actually mapped,
//! drawn as the panel under the performer's hands so a glance maps hand to
//! screen — a fader row, the rotaries above it, the S/M/R grid and the
//! transport strip, each control captioned with what it does in two words at
//! most.
//!
//! Drawn from the [`Map`] in force and the keys' own short captions, the way
//! [`Map::card`] prints its card — never from a picture kept beside them, so
//! a `midi.toml` that moves a knob moves it here too. The image is rasterized
//! once on the CPU into a texture, and the present pass blits it over a
//! corner: a dozen captions do not justify a text-shaping stack or a second
//! render architecture, and a texture built at startup works the same in a
//! browser as on the deployed display.

use crate::midi::{nano_kontrol2, Map};

/// The image, `width * height` RGBA texels, row 0 at the top.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The panel's backdrop: dark but not opaque, so the picture keeps playing
/// underneath the help rather than stopping for it.
const BACK: [u8; 4] = [0, 0, 0, 200];
/// A control the map leaves unbound. Still drawn — the panel should look
/// like the device, absences included — but visibly asleep.
const DIM: [u8; 4] = [255, 255, 255, 70];
const LIT: [u8; 4] = [255, 255, 255, 255];

/// Every measure below is in texels of the raster, which the blit then
/// scales whole; the font is 8x8, so 8 is the unit everything else is
/// spaced around.
const GLYPH: i32 = 8;
/// One channel strip's width: room for the widest single word a knob's name
/// carries ("saturation", ten glyphs) with a texel of air either side.
const STRIP_W: i32 = 84;
/// A transport button and the pitch between them, sized to a caption of
/// seven glyphs — "swing +" — with a texel each side.
const BUTTON_W: i32 = 60;
const BUTTON_H: i32 = 16;
const BUTTON_PITCH: i32 = 64;
const PAD: i32 = 10;
/// The transport strip: two track buttons, then cycle and the markers, then
/// the tape row — three rows, the widest of them five buttons.
const TRANSPORT_W: i32 = 5 * BUTTON_PITCH - (BUTTON_PITCH - BUTTON_W);
const STRIPS_X: i32 = PAD + TRANSPORT_W + 16;
const PANEL_W: i32 = STRIPS_X + 8 * STRIP_W + PAD;
/// The three button rows, shared by the S/M/R grid and the transport strip
/// so the two halves of the panel read as one device.
const ROWS_Y: [i32; 3] = [66, 84, 102];
const SQUARE: i32 = 14;
const PANEL_H: i32 = 152;
/// The pitch of the plain text lines: overflow bindings under the panel, and
/// the whole listing for a surface whose shape this crate does not know.
const LINE: i32 = 10;

/// Where a control number sits on a nanoKONTROL2's panel.
///
/// A second copy of the physical facts [`crate::midi::silkscreen`] spells as
/// names — that one prints, this one places, and the test below holds each
/// entry to the name silkscreen gives the same number so the two cannot
/// drift.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Spot {
    Fader(u8),
    Rotary(u8),
    S(u8),
    M(u8),
    R(u8),
    /// `(row, column)` in [`ROWS_Y`].
    Transport(u8, u8),
}

/// The transport strip's grid, `(cc, row, column)`, in the arrangement the
/// device has them: track prev/next on top, cycle beside the three marker
/// buttons, and the tape row underneath.
const TRANSPORT: &[(u8, u8, u8)] = &[
    (58, 0, 0),
    (59, 0, 1),
    (46, 1, 0),
    (60, 1, 1),
    (61, 1, 2),
    (62, 1, 3),
    (43, 2, 0),
    (44, 2, 1),
    (42, 2, 2),
    (41, 2, 3),
    (45, 2, 4),
];

fn spot(cc: u8) -> Option<Spot> {
    let block = |first: u8| (cc >= first && cc < first + 8).then(|| cc - first);
    if let Some(i) = block(0) {
        return Some(Spot::Fader(i));
    }
    if let Some(i) = block(16) {
        return Some(Spot::Rotary(i));
    }
    if let Some(i) = block(32) {
        return Some(Spot::S(i));
    }
    if let Some(i) = block(48) {
        return Some(Spot::M(i));
    }
    if let Some(i) = block(64) {
        return Some(Spot::R(i));
    }
    TRANSPORT
        .iter()
        .find(|(t, _, _)| *t == cc)
        .map(|(_, row, col)| Spot::Transport(*row, *col))
}

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

    /// `text` left-aligned at `(x, y)`, stopping at `max_x`: a caption from a
    /// hand-written map can outgrow its control, and clipping it beats
    /// writing over the next one.
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

    /// A caption of up to two words under a strip-wide control: on one line
    /// when it fits the strip, a word per line when it does not.
    fn caption(&mut self, cx: i32, y: i32, text: &str, colour: [u8; 4]) {
        if text.chars().count() as i32 * GLYPH <= STRIP_W - 4 {
            return self.text_centred(cx, y, text, colour);
        }
        for (i, word) in text.split_whitespace().enumerate() {
            self.text_centred(cx, y + i as i32 * (GLYPH + 2), word, colour);
        }
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

/// The fader: a vertical slot spanning the three button rows, with a thumb.
fn fader_track(c: &mut Canvas, i: u8, colour: [u8; 4]) {
    // On the strip's right edge: every texel between it and the S/M/R
    // squares is caption room, and "slot 1" needs all six glyphs of it.
    let x = strip_x(i) + STRIP_W - 14;
    c.frame(x, ROWS_Y[0], 12, ROWS_Y[2] + SQUARE - ROWS_Y[0], colour);
    c.fill(x - 1, 88, 14, 3, colour);
}

fn transport_button(c: &mut Canvas, row: u8, col: u8, colour: [u8; 4]) {
    c.frame(
        PAD + col as i32 * BUTTON_PITCH,
        ROWS_Y[row as usize],
        BUTTON_W,
        BUTTON_H,
        colour,
    );
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
        c.text(
            x + SQUARE + 4,
            ROWS_Y[row] + 3,
            label,
            x + STRIP_W - 15,
            LIT,
        );
    };
    match spot {
        Spot::Fader(i) => {
            fader_track(c, i, LIT);
            c.caption(strip_x(i) + STRIP_W / 2, 124, label, LIT);
        }
        Spot::Rotary(i) => {
            let cx = strip_x(i) + STRIP_W / 2;
            c.ring(cx, 24, 12, LIT);
            c.fill(cx, 14, 1, 6, LIT);
            c.caption(cx, 42, label, LIT);
        }
        Spot::S(i) => beside(c, i, 0, label),
        Spot::M(i) => beside(c, i, 1, label),
        Spot::R(i) => beside(c, i, 2, label),
        Spot::Transport(row, col) => {
            transport_button(c, row, col, LIT);
            let x = PAD + col as i32 * BUTTON_PITCH;
            // Two texels in from the frame: "swing -" is seven glyphs, and
            // the button is sized to hold exactly them.
            c.text(
                x + 2,
                ROWS_Y[row as usize] + 4,
                label,
                x + BUTTON_W - 1,
                LIT,
            );
        }
    }
}

/// What the overlay captions a binding with: a knob's name, or a button's
/// two-word short. The map in force has been validated, so every key
/// resolves; the fallback spells the key itself rather than leaving a lit
/// control mute if an unvalidated map ever reaches a test's raster.
fn labels(map: &Map) -> impl Iterator<Item = (u8, String)> + '_ {
    let faders = map.fader.iter().map(|f| (f.cc, f.knob.name().to_string()));
    let buttons = map.button.iter().map(|b| {
        (
            b.cc,
            crate::keys::short(&b.key).unwrap_or_else(|| b.key.clone()),
        )
    });
    faders.chain(buttons)
}

/// The whole image for the map in force. A surface this crate knows the
/// shape of is drawn as that panel; any other is a listing, because a drawn
/// panel the performer's hands cannot find is worse than the list they can
/// read — the same retreat [`crate::midi::silkscreen`] makes to numbers.
pub fn rasterize(map: &Map) -> Raster {
    if !nano_kontrol2(&map.device) {
        return listing(map);
    }
    // Bindings off the panel — control numbers no silkscreen names — still
    // exist and must not vanish from the help: they get lines below it.
    let spare: Vec<(u8, String)> = labels(map).filter(|(cc, _)| spot(*cc).is_none()).collect();
    let height = PANEL_H
        + if spare.is_empty() {
            0
        } else {
            spare.len() as i32 * LINE + 6
        };
    let mut c = Canvas::new(PANEL_W, height);
    for i in 0..8 {
        strip_chrome(&mut c, i);
    }
    for (_, row, col) in TRANSPORT {
        transport_button(&mut c, *row, *col, DIM);
    }
    for (cc, label) in labels(map) {
        if let Some(spot) = spot(cc) {
            place(&mut c, spot, &label);
        }
    }
    for (i, (cc, label)) in spare.iter().enumerate() {
        let y = PANEL_H - 4 + i as i32 * LINE;
        c.text(PAD, y, &format!("cc {cc}  {label}"), c.width, LIT);
    }
    Raster {
        width: c.width as u32,
        height: c.height as u32,
        pixels: c.pixels,
    }
}

/// One line per binding for a surface whose panel this crate cannot draw.
fn listing(map: &Map) -> Raster {
    let lines: Vec<String> = std::iter::once(map.device.clone())
        .chain(labels(map).map(|(cc, label)| format!("cc {cc:<3} {label}")))
        .collect();
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as i32;
    let mut c = Canvas::new(
        widest * GLYPH + 2 * PAD,
        lines.len() as i32 * LINE + 2 * PAD,
    );
    for (i, line) in lines.iter().enumerate() {
        let colour = if i == 0 { DIM } else { LIT };
        c.text(PAD, PAD + i as i32 * LINE, line, c.width, colour);
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
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

/// How far the panel stands off the target's corner, in target texels.
const MARGIN: f32 = 24.0;

impl Overlay {
    /// `format` is the present target's, because the overlay draws into the
    /// present pass's own attachment.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        map: &Map,
    ) -> Overlay {
        let raster = rasterize(map);
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
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
        Overlay {
            pipeline,
            bind_group,
            size: (raster.width, raster.height),
        }
    }

    /// Blit into the bottom-right of a pass already drawing to a target of
    /// `target_size`. Scaled by a whole number where one fits inside ninety
    /// percent of the target — texel-crisp — and shrunk to fit where none
    /// does.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass, target_size: (u32, u32)) {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
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
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::silkscreen;

    #[test]
    fn the_geometry_and_the_silkscreen_name_the_same_controls() {
        // Two encodings of one panel: silkscreen prints a control's name,
        // this module places it. Every number one knows, the other must know
        // the same way — otherwise the overlay draws "hue" on a control whose
        // card says another.
        for cc in 0..=127u8 {
            let name = silkscreen("nanoKONTROL", cc);
            let expected = match spot(cc) {
                Some(Spot::Fader(i)) => format!("fader {}", i + 1),
                Some(Spot::Rotary(i)) => format!("rotary {}", i + 1),
                Some(Spot::S(i)) => format!("S{}", i + 1),
                Some(Spot::M(i)) => format!("M{}", i + 1),
                Some(Spot::R(i)) => format!("R{}", i + 1),
                // The transport is placed by grid position and named by
                // word, so position is checked against the strip's table
                // and the name only for being one the silkscreen has.
                Some(Spot::Transport(..)) => {
                    assert!(!name.starts_with("cc "), "cc {cc} placed but unnamed");
                    continue;
                }
                None => format!("cc {cc}"),
            };
            assert_eq!(name, expected, "cc {cc}");
        }
        // And the strip's own arrangement matches the device: the top row is
        // the track pair, cycle starts the middle row, the tape row is five.
        assert_eq!(spot(58), Some(Spot::Transport(0, 0)));
        assert_eq!(spot(46), Some(Spot::Transport(1, 0)));
        assert_eq!(spot(41), Some(Spot::Transport(2, 3)));
        assert_eq!(spot(45), Some(Spot::Transport(2, 4)));
    }

    /// The texels that differ between two images — the overlay's whole claim
    /// is that the picture follows the map, which is a claim about texels.
    fn texels_differing(a: &Raster, b: &Raster) -> usize {
        assert_eq!((a.width, a.height), (b.width, b.height));
        a.pixels
            .chunks(4)
            .zip(b.pixels.chunks(4))
            .filter(|(a, b)| a != b)
            .count()
    }

    fn lit_texels(r: &Raster) -> usize {
        r.pixels.chunks(4).filter(|p| *p == LIT).count()
    }

    #[test]
    fn the_factory_panel_is_drawn_and_captioned() {
        let raster = rasterize(&Map::nano_kontrol2());
        assert_eq!(
            (raster.width, raster.height),
            (PANEL_W as u32, PANEL_H as u32)
        );
        // Captions are the lit texels; a panel with none is chrome around
        // nothing. The exact count is the drawing's business, but a full
        // factory map captions every strip, which is thousands of texels.
        assert!(lit_texels(&raster) > 1000, "{}", lit_texels(&raster));
    }

    #[test]
    fn the_overlay_follows_the_map_not_the_factory_layout() {
        // The rule inherited from rl's controls display: a picture that
        // drifts from the map in force is disallowed. Move one knob in the
        // map and the picture must move with it.
        let factory = rasterize(&Map::nano_kontrol2());
        let mut moved = Map::nano_kontrol2();
        moved.fader[0].knob = crate::params::Knob::Noise;
        let moved = rasterize(&moved);
        assert!(texels_differing(&factory, &moved) > 100);
    }

    #[test]
    fn a_binding_off_the_panel_gets_a_line_rather_than_vanishing() {
        let mut map = Map::nano_kontrol2();
        map.button.push(crate::midi::Button {
            cc: 100,
            key: "space".into(),
        });
        let raster = rasterize(&map);
        assert!(raster.height > PANEL_H as u32);
    }

    #[test]
    fn an_unknown_surface_is_listed_not_drawn() {
        let mut map = Map::nano_kontrol2();
        map.device = "Launchpad".into();
        let raster = rasterize(&map);
        // A listing is one line per binding plus the device's own, and
        // nothing panel-shaped about it.
        assert_eq!(
            raster.height,
            ((map.fader.len() + map.button.len() + 1) as i32 * LINE + 2 * PAD) as u32
        );
        assert!(lit_texels(&raster) > 100);
    }

    #[test]
    fn every_factory_caption_keeps_to_two_words() {
        // The owner's ceiling, held where the captions are made: no control
        // on the shipped panel may say more than two words.
        let map = Map::nano_kontrol2();
        for f in &map.fader {
            assert!(
                f.knob.name().split_whitespace().count() <= 2,
                "{:?}",
                f.knob
            );
        }
        for b in &map.button {
            let short = crate::keys::short(&b.key).unwrap();
            assert!(short.split_whitespace().count() <= 2, "{short:?}");
        }
    }
}
