//! End-to-end checks that the loop really runs on a GPU: the seed lights the
//! monitor, the previous frame comes back, and the knobs reach the shader.
//!
//! On a machine with no adapter (a CI container, anything without a Vulkan
//! loader) each test prints why and returns. The message goes straight to the
//! process's stderr, since libtest swallows `eprintln!` from a passing test
//! and a skip nobody sees is the silent pass this suite exists to prevent.

use std::io::Write;
use std::sync::OnceLock;

use lightherder::affine::Framing;
use lightherder::capture::Capture;
use lightherder::feedback::Feedback;
use lightherder::input::{Input, Pattern, Source};
use lightherder::params::{Camera, Colour, Key, Monitor, Params, Plug, Rate};
use lightherder::present::{Present, View};
use lightherder::rig::{Rig, Select, MONITORS};

/// Where the spot this suite lights sits, in screen units — off-centre on
/// purpose: a radially symmetric spot at the centre is a fixed point of
/// rotation, so a centred one would make the rotation knob do nothing
/// visible. The radius is in the same units, where the monitor is 1.0 tall.
const SPOT: [f32; 2] = [0.25, 0.0];
const SPOT_RADIUS: f32 = 0.06;

/// The rig's one loop, as this suite's shorthand: most of what it checks —
/// the colour stage, the framing, the seed — needs one loop and reads better
/// without graph plumbing. [`graph`] turns it into the real thing.
#[derive(Clone, Copy)]
struct Single {
    framing: Framing,
    loop_gain: [f32; 3],
    /// How far switcher D stands toward the seed: how much of the seed's
    /// frame the monitor is handed each pass, and how much of the loop it
    /// keeps. Zero is the loop alone.
    seed: f32,
    colour: Colour,
}

impl Default for Single {
    /// One camera pulling back and turning a little on the one monitor it
    /// draws to, at a gain just under unity and a trickle of the seed: the
    /// classic loop, and the least graph any of the stages below can be seen
    /// in.
    fn default() -> Single {
        Single {
            framing: Framing {
                zoom: 0.994,
                rotation: 0.05,
            },
            loop_gain: [0.980, 0.986, 0.992],
            seed: 0.10,
            colour: Colour::NEUTRAL,
        }
    }
}

/// The rig with one clean loop on it: camera 3 watching monitor 3 and drawing
/// to it through the switcher chain, every other camera blind. The chain
/// above switcher D is wide open, so what monitor 3 shows is `seed` of the
/// seed's frame and the rest of camera 3 — which is what makes this the least
/// graph any of the stages below can be seen in.
fn graph(s: &Single) -> Params {
    let mut p = blank();
    p.rig.selects[2] = Select::Program;
    p.rig.switchers = [0.0, 1.0, 1.0, s.seed];
    p.shafts = [s.framing; 2];
    p.cameras[2].gain = s.loop_gain;
    p.cameras[2].look = one_hot(SEEDED);
    p.monitors[SEEDED].colour = s.colour;
    p
}

const SIZE: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Why there is no device. Only the first is a reason to let a test pass.
#[derive(Debug)]
enum NoGpu {
    NoAdapter(String),
    DeviceRefused(String),
}

/// One device for the whole suite. Tests run in parallel, and standing up
/// several wgpu devices at once — then tearing them all down at once — is
/// enough to crash the NVIDIA driver outright.
fn gpu() -> &'static Result<(wgpu::Device, wgpu::Queue), NoGpu> {
    static GPU: OnceLock<Result<(wgpu::Device, wgpu::Queue), NoGpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: lightherder::BACKENDS,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| NoGpu::NoAdapter(e.to_string()))?;
        let name = adapter.get_info().name.clone();
        let _ = writeln!(std::io::stderr(), "lightherder tests: adapter {name}");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("lightherder tests"),
            ..Default::default()
        }))
        .map_err(|e| NoGpu::DeviceRefused(format!("adapter {name} refused a device: {e}")))
    })
}

/// A bank of monitors, the cameras wired to them, and somewhere to read the
/// result back from.
struct Harness {
    device: &'static wgpu::Device,
    queue: &'static wgpu::Queue,
    feedback: Feedback,
    present: Present,
    target: wgpu::Texture,
    readback: wgpu::Buffer,
    target_size: (u32, u32),
}

impl Harness {
    fn new(monitor: (u32, u32), target_size: (u32, u32), params: &Params) -> Harness {
        // Read-back is the point of this harness, and a texture-to-buffer
        // copy demands 256-byte rows.
        assert!(
            (target_size.0 * 4).is_multiple_of(256),
            "target width {} breaks the read-back row alignment",
            target_size.0
        );
        let (device, queue) = gpu().as_ref().expect("checked by harness()");

        let feedback = Feedback::new(device, monitor.0, monitor.1, params);
        let present = Present::new(device, &feedback, TARGET_FORMAT);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("readback target"),
            size: wgpu::Extent3d {
                width: target_size.0,
                height: target_size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (target_size.0 * target_size.1 * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut h = Harness {
            device,
            queue,
            feedback,
            present,
            target,
            readback,
            target_size,
        };
        // The spot every test that needs a picture on the glass starts from,
        // on the seed's layer where the switcher will find it. Written once
        // and never again: the seed is a still upload, so how much of it
        // reaches a monitor is the switchers' business and nothing else's.
        let frame = spot_frame(h.feedback.size());
        h.feedback.write_seed(h.queue, &frame);
        h
    }

    /// Where the spot lands on this harness's monitors, in uv.
    fn spot_uv(&self) -> [f32; 2] {
        lightherder::affine::screen_to_uv(self.feedback.aspect()).apply(SPOT)
    }

    fn step(&mut self, params: &Single) {
        let params = graph(params);
        self.feedback.step(self.device, self.queue, &params);
        // Soloed: the single loop is monitor 3 of five — the one the seed can
        // reach — and every test built on this shorthand reads the whole
        // target as that monitor.
        self.present(Some(SEEDED));
    }

    fn step_graph(&mut self, params: &Params) {
        self.feedback.step(self.device, self.queue, params);
        self.present(None);
    }

    /// A pass with one monitor of the bank on the whole target, for the
    /// tests that read a single loop's own pixels rather than the grid.
    fn step_solo(&mut self, params: &Params, monitor: usize) {
        self.feedback.step(self.device, self.queue, params);
        self.present(Some(monitor));
    }

    fn present(&self, solo: Option<usize>) {
        let view = solo.map_or(View::Bank { focus: None }, View::Solo);
        self.present.draw(
            self.device,
            self.queue,
            &self.target,
            &self.feedback,
            view,
            None,
        );
    }

    /// The three channels where the seed lands, which is the one place the
    /// colour tests look.
    fn spot(&self) -> [f32; 3] {
        let at = self.spot_uv();
        self.read().rgb_at(at[0], at[1])
    }

    fn read(&self) -> Image {
        let (width, height) = self.target_size;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map readback buffer"));
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let pixels = slice
            .get_mapped_range()
            .expect("map readback range")
            .to_vec();
        self.readback.unmap();
        Image {
            pixels,
            width,
            height,
        }
    }
}

struct Image {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

impl Image {
    /// The three channels at a uv position, 0..=255. Values are linear: the
    /// target format is not sRGB, unlike the window's usual surface.
    fn rgb_at(&self, u: f32, v: f32) -> [f32; 3] {
        let x = ((u * self.width as f32) as u32).min(self.width - 1);
        let y = ((v * self.height as f32) as u32).min(self.height - 1);
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i] as f32,
            self.pixels[i + 1] as f32,
            self.pixels[i + 2] as f32,
        ]
    }

    /// Mean of the three channels, for the tests that only care how much
    /// light there is and not what colour it is.
    fn at(&self, u: f32, v: f32) -> f32 {
        let rgb = self.rgb_at(u, v);
        (rgb[0] + rgb[1] + rgb[2]) / 3.0
    }

    fn brightest(&self) -> f32 {
        self.brightest_in(0.0, 0.0, 1.0, 1.0)
    }

    /// The brightest one channel gets inside a uv rectangle. Separate from
    /// [`Image::brightest_in`] because that averages the three, which cannot
    /// tell a settled picture from one that has lost a channel outright.
    fn brightest_channel_in(&self, channel: usize, u0: f32, v0: f32, u1: f32, v1: f32) -> f32 {
        let (x0, x1) = (
            (u0 * self.width as f32) as u32,
            (u1 * self.width as f32) as u32,
        );
        let (y0, y1) = (
            (v0 * self.height as f32) as u32,
            (v1 * self.height as f32) as u32,
        );
        let mut peak = 0.0f32;
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                let i = ((y * self.width + x) * 4) as usize + channel;
                peak = peak.max(self.pixels[i] as f32);
            }
        }
        peak
    }

    /// The brightest pixel inside a uv rectangle — one tile of the grid,
    /// when the caller is asking about a single monitor.
    fn brightest_in(&self, u0: f32, v0: f32, u1: f32, v1: f32) -> f32 {
        let (x0, x1) = (
            (u0 * self.width as f32) as u32,
            (u1 * self.width as f32) as u32,
        );
        let (y0, y1) = (
            (v0 * self.height as f32) as u32,
            (v1 * self.height as f32) as u32,
        );
        let mut peak = 0.0f32;
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                let i = ((y * self.width + x) * 4) as usize;
                let p = &self.pixels[i..i + 3];
                peak = peak.max((p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0);
            }
        }
        peak
    }

    /// Where the brightest pixel is, in uv — an oracle for "the loop put its
    /// spot there" that does not ask the code under test where it put it.
    fn brightest_uv(&self) -> [f32; 2] {
        let (index, _) = self
            .pixels
            .chunks_exact(4)
            .enumerate()
            .map(|(i, p)| (i, p[0] as u32 + p[1] as u32 + p[2] as u32))
            .max_by_key(|(_, sum)| *sum)
            .expect("a non-empty image");
        let (x, y) = (index as u32 % self.width, index as u32 / self.width);
        [
            (x as f32 + 0.5) / self.width as f32,
            (y as f32 + 0.5) / self.height as f32,
        ]
    }

    /// How far the lit region reaches from `centre` along an axis, in pixels:
    /// the last offset whose pixel is at least half the peak there.
    fn half_extent(&self, centre: [f32; 2], horizontal: bool) -> u32 {
        let peak = self.at(centre[0], centre[1]);
        let (span, along) = if horizontal {
            (self.width, centre[0])
        } else {
            (self.height, centre[1])
        };
        let start = (along * span as f32) as u32;
        (1..span - start)
            .take_while(|d| {
                let moved = (start + d) as f32 / span as f32;
                let (u, v) = if horizontal {
                    (moved, centre[1])
                } else {
                    (centre[0], moved)
                };
                self.at(u, v) >= peak / 2.0
            })
            .count() as u32
    }
}

