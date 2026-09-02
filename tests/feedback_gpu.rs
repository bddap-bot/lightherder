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
use lightherder::params::{Camera, Character, Colour, Key, Monitor, Params, Seed};
use lightherder::present::Present;

/// The bootstrap stage's one-camera-one-monitor params, kept as this suite's
/// shorthand: most of what it checks — the colour stage, the framing, the
/// seed — needs only the single loop, and reads better without graph
/// plumbing. [`graph`] turns it into the real thing.
#[derive(Clone, Copy)]
struct Single {
    framing: Framing,
    loop_gain: [f32; 3],
    character: Character,
    seed: Seed,
    colour: Colour,
    headroom: f32,
}

impl Default for Single {
    /// The single preset's values, taken from it rather than copied, so the
    /// shorthand cannot drift from the instrument.
    fn default() -> Single {
        let p = lightherder::config::single();
        Single {
            framing: p.cameras[0].framing,
            loop_gain: p.cameras[0].gain,
            character: p.cameras[0].character,
            seed: p.monitors[0].seed,
            colour: p.monitors[0].colour,
            headroom: p.monitors[0].headroom,
        }
    }
}

fn graph(s: &Single) -> Params {
    Params {
        cameras: vec![Camera {
            framing: s.framing,
            gain: s.loop_gain,
            character: s.character,
            key: Key::OFF,
            look: vec![1.0],
            delay: 0,
        }],
        monitors: vec![Monitor {
            colour: s.colour,
            seed: s.seed,
            headroom: s.headroom,
        }],
        inputs: Vec::new(),
        routing: vec![vec![1.0]],
        routing_inputs: Vec::new(),
        delay: 0,
    }
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
        Harness {
            device,
            queue,
            feedback,
            present,
            target,
            readback,
            target_size,
        }
    }

    fn step(&mut self, params: &Single) {
        self.step_graph(&graph(params));
    }

    fn step_graph(&mut self, params: &Params) {
        self.feedback.step(self.device, self.queue, params);
        self.present(None);
    }

    fn present(&self, solo: Option<usize>) {
        self.present.draw(
            self.device,
            self.queue,
            &self.target,
            &self.feedback,
            solo,
            None,
        );
    }

    /// The three channels where the seed lands, which is the one place the
    /// colour tests look.
    fn spot(&self) -> [f32; 3] {
        let seed = self.feedback.blob_uv();
        self.read().rgb_at(seed[0], seed[1])
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

    /// Every channel of every texel added up: the oracle for whether
    /// something moved the light around or made more of it.
    fn total(&self) -> f64 {
        self.pixels
            .chunks_exact(4)
            .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
            .sum()
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
        seed: Seed::WhiteBlob(1.0),
        ..Default::default()
    }
}

/// Params whose camera does not move, so a lit spot stays where it was put.
fn frozen(params: Single) -> Single {
    Single {
        framing: Framing {
            zoom: 1.0,
            rotation: 0.0,
            ..params.framing
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
        seed: Seed::WhiteBlob(0.5),
        loop_gain: [0.0; 3],
        ..still
    });
    h.step(&Single {
        seed: Seed::Dark,
        loop_gain: TINT,
        ..still
    });
    h.spot()
}

