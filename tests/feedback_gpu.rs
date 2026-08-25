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
use lightherder::feedback::Feedback;
use lightherder::params::Params;
use lightherder::present::Present;

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

/// A monitor, a camera, and somewhere to read the result back from.
struct Harness {
    device: &'static wgpu::Device,
    queue: &'static wgpu::Queue,
    feedback: Feedback,
    present: Present,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    readback: wgpu::Buffer,
    target_size: (u32, u32),
}

impl Harness {
    fn new(monitor: (u32, u32), target_size: (u32, u32)) -> Harness {
        // Read-back is the point of this harness, and a texture-to-buffer
        // copy demands 256-byte rows.
        assert!(
            (target_size.0 * 4).is_multiple_of(256),
            "target width {} breaks the read-back row alignment",
            target_size.0
        );
        let (device, queue) = gpu().as_ref().expect("checked by harness()");

        let feedback = Feedback::new(device, monitor.0, monitor.1);
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
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
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
            target_view,
            readback,
            target_size,
        }
    }

    fn step(&mut self, params: &Params) {
        self.feedback.step(self.device, self.queue, params);
        self.present();
    }

    fn present(&self) {
        self.present.draw(
            self.device,
            self.queue,
            &self.target_view,
            self.target_size,
            &self.feedback,
        );
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
    /// Mean of the RGB channels at a uv position, 0..=255. Values are linear:
    /// the target format is not sRGB, unlike the window's usual surface.
    fn at(&self, u: f32, v: f32) -> f32 {
        let x = ((u * self.width as f32) as u32).min(self.width - 1);
        let y = ((v * self.height as f32) as u32).min(self.height - 1);
        let i = ((y * self.width + x) * 4) as usize;
        (self.pixels[i] as f32 + self.pixels[i + 1] as f32 + self.pixels[i + 2] as f32) / 3.0
    }

    fn brightest(&self) -> f32 {
        self.pixels
            .chunks_exact(4)
            .map(|p| (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0)
            .fold(0.0, f32::max)
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
fn harness(monitor: (u32, u32), target: (u32, u32)) -> Option<Harness> {
    match gpu() {
        Ok(_) => Some(Harness::new(monitor, target)),
        Err(NoGpu::NoAdapter(why)) => {
            let _ = writeln!(std::io::stderr(), "SKIPPED: no adapter: {why}");
            None
        }
        Err(NoGpu::DeviceRefused(why)) => panic!("{why}"),
    }
}

fn square() -> Option<Harness> {
    harness((SIZE, SIZE), (SIZE, SIZE))
}

fn seeded() -> Params {
    Params {
        seed_brightness: 1.0,
        ..Default::default()
    }
}

/// Params whose camera does not move, so a lit spot stays where it was put.
fn frozen(params: Params) -> Params {
    Params {
        framing: Framing {
            zoom: 1.0,
            rotation: 0.0,
            ..params.framing
        },
        ..params
    }
}

#[test]
fn the_seed_lights_the_spot_it_says_it_does() {
    let Some(mut h) = square() else { return };
    let seed = h.feedback.seed_uv();
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
    let seed = h.feedback.seed_uv();
    h.step(&seeded());
    let mut previous = h.read().at(seed[0], seed[1]);

    let params = Params {
        seed_brightness: 0.0,
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
    let seed = h.feedback.seed_uv();
    h.step(&seeded());
    assert!(h.read().at(seed[0], seed[1]) > 200.0);

    let params = Params {
        seed_brightness: 0.0,
        loop_gain: [0.0; 3],
        ..frozen(seeded())
    };
    h.step(&params);
    let left = h.read().at(seed[0], seed[1]);
    assert!(left < 2.0, "gain 0 left {left}");
}

#[test]
fn panning_moves_the_image_the_way_the_knobs_say() {
    for (translate, offset, name) in [
        ([0.15f32, 0.0], [0.15f32, 0.0], "right"),
        ([-0.15, 0.0], [-0.15, 0.0], "left"),
        // Screen units are y-up; texture v is not.
        ([0.0, 0.15], [0.0, -0.15], "up"),
        ([0.0, -0.15], [0.0, 0.15], "down"),
    ] {
        let Some(mut h) = square() else { return };
        let seed = h.feedback.seed_uv();
        h.step(&seeded());

        let params = Params {
            seed_brightness: 0.0,
            loop_gain: [1.0; 3],
            framing: Framing {
                translate,
                ..frozen(seeded()).framing
            },
        };
        h.step(&params);

        // A square monitor, so a screen-unit shift is the same shift in uv.
        let (u, v) = (seed[0] + offset[0], seed[1] + offset[1]);
        let img = h.read();
        assert!(
            img.at(u, v) > img.at(seed[0], seed[1]) && img.at(u, v) > 100.0,
            "pan {name}: {} where it should have gone vs {} left behind",
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

    let params = Params {
        seed_brightness: 0.0,
        loop_gain: [1.0; 3],
        framing: Framing {
            zoom: 2.0,
            rotation: 0.0,
            translate: [-0.3, 0.0],
        },
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
    let seed = h.feedback.seed_uv();
    h.step(&Params {
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
    let params = Params::default();
    for _ in 0..400 {
        h.feedback.step(h.device, h.queue, &params);
    }
    h.step(&params);

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
    h.present();
    let left = h.read().brightest();
    assert!(left < 2.0, "blanking left {left}");
}

#[test]
fn the_gain_is_applied_once_per_pass() {
    let Some(mut h) = square() else { return };
    let seed = h.feedback.seed_uv();
    h.step(&seeded());
    let mut previous = h.read().at(seed[0], seed[1]);

    let gain = 0.8;
    let params = Params {
        seed_brightness: 0.0,
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
    let pan = |dx: f32| Params {
        seed_brightness: 0.0,
        loop_gain: [1.0; 3],
        framing: Framing {
            translate: [dx, 0.0],
            ..still.framing
        },
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
    h.step(&Params {
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
    let seed = h.feedback.seed_uv();
    let expected = 0.375 + seed[0] / 4.0;
    let found = img.brightest_uv();
    assert!(
        (found[0] - expected).abs() < 0.02 && (found[1] - 0.5).abs() < 0.02,
        "spot at {found:?}, expected [{expected}, 0.5]"
    );
}

#[test]
fn the_seed_sits_where_the_convention_says_it_does() {
    // An oracle that does not ask seed_uv where the seed is: the spot lands a
    // quarter of the monitor's HEIGHT right of centre, which on a 2:1 monitor
    // is an eighth of its width.
    let Some(mut h) = harness((SIZE * 4, SIZE * 2), (SIZE * 4, SIZE * 2)) else {
        return;
    };
    h.step(&Params {
        loop_gain: [0.0; 3],
        ..seeded()
    });

    let found = h.read().brightest_uv();
    assert!(
        (found[0] - 0.625).abs() < 0.02 && (found[1] - 0.5).abs() < 0.02,
        "seed at {found:?}, expected [0.625, 0.5]"
    );
}