/// `None` means this machine has no GPU to test with, which is not a failure.
/// A machine that has one and cannot open it is a failure, and panics.
fn graph_harness(monitor: (u32, u32), target: (u32, u32), params: &Params) -> Option<Harness> {
    match gpu() {
        Ok(_) => Some(Harness::new(monitor, target, params)),
        Err(NoGpu::NoAdapter(why)) => {
            let _ = writeln!(std::io::stderr(), "SKIPPED: no adapter: {why}");
            None
        }
        Err(NoGpu::DeviceRefused(why)) => panic!("{why}"),
    }
}

/// The single loop, which is all most of this suite needs.
fn harness(monitor: (u32, u32), target: (u32, u32)) -> Option<Harness> {
    graph_harness(monitor, target, &graph(&Single::default()))
}

fn square() -> Option<Harness> {
    harness((SIZE, SIZE), (SIZE, SIZE))
}

fn seeded() -> Single {
    Single {
        seed: 1.0,
        ..Default::default()
    }
}

/// Params whose camera does not move, so a lit spot stays where it was put.
fn frozen(params: Single) -> Single {
    Single {
        framing: Framing {
            zoom: 1.0,
            rotation: 0.0,
        },
        ..params
    }
}

/// Per-channel loop gain steep enough to leave the white seed strongly
/// coloured. Hue and saturation do nothing to grey, so the light has to be
/// coloured before either knob has anything to act on.
const TINT: [f32; 3] = [1.0, 0.4, 0.1];

/// Lights the seed at half brightness and tints it, leaving the spot at
/// `(0.5, 0.2, 0.05)`. Half brightness on purpose: red then sits exactly on
/// the contrast pivot, and a knob that moves light into a channel has
/// somewhere to put it before the 8-bit target clips.
fn tinted(h: &mut Harness) -> [f32; 3] {
    let still = frozen(seeded());
    h.step(&Single {
        seed: 0.5,
        loop_gain: [0.0; 3],
        ..still
    });
    h.step(&Single {
        seed: 0.0,
        loop_gain: TINT,
        ..still
    });
    h.spot()
}

/// One more pass with the loop passing light straight through, so the only
/// thing between the previous frame and this one is the colour stage.
fn recolour(h: &mut Harness, colour: Colour) -> [f32; 3] {
    h.step(&Single {
        seed: 0.0,
        loop_gain: [1.0; 3],
        colour,
        ..frozen(seeded())
    });
    h.spot()
}