/// One more pass with the loop passing light straight through, so the only
/// thing between the previous frame and this one is the colour stage.
fn recolour(h: &mut Harness, colour: Colour) -> [f32; 3] {
    h.step(&Single {
        seed: Seed::Dark,
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
fn gamma_bends_the_response_rather_than_scaling_it() {
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    let after = recolour(
        &mut h,
        Colour {
            gamma: 2.0,
            ..Colour::NEUTRAL
        },
    );
    let (bright, dim) = (after[0] / before[0], after[1] / before[1]);
    assert!(
        bright < 0.7 && dim < 0.35,
        "{before:?} -> {after:?}: nothing was dimmed"
    );
    // A curve costs the dim level proportionally more than the bright one.
    // Any single multiply would leave the two ratios equal.
    assert!(
        bright > 2.0 * dim,
        "ratios {bright} and {dim} are too close to be a curve"
    );
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
fn the_phosphor_curve_comes_last() {
    // Same argument one stage along: the curve bends what the amplifier
    // produced, not the other way round.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);

    let (brightness, contrast, gamma) = (0.2, 2.0, 2.0);
    let after = recolour(
        &mut h,
        Colour {
            brightness,
            contrast,
            gamma,
            ..Colour::NEUTRAL
        },
    );
    let amplify = |v: f32| (v - 127.5) * contrast + 127.5 + brightness * 255.0;
    let curve = |v: f32| 255.0 * (v / 255.0).powf(gamma);
    assert!(
        (after[0] - curve(amplify(before[0]))).abs() < 6.0,
        "{:?} -> {:?}: red belongs at {}, and curving first would put it at {}",
        before,
        after,
        curve(amplify(before[0])),
        amplify(curve(before[0]))
    );
}

#[test]
fn the_knobs_colour_the_seed_too() {
    // The front panel is on the monitor, not on the camera, so it acts on
    // everything the monitor displays. With the loop dark the seed is the
    // only thing on it, and the curve has to reach it there.
    let Some(mut h) = square() else { return };
    let dark_loop = Single {
        seed: Seed::WhiteBlob(0.5),
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    };
    h.step(&dark_loop);
    let plain = h.spot();

    h.step(&Single {
        colour: Colour {
            gamma: 2.0,
            ..Colour::NEUTRAL
        },
        ..dark_loop
    });
    let curved = h.spot();
    let expected = plain[0] * plain[0] / 255.0;
    assert!(
        (curved[0] - expected).abs() < 5.0,
        "seed {} -> {}, expected {expected}: the panel did not reach it",
        plain[0],
        curved[0]
    );
}

#[test]
fn a_level_pushed_below_black_comes_back_black() {
    // Contrast carries a dark channel under zero, and the phosphor curve is a
    // pow(), which has no answer for a negative base. Without the floor the
    // pass writes not-a-number into a loop that feeds itself forever.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    assert!(before[2] > 5.0, "blue was already black: {before:?}");

    let after = recolour(
        &mut h,
        Colour {
            contrast: 1.5,
            gamma: 2.0,
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
    let squared = Colour {
        gamma: 2.0,
        ..Colour::NEUTRAL
    };
    let once = recolour(&mut h, squared);
    let twice = recolour(&mut h, squared);
    // Each pass squares the level it was handed, whatever that level was.
    let square = |v: f32| v * v / 255.0;
    assert!(
        (once[0] - square(before[0])).abs() < 5.0,
        "{:?} -> {:?}: expected {}",
        before,
        once,
        square(before[0])
    );
    assert!(
        (twice[0] - square(once[0])).abs() < 5.0,
        "{:?} -> {:?}: expected {}",
        once,
        twice,
        square(once[0])
    );
    assert!(
        twice[0] < once[0] - 20.0,
        "the second pass changed nothing: {once:?} -> {twice:?}"
    );
}

#[test]
fn the_seed_lights_the_spot_it_says_it_does() {
    let Some(mut h) = square() else { return };
    let seed = h.feedback.blob_uv();
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
    let seed = h.feedback.blob_uv();
    h.step(&seeded());
    let mut previous = h.read().at(seed[0], seed[1]);

    let params = Single {
        seed: Seed::Dark,
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
    let seed = h.feedback.blob_uv();
    h.step(&seeded());
    assert!(h.read().at(seed[0], seed[1]) > 200.0);

    let params = Single {
        seed: Seed::Dark,
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    };
    h.step(&params);
    let left = h.read().at(seed[0], seed[1]);
    assert!(left < 2.0, "gain 0 left {left}");
}

#[test]
fn panning_moves_the_image_the_way_the_knobs_say() {
    // Straight, then through each mirror: a flipped path sends the framed
    // picture, seed and pan together, to the other side of the monitor's
    // centre on that axis.
    let flips = [(false, false), (true, false), (false, true), (true, true)];
    let pans = [
        ([0.15f32, 0.0], [0.15f32, 0.0], "right"),
        ([-0.15, 0.0], [-0.15, 0.0], "left"),
        // Screen units are y-up; texture v is not.
        ([0.0, 0.15], [0.0, -0.15], "up"),
        ([0.0, -0.15], [0.0, 0.15], "down"),
    ];
    for ((flip_x, flip_y), (translate, offset, name)) in flips
        .iter()
        .flat_map(|f| pans.iter().map(move |p| (*f, *p)))
    {
        let Some(mut h) = square() else { return };
        let seed = h.feedback.blob_uv();
        h.step(&seeded());

        let params = Single {
            seed: Seed::Dark,
            loop_gain: [1.0; 3],
            colour: Colour::NEUTRAL,
            framing: Framing {
                translate,
                flip_x,
                flip_y,
                ..frozen(seeded()).framing
            },
            ..Default::default()
        };
        h.step(&params);

        // A square monitor, so a screen-unit shift is the same shift in uv,
        // and a mirror is about the same 0.5 on either axis.
        let mirror = |on: bool, x: f32| if on { 1.0 - x } else { x };
        let (u, v) = (
            mirror(flip_x, seed[0] + offset[0]),
            mirror(flip_y, seed[1] + offset[1]),
        );
        let img = h.read();
        assert!(
            img.at(u, v) > img.at(seed[0], seed[1]) && img.at(u, v) > 100.0,
            "pan {name}, flip {flip_x}/{flip_y}: {} where it should have gone vs {} left behind",
            img.at(u, v),
            img.at(seed[0], seed[1]),
        );
    }
}

#[test]
fn pan_is_applied_in_the_frame_the_camera_moves_in() {
    // The camera pans and then magnifies, so a pan is the same distance on
    // screen at any zoom. Composing it the other way round would scale the
    // pan by the zoom too, putting the spot somewhere else entirely.
    let Some(mut h) = square() else { return };
    h.step(&seeded());

    let params = Single {
        seed: Seed::Dark,
        loop_gain: [1.0; 3],
        colour: Colour::NEUTRAL,
        framing: Framing {
            zoom: 2.0,
            translate: [-0.3, 0.0],
            ..Framing::identity()
        },
        ..Default::default()
    };
    h.step(&params);

    // Seed at 0.25 right of centre: magnified to 0.5, panned back to 0.2.
    // Panning inside the zoom would put it at 2 * (0.25 - 0.3) = -0.1.
    let img = h.read();
    assert!(
        img.at(0.7, 0.5) > 100.0 && img.at(0.7, 0.5) > img.at(0.4, 0.5),
        "{} at 0.7 vs {} at 0.4",
        img.at(0.7, 0.5),
        img.at(0.4, 0.5),
    );
}

#[test]
fn the_seed_is_round_on_a_wide_monitor() {
    // The only end-to-end check of the aspect correction: on a 2:1 monitor an
    // uncorrected seed radius would be twice as wide as it is tall.
    let Some(mut h) = harness((SIZE * 4, SIZE * 2), (SIZE * 4, SIZE * 2)) else {
        return;
    };
    let seed = h.feedback.blob_uv();
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
    h.step_graph(&params);

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
    let seed = h.feedback.blob_uv();
    h.step(&seeded());
    let mut previous = h.read().at(seed[0], seed[1]);

    let gain = 0.8;
    let params = Single {
        seed: Seed::Dark,
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
    h.step(&seeded());

    // Walk the spot out to the right edge, so the edge texels are LIT. A
    // clamped sampler would then have something bright to smear, which is
    // what distinguishes "outside reads black" from "outside is whatever the
    // sampler clamped to".
    let still = frozen(seeded());
    let pan = |dx: f32| Single {
        seed: Seed::Dark,
        loop_gain: [1.0; 3],
        colour: Colour::NEUTRAL,
        framing: Framing {
            translate: [dx, 0.0],
            ..still.framing
        },
        ..Default::default()
    };
    h.step(&pan(0.25));
    let img = h.read();
    assert!(
        img.at(0.99, 0.5) > 50.0,
        "the right edge should be lit before the test means anything: {}",
        img.at(0.99, 0.5)
    );

    // Now pan back left: the right-hand band can only be sourced from beyond
    // the monitor's right edge.
    h.step(&pan(-0.2));
    let img = h.read();
    assert!(
        img.at(0.95, 0.5) < 2.0,
        "outside the monitor read {}, not black",
        img.at(0.95, 0.5)
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
    let seed = h.feedback.blob_uv();
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

fn one_hot(len: usize, hot: usize) -> Vec<f32> {
    let mut look = vec![0.0; len];
    look[hot] = 1.0;
    look
}

fn plain_camera(look: Vec<f32>) -> Camera {
    Camera {
        framing: Framing::identity(),
        gain: [1.0; 3],
        character: Character::CLEAN,
        key: Key::OFF,
        look,
        delay: 0,
    }
}

fn silent_monitor() -> Monitor {
    Monitor {
        colour: Colour::NEUTRAL,
        seed: Seed::Dark,
        headroom: Monitor::KNEE_AT_WHITE,
    }
}

#[test]
fn the_routing_matrix_sends_each_camera_across() {
    // The crossed two-structure wiring, distilled: camera j is aimed straight
    // at monitor j but routed to the other monitor, so a seed lit on monitor
    // 0 must appear on monitor 1 one pass later, and bounce back the pass
    // after — and never sit still where it was.
    let mut p = Params {
        cameras: vec![plain_camera(one_hot(2, 0)), plain_camera(one_hot(2, 1))],
        monitors: vec![
            Monitor {
                seed: Seed::WhiteBlob(1.0),
                ..silent_monitor()
            },
            silent_monitor(),
        ],
        inputs: Vec::new(),
        routing: vec![vec![0.0, 1.0], vec![1.0, 0.0]],
        routing_inputs: Vec::new(),
        delay: 0,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    let seed = h.feedback.blob_uv();
    let at = |img: &Image, m: usize| {
        let (u, v) = tile(2, m, seed[0], seed[1]);
        img.at(u, v)
    };

    h.step_graph(&p);
    let img = h.read();
    assert!(at(&img, 0) > 200.0, "the seed never lit: {}", at(&img, 0));
    assert!(
        at(&img, 1) < 2.0,
        "monitor 1 lit before anything crossed: {}",
        at(&img, 1)
    );

    p.monitors[0].seed = Seed::Dark;
    h.step_graph(&p);
    let img = h.read();
    assert!(
        at(&img, 1) > 200.0,
        "the seed did not cross: {}",
        at(&img, 1)
    );
    assert!(
        at(&img, 0) < 2.0,
        "monitor 0 kept light its routing row does not grant: {}",
        at(&img, 0)
    );

    h.step_graph(&p);
    let img = h.read();
    assert!(
        at(&img, 0) > 200.0,
        "the seed did not cross back: {}",
        at(&img, 0)
    );
    assert!(
        at(&img, 1) < 2.0,
        "or it left a copy behind: {}",
        at(&img, 1)
    );
}

#[test]
fn mix_weights_scale_each_camera_s_contribution() {
    // Two cameras on one monitor, mixed 2:1. Their framings differ — one
    // holds still, one pans the image aside — so the two contributions land
    // in different places and each weight can be read off on its own.
    let Some(mut h) = square() else { return };
    let mut p = Params {
        cameras: vec![
            plain_camera(vec![1.0]),
            Camera {
                framing: Framing {
                    translate: [0.25, 0.0],
                    ..Framing::identity()
                },
                ..plain_camera(vec![1.0])
            },
        ],
        monitors: vec![Monitor {
            seed: Seed::WhiteBlob(1.0),
            ..silent_monitor()
        }],
        inputs: Vec::new(),
        routing: vec![vec![0.0, 0.0]],
        routing_inputs: Vec::new(),
        delay: 0,
    };
    h.step_graph(&p);
    let seed = h.feedback.blob_uv();
    let base = h.read().at(seed[0], seed[1]);
    assert!(base > 200.0, "the seed never lit: {base}");

    p.monitors[0].seed = Seed::Dark;
    p.routing[0] = vec![0.5, 0.25];
    h.step_graph(&p);
    let img = h.read();
    let (held, panned) = (img.at(seed[0], seed[1]), img.at(seed[0] + 0.25, seed[1]));
    assert!(
        (held / base - 0.5).abs() < 0.04,
        "weight 0.5 delivered {held} of {base}"
    );
    assert!(
        (panned / base - 0.25).abs() < 0.04,
        "weight 0.25 delivered {panned} of {base}"
    );
}

#[test]
fn a_beam_splitter_blends_two_monitors_into_one_camera() {
    // One camera looking through 50/50 splitter glass at both monitors,
    // feeding monitor 0. Light monitor 1 alone: half its light arrives on
    // monitor 0, which no routing row could do — the blend happens in front
    // of the lens.
    let mut p = Params {
        cameras: vec![plain_camera(vec![0.5, 0.5])],
        monitors: vec![
            silent_monitor(),
            Monitor {
                seed: Seed::WhiteBlob(1.0),
                ..silent_monitor()
            },
        ],
        inputs: Vec::new(),
        routing: vec![vec![1.0], vec![0.0]],
        routing_inputs: Vec::new(),
        delay: 0,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    let seed = h.feedback.blob_uv();
    h.step_graph(&p);
    let (u, v) = tile(2, 1, seed[0], seed[1]);
    let bright = h.read().at(u, v);
    assert!(bright > 200.0, "the seed never lit: {bright}");

    p.monitors[1].seed = Seed::Dark;
    h.step_graph(&p);
    let img = h.read();
    let (u, v) = tile(2, 0, seed[0], seed[1]);
    assert!(
        (img.at(u, v) - bright / 2.0).abs() < 8.0,
        "the splitter delivered {} of {bright}",
        img.at(u, v)
    );
}

#[test]
fn insanity_mode_composes_every_monitor_from_one_seed() {
    // All-to-all: each of four monitors shows a quarter of every camera, so
    // one seeded monitor puts a quarter of its light on all four — itself
    // included — a pass later.
    let mut p = Params {
        cameras: (0..4).map(|c| plain_camera(one_hot(4, c))).collect(),
        monitors: (0..4)
            .map(|m| Monitor {
                seed: if m == 0 {
                    Seed::WhiteBlob(1.0)
                } else {
                    Seed::Dark
                },
                ..silent_monitor()
            })
            .collect(),
        inputs: Vec::new(),
        routing: vec![vec![0.25; 4]; 4],
        routing_inputs: Vec::new(),
        delay: 0,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE * 2), &p) else {
        return;
    };
    h.step_graph(&p);
    p.monitors[0].seed = Seed::Dark;
    h.step_graph(&p);

    let seed = h.feedback.blob_uv();
    let img = h.read();
    for m in 0..4 {
        let (u, v) = tile(4, m, seed[0], seed[1]);
        assert!(
            (img.at(u, v) - 255.0 / 4.0).abs() < 10.0,
            "monitor {m} shows {}, not a quarter of the seed",
            img.at(u, v)
        );
    }
}

#[test]
fn a_solo_puts_one_monitor_on_the_whole_target() {
    // The tiled bank and a soloed monitor are the same picture at two sizes:
    // light seeded on monitor 2 sits in monitor 2's cell while the bank is
    // tiled and at the same place on the whole target once that monitor is
    // soloed. Nothing is routed anywhere here — each monitor is its own seed
    // and nothing else, which is what lets the read say which monitor it is
    // looking at.
    let p = Params {
        cameras: (0..4).map(|c| plain_camera(one_hot(4, c))).collect(),
        monitors: (0..4)
            .map(|m| Monitor {
                seed: if m == 2 {
                    Seed::WhiteBlob(1.0)
                } else {
                    Seed::Dark
                },
                ..silent_monitor()
            })
            .collect(),
        inputs: Vec::new(),
        routing: vec![vec![0.0; 4]; 4],
        routing_inputs: Vec::new(),
        delay: 0,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE * 2), &p) else {
        return;
    };
    h.step_graph(&p);

    let seed = h.feedback.blob_uv();
    let (u, v) = tile(4, 2, seed[0], seed[1]);
    let found = h.read().brightest_uv();
    assert!(
        (found[0] - u).abs() < 0.02 && (found[1] - v).abs() < 0.02,
        "tiled: the seed is at {found:?}, not [{u}, {v}]"
    );

    h.present(Some(2));
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
fn the_shipped_presets_settle_without_clipping() {
    // Same bar the single default is held to, per monitor: left running,
    // every monitor of every preset keeps an image — not flat white, not
    // black. Off `PRESETS` rather than listed here, so a preset shipped
    // without a line in this test cannot slip past it.
    for (name, build) in lightherder::config::PRESETS {
        let p = build();
        let n = p.monitors.len();
        let (cols, rows) = lightherder::present::grid(n);
        let Some(mut h) = graph_harness((SIZE, SIZE), (cols * SIZE, rows * SIZE), &p) else {
            return;
        };
        feed_inputs(&mut h, &p);
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

#[test]
fn the_single_shim_is_the_single_preset() {
    // The shim copies the preset's values but hardcodes its wiring; this is
    // what notices if `single()` ever rewires and leaves the suite testing a
    // graph the instrument no longer ships.
    assert_eq!(graph(&Single::default()), lightherder::config::single());
}

// ---- Analog character: the signal path, and the amplifier's rails --------

/// One pass with the loop passing light straight through, so the only thing
/// between the previous frame and this one is the path's character. The
/// mirror of [`recolour`], for the other half of the front panel.
fn recharacter(h: &mut Harness, character: Character, headroom: f32) -> Image {
    h.step(&Single {
        seed: Seed::Dark,
        loop_gain: [1.0; 3],
        character,
        headroom,
        ..frozen(seeded())
    });
    h.read()
}

/// A lit spot and nothing moving: the frame every character test starts from.
fn still_spot(h: &mut Harness) -> Image {
    h.step(&Single {
        seed: Seed::WhiteBlob(1.0),
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    });
    h.read()
}

#[test]
fn the_character_stage_is_inert_at_its_defaults() {
    // A clean path and a wide-open rail have to be an exact identity, not
    // nearly one: they run on every pass of a loop that feeds itself, so a
    // residual is not a residual for long. A hundred passes is what tells
    // "exact" apart from "close" — the same reason the colour stage gets a
    // hundred rather than one.
    let Some(mut h) = square() else { return };
    let before = tinted(&mut h);
    assert!(spread(before) > 50.0, "nothing to preserve: {before:?}");

    for _ in 0..100 {
        recharacter(&mut h, Character::CLEAN, Monitor::KNEE_AT_WHITE);
    }
    let after = h.spot();
    for channel in 0..3 {
        assert!(
            (after[channel] - before[channel]).abs() < 1.0,
            "{before:?} walked to {after:?} in a hundred clean passes"
        );
    }
}

#[test]
fn the_lens_spreads_the_light_without_making_any() {
    // Scatter is a redistribution. Anything that adds light instead is a
    // term the loop multiplies, and a few passes later it owns the monitor.
    let Some(mut h) = square() else { return };
    let seed = h.feedback.blob_uv();
    let before = still_spot(&mut h);
    let (peak, width, light) = (
        before.at(seed[0], seed[1]),
        before.half_extent(seed, true),
        before.total(),
    );
    assert!(peak > 200.0, "nothing to spread: {peak}");

    let after = recharacter(
        &mut h,
        Character {
            bloom: 0.8,
            bloom_radius: 0.08,
            ..Character::CLEAN
        },
        Monitor::KNEE_AT_WHITE,
    );
    assert!(
        after.half_extent(seed, true) > width,
        "the spot is no wider: {width} px either way"
    );
    assert!(
        after.half_extent(seed, false) > before.half_extent(seed, false),
        "the ring's vertical arm did nothing"
    );
    assert!(after.at(seed[0], seed[1]) < peak, "the middle did not give");
    // Sampling and 8-bit read-back cost a little at the edges; making light
    // would cost far more than that, in the other direction.
    let ratio = after.total() / light;
    assert!(
        (0.92..=1.02).contains(&ratio),
        "the lens changed the total light by {:.1}%",
        (ratio - 1.0) * 100.0
    );
}

#[test]
fn the_bleed_smears_the_colour_and_leaves_the_luma_where_it_was() {
    // Composite carries chroma on a subcarrier with a fraction of luma's
    // bandwidth: the colour arrives blurred and the detail does not. Luma is
    // preserved exactly wherever no channel is driven under black, which is
    // the whole lit spot — the colour crawl out in the dark is the clipping,
    // and is the artefact rather than a failure of the claim.
    let Some(mut h) = square() else { return };
    let seed = h.feedback.blob_uv();
    let before_spot = tinted(&mut h);
    assert!(spread(before_spot) > 50.0, "no colour to smear");
    let before = h.read();

    // A square monitor, so a screen-unit offset is the same offset in uv.
    const BLEED: f32 = 0.05;
    let after = recharacter(
        &mut h,
        Character {
            chroma_bleed: BLEED,
            ..Character::CLEAN
        },
        Monitor::KNEE_AT_WHITE,
    );

    for step in [-0.03, -0.015, 0.0, 0.015, 0.03] {
        let (u, v) = (seed[0] + step, seed[1]);
        let (was, now) = (before.rgb_at(u, v), after.rgb_at(u, v));
        assert!(
            (luma(now) - luma(was)).abs() < 2.0,
            "luma moved at {step:+}: {was:?} -> {now:?}"
        );
    }

    // And out past the spot's edge, where there was nothing but a trace of
    // colour, there is now the neighbour's.
    let (u, v) = (seed[0] + BLEED, seed[1]);
    assert!(
        spread(after.rgb_at(u, v)) > spread(before.rgb_at(u, v)) + 4.0,
        "colour did not travel: {:?} -> {:?}",
        before.rgb_at(u, v),
        after.rgb_at(u, v)
    );
}

#[test]
fn the_grain_moves_every_frame_and_only_when_it_is_asked_for() {
    // Grain is what keeps a loop that has decayed to black from staying
    // there, so it has to arrive with no light at all — and it has to be
    // different every frame, or it is a fixed pattern the loop will bake in.
    let Some(mut h) = square() else { return };
    let dark = Single {
        seed: Seed::Dark,
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    };
    h.step(&dark);
    let clean = h.read();
    h.step(&dark);
    assert_eq!(
        clean.pixels,
        h.read().pixels,
        "a clean path is not still frame to frame"
    );
    assert_eq!(clean.brightest(), 0.0, "an unlit monitor is not black");

    let noisy = Single {
        character: Character {
            noise: 0.2,
            ..Character::CLEAN
        },
        ..dark
    };
    h.step(&noisy);
    let first = h.read();
    h.step(&noisy);
    let second = h.read();
    // Half the grain is negative and the phosphor floors it, so the peak is
    // the positive half of a 0.2 swing.
    assert!(first.brightest() > 20.0, "no grain: {}", first.brightest());
    assert_ne!(first.pixels, second.pixels, "the grain is a fixed pattern");
}

#[test]
fn the_amplifier_bends_onto_its_headroom_instead_of_clipping() {
    // The rail's whole job is that an overdriven loop compresses into a
    // structure rather than clipping the monitor to flat white. Its curve is
    // checked against the wide-open reading it is derived from, so this
    // measures the shape and not a number someone typed twice.
    let Some(mut h) = square() else { return };
    let seed = h.feedback.blob_uv();
    let drive = |h: &mut Harness, headroom: f32| {
        h.step(&Single {
            seed: Seed::WhiteBlob(1.0),
            loop_gain: [0.0; 3],
            headroom,
            ..frozen(seeded())
        });
        h.read()
    };

    let wide = drive(&mut h, Monitor::KNEE_AT_WHITE);
    let peak = wide.at(seed[0], seed[1]);
    assert!(
        (200.0..255.0).contains(&peak),
        "the seed must be bright and unclipped to say anything: {peak}"
    );
    // A dim point, deliberately under the knee of the rail below.
    let dim = (seed[0] + 0.07, seed[1]);
    let was_dim = wide.at(dim.0, dim.1);
    assert!(
        (10.0..100.0).contains(&was_dim),
        "no shoulder to read: {was_dim}"
    );

    // The arm the shader takes above the knee, `h - h^2/4x`, read at two
    // rails. At h = 1.0 that expression cannot be told from `h - h/4x` or
    // `h - 1/4x` — all three coincide there, so a single reading would pass
    // for two wrong curves. The lower rail is what separates them, which is
    // the difference between guarding the rail's shape and guarding a point.
    let x = peak / 255.0;
    let bends_onto = |img: &Image, rail: f32| {
        let expected = 255.0 * (rail - rail * rail / (4.0 * x));
        let got = img.at(seed[0], seed[1]);
        assert!(
            (got - expected).abs() < 3.0,
            "peak {peak} at rail {rail} should bend to {expected:.1}, got {got}"
        );
    };
    let railed = drive(&mut h, 1.0);
    bends_onto(&railed, 1.0);
    bends_onto(&drive(&mut h, 0.6), 0.6);

    // Below the knee: untouched, which is what makes the rail a rail and not
    // a gain knob wearing one's hat. Read at the higher rail, the one whose
    // knee the dim point is definitely under.
    let now_dim = railed.at(dim.0, dim.1);
    assert!(
        (now_dim - was_dim).abs() < 2.0,
        "the rail reached under its knee: {was_dim} -> {now_dim}"
    );
}

#[test]
fn each_camera_carries_its_own_character() {
    // The reason it hangs on the camera rather than on the instrument: one
    // path in a graph glows while the one beside it stays sharp. Two
    // monitors, each its own loop, differing in nothing but their lens.
    let mut p = Params {
        cameras: vec![plain_camera(one_hot(2, 0)), plain_camera(one_hot(2, 1))],
        monitors: vec![
            Monitor {
                seed: Seed::WhiteBlob(1.0),
                ..silent_monitor()
            },
            Monitor {
                seed: Seed::WhiteBlob(1.0),
                ..silent_monitor()
            },
        ],
        inputs: Vec::new(),
        routing: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
        routing_inputs: Vec::new(),
        delay: 0,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    h.step_graph(&p);
    let lit = h.read();
    let tiles = |img: &Image| {
        (
            img.brightest_in(0.0, 0.0, 0.5, 1.0),
            img.brightest_in(0.5, 0.0, 1.0, 1.0),
        )
    };
    let (left, right) = tiles(&lit);
    assert!((left - right).abs() < 1.0, "the two loops start apart");
    assert!(left > 200.0, "nothing lit: {left}");

    for monitor in &mut p.monitors {
        monitor.seed = Seed::Dark;
    }
    p.cameras[1].character = Character {
        bloom: 0.9,
        bloom_radius: 0.1,
        ..Character::CLEAN
    };
    h.step_graph(&p);
    let (clean, bloomed) = tiles(&h.read());
    assert!(
        (clean - left).abs() < 2.0,
        "the clean path changed too: {left} -> {clean}"
    );
    assert!(
        bloomed < clean - 20.0,
        "the lens on camera 2 did nothing: {clean} vs {bloomed}"
    );
}

#[test]
fn the_halo_is_round_on_a_wide_monitor() {
    // The only end-to-end check of the offsets, which are worked out on the
    // CPU through each tap's affine for exactly this reason. On a 2:1
    // monitor an uncorrected bloom radius is twice as wide as it is tall,
    // and everything else in this suite runs on a square one where that is
    // invisible.
    let Some(mut h) = harness((SIZE * 4, SIZE * 2), (SIZE * 4, SIZE * 2)) else {
        return;
    };
    let seed = h.feedback.blob_uv();
    let before = still_spot(&mut h);
    let (was_across, was_down) = (
        before.half_extent(seed, true),
        before.half_extent(seed, false),
    );

    let after = recharacter(
        &mut h,
        Character {
            bloom: 0.9,
            bloom_radius: 0.08,
            ..Character::CLEAN
        },
        Monitor::KNEE_AT_WHITE,
    );
    let across = after.half_extent(seed, true) - was_across;
    let down = after.half_extent(seed, false) - was_down;
    assert!(across > 2, "the halo did not widen the spot: {across} px");
    assert!(
        across.abs_diff(down) <= 2,
        "halo grew {across} px across and {down} px down"
    );
}

#[test]
fn the_grain_is_monochrome_and_signed() {
    // Two claims the loop cares about. Monochrome, because it is luma noise
    // and the chroma knobs have nothing to do to grey. Signed, because a
    // grain with a mean above zero is a brightness knob nobody asked for,
    // and inside a loop that lifts every pass until the monitor floods.
    let Some(mut h) = square() else { return };
    h.step(&Single {
        seed: Seed::Dark,
        loop_gain: [0.0; 3],
        character: Character {
            noise: 0.2,
            ..Character::CLEAN
        },
        ..frozen(seeded())
    });
    let img = h.read();

    let mut floored = 0usize;
    for pixel in img.pixels.chunks_exact(4) {
        assert_eq!(
            [pixel[0], pixel[1], pixel[2]],
            [pixel[0]; 3],
            "the grain is coloured"
        );
        floored += usize::from(pixel[0] == 0);
    }
    // The phosphor floors the negative half, so about half the texels come
    // back black. An unsigned hash leaves none of them there.
    let share = floored as f32 / (SIZE * SIZE) as f32;
    assert!(
        (0.4..0.6).contains(&share),
        "{:.0}% of the grain landed at black, not about half",
        share * 100.0
    );
}

// ---- External inputs: what the switcher has that the graph did not make --

/// Opens a graph's own inputs and puts a frame of each on its layer. The
/// shipped patterns are still, so one delivery is the whole of it; a moving
/// source would want this every step, as the app does.
fn feed_inputs(h: &mut Harness, params: &Params) {
    for (i, input) in params.inputs.iter().enumerate() {
        let frame = match input {
            // A capture device is real hardware this suite cannot demand —
            // the webcam preset names /dev/video0. Its layer gets a
            // stand-in of the scene such a preset expects, a bright subject
            // on a dark backdrop, written deliberately here rather than
            // decoded; the capture path itself is input.rs's to test.
            Input::Capture { .. } => {
                quartered_frame(h.feedback.size(), [[200; 3], [30; 3], [200; 3], [30; 3]])
            }
            _ => {
                let mut source = Source::open(input, h.feedback.size())
                    .unwrap_or_else(|e| panic!("input {i}: {e}"));
                source
                    .frame()
                    .expect("open() waits for the first frame")
                    .to_vec()
            }
        };
        h.feedback.write_input(h.queue, i, &frame);
    }
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

/// A graph of one monitor whose only light is one input, patched straight
/// onto it at full send. The one camera is routed nowhere, so every step
/// puts the input on the monitor and nothing else does.
fn one_input_on_one_monitor() -> Params {
    Params {
        cameras: vec![plain_camera(vec![1.0])],
        monitors: vec![silent_monitor()],
        inputs: vec![Input::Pattern(Pattern::Bars)],
        routing: vec![vec![0.0]],
        routing_inputs: vec![vec![1.0]],
        delay: 0,
    }
}

#[test]
fn an_input_shows_on_the_monitor_the_switcher_sends_it_to() {
    let p = one_input_on_one_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    // Four different quarters, so this is the one test that would notice an
    // upload that arrived flipped or transposed — and not grey, so a channel
    // swap between the CPU frame, the half-float conversion and the layer
    // cannot pass either.
    let quarters = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    h.feedback
        .write_input(h.queue, 0, &quartered_frame((SIZE, SIZE), quarters));
    h.step_graph(&p);

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
fn an_input_layer_is_current_however_the_ring_turns() {
    // An input layer is written once and never rendered into, so a frame
    // that reached only the view of one pass would show up on some frames
    // and be black on the rest. Six steps is three turns of a two-slab ring.
    let p = one_input_on_one_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_input(h.queue, 0, &flat_frame((SIZE, SIZE), [200; 3]));
    for step in 0..6 {
        h.step_graph(&p);
        let seen = h.read().at(0.5, 0.5);
        assert!((seen - 200.0).abs() < 3.0, "step {step}: {seen}");
    }
}

#[test]
fn each_input_lands_on_its_own_layer() {
    // Two monitors and two inputs, so the arithmetic that puts input i on
    // its layer past the ring has four distinct answers to get wrong instead of
    // the one a single monitor and a single input collapse it to.
    let p = Params {
        cameras: vec![plain_camera(vec![0.0; 2])],
        monitors: vec![silent_monitor(), silent_monitor()],
        inputs: vec![Input::Pattern(Pattern::Bars); 2],
        routing: vec![vec![0.0], vec![0.0]],
        routing_inputs: vec![one_hot(2, 0), one_hot(2, 1)],
        delay: 0,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_input(h.queue, 0, &flat_frame((SIZE, SIZE), [200, 0, 0]));
    h.feedback
        .write_input(h.queue, 1, &flat_frame((SIZE, SIZE), [0, 0, 200]));
    h.step_graph(&p);

    let img = h.read();
    for (m, channel) in [(0, 0), (1, 2)] {
        let (u, v) = tile(2, m, 0.5, 0.5);
        let seen = img.rgb_at(u, v);
        assert!(seen[channel] > 190.0, "monitor {m}: {seen:?}");
        assert!(
            seen[2 - channel] < 5.0,
            "monitor {m} has the other: {seen:?}"
        );
    }
}

#[test]
fn blanking_the_monitors_leaves_the_inputs_alone() {
    // Space is "restart the loops", not "unplug the video player" — and a
    // still pattern that got blanked would never come back, because it is
    // uploaded once.
    let p = one_input_on_one_monitor();
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_input(h.queue, 0, &flat_frame((SIZE, SIZE), [200; 3]));
    h.feedback.clear(h.device, h.queue);
    h.step_graph(&p);
    let seen = h.read().at(0.5, 0.5);
    assert!(
        (seen - 200.0).abs() < 3.0,
        "the input was blanked too: {seen}"
    );
}

#[test]
fn the_switcher_mixes_an_input_with_a_camera_on_one_monitor() {
    // The point of an input being a source on the mix side: one row of the
    // switcher sums outside light and a camera's, and the monitor cannot
    // tell them apart. Unequal weights, so a pair swapped anywhere between
    // the config and the tap cannot come out the same.
    let mut p = one_input_on_one_monitor();
    p.routing = vec![vec![0.25]];
    p.routing_inputs = vec![vec![0.75]];
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_input(h.queue, 0, &flat_frame((SIZE, SIZE), [255; 3]));

    // Nothing on the monitor yet, so this is three quarters of the input.
    h.step_graph(&p);
    let split = h.read().at(0.5, 0.5);
    assert!((split - 191.0).abs() < 4.0, "0.75 of white is {split}");

    // Now the monitor holds that, and the camera brings a quarter of it back.
    h.step_graph(&p);
    let both = h.read().at(0.5, 0.5);
    assert!((both - 239.0).abs() < 6.0, "0.75 + 0.25 x 0.75 is {both}");
}

#[test]
fn an_input_arrives_whole_however_the_cameras_are_set() {
    // There is no camera between the switcher and an input, so nothing a
    // camera does can reach one. The camera here is loaded with every stage
    // that could leak — pulled back by two, scattering, smearing, graining,
    // keyed above the picture and down to a tenth of the light — and *is* in
    // the monitor's pass, routed at a weight low enough to leave the input's
    // levels alone on the first step, so this is a graph where the stages
    // are live rather than one where they were filtered out. The input must
    // arrive framed edge to edge, at full level, quarters still meeting at a
    // hard edge.
    let mut p = one_input_on_one_monitor();
    p.routing = vec![vec![0.02]];
    p.cameras[0].framing.zoom = 0.5;
    p.cameras[0].gain = [0.1; 3];
    p.cameras[0].character = Character {
        bloom: 0.9,
        bloom_radius: 0.1,
        chroma_bleed: 0.2,
        noise: 0.2,
    };
    p.cameras[0].key = Key {
        threshold: 0.9,
        softness: 0.1,
        ..Key::OFF
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    let quarters = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    h.feedback
        .write_input(h.queue, 0, &quartered_frame((SIZE, SIZE), quarters));
    h.step_graph(&p);

    let img = h.read();
    // Corners a half zoom would have left unlit, and either side of the
    // quarters' seam — a hard edge is what no halo, smear or key survives.
    for (quarter, (u, v)) in [(0.05, 0.05), (0.95, 0.05), (0.95, 0.95), (0.05, 0.95)]
        .into_iter()
        .enumerate()
        .chain([(0, (0.47, 0.25)), (1, (0.53, 0.25))])
    {
        let seen = img.rgb_at(u, v);
        let want = quarters[quarter].map(f32::from);
        assert!(
            seen.iter().zip(want).all(|(a, b)| (a - b).abs() < 3.0),
            "({u}, {v}): {seen:?}, wanted {want:?}"
        );
    }
}

// ---- The keyer: what a camera's path refuses to hand on ------------------

/// A picture on one monitor and a keyed camera watching it, handing what
/// survives to a second. Two steps: the switcher puts the input on monitor 0,
/// then the camera carries it through its key onto monitor 1, which is where
/// a key test reads. The picture is a still upload rather than a loop, so the
/// second step's light is exactly one pass of the key on the frame written.
fn keyed_camera_watching_a_picture(key: Key) -> Params {
    Params {
        cameras: vec![Camera {
            key,
            ..plain_camera(one_hot(2, 0))
        }],
        monitors: vec![silent_monitor(), silent_monitor()],
        inputs: vec![Input::Pattern(Pattern::Bars)],
        routing: vec![vec![0.0], vec![1.0]],
        routing_inputs: vec![vec![1.0, 0.0]],
        delay: 0,
    }
}

#[test]
fn the_luma_key_cuts_the_dark_passes_the_bright_and_blends_the_edge() {
    // Quarters below the key's band, inside it, and above it: the dark
    // vanishes, the bright arrives intact, and the middle lands part-way up
    // — the soft edge asserted as an effect on the light, not as a shader
    // detail. The key passes at 0.5 and has finished cutting one softness
    // down at 0.3; the quarters' lumas are 0.16, 0.39 and 0.86.
    let p = keyed_camera_watching_a_picture(Key {
        threshold: 0.5,
        softness: 0.2,
        ..Key::OFF
    });
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    h.feedback.write_input(
        h.queue,
        0,
        &quartered_frame((SIZE, SIZE), [[40; 3], [100; 3], [220; 3], [220; 3]]),
    );
    h.step_graph(&p);
    h.step_graph(&p);

    let img = h.read();
    let keyed = |u, v| {
        let (u, v) = tile(2, 1, u, v);
        img.at(u, v)
    };
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

#[test]
fn the_chroma_key_cuts_its_colour_and_spares_grey_and_the_far_hue() {
    // A green sheet, keyed by its hue: the green quarters vanish while a
    // grey of the same brightness — whose chroma is zero, whatever the key
    // hue — and a magenta — whose chroma leans the other way — both arrive
    // intact. The atan2 spells out green's chroma coordinates — transcribed
    // from the decode axes, so a change of axes fails this loudly rather
    // than following along.
    let p = keyed_camera_watching_a_picture(Key {
        hue: (-0.5227f32).atan2(-0.2746),
        tolerance: 0.2,
        softness: 0.02,
        ..Key::OFF
    });
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    let (green, grey, magenta) = ([0, 200, 0], [200; 3], [200, 0, 200]);
    h.feedback.write_input(
        h.queue,
        0,
        &quartered_frame((SIZE, SIZE), [green, grey, green, magenta]),
    );
    h.step_graph(&p);
    h.step_graph(&p);

    let img = h.read();
    for (u, v) in [(0.25, 0.25), (0.75, 0.75)] {
        let (u, v) = tile(2, 1, u, v);
        let seen = img.at(u, v);
        assert!(seen < 3.0, "the key colour at ({u}, {v}) survives: {seen}");
    }
    let (u, v) = tile(2, 1, 0.75, 0.25);
    let grey_seen = img.at(u, v);
    assert!(
        (grey_seen - 200.0).abs() < 4.0,
        "grey was keyed: {grey_seen}"
    );
    let (u, v) = tile(2, 1, 0.25, 0.75);
    let magenta_seen = img.rgb_at(u, v);
    assert!(
        (magenta_seen[0] - 200.0).abs() < 4.0 && (magenta_seen[2] - 200.0).abs() < 4.0,
        "the far hue was keyed: {magenta_seen:?}"
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
    let params = graph(&Single {
        loop_gain: TINT,
        ..frozen(seeded())
    });
    for _ in 0..4 {
        h.step_graph(&params);
    }

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
            .frame(h.device, h.queue, &h.present, &h.feedback, None, None)
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
        .frame(h.device, h.queue, &present, &h.feedback, None, None)
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
    // A one-pass flash on monitor 0, and a camera on it routed to monitor 1
    // with `delay` frames on its cable: monitor 1 lights on pass delay + 1
    // and on no other, and the frame it shows then is byte for byte the one
    // an undelayed camera shows on pass 1 — a delay moves when a frame
    // arrives, never what arrives. The camera zooms and dims so that a frame
    // held for the delay and a frame sent round the camera that many more
    // times come out different. The reach is the full thirty frames so that
    // every ring is the deepest one: the short delays then read from its
    // middle, and the longest from the slab just after the one being
    // written.
    let flash = |delay: u32| Params {
        cameras: vec![Camera {
            delay,
            gain: [0.9; 3],
            framing: Framing {
                zoom: 0.9,
                ..Framing::identity()
            },
            ..plain_camera(one_hot(2, 0))
        }],
        monitors: vec![
            Monitor {
                seed: Seed::WhiteBlob(1.0),
                ..silent_monitor()
            },
            silent_monitor(),
        ],
        inputs: Vec::new(),
        routing: vec![vec![0.0], vec![1.0]],
        routing_inputs: Vec::new(),
        delay: Camera::MAX_DELAY,
    };
    let mut undelayed: Option<Vec<u8>> = None;
    for delay in [0, 1, 2, Camera::MAX_DELAY] {
        let mut p = flash(delay);
        let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
            return;
        };
        let (u0, v0) = tile(2, 1, 0.0, 0.0);
        let (u1, v1) = tile(2, 1, 1.0, 1.0);
        for pass in 0..=delay + 2 {
            h.step_graph(&p);
            p.monitors[0].seed = Seed::Dark;
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
fn an_input_lands_past_the_whole_ring() {
    // On an undelayed graph the ring is one slab and an input's layer is
    // where it always was, so only a delayed graph can tell the layer an
    // input is written to from the one its tap reads. The camera is routed
    // nowhere and is there only to deepen the ring.
    let p = Params {
        cameras: vec![Camera {
            delay: 5,
            ..plain_camera(one_hot(1, 0))
        }],
        monitors: vec![silent_monitor()],
        inputs: vec![Input::Pattern(Pattern::Bars)],
        routing: vec![vec![0.0]],
        routing_inputs: vec![vec![1.0]],
        delay: 5,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE, SIZE), &p) else {
        return;
    };
    h.feedback
        .write_input(h.queue, 0, &flat_frame((SIZE, SIZE), [200; 3]));
    // A whole turn of the ring, so the pass that draws the last slab — the
    // one whose upper view is the input layers alone — is among them.
    for pass in 0..p.history() {
        h.step_graph(&p);
        let shown = h.read().rgb_at(0.5, 0.5);
        assert!(
            shown.iter().all(|c| (c - 200.0).abs() <= 1.0),
            "pass {pass}: the monitor shows {shown:?}, not the input's flat 200"
        );
    }
}

#[test]
fn blanking_the_monitors_empties_the_whole_ring() {
    // A flash in flight down a delayed cable is in the ring and nowhere
    // else, so a blank that left any slab alone would deliver it late: after
    // the blank, monitor 1 stays dark for longer than the delay.
    let delay = 4;
    let mut p = Params {
        cameras: vec![Camera {
            delay,
            ..plain_camera(one_hot(2, 0))
        }],
        monitors: vec![
            Monitor {
                seed: Seed::WhiteBlob(1.0),
                ..silent_monitor()
            },
            silent_monitor(),
        ],
        inputs: Vec::new(),
        routing: vec![vec![0.0], vec![1.0]],
        routing_inputs: Vec::new(),
        delay,
    };
    let Some(mut h) = graph_harness((SIZE, SIZE), (SIZE * 2, SIZE), &p) else {
        return;
    };
    let (u0, v0) = tile(2, 1, 0.0, 0.0);
    let (u1, v1) = tile(2, 1, 1.0, 1.0);
    // First the cable delivers, or the dark below proves nothing.
    h.step_graph(&p);
    p.monitors[0].seed = Seed::Dark;
    for _ in 0..delay {
        h.step_graph(&p);
    }
    h.step_graph(&p);
    let lit = h.read().brightest_in(u0, v0, u1, v1);
    assert!(lit > 200.0, "the flash never arrived: {lit}");
    // Two flashes, two passes apart, so that both the slab the blank sees as
    // newest and one further back hold light.
    p.monitors[0].seed = Seed::WhiteBlob(1.0);
    h.step_graph(&p);
    p.monitors[0].seed = Seed::Dark;
    h.step_graph(&p);
    p.monitors[0].seed = Seed::WhiteBlob(1.0);
    h.step_graph(&p);
    p.monitors[0].seed = Seed::Dark;
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
