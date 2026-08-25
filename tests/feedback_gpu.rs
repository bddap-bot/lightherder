//! End-to-end checks that the loop really runs on a GPU: the seed lights the
//! monitor, the previous frame comes back, and the knobs reach the shader.
//!
//! Skipped where no adapter exists (CI containers, machines with no Vulkan
//! loader). The skip is loud, so a silent pass cannot be mistaken for a run.

use lightherder::feedback::Feedback;
use lightherder::params::Params;
use lightherder::present::Present;

const SIZE: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A monitor, a camera, and somewhere to read the result back from.
struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    feedback: Feedback,
    present: Present,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    readback: wgpu::Buffer,
}

impl Harness {
    fn new() -> Option<Harness> {
        let (adapter, device, queue) = pollster::block_on(lightherder::headless_device())?;
        eprintln!("adapter: {:?}", adapter.get_info());
        let feedback = Feedback::new(&device, SIZE, SIZE);
        let present = Present::new(&device, feedback.layout(), TARGET_FORMAT);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("readback target"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
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
            // 64 px * 4 bytes is already a multiple of the 256-byte row
            // alignment a texture-to-buffer copy demands.
            size: (SIZE * SIZE * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Harness {
            device,
            queue,
            feedback,
            present,
            target,
            target_view,
            readback,
        })
    }

    fn step(&mut self, params: &Params) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("test step"),
            });
        self.feedback.step(&self.queue, &mut encoder, params);
        self.present
            .draw(&mut encoder, &self.target_view, self.feedback.bind_group());
        self.queue.submit([encoder.finish()]);
    }

    fn read(&self) -> Image {
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
                    bytes_per_row: Some(SIZE * 4),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
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
        Image { pixels }
    }
}

struct Image {
    pixels: Vec<u8>,
}

impl Image {
    /// Mean of the RGB channels at a uv position, 0..=255.
    fn at(&self, u: f32, v: f32) -> f32 {
        let x = ((u * SIZE as f32) as u32).min(SIZE - 1);
        let y = ((v * SIZE as f32) as u32).min(SIZE - 1);
        let i = ((y * SIZE + x) * 4) as usize;
        (self.pixels[i] as f32 + self.pixels[i + 1] as f32 + self.pixels[i + 2] as f32) / 3.0
    }
}

/// `None` means this machine has no GPU to test with, which is not a failure.
fn harness() -> Option<Harness> {
    match Harness::new() {
        Some(h) => Some(h),
        None => {
            eprintln!("SKIPPED: no wgpu adapter on this machine");
            None
        }
    }
}

fn still(params: Params) -> Params {
    Params {
        framing: lightherder::affine::Framing {
            rotation: 0.0,
            ..params.framing
        },
        ..params
    }
}

#[test]
fn the_seed_lights_the_middle_of_the_monitor() {
    let Some(mut h) = harness() else { return };
    h.step(&Params::default());
    let img = h.read();
    assert!(img.at(0.5, 0.5) > 200.0, "centre was {}", img.at(0.5, 0.5));
    assert!(
        img.at(0.02, 0.02) < 10.0,
        "corner was {}",
        img.at(0.02, 0.02)
    );
}

#[test]
fn the_image_survives_the_seed_being_switched_off() {
    let Some(mut h) = harness() else { return };
    h.step(&Params::default());
    let lit = h.read().at(0.5, 0.5);

    let mut params = still(Params::default());
    params.seed_gain = 0.0;
    params.decay = [0.9; 3];
    let mut previous = lit;
    for _ in 0..4 {
        h.step(&params);
        let now = h.read().at(0.5, 0.5);
        assert!(now < previous, "{now} should be dimmer than {previous}");
        previous = now;
    }
    // Still visible: this is the previous frame coming back round, not a clear.
    assert!(previous > 20.0, "the loop went dark: {previous}");
}

#[test]
fn zero_gain_ends_the_loop_in_one_pass() {
    let Some(mut h) = harness() else { return };
    h.step(&Params::default());
    assert!(h.read().at(0.5, 0.5) > 200.0);

    let mut params = still(Params::default());
    params.seed_gain = 0.0;
    params.decay = [0.0; 3];
    h.step(&params);
    assert!(
        h.read().at(0.5, 0.5) < 2.0,
        "gain 0 left {}",
        h.read().at(0.5, 0.5)
    );
}

#[test]
fn panning_moves_the_image_the_way_the_knob_says() {
    let Some(mut h) = harness() else { return };
    h.step(&Params::default());

    let mut params = still(Params::default());
    params.seed_gain = 0.0;
    params.decay = [1.0; 3];
    params.framing.translate = [0.2, 0.0];
    h.step(&params);

    let img = h.read();
    // A square monitor, so +0.2 in screen units is +0.2 in u.
    assert!(
        img.at(0.7, 0.5) > img.at(0.5, 0.5),
        "spot did not move right: {} at 0.7 vs {} at 0.5",
        img.at(0.7, 0.5),
        img.at(0.5, 0.5)
    );
    assert!(
        img.at(0.7, 0.5) > 100.0,
        "spot faded away: {}",
        img.at(0.7, 0.5)
    );
}