/// NTSC luma — the quantity the chroma knobs claim to leave alone, so the
/// tests need their own copy of it to check that claim against.
fn luma(rgb: [f32; 3]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

fn spread(rgb: [f32; 3]) -> f32 {
    rgb.iter().fold(0.0, |a: f32, c| a.max(*c)) - rgb.iter().fold(255.0, |a: f32, c| a.min(*c))
}

#[test]
fn the_colour_stage_is_inert_at_its_defaults() {
    // Neutral means neutral, and it has to keep meaning it: the stage runs
    // every pass forever, so anything less than an exact identity compounds
    // into a colour cast. One pass hides that; a hundred does not. Composed
    // on the CPU this does not move at all, so the tolerance is a level
    // rather than a fudge: the published four-digit inverse walks off by 12
    // levels of blue, and chaining the two matrices per fragment instead of
    // composing them first walks red off by 4.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    assert!(spread(before) > 50.0, "nothing to preserve: {before:?}");

    let mut after = before;
    for _ in 0..100 {
        after = recolour(&mut h, Colour::NEUTRAL);
    }
    for channel in 0..3 {
        assert!(
            (after[channel] - before[channel]).abs() < 1.0,
            "{before:?} walked to {after:?} in a hundred neutral passes"
        );
    }
}

#[test]
fn saturation_at_zero_greys_without_dimming() {
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    let after = recolour(
        &mut h,
        Colour {
            saturation: 0.0,
            ..Colour::NEUTRAL
        },
    );
    assert!(spread(after) < 4.0, "still coloured: {after:?}");
    // Pulling the chroma to zero must not take any luma with it: what is left
    // is the grey the colour was carrying all along.
    assert!(
        (luma(after) - luma(before)).abs() < 4.0,
        "{:?} -> {:?}: luma {} -> {}",
        before,
        after,
        luma(before),
        luma(after)
    );
}

#[test]
fn hue_moves_light_between_the_channels_at_constant_luma() {
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    // A third of a turn of the subcarrier, which is far enough that no
    // rounding could be mistaken for it.
    let after = recolour(
        &mut h,
        Colour {
            hue: core::f32::consts::TAU / 3.0,
            ..Colour::NEUTRAL
        },
    );
    assert!(
        after[2] > before[2] + 100.0 && after[0] < before[0] - 40.0,
        "{before:?} -> {after:?}: the light did not move from red to blue"
    );
    // The whole point of turning a phase rather than mixing channels: the
    // colour changes and the brightness does not.
    assert!(
        (luma(after) - luma(before)).abs() < 5.0,
        "{:?} -> {:?}: luma {} -> {}",
        before,
        after,
        luma(before),
        luma(after)
    );
}

#[test]
fn temperature_tints_grey_at_constant_luma_and_leaves_it_grey_at_rest() {
    let Some(mut h) = square() else { return };
    // A grey spot, lit afresh before every setting: the seed at half
    // brightness with nothing fed back, so there is no chroma for the tint
    // to be mistaken for a turn of, and no previous tint under this one.
    let mut grey_through = |temperature: f32| {
        h.step(&Single {
            seed: 0.5,
            loop_gain: [0.0; 3],
            ..frozen(seeded())
        });
        let grey = h.spot();
        assert_eq!(spread(grey), 0.0, "not grey to begin with: {grey:?}");
        let tinted = recolour(
            &mut h,
            Colour {
                temperature,
                ..Colour::NEUTRAL
            },
        );
        (grey, tinted)
    };
    let (grey, at_rest) = grey_through(0.0);
    assert_eq!(at_rest, grey, "grey did not stay grey at rest");
    let (_, warm) = grey_through(340.0);
    assert!(
        warm[0] > warm[1] + 8.0 && warm[1] > warm[2] + 8.0,
        "{grey:?} -> {warm:?}: not warmed"
    );
    let (_, cool) = grey_through(-100.0);
    assert!(
        cool[2] > cool[1] + 4.0 && cool[1] > cool[0] + 4.0,
        "{grey:?} -> {cool:?}: not cooled"
    );
    // A white point is a tint, not a level: what changed is where the light
    // sits between the channels, not how much of it there is.
    for tinted in [warm, cool] {
        assert!(
            (luma(tinted) - luma(grey)).abs() < 3.0,
            "{:?} -> {:?}: luma {} -> {}",
            grey,
            tinted,
            luma(grey),
            luma(tinted)
        );
    }
}

#[test]
fn brightness_lifts_black_itself() {
    let Some(mut h) = square() else { return };
    let lift = 0.2;
    let before = tinted(&mut h);
    let after = recolour(
        &mut h,
        Colour {
            brightness: lift,
            ..Colour::NEUTRAL
        },
    );
    for channel in 0..3 {
        assert!(
            (after[channel] - before[channel] - lift * 255.0).abs() < 5.0,
            "{before:?} -> {after:?}, expected every channel up by {}",
            lift * 255.0
        );
    }
    // An unlit corner comes up with everything else, which a gain could never
    // manage: this is a black level, not another multiply.
    let corner = h.read().rgb_at(0.02, 0.02);
    assert!(
        (luma(corner) - lift * 255.0).abs() < 5.0,
        "black stayed at {corner:?}"
    );
}

#[test]
fn contrast_pivots_about_mid_grey() {
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    assert!(spread(before) > 50.0, "nothing to push apart: {before:?}");

    let contrast = 1.5;
    let after = recolour(
        &mut h,
        Colour {
            contrast,
            ..Colour::NEUTRAL
        },
    );
    // Mid-grey is the fixed point: red, a hair under it, barely moves, while
    // green far below it is pushed further down. A gain about black — which
    // is what the loop gain already is — would have raised both instead.
    // Blue is left out: the pivot pushes it past black, where the clamp is.
    let pivoted = |v: f32| (v - 127.5) * contrast + 127.5;
    for channel in 0..2 {
        assert!(
            (after[channel] - pivoted(before[channel])).abs() < 5.0,
            "{:?} -> {:?}: channel {channel} belongs at {}, and a gain about black would have put it at {}",
            before,
            after,
            pivoted(before[channel]),
            before[channel] * contrast
        );
    }
}

#[test]
fn the_amplifier_lifts_after_it_expands() {
    // Turning one knob at a time leaves the other stages at identity, where
    // every order of them looks alike. Contrast and brightness together is
    // the case that can tell them apart: lifting before the expansion would
    // scale the lift too, and land a fifth of full scale higher.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);

    let (brightness, contrast) = (0.2, 2.0);
    let after = recolour(
        &mut h,
        Colour {
            brightness,
            contrast,
            ..Colour::NEUTRAL
        },
    );
    let expected = (before[0] - 127.5) * contrast + 127.5 + brightness * 255.0;
    assert!(
        (after[0] - expected).abs() < 5.0,
        "{:?} -> {:?}: red belongs at {expected}, and lifting first would put it at {}",
        before,
        after,
        (before[0] - 127.5 + brightness * 255.0) * contrast + 127.5
    );
}

#[test]
fn the_knobs_colour_the_seed_too() {
    // The front panel is on the monitor, not on the camera, so it acts on
    // everything the monitor displays. With the loop dark the seed is the
    // only thing on it, and the curve has to reach it there.
    let Some(mut h) = square() else { return };
    let dark_loop = Single {
        seed: 0.5,
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    };
    h.step(&dark_loop);
    let plain = h.spot();

    h.step(&Single {
        colour: Colour {
            contrast: 1.5,
            ..Colour::NEUTRAL
        },
        ..dark_loop
    });
    let curved = h.spot();
    let expected = (plain[0] - 127.5) * 1.5 + 127.5;
    assert!(
        (curved[0] - expected).abs() < 5.0,
        "seed {} -> {}, expected {expected}: the panel did not reach it",
        plain[0],
        curved[0]
    );
}

#[test]
fn a_level_pushed_below_black_comes_back_black() {
    // Contrast carries a dark channel under zero, and a phosphor emits no
    // negative light. Without the floor the pass writes that negative into a
    // loop that feeds itself forever.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    assert!(before[2] > 5.0, "blue was already black: {before:?}");

    let after = recolour(
        &mut h,
        Colour {
            contrast: 1.5,
            ..Colour::NEUTRAL
        },
    );
    assert!(after[2] < 2.0, "blue came back as {}", after[2]);
    assert!(after[0] > 20.0, "the frame died with it: {after:?}");

    // Black, and still a number: lifting the black level brings it back.
    // Not-a-number would have stayed not-a-number for the rest of the run.
    let lift = 0.3;
    let lifted = recolour(
        &mut h,
        Colour {
            brightness: lift,
            ..Colour::NEUTRAL
        },
    );
    assert!(
        (lifted[2] - lift * 255.0).abs() < 8.0,
        "blue did not come back: {lifted:?}"
    );
}

#[test]
fn the_colour_stage_is_inside_the_loop() {
    // The knobs are on the monitor being drawn, not on the window showing it,
    // so a second pass bends an already-bent frame again. Moved into the
    // present pass the stage would sit at one application forever.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    let lift = 0.1;
    let lifted = Colour {
        brightness: lift,
        ..Colour::NEUTRAL
    };
    let once = recolour(&mut h, lifted);
    let twice = recolour(&mut h, lifted);
    // Each pass lifts the level it was handed, whatever that level was.
    let step = lift * 255.0;
    assert!(
        (once[1] - (before[1] + step)).abs() < 5.0,
        "{:?} -> {:?}: expected {}",
        before,
        once,
        before[1] + step
    );
    assert!(
        (twice[1] - (once[1] + step)).abs() < 5.0,
        "{:?} -> {:?}: expected {}",
        once,
        twice,
        once[1] + step
    );
}

#[test]
fn the_seed_lights_the_spot_it_says_it_does() {
    let Some(mut h) = square() else { return };
    let seed = h.spot_uv();
    h.step(&seeded());
    let img = h.read();
    assert!(
        img.at(seed[0], seed[1]) > 200.0,
        "seed was {}",
        img.at(seed[0], seed[1])
    );
    assert!(
        img.at(0.02, 0.02) < 10.0,
        "corner was {}",
        img.at(0.02, 0.02)
    );
}

#[test]
fn the_image_survives_the_seed_being_switched_off() {
    let Some(mut h) = square() else { return };
    let seed = h.spot_uv();
    h.step(&seeded());
    let mut previous = h.read().at(seed[0], seed[1]);

    let params = Single {
        seed: 0.0,
        loop_gain: [0.9; 3],
        ..frozen(seeded())
    };
    for _ in 0..4 {
        h.step(&params);
        let now = h.read().at(seed[0], seed[1]);
        assert!(now < previous, "{now} should be dimmer than {previous}");
        previous = now;
    }
    // Still visible: this is the previous frame coming back round, not a clear.
    assert!(previous > 20.0, "the loop went dark: {previous}");
}

#[test]
fn zero_gain_ends_the_loop_in_one_pass() {
    let Some(mut h) = square() else { return };
    let seed = h.spot_uv();
    h.step(&seeded());
    assert!(h.read().at(seed[0], seed[1]) > 200.0);

    let params = Single {
        seed: 0.0,
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    };
    h.step(&params);
    let left = h.read().at(seed[0], seed[1]);
    assert!(left < 2.0, "gain 0 left {left}");
}

#[test]
fn the_seed_is_round_on_a_wide_monitor() {
    // The only end-to-end check of the aspect correction: on a 2:1 monitor an
    // uncorrected seed radius would be twice as wide as it is tall.
    let Some(mut h) = harness((SIZE * 4, SIZE * 2), (SIZE * 4, SIZE * 2)) else {
        return;
    };
    let seed = h.spot_uv();
    h.step(&Single {
        loop_gain: [0.0; 3],
        ..seeded()
    });

    let img = h.read();
    let across = img.half_extent(seed, true);
    let down = img.half_extent(seed, false);
    assert!(across > 4, "seed too small to measure: {across}");
    assert!(
        across.abs_diff(down) <= 1,
        "seed is {across} px wide and {down} px tall"
    );
}

#[test]
fn the_default_knobs_settle_without_clipping() {
    // Left running, the default loop settles somewhere. Clipped to flat white
    // and every bit of structure is lost; too dark and there is nothing to
    // see at all.
    let Some(mut h) = square() else { return };
    let params = graph(&Single::default());
    for _ in 0..400 {
        h.feedback.step(h.device, h.queue, &params);
    }
    h.step_solo(&params, SEEDED);

    let img = h.read();
    let peak = img.brightest();
    assert!(
        peak < 250.0,
        "the default settles clipped: brightest {peak}"
    );
    assert!(
        peak > 60.0,
        "the default settles too dim to see: brightest {peak}"
    );
}

#[test]
fn blanking_the_monitor_puts_out_everything_on_it() {
    let Some(mut h) = square() else { return };
    h.step(&seeded());
    assert!(h.read().brightest() > 200.0, "nothing to blank");

    h.feedback.clear(h.device, h.queue);
    h.present(None);
    let left = h.read().brightest();
    assert!(left < 2.0, "blanking left {left}");
}

#[test]
fn the_gain_is_applied_once_per_pass() {
    let Some(mut h) = square() else { return };
    let seed = h.spot_uv();
    h.step(&seeded());
    let mut previous = h.read().at(seed[0], seed[1]);

    let gain = 0.8;
    let params = Single {
        seed: 0.0,
        loop_gain: [gain; 3],
        ..frozen(seeded())
    };
    for pass in 1..=3 {
        h.step(&params);
        let now = h.read().at(seed[0], seed[1]);
        // Applying the gain twice per pass would put this at 0.64 of the
        // previous value, which is well outside the tolerance.
        let ratio = now / previous;
        assert!(
            (ratio - gain).abs() < 0.05,
            "pass {pass}: {now} / {previous} = {ratio}, expected {gain}"
        );
        previous = now;
    }
}

#[test]
fn what_the_camera_sees_past_the_monitor_is_black() {
    let Some(mut h) = square() else { return };
    // A flat frame, so the monitor is lit edge to edge before anything is
    // asked of the sampler: a clamped one would then have something bright
    // to smear, which is what distinguishes "outside reads black" from
    // "outside is whatever the sampler clamped to".
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), [220; 3]));
    h.step(&Single {
        seed: 1.0,
        loop_gain: [0.0; 3],
        ..frozen(Single::default())
    });
    let img = h.read();
    assert!(
        img.at(0.99, 0.5) > 50.0,
        "the right edge should be lit before the test means anything: {}",
        img.at(0.99, 0.5)
    );

    // Now minify: the camera sees a wider field than the monitor holds, so
    // the border of the next frame can only be sourced from beyond its edge.
    h.step(&Single {
        seed: 0.0,
        loop_gain: [1.0; 3],
        colour: Colour::NEUTRAL,
        framing: Framing {
            zoom: 0.7,
            rotation: 0.0,
        },
    });
    let img = h.read();
    assert!(
        img.at(0.02, 0.5) < 2.0,
        "outside the monitor read {}, not black",
        img.at(0.02, 0.5)
    );
    assert!(
        img.brightest() > 50.0,
        "the image vanished entirely, so nothing was proved: {}",
        img.brightest()
    );
}

#[test]
fn a_window_of_the_wrong_shape_gets_bars_rather_than_a_stretch() {
    // A square monitor in a 4:1 target: the image belongs in the middle
    // quarter, and the rest of the target must stay black.
    let Some(mut h) = harness((SIZE, SIZE), (SIZE * 4, SIZE)) else {
        return;
    };
    h.step(&Single {
        loop_gain: [0.0; 3],
        ..seeded()
    });

    let img = h.read();
    assert!(
        img.at(0.05, 0.5) < 2.0 && img.at(0.95, 0.5) < 2.0,
        "the bars are lit: {} and {}",
        img.at(0.05, 0.5),
        img.at(0.95, 0.5)
    );

    // The seed sits a quarter of the monitor's height right of its centre,
    // which lands at that fraction of the letterboxed rectangle.
    let seed = h.spot_uv();
    let expected = 0.375 + seed[0] / 4.0;
    let found = img.brightest_uv();
    assert!(
        (found[0] - expected).abs() < 0.02 && (found[1] - 0.5).abs() < 0.02,
        "spot at {found:?}, expected [{expected}, 0.5]"
    );
}

