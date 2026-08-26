//! What a frame costs, off screen.
//!
//! On a display the loop runs at the vertical blank whatever it costs, so a
//! window reporting sixty says only that a frame fit — not by how much. This
//! runs the same work with nothing pacing it: the graph stepped and then
//! presented into a target the size of the display, which is every pass the
//! deployed instrument makes bar the handoff to the compositor.

use std::time::Instant;

use crate::cli::{BENCH_FRAMES, BENCH_WARMUP};
use crate::feedback::Feedback;
use crate::params::Params;
use crate::present::Present;

/// Frames submitted between waits. Waiting after every frame would time the
/// round trip to the driver as well as the work; waiting after all six
/// hundred would queue up more than a GPU has memory for. Sixty is a second
/// of the real thing.
const BATCH: u32 = 60;

/// The format a window on this machine most likely presents in. It matters
/// only to the present pass's writes, and every candidate is four bytes.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// `resolution` is the monitors' size, and stands in for the display's as
/// well: an instrument deployed to a 4K screen wants its bank at 4K, which is
/// the case worth timing.
pub fn run(params: &Params, resolution: (u32, u32)) -> Result<(), String> {
    crate::feedback::bank_fits(params, resolution)?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: crate::BACKENDS,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU adapter: {e}"))?;
    let name = adapter.get_info().name.clone();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lightherder bench"),
        ..Default::default()
    }))
    .map_err(|e| format!("adapter {name} refused a device: {e}"))?;

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
    for _ in 0..BENCH_WARMUP {
        frame(&mut feedback);
    }
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;

    let (mut total, mut worst) = (0.0f64, 0.0f64);
    for _ in 0..BENCH_FRAMES / BATCH {
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
    let mean = total / (BENCH_FRAMES / BATCH) as f64;
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
