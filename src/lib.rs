//! A GPU video-feedback instrument: cameras pointed at the monitors they are
//! drawing to, which is enough to make spirals, tunnels and fractals — and,
//! wired into a graph, monitors composed of each other's past.

pub mod affine;
pub mod app;
pub mod bench;
pub mod capture;
pub mod cli;
pub mod config;
pub mod feedback;
pub mod gpu;
pub mod input;
pub mod keys;
pub(crate) mod lamps;
pub mod midi;
pub mod overlay;
pub mod params;
pub mod present;
pub mod tempo;
#[cfg(target_arch = "wasm32")]
pub mod web;

/// The controls as they actually are: the keys, and the control surface under
/// whatever map is in force. Printed by `--cheatsheet` and on the way up,
/// from the tables that drive the instrument rather than from a copy kept
/// beside them. Against `params` because the select rows are as wide as the
/// graph — the card is of the rig about to be played, not of a rig in
/// general.
pub fn cheatsheet(params: &params::Params) -> Result<String, String> {
    let map = midi::Map::load(&midi::map_path(), params)?;
    Ok(format!("{}{}", keys::help(), map.card()))
}

/// Vulkan, Metal, DX12 and WebGPU. Deliberately not `Backends::all()`, which
/// also brings up a GL context per instance purely to enumerate adapters.
pub const BACKENDS: wgpu::Backends = wgpu::Backends::PRIMARY;

/// Every pass here draws one full-screen triangle over a bound texture, so
/// they differ only in fragment entry point, target format, and — for the
/// overlay, which composites rather than replaces — blend state.
pub(crate) fn fullscreen_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    bind_group_layout: &wgpu::BindGroupLayout,
    fragment_entry: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    label: &str,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_fullscreen"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}