#[test]
fn the_seed_sits_where_the_convention_says_it_does() {
    // An oracle that does not ask blob_uv where the blob is: the spot lands a
    // quarter of the monitor's HEIGHT right of centre, which on a 2:1 monitor
    // is an eighth of its width.
    let Some(mut h) = harness((SIZE * 4, SIZE * 2), (SIZE * 4, SIZE * 2)) else {
        return;
    };
    h.step(&Single {
        loop_gain: [0.0; 3],
        ..seeded()
    });

    let found = h.read().brightest_uv();
    assert!(
        (found[0] - 0.625).abs() < 0.02 && (found[1] - 0.5).abs() < 0.02,
        "seed at {found:?}, expected [0.625, 0.5]"
    );
}

// ---- The graph itself: routing, splitters, mixing, and the tiled window ----

/// uv on monitor `m` -> uv in the tiled read-back target. Only exact for a
/// target sized as the grid times the monitor, which the graph tests use.
fn tile(monitors: usize, m: usize, u: f32, v: f32) -> (f32, f32) {
    let (cols, rows) = lightherder::present::grid(monitors);
    let (col, row) = (m as u32 % cols, m as u32 / cols);
    (
        (col as f32 + u) / cols as f32,
        (row as f32 + v) / rows as f32,
    )
}

fn one_hot(hot: usize) -> [f32; MONITORS] {
    let mut look = [0.0; MONITORS];
    look[hot] = 1.0;
    look
}

fn plain_camera(look: [f32; MONITORS]) -> Camera {
    Camera {
        gain: [1.0; 3],
        look,
        delay: 0,
    }
}

fn silent_monitor() -> Monitor {
    Monitor {
        colour: Colour::NEUTRAL,
        flip: [false; 2],
        rate: Rate::Full,
        sharpness: 0.0,
    }
}

/// The rig with every camera blind and every monitor silent: the blank the
/// wiring tests below build on. The switchers stand at their identity, so
/// each structure monitor is on its own camera direct and the seed reaches
/// none of them — what a test wants of the matrix it says with the switchers,
/// and what it wants each camera to see it says with `look`.
fn blank() -> Params {
    let mut p = lightherder::config::instrument();
    p.rig = Rig::IDENTITY;
    p.delay = 0;
    p.shafts = [Framing::identity(); 2];
    for camera in &mut p.cameras {
        *camera = plain_camera([0.0; MONITORS]);
    }
    for monitor in &mut p.monitors {
        *monitor = silent_monitor();
    }
    p
}

/// The whole target, as the grid the bank is tiled into.
fn tiled() -> (u32, u32) {
    let (cols, rows) = lightherder::present::grid(MONITORS);
    (cols * SIZE, rows * SIZE)
}

fn bars(key: Key) -> Plug {
    Plug {
        source: Input::Pattern(Pattern::Bars),
        key,
    }
}

#[test]
fn the_routing_matrix_sends_each_camera_across() {
    // The crossed two-structure wiring, distilled: light lit on monitor 3
    // must appear on monitor 1 one pass later and bounce back the pass after,
    // and never sit still where it was. Camera A watches monitor 3 and camera
    // B watches monitor 1, each drawing to its own structure.
    let mut p = blank();
    p.cameras[0].look = one_hot(SEEDED);
    p.cameras[1].look = one_hot(0);
    seeding(&mut p);
    let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
        return;
    };
    let seed = h.spot_uv();
    let at = |img: &Image, m: usize| {
        let (u, v) = tile(MONITORS, m, seed[0], seed[1]);
        img.at(u, v)
    };

    h.step_graph(&p);
    let img = h.read();
    assert!(at(&img, 2) > 200.0, "the seed never lit: {}", at(&img, 2));
    assert!(
        at(&img, 0) < 2.0,
        "monitor 1 lit before anything crossed: {}",
        at(&img, 0)
    );

    seeded_no_more(&mut p);
    h.step_graph(&p);
    let img = h.read();
    assert!(
        at(&img, 0) > 200.0,
        "the seed did not cross: {}",
        at(&img, 0)
    );
    assert!(
        at(&img, 2) < 2.0,
        "monitor 3 kept light no camera of its own hands it: {}",
        at(&img, 2)
    );

    h.step_graph(&p);
    let img = h.read();
    assert!(
        at(&img, 2) > 200.0,
        "the seed did not cross back: {}",
        at(&img, 2)
    );
    assert!(
        at(&img, 0) < 2.0,
        "or it left a copy behind: {}",
        at(&img, 0)
    );
}

#[test]
fn the_focused_tile_is_framed_and_only_in_the_bank() {
    // The front panel plays one monitor of five, and the tiled bank shows
    // which by a line round its tile: the line is at the tile's very edge,
    // on the focused tile alone, and not at all when a solo already shows
    // one monitor and nothing else.
    let p = blank();
    let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
        return;
    };
    h.feedback.step(h.device, h.queue, &p);
    let (width, height) = tiled();
    let texel = |img: &Image, m: usize, x: u32, y: u32| {
        let (u, v) = tile(MONITORS, m, 0.0, 0.0);
        img.rgb_at(u + x as f32 / width as f32, v + y as f32 / height as f32)
    };
    let draw = |view| {
        h.present
            .draw(h.device, h.queue, &h.target, &h.feedback, view, None);
        h.read()
    };

    let img = draw(View::Bank { focus: Some(1) });
    assert_eq!(texel(&img, 1, 0, 0), [255.0; 3], "the corner is not lined");
    assert_eq!(
        texel(&img, 1, SIZE / 2, 0),
        [255.0; 3],
        "the top edge is not lined"
    );
    assert_eq!(
        texel(&img, 1, SIZE - 1, SIZE - 1),
        [255.0; 3],
        "the far corner is not lined"
    );
    for inside in [1, SIZE - 2] {
        assert_eq!(
            texel(&img, 1, inside, inside),
            [0.0; 3],
            "the line ate into the picture"
        );
    }
    assert_eq!(texel(&img, 0, 0, 0), [0.0; 3], "an unfocused tile is lined");
    assert_eq!(
        texel(&img, 2, SIZE - 1, SIZE - 1),
        [0.0; 3],
        "an unfocused tile is lined"
    );

    let img = draw(View::Bank { focus: None });
    assert_eq!(texel(&img, 1, 0, 0), [0.0; 3], "a focus-less draw is lined");

    let img = draw(View::Solo(1));
    let (x, w) = ((width - height) as f32 / 2.0, height as f32);
    assert_eq!(
        img.rgb_at((x + 0.5) / width as f32, 0.0),
        [0.0; 3],
        "a solo is lined at its near corner"
    );
    assert_eq!(
        img.rgb_at((x + w - 0.5) / width as f32, 0.0),
        [0.0; 3],
        "a solo is lined at its far corner"
    );
}

#[test]
fn a_crossfade_delivers_the_fractions_it_names() {
    // Two cameras on one monitor, crossfaded 3:1. Their framings differ —
    // one holds still, one turns the picture a quarter round — so the two
    // contributions land in different places and each share can be read off
    // on its own.
    let Some(mut h) = square() else { return };
    let mut p = blank();
    p.cameras[0].look = one_hot(SEEDED);
    p.cameras[1].look = one_hot(SEEDED);
    p.shafts[1].rotation = std::f32::consts::FRAC_PI_2;
    // Monitor 3 takes the whole seed; monitor 1 takes switcher A's program,
    // a quarter of the way from camera A toward camera B.
    p.rig.selects[0] = Select::Program;
    p.rig.selects[SEEDED] = Select::Program;
    p.rig.switchers = [0.25, 1.0, 1.0, 1.0];

    h.step_solo(&p, SEEDED);
    let at = h.spot_uv();
    let base = h.read().at(at[0], at[1]);
    assert!(base > 200.0, "the seed never lit: {base}");

    h.step_solo(&p, 0);
    let img = h.read();
    // The spot sits a quarter of the monitor's height right of centre, so a
    // quarter turn counter-clockwise puts it the same distance above centre.
    let (held, turned) = (img.at(at[0], at[1]), img.at(0.5, 0.25));
    assert!(
        (held / base - 0.75).abs() < 0.04,
        "the crossfade delivered {held} of {base}, not three quarters"
    );
    assert!(
        (turned / base - 0.25).abs() < 0.04,
        "the crossfade delivered {turned} of {base}, not a quarter"
    );
}

