//! What a frame costs, off screen.
//!
//! On a display the loop runs at the vertical blank whatever it costs, so a
//! window reporting sixty says only that a frame fit — not by how much. This
//! runs the same work with nothing pacing it: the graph stepped, and then
//! presented into a target the same size, which is the deployed case worth
//! timing — a bank at the display's resolution.
//!
//! Two things it leaves out, both of them a frame's edges rather than its
//! loop. Handing the frame to the compositor, which is not this program's
//! work; and the upload of a live input, which for a file or a capture device
//! is a conversion and two writes of a whole frame per input per frame. A
//! graph whose inputs are drawn patterns — every one that ships — uploads
//! them once and is timed whole.

use web_time::Instant;

use crate::feedback::Feedback;
use crate::gpu::Gpu;
use crate::params::Params;
use crate::present::Present;

/// How many frames are timed, after a warm-up. Ten seconds' worth at sixty,
/// long enough that a shader compile or a first-touch allocation cannot be
/// most of the answer. A whole number of [`BATCH`]es, which the mean divides
/// by.
pub const FRAMES: u32 = 600;

/// Frames run and thrown away first: the pipelines are built and every
/// texture in the bank is touched for the first time on the way through frame
/// one.
const WARMUP: u32 = 60;

/// Frames submitted between waits. Waiting after every frame would time the
/// round trip to the driver as well as the work; waiting after all six
/// hundred would queue up more than a GPU has memory for. Sixty is a second
/// of the real thing.
const BATCH: u32 = 60;

const _: () = assert!(FRAMES.is_multiple_of(BATCH) && FRAMES > 0);

/// The format a window on this machine most likely presents in. It matters
/// only to the present pass's writes, and every candidate is four bytes.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// `resolution` is the monitors' size, and stands in for the display's as
/// well: an instrument deployed to a 4K screen wants its bank at 4K, which is
/// the case worth timing.
pub async fn run(params: &Params, resolution: (u32, u32)) -> Result<(), String> {
    crate::feedback::bank_fits(params, resolution)?;
    // No display handle: nothing here is ever presented to one.
    let Gpu {
        adapter,
        device,
        queue,
        ..
    } = Gpu::open(None, "lightherder bench").await?;
    let name = adapter.get_info().name.clone();

    let (width, height) = resolution;
    let mut feedback = Feedback::new(&device, width, height, params);
    let present = Present::new(&device, &feedback, TARGET_FORMAT);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let frame = |feedback: &mut Feedback| {
        feedback.step(&device, &queue, params);
        present.draw(&device, &queue, &view, resolution, feedback);
    };
    for _ in 0..WARMUP {
        frame(&mut feedback);
    }
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;

    let (mut total, mut worst) = (0.0f64, 0.0f64);
    for _ in 0..FRAMES / BATCH {
        let started = Instant::now();
        for _ in 0..BATCH {
            frame(&mut feedback);
        }
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("poll: {e}"))?;
        let ms = started.elapsed().as_secs_f64() * 1e3 / BATCH as f64;
        total += ms;
        worst = worst.max(ms);
    }
    let mean = total / (FRAMES / BATCH) as f64;
    println!(
        "{name}: {} monitors and {} inputs at {width}x{height}, {:.2} GiB of bank",
        params.monitors.len(),
        params.inputs.len(),
        crate::feedback::bank_bytes(params, resolution) as f64 / (1u64 << 30) as f64,
    );
    println!(
        "{mean:.2} ms/frame ({:.0} fps), worst second {worst:.2} ms ({:.0} fps)",
        1e3 / mean,
        1e3 / worst,
    );
    Ok(())
}