#[test]
fn a_router_output_flip_mirrors_what_the_monitor_is_handed() {
    // The mirror is on the output, not on any one source, so it turns over
    // the whole picture the switcher hands the monitor — and it is its own
    // inverse, so a second flip puts it back exactly.
    let Some(mut h) = square() else { return };
    let lit = Single {
        seed: 1.0,
        loop_gain: [0.0; 3],
        ..frozen(Single::default())
    };
    h.step(&lit);
    let at = h.spot_uv();
    let plain = h.read();
    assert!(plain.at(at[0], at[1]) > 200.0, "nothing to mirror");

    let mut p = graph(&lit);
    p.monitors[SEEDED].flip = [true, false];
    h.step_solo(&p, SEEDED);
    let img = h.read();
    assert!(
        img.at(1.0 - at[0], at[1]) > 200.0,
        "the spot is not across from where it was: {}",
        img.at(1.0 - at[0], at[1])
    );
    assert!(
        img.at(at[0], at[1]) < 2.0,
        "it left a copy where it was: {}",
        img.at(at[0], at[1])
    );

    p.monitors[SEEDED].flip = [true, true];
    h.step_solo(&p, SEEDED);
    let img = h.read();
    assert!(
        img.at(1.0 - at[0], 1.0 - at[1]) > 200.0,
        "the second axis did not turn it over too"
    );

    // Off again, and the picture is the one it started as, texel for texel.
    p.monitors[SEEDED].flip = [false; 2];
    h.step_solo(&p, SEEDED);
    assert_eq!(
        h.read().pixels,
        plain.pixels,
        "a flip taken off did not put the picture back"
    );

    // And the mirror is on the output, not on what the camera sees: with the
    // camera turned a quarter, mirroring the picture it hands over and
    // mirroring the picture it looks at are different places. A quarter turn
    // puts the spot a quarter of the monitor above centre; the output flip
    // then puts it the same distance below, where a flip on the camera's own
    // sampling would have left it above — the turn's axis is the one the
    // mirror would have been about.
    h.step(&lit);
    let mut turned = graph(&Single {
        seed: 0.0,
        loop_gain: [1.0; 3],
        framing: Framing {
            zoom: 1.0,
            rotation: std::f32::consts::FRAC_PI_2,
        },
        ..lit
    });
    turned.monitors[SEEDED].flip = [false, true];
    h.step_solo(&turned, SEEDED);
    let img = h.read();
    assert!(
        img.at(0.5, 0.75) > 200.0,
        "the output was not mirrored: {}",
        img.at(0.5, 0.75)
    );
    assert!(
        img.at(0.5, 0.25) < 2.0,
        "the mirror landed on what the camera saw instead: {}",
        img.at(0.5, 0.25)
    );
}

#[test]
fn a_beam_splitter_blends_two_monitors_into_one_camera() {
    // One camera looking through 50/50 splitter glass at two monitors,
    // feeding a third. Light one of the pair alone: half its light arrives,
    // which no row of the matrix could do — the blend happens in front of
    // the lens.
    let mut p = blank();
    p.cameras[0].look[SEEDED] = 0.5;
    p.cameras[0].look[1] = 0.5;
    seeding(&mut p);
    let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
        return;
    };
    let seed = h.spot_uv();
    h.step_graph(&p);
    let (u, v) = tile(MONITORS, SEEDED, seed[0], seed[1]);
    let bright = h.read().at(u, v);
    assert!(bright > 200.0, "the seed never lit: {bright}");

    seeded_no_more(&mut p);
    h.step_graph(&p);
    let img = h.read();
    let (u, v) = tile(MONITORS, 0, seed[0], seed[1]);
    assert!(
        (img.at(u, v) - bright / 2.0).abs() < 8.0,
        "the splitter delivered {} of {bright}",
        img.at(u, v)
    );
}

#[test]
fn a_structure_takes_half_of_the_other_through_the_cross_link() {
    // Switcher A a quarter open puts a quarter of camera B on both of
    // structure A's monitors. Camera B watches the seeded monitor and camera
    // A is blind, so what lands on structure A is exactly the light the
    // crossfade passed, and structure B keeps the whole of it.
    let mut p = blank();
    p.cameras[1].look = one_hot(SEEDED);
    seeding(&mut p);
    let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
        return;
    };
    h.step_graph(&p);
    p.rig.selects = [Select::Program; lightherder::rig::SELECTS];
    p.rig.switchers = [0.25, 0.0, 0.0, 0.0];
    h.step_graph(&p);

    let seed = h.spot_uv();
    let img = h.read();
    let want = [0.25, 0.25, 1.0, 1.0, 1.0];
    for (m, want) in want.into_iter().enumerate() {
        let (u, v) = tile(MONITORS, m, seed[0], seed[1]);
        assert!(
            (img.at(u, v) - 255.0 * want).abs() < 10.0,
            "monitor {m} shows {}, not {want} of the seed",
            img.at(u, v)
        );
    }
}

#[test]
fn a_solo_puts_one_monitor_on_the_whole_target() {
    // The tiled bank and a soloed monitor are the same picture at two sizes:
    // light on monitor 3 sits in monitor 3's cell while the bank is tiled
    // and at the same place on the whole target once that monitor is soloed.
    // Every camera is blind, so the seed is the only light there is and the
    // read can say which monitor it is looking at.
    let mut p = blank();
    seeding(&mut p);
    let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
        return;
    };
    h.step_graph(&p);

    let seed = h.spot_uv();
    let (u, v) = tile(MONITORS, 2, seed[0], seed[1]);
    let found = h.read().brightest_uv();
    assert!(
        (found[0] - u).abs() < 0.02 && (found[1] - v).abs() < 0.02,
        "tiled: the seed is at {found:?}, not [{u}, {v}]"
    );

    // The same graph on a target the shape of one monitor, so a solo fills
    // it edge to edge and the seed is where the monitor puts it.
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.step_solo(&p, 2);
    let found = h.read().brightest_uv();
    assert!(
        (found[0] - seed[0]).abs() < 0.02 && (found[1] - seed[1]).abs() < 0.02,
        "soloed: the seed is at {found:?}, not {seed:?}"
    );

    // And the solo shows the monitor it names rather than the lit one it
    // was tiled beside: a dark monitor soloed is a dark target.
    h.present(Some(0));
    let peak = h.read().brightest_in(0.0, 0.0, 1.0, 1.0);
    assert!(peak < 10.0, "a soloed dark monitor shows {peak}");
}

#[test]
fn the_instrument_settles_without_clipping() {
    // Same bar the single default is held to, per monitor: left running,
    // every monitor of the instrument keeps an image — not flat white, not
    // black.
    {
        let name = "instrument";
        let p = lightherder::config::instrument();
        let n = p.monitors.len();
        let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
            return;
        };
        feed_seed(&mut h, &p);
        for _ in 0..399 {
            h.feedback.step(h.device, h.queue, &p);
        }
        h.step_graph(&p);
        let img = h.read();
        for m in 0..n {
            let (u0, v0) = tile(n, m, 0.0, 0.0);
            let (u1, v1) = tile(n, m, 1.0, 1.0);
            let peak = img.brightest_in(u0, v0, u1, v1);
            assert!(peak < 250.0, "{name} monitor {m} settles clipped: {peak}");
            assert!(peak > 30.0, "{name} monitor {m} goes dark: {peak}");
            // Per channel too, and not because a channel dying is unlikely:
            // black is an absorbing state under the front panel's floor, so
            // a chroma stage that pulls one channel under zero once has
            // ended it for good — and the mean above would still read 111.
            for channel in 0..3 {
                let peak = img.brightest_channel_in(channel, u0, v0, u1, v1);
                assert!(
                    peak > 10.0,
                    "{name} monitor {m} loses channel {channel}: {peak}"
                );
            }
        }
    }
}

// ---- Analog character: the signal path, and the amplifier's rails --------

/// A lit spot and nothing moving: the frame the tests that measure a picture
/// start from — the seed whole on the monitor, and no loop to move it.
fn still_spot(h: &mut Harness) -> Image {
    h.step(&Single {
        seed: 1.0,
        loop_gain: [0.0; 3],
        ..frozen(Single::default())
    });
    h.read()
}

#[test]
fn an_overdriven_loop_settles_on_the_rail_instead_of_running_away() {
    // The rail's whole job, read where an eight-bit present can see it. Its
    // knee is at half of twice display white, so the bend itself happens
    // above anything the glass shows — but what the bank holds comes back
    // round, so driving the loop far past white and then turning the gain
    // right down brings the stored level into view. On the rail it is 2.0
    // however hard it was driven; without one it is whatever the gain
    // compounded to, which is off the top of the scale.
    let Some(mut h) = square() else { return };
    let at = h.spot_uv();
    let lit = Single {
        seed: 1.0,
        loop_gain: [0.0; 3],
        ..frozen(Single::default())
    };
    h.step(&lit);
    let seeded = h.read().at(at[0], at[1]);
    assert!(seeded > 240.0, "the spot must start near white: {seeded}");

    const GAIN: f32 = 1.8;
    let overdriven = Single {
        seed: 0.0,
        loop_gain: [GAIN; 3],
        ..lit
    };
    for _ in 0..20 {
        h.step(&overdriven);
    }
    // A twentieth of what the bank holds, which is the rail's fixed point if
    // there is a rail and 1.8^20 of white if there is not.
    let readout = 0.05;
    h.step(&Single {
        seed: 0.0,
        loop_gain: [readout; 3],
        ..lit
    });
    let peak = h.read().at(at[0], at[1]);
    // Where the rail settles, not where it tops out: the arm above the knee
    // is `h - h^2/4a`, an asymptote and not a clamp, so a loop of gain `g`
    // comes to rest at the fixed point of `x = h - h^2/4gx`, which is
    // `(h/2)(1 + sqrt(1 - 1/g))` — under `h` for any finite gain. Predicting
    // `h` itself would pass a hard clamp too, and that is a different curve.
    let rail = lightherder::feedback::HEADROOM;
    let settled = 0.5 * rail * (1.0 + (1.0 - 1.0 / GAIN).sqrt());
    let want = 255.0 * settled * readout;
    assert!(
        (peak - want).abs() < 1.5,
        "the bank held {peak} of the rail's {want}: a loop that ran away reads 255, \
         one the rail clamped rather than bent reads over, and one it cut short reads under"
    );
    // And the dark room around it is still dark: a runaway that had become a
    // NaN would have carried the whole monitor with it.
    let corner = h.read().at(0.02, 0.02);
    assert!(corner < 2.0, "the dark went with it: {corner}");
}

// ---- External inputs: what the switcher has that the graph did not make --

/// Opens a graph's own inputs and puts a frame of each on its layer. The
/// shipped patterns are still, so one delivery is the whole of it; a moving
/// source would want this every step, as the app does.
fn feed_seed(h: &mut Harness, params: &Params) {
    {
        let frame = match &params.input.source {
            // A capture device is real hardware this suite cannot demand —
            // the webcam preset names /dev/video0. Its layer gets a
            // stand-in of the scene such a preset expects, a bright subject
            // on a dark backdrop, written deliberately here rather than
            // decoded; the capture path itself is input.rs's to test.
            Input::Capture { .. } => {
                quartered_frame(h.feedback.size(), [[200; 3], [30; 3], [200; 3], [30; 3]])
            }
            _ => {
                let mut source =
                    pollster::block_on(Source::open(&params.input.source, h.feedback.size()))
                        .unwrap_or_else(|e| panic!("the seed: {e}"));
                source
                    .frame()
                    .expect("open() waits for the first frame")
                    .to_vec()
            }
        };
        h.feedback.write_seed(h.queue, &frame);
    }
}

/// A soft white spot as one tightly packed RGBA8 frame: a gaussian of
/// [`SPOT_RADIUS`] at [`SPOT`], round on screen whatever the monitor's shape.
fn spot_frame(size: (u32, u32)) -> Vec<u8> {
    let aspect = size.0 as f32 / size.1 as f32;
    let centre = lightherder::affine::screen_to_uv(aspect).apply(SPOT);
    let radii = [SPOT_RADIUS / aspect, SPOT_RADIUS];
    let mut pixels = vec![0u8; (size.0 * size.1 * 4) as usize];
    for y in 0..size.1 {
        for x in 0..size.0 {
            let uv = [
                (x as f32 + 0.5) / size.0 as f32,
                (y as f32 + 0.5) / size.1 as f32,
            ];
            let d = ((uv[0] - centre[0]) / radii[0]).hypot((uv[1] - centre[1]) / radii[1]);
            let level = ((-d * d).exp() * 255.0).round() as u8;
            let i = ((y * size.0 + x) * 4) as usize;
            pixels[i..i + 3].fill(level);
            pixels[i + 3] = 255;
        }
    }
    pixels
}

/// A flat colour as one tightly packed RGBA8 frame.
fn flat_frame(size: (u32, u32), rgb: [u8; 3]) -> Vec<u8> {
    quartered_frame(size, [rgb; 4])
}

/// One frame in four flat quarters, clockwise from the top left. The frame a
/// test uses when it cares which way up the layer ended: a flat colour is
/// invariant under every flip and transpose an upload can get wrong.
fn quartered_frame(size: (u32, u32), quarters: [[u8; 3]; 4]) -> Vec<u8> {
    let mut pixels = vec![255u8; lightherder::input::frame_bytes(size)];
    for y in 0..size.1 {
        for x in 0..size.0 {
            let quarter = match (x * 2 / size.0, y * 2 / size.1) {
                (0, 0) => 0,
                (_, 0) => 1,
                (0, _) => 3,
                (_, _) => 2,
            };
            let i = ((y * size.0 + x) * 4) as usize;
            pixels[i..i + 3].copy_from_slice(&quarters[quarter]);
        }
    }
    pixels
}

/// The rig with the whole seed on structure B's upper monitor and nothing
/// else anywhere: the switcher chain wide open at In2 all the way down, so
/// what monitor 3 shows every pass is the seed and only the seed. Every
/// camera is blind, so nothing goes round.
fn seed_on_a_monitor() -> Params {
    let mut p = blank();
    p.rig.selects[2] = Select::Program;
    p.rig.switchers = [0.0, 1.0, 1.0, 1.0];
    p.input = bars(Key::OFF);
    p
}

/// The monitor [`seed_on_a_monitor`] lights.
const SEEDED: usize = 2;

/// The switcher chain set to hand the whole seed to monitor 3, which is the
/// one monitor it can reach: on this rig the seed enters at switcher D and
/// climbs into structure B, and structure A only ever sees light that has
/// already been round B's loop.
fn seeding(p: &mut Params) {
    p.rig.selects[SEEDED] = Select::Program;
    p.rig.switchers = [0.0, 1.0, 1.0, 1.0];
}

/// And the chain shut again, so the loop runs on what it was given.
fn seeded_no_more(p: &mut Params) {
    p.rig.selects[SEEDED] = Select::Direct;
    p.rig.switchers = [0.0; 4];
}

#[test]
fn the_seed_shows_on_the_monitor_the_switcher_sends_it_to() {
    let p = seed_on_a_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    // Four different quarters, so this is the one test that would notice an
    // upload that arrived flipped or transposed — and not grey, so a channel
    // swap between the CPU frame, the half-float conversion and the layer
    // cannot pass either.
    let quarters = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    h.feedback
        .write_seed(h.queue, &quartered_frame((SIZE, SIZE), quarters));
    h.step_solo(&p, SEEDED);

    let img = h.read();
    for (quarter, (u, v)) in [(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)]
        .into_iter()
        .enumerate()
    {
        let seen = img.rgb_at(u, v);
        let want = quarters[quarter].map(f32::from);
        assert!(
            seen.iter().zip(want).all(|(a, b)| (a - b).abs() < 3.0),
            "quarter {quarter} at ({u}, {v}): {seen:?}, wanted {want:?}"
        );
    }
}

#[test]
fn the_seed_layer_is_current_however_the_ring_turns() {
    // The seed's layer is written once and never rendered into, so a frame
    // that reached only the view of one pass would show up on some frames
    // and be black on the rest. Six steps is three turns of a two-slab ring.
    let p = seed_on_a_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), [200; 3]));
    for step in 0..6 {
        h.step_solo(&p, SEEDED);
        let seen = h.read().at(0.5, 0.5);
        assert!((seen - 200.0).abs() < 3.0, "step {step}: {seen}");
    }
}

#[test]
fn the_seed_layer_sits_past_the_whole_ring() {
    // The arithmetic that puts the seed past every slab of every monitor: a
    // layer index short by one lands it on a monitor of the newest slab, and
    // a deep ring is where that shows.
    let mut p = seed_on_a_monitor();
    p.delay = 3;
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), [200, 0, 0]));
    for step in 0..8 {
        h.step_solo(&p, SEEDED);
        let seen = h.read().rgb_at(0.5, 0.5);
        assert!(seen[0] > 190.0 && seen[2] < 5.0, "step {step}: {seen:?}");
    }
}

#[test]
fn blanking_the_monitors_leaves_the_seed_alone() {
    // Blank is "restart the loops", not "unplug the video player" — and a
    // still pattern that got blanked would never come back, because it is
    // uploaded once.
    let p = seed_on_a_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), [200; 3]));
    h.feedback.clear(h.device, h.queue);
    h.step_solo(&p, SEEDED);
    let seen = h.read().at(0.5, 0.5);
    assert!(
        (seen - 200.0).abs() < 3.0,
        "the input was blanked too: {seen}"
    );
}

#[test]
fn the_switcher_mixes_the_seed_with_a_camera_on_one_monitor() {
    // The point of the seed being a source on the mix side: one column of
    // the switcher sums outside light and a camera's, and the monitor cannot
    // tell them apart. Switcher D three quarters of the way toward the seed
    // and the chain above it wide open, so monitor 3 shows three quarters
    // seed and a quarter of camera 3 — which is watching that same monitor.
    let mut p = seed_on_a_monitor();
    p.rig.switchers = [0.0, 1.0, 1.0, 0.75];
    p.cameras[2].look = one_hot(SEEDED);
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), [255; 3]));

    // Nothing on the monitor yet, so this is three quarters of the seed.
    h.step_solo(&p, SEEDED);
    let split = h.read().at(0.5, 0.5);
    assert!((split - 191.0).abs() < 4.0, "0.75 of white is {split}");

    // Now the monitor holds that, and the camera brings a quarter of it back.
    h.step_solo(&p, SEEDED);
    let both = h.read().at(0.5, 0.5);
    assert!((both - 239.0).abs() < 6.0, "0.75 + 0.25 x 0.75 is {both}");
}

#[test]
fn the_seed_arrives_whole_however_the_cameras_are_set() {
    // There is no camera between the switcher and an input, so nothing a
    // camera does can reach one. The camera here is loaded with every stage
    // that could leak — pulled back by two, scattering, smearing, graining,
    // keyed above the picture and down to a tenth of the light — and *is* in
    // the monitor's pass, routed at a weight low enough to leave the input's
    // levels alone on the first step, so this is a graph where the stages
    // are live rather than one where they were filtered out. The input must
    // arrive framed edge to edge, at full level, quarters still meeting at a
    // hard edge.
    let mut p = seed_on_a_monitor();
    // A fiftieth of camera 3 in the same pass, which is where every stage a
    // camera has is live.
    const SEED_SHARE: f32 = 0.98;
    p.rig.switchers = [0.0, 1.0, 1.0, SEED_SHARE];
    p.cameras[2].look = one_hot(SEEDED);
    p.shafts[0].zoom = 0.5;
    p.cameras[2].gain = [0.1; 3];
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    let quarters = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    h.feedback
        .write_seed(h.queue, &quartered_frame((SIZE, SIZE), quarters));
    h.step_solo(&p, SEEDED);

    let img = h.read();
    // Corners a half zoom would have left unlit, and either side of the
    // quarters' seam — a hard edge is what no halo, smear or key survives.
    for (quarter, (u, v)) in [(0.05, 0.05), (0.95, 0.05), (0.95, 0.95), (0.05, 0.95)]
        .into_iter()
        .enumerate()
        .chain([(0, (0.47, 0.25)), (1, (0.53, 0.25))])
    {
        let seen = img.rgb_at(u, v);
        let want = quarters[quarter].map(|c| f32::from(c) * SEED_SHARE);
        assert!(
            seen.iter().zip(want).all(|(a, b)| (a - b).abs() < 3.0),
            "({u}, {v}): {seen:?}, wanted {want:?}"
        );
    }
}

// ---- The keyer: what a camera's path refuses to hand on ------------------

/// The seed on its monitor through a key: the switcher is where this rig
/// keys, so this is the whole of the keyer. The picture is a still upload
/// rather than a loop, so what the monitor holds after one step is exactly
/// one pass of the key on the frame written.
fn keyed_seed(key: Key) -> Params {
    let mut p = seed_on_a_monitor();
    p.input.key = key;
    p
}

#[test]
fn the_luma_key_cuts_the_dark_passes_the_bright_and_blends_the_edge() {
    // Quarters below the key's band, inside it, and above it: the dark
    // vanishes, the bright arrives intact, and the middle lands part-way up
    // — the soft edge asserted as an effect on the light, not as a shader
    // detail. The key passes at 0.5 and has finished cutting one softness
    // down at 0.3; the quarters' lumas are 0.16, 0.39 and 0.86.
    let p = keyed_seed(Key {
        threshold: 0.5,
        softness: 0.2,
    });
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback.write_seed(
        h.queue,
        &quartered_frame((SIZE, SIZE), [[40; 3], [100; 3], [220; 3], [220; 3]]),
    );
    h.step_solo(&p, SEEDED);

    let img = h.read();
    let keyed = |u, v| img.at(u, v);
    let below = keyed(0.25, 0.25);
    assert!(below < 3.0, "below the key, {below} survives");
    let above = keyed(0.75, 0.75);
    assert!((above - 220.0).abs() < 4.0, "above the key: {above}");
    // Inside the band: attenuated but alive, which neither a hard cut at
    // the threshold nor no key at all can produce.
    let edge = keyed(0.75, 0.25);
    assert!(
        edge > 25.0 && edge < 70.0,
        "the soft edge should blend 100 part-way down, not {edge}"
    );
}

/// The pixels a file holds, decoded by the same ffmpeg that wrote it — the
/// one oracle here that does not ask the capture what it captured.
fn decoded(path: &std::path::Path, size: (u32, u32)) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .expect("ffmpeg to read a capture back");
    assert!(
        out.status.success(),
        "{}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        (out.stdout.len() as u32).is_multiple_of(size.0 * size.1 * 4),
        "{}: {} bytes is not whole frames of {size:?}",
        path.display(),
        out.stdout.len()
    );
    out.stdout
}

/// The brightest channel anywhere in a decoded capture, 0..=255.
fn peak(pixels: &[u8]) -> u8 {
    pixels
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .max()
        .expect("a non-empty capture")
}

#[test]
fn a_capture_writes_the_lit_picture_to_a_file() {
    let Some(mut h) = harness((SIZE, SIZE), (SIZE, SIZE)) else {
        return;
    };
    // Light on the glass first, and coloured light: a working capture and a
    // black one have to be different files, and a red one and a blue one
    // have to be different files too.
    h.step(&Single {
        seed: 1.0,
        loop_gain: [0.0; 3],
        ..frozen(Single::default())
    });
    let tinting = graph(&Single {
        seed: 0.0,
        loop_gain: TINT,
        ..frozen(Single::default())
    });
    h.step_solo(&tinting, SEEDED);

    // 300 rather than a round 320: four bytes a texel is 1200 to the row,
    // which a texture-to-buffer copy pads out to 1280 — so the packing this
    // capture does is on the path rather than skipped by a width that
    // happened to be aligned already.
    let size = (300, 180);
    let dir = std::env::temp_dir().join(format!("lightherder-capture-{}", std::process::id()));
    let still = still_at(&h, &dir, size, TARGET_FORMAT);
    assert_eq!(still.extension().and_then(|e| e.to_str()), Some("png"));
    let pixels = decoded(&still, size);
    assert_eq!(pixels.len() as u32, size.0 * size.1 * 4, "one frame");
    assert!(peak(&pixels) > 32, "the still is black");

    // The same picture through the other byte order a display comes in —
    // and the deployed instrument's surface is the Bgra one. Decoded they
    // must be the same picture: a capture that names its byte order wrongly
    // swaps red and blue, which every oracle that asks only how bright a
    // frame is passes.
    let swapped = still_at(&h, &dir, size, wgpu::TextureFormat::Bgra8Unorm);
    let coloured = pixels.chunks_exact(4).any(|p| p[0].abs_diff(p[2]) > 16);
    assert!(coloured, "a grey picture cannot tell red from blue");
    assert_eq!(
        decoded(&swapped, size),
        pixels,
        "red and blue changed places"
    );

    // And the recording: frames fall due on the wall clock rather than on
    // calls, so it is as long as the hand was on the button however often
    // the display asked for one. Slower than the capture's own rate on
    // purpose — a display handing out fewer frames than that must duplicate
    // rather than write a shorter file.
    let mut video = Capture::video(h.device, &dir, size, TARGET_FORMAT).expect("ffmpeg");
    let started = std::time::Instant::now();
    let mut last = started;
    while started.elapsed() < std::time::Duration::from_millis(400) {
        video
            .frame(
                h.device,
                h.queue,
                &h.present,
                &h.feedback,
                View::Bank { focus: None },
                None,
            )
            .expect("a frame down the pipe");
        last = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // To the last frame asked for rather than to the release: nothing writes
    // the span after it, and a display asked this rarely leaves a tenth of a
    // second of one.
    let held = last.duration_since(started).as_secs_f64();
    let recording = video.finish().expect("a recording");
    let pixels = decoded(&recording, size);
    let frames = pixels.len() as f64 / f64::from(size.0 * size.1 * 4);
    assert!(
        (frames - held * 30.0).abs() <= 2.0,
        "{frames} frames for {held:.3}s held"
    );
    assert!(peak(&pixels) > 32, "the recording is black");
    // What the file says it is, which is a different fact from what it
    // holds: a file written at the wrong declared rate plays back at the
    // wrong speed with every frame present and correct.
    let played: f64 = probed(&recording, "format=duration")
        .parse()
        .expect("a duration");
    assert!(
        (played - held).abs() <= 0.2,
        "{played}s of video for {held:.3}s held"
    );

    // A capture nothing was written to is not a capture, and leaves nothing
    // behind — the file ffmpeg opened for it included.
    let left = std::fs::read_dir(&dir)
        .expect("the capture directory")
        .count();
    let empty = Capture::video(h.device, &dir, size, TARGET_FORMAT).expect("ffmpeg");
    assert!(empty.finish().is_err(), "an empty capture passed for one");
    assert_eq!(
        std::fs::read_dir(&dir)
            .expect("the capture directory")
            .count(),
        left,
        "an empty capture left a file behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// One still of whatever the harness has on its monitors, in `format` — the
/// present pass is built for the target it draws to, so a second byte order
/// is a second pipeline and not a second capture path.
fn still_at(
    h: &Harness,
    dir: &std::path::Path,
    size: (u32, u32),
    format: wgpu::TextureFormat,
) -> std::path::PathBuf {
    let present = Present::new(h.device, &h.feedback, format);
    let mut capture =
        Capture::still(h.device, dir, size, format).expect("ffmpeg, and somewhere to write");
    capture
        .frame(
            h.device,
            h.queue,
            &present,
            &h.feedback,
            View::Bank { focus: None },
            None,
        )
        .expect("a frame down the pipe");
    capture.finish().expect("a still")
}

/// One field of what ffprobe says about a file.
fn probed(path: &std::path::Path, entry: &str) -> String {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entry])
        .args(["-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()
        .expect("ffprobe to read a capture");
    assert!(
        out.status.success(),
        "{}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_delayed_camera_hands_on_the_frame_it_saw_that_many_passes_ago() {
    // A one-pass flash on monitor 3, and a camera on it drawing to monitor 1
    // with `delay` frames on its cable: monitor 1 lights on pass delay + 1
    // and on no other, and the frame it shows then is byte for byte the one
    // an undelayed camera shows on pass 1 — a delay moves when a frame
    // arrives, never what arrives. The camera zooms and dims so that a frame
    // held for the delay and a frame sent round the camera that many more
    // times come out different. The reach is the full thirty frames so that
    // every ring is the deepest one: the short delays then read from its
    // middle, and the longest from the slab just after the one being
    // written.
    // Camera A carries the flash from structure B's monitor to its own.
    let flash = |delay: u32| {
        let mut p = blank();
        p.cameras[0].delay = delay;
        p.cameras[0].gain = [0.9; 3];
        p.shafts[0].zoom = 0.9;
        p.cameras[0].look = one_hot(SEEDED);
        p.delay = Params::MAX_DELAY;
        seeding(&mut p);
        p
    };
    let mut undelayed: Option<Vec<u8>> = None;
    for delay in [0, 1, 2, Params::MAX_DELAY] {
        let mut p = flash(delay);
        let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
            return;
        };
        let (u0, v0) = tile(MONITORS, 0, 0.0, 0.0);
        let (u1, v1) = tile(MONITORS, 0, 1.0, 1.0);
        for pass in 0..=delay + 2 {
            h.step_graph(&p);
            seeded_no_more(&mut p);
            let img = h.read();
            let lit = img.brightest_in(u0, v0, u1, v1);
            if pass == delay + 1 {
                assert!(
                    lit > 200.0,
                    "delay {delay}: the flash never arrived on pass {pass}: {lit}"
                );
                match &undelayed {
                    None => undelayed = Some(img.pixels),
                    Some(reference) => assert!(
                        &img.pixels == reference,
                        "delay {delay}: the delayed frame is not the undelayed one"
                    ),
                }
            } else {
                assert!(
                    lit < 2.0,
                    "delay {delay}: monitor 1 lit on pass {pass}: {lit}"
                );
            }
        }
    }
}

#[test]
fn the_seed_lands_past_the_whole_ring() {
    // On an undelayed graph the ring is one slab and the seed's layer is
    // where it always was, so only a delayed graph can tell the layer the
    // seed is written to from the one its tap reads. The cameras are blind
    // and the reach is there only to deepen the ring.
    let mut p = seed_on_a_monitor();
    p.cameras[0].delay = 5;
    p.delay = 5;
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), [200; 3]));
    // A whole turn of the ring, so the pass that draws the last slab — the
    // one whose upper view is the seed layer alone — is among them.
    for pass in 0..p.history() {
        h.step_solo(&p, SEEDED);
        let shown = h.read().rgb_at(0.5, 0.5);
        assert!(
            shown.iter().all(|c| (c - 200.0).abs() <= 1.0),
            "pass {pass}: the monitor shows {shown:?}, not the seed's flat 200"
        );
    }
}

#[test]
fn blanking_the_monitors_empties_the_whole_ring() {
    // A flash in flight down a delayed cable is in the ring and nowhere
    // else, so a blank that left any slab alone would deliver it late: after
    // the blank, monitor 1 stays dark for longer than the delay.
    // The flash is the seed on monitor 3; camera A carries it across.
    let delay = 4;
    let mut p = blank();
    p.cameras[0].delay = delay;
    p.cameras[0].look = one_hot(SEEDED);
    p.delay = delay;
    seeding(&mut p);
    let Some(mut h) = graph_harness((SIZE, SIZE), tiled(), &p) else {
        return;
    };
    let (u0, v0) = tile(MONITORS, 0, 0.0, 0.0);
    let (u1, v1) = tile(MONITORS, 0, 1.0, 1.0);
    // First the cable delivers, or the dark below proves nothing.
    h.step_graph(&p);
    seeded_no_more(&mut p);
    for _ in 0..delay {
        h.step_graph(&p);
    }
    h.step_graph(&p);
    let lit = h.read().brightest_in(u0, v0, u1, v1);
    assert!(lit > 200.0, "the flash never arrived: {lit}");
    // Two flashes, two passes apart, so that both the slab the blank sees as
    // newest and one further back hold light.
    seeding(&mut p);
    h.step_graph(&p);
    seeded_no_more(&mut p);
    h.step_graph(&p);
    seeding(&mut p);
    h.step_graph(&p);
    seeded_no_more(&mut p);
    h.feedback.clear(h.device, h.queue);
    for pass in 0..=delay + 2 {
        h.step_graph(&p);
        let lit = h.read().brightest_in(u0, v0, u1, v1);
        assert!(
            lit < 2.0,
            "a flash survived the blank: pass {pass} lit {lit}"
        );
    }
}

/// One more pass with the loop passing light straight through, a clean
/// camera and the monitor's sharpness set, so the mask is the only thing
/// between frames.
fn resharpen(h: &mut Harness, sharpness: f32) -> Image {
    let mut params = graph(&Single {
        seed: 0.0,
        loop_gain: [1.0; 3],
        ..frozen(Single::default())
    });
    params.monitors[SEEDED].sharpness = sharpness;
    h.step_solo(&params, SEEDED);
    h.read()
}

/// The steepest step between neighbouring texels on the line through
/// `centre`, across it or down it.
fn steepest(image: &Image, centre: [f32; 2], horizontal: bool) -> f32 {
    let at = |i: u32| {
        let moved = (i as f32 + 0.5) / SIZE as f32;
        if horizontal {
            image.at(moved, centre[1])
        } else {
            image.at(centre[0], moved)
        }
    };
    (1..SIZE)
        .map(|i| (at(i) - at(i - 1)).abs())
        .fold(0.0, f32::max)
}

#[test]
fn sharpness_is_exact_at_rest_and_steepens_the_seeds_rim_when_turned_up() {
    let Some(mut h) = square() else { return };
    let seed = h.spot_uv();
    let before = still_spot(&mut h);
    assert!(before.at(seed[0], seed[1]) > 200.0, "nothing to sharpen");
    // Rest is the stage skipped, so a pass through it is a pass through
    // nothing: not close to the frame before, the same bytes.
    let at_rest = resharpen(&mut h, 0.0);
    assert_eq!(at_rest.pixels, before.pixels, "sharpness 0 moved a texel");
    // An unsharp mask puts back detail, and on a soft-edged spot detail is
    // a steeper rim.
    let sharpened = resharpen(&mut h, 2.0);
    for horizontal in [true, false] {
        let (was, is) = (
            steepest(&at_rest, seed, horizontal),
            steepest(&sharpened, seed, horizontal),
        );
        assert!(
            is > was + 4.0,
            "{} rim went {was} -> {is} per texel",
            if horizontal { "horizontal" } else { "vertical" }
        );
    }
}

/// One pass of a graph whose only light is the seed, at a sharpness.
fn sharpened_seed(h: &mut Harness, p: &Params, sharpness: f32) -> Image {
    let mut p = p.clone();
    p.monitors[SEEDED].sharpness = sharpness;
    h.step_solo(&p, SEEDED);
    h.read()
}

#[test]
fn sharpness_steepens_a_step_both_ways_and_reaches_one_texel() {
    let p = seed_on_a_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    // Two greys in a checker of quarters: a step in x and a step in y,
    // neither at a rail, so the mask has room to overshoot both sides of
    // each — a step from black to white is already as steep as eight bits
    // can say.
    let (light, dark) = ([128; 3], [64; 3]);
    h.feedback.write_seed(
        h.queue,
        &quartered_frame((SIZE, SIZE), [light, dark, light, dark]),
    );
    let sharpness = 2.0;
    let rest = sharpened_seed(&mut h, &p, 0.0);
    let sharp = sharpened_seed(&mut h, &p, sharpness);
    // Probed a quarter of the way in, off the other step: on the centre
    // lines a mask with one arm missing still steepens both ways, since
    // every texel there straddles both steps. The number is exact — each
    // side of a step moves by a quarter of it per arm that crosses, and
    // one arm does — so a mask at half strength cannot pass either.
    let corner = [0.25, 0.25];
    for horizontal in [true, false] {
        let (was, is) = (
            steepest(&rest, corner, horizontal),
            steepest(&sharp, corner, horizontal),
        );
        let want = was * (1.0 + sharpness / 2.0);
        assert!(
            was > 48.0 && (is - want).abs() <= 1.0,
            "{} step went {was} -> {is}, wanted {want}",
            if horizontal { "horizontal" } else { "vertical" }
        );
    }
    // A mask a texel wide reaches a texel: away from the two steps every
    // texel's four arms read what it reads, and the mask adds exactly
    // nothing — the frame's own border included, where an arm past the edge
    // reads the centre again rather than the dark room beyond.
    let edge = SIZE / 2;
    let near_a_step = |i: u32| i + 1 == edge || i == edge;
    for y in 0..SIZE {
        for x in 0..SIZE {
            if near_a_step(x) || near_a_step(y) {
                continue;
            }
            let i = ((y * SIZE + x) * 4) as usize;
            assert_eq!(
                sharp.pixels[i..i + 3],
                rest.pixels[i..i + 3],
                "the mask reached ({x}, {y})"
            );
        }
    }
    h.feedback
        .write_seed(h.queue, &flat_frame((SIZE, SIZE), light));
    let rest = sharpened_seed(&mut h, &p, 0.0);
    let sharp = sharpened_seed(&mut h, &p, 2.0);
    assert_eq!(
        sharp.pixels, rest.pixels,
        "the mask found detail in a flat field"
    );
}

#[test]
fn a_slow_router_output_holds_its_frame_and_a_camera_on_it_sees_the_hold() {
    // A one-pass flash on monitor 3 on frame `flash`, with monitor 3's
    // router output at `rate`, and camera A on it drawing to monitor 1 with
    // `delay` frames on its cable. Monitor 3 shows the flash for as long as
    // its output holds that frame, and monitor 1 lights on exactly the
    // passes on which the camera, that many frames late, sees it — the
    // passes `Rate::hold` names, which the lib tests pin to the film
    // cadence. Every lit frame on monitor 1 is byte for byte the full-rate
    // undelayed one: a hold moves when a frame changes, never what it is.
    // The flash lands on frames 0 and 3 so both lengths of the film cadence
    // (three, then two) are crossed, and the delay reaches 3 so a held
    // frame is read from past the ring's newest slabs.
    let flashing = |delay: u32, rate: Rate| {
        let mut p = blank();
        p.cameras[0].delay = delay;
        p.cameras[0].look = one_hot(SEEDED);
        p.monitors[SEEDED].rate = rate;
        p.delay = 3;
        p
    };
    let mut full: Vec<Vec<u8>> = Vec::new();
    for flash in [0u64, 3] {
        for delay in [0u32, 3] {
            for rate in Rate::ALL {
                let mut p = flashing(delay, rate);
                let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
                    return;
                };
                let held = |frame: u64| frame - rate.hold(frame as i64) as u64 == flash;
                let last = flash + delay as u64 + Rate::SLOWEST.longest_hold() as u64 + 3;
                for pass in 0..=last {
                    if pass == flash {
                        seeding(&mut p);
                    } else {
                        seeded_no_more(&mut p);
                    }
                    h.step_graph(&p);
                    h.present(Some(SEEDED));
                    let shows = h.read().brightest() > 200.0;
                    assert_eq!(
                        shows,
                        held(pass),
                        "{rate:?} flash {flash}: monitor 3 on pass {pass}"
                    );
                    h.present(Some(0));
                    let img = h.read();
                    let seen = pass as i64 - 1 - delay as i64;
                    let lit = seen >= 0 && held(seen as u64);
                    assert_eq!(
                        img.brightest() > 200.0,
                        lit,
                        "{rate:?} flash {flash} delay {delay}: monitor 1 on pass {pass}"
                    );
                    if rate == Rate::Full && delay == 0 && flash == 0 {
                        full.push(img.pixels);
                    } else if lit {
                        assert_eq!(
                            img.pixels, full[1],
                            "{rate:?} flash {flash} delay {delay}: pass {pass} is not the full-rate frame"
                        );
                    }
                }
            }
        }
    }
}
