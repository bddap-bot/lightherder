// The controls overlay's blit: the panel image, already drawn on the CPU,
// alpha-blended into whatever viewport the present pass points at. Its own
// tiny module rather than a corner of feedback.wgsl because it binds one
// plain texture where the feedback passes bind the bank.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// The same oversized triangle as feedback.wgsl's, restated because a WGSL
// module cannot import one: three vertices cover the viewport with no
// vertex buffer.
@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> VsOut {
    let c = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(c * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(c.x, 1.0 - c.y);
    return out;
}

@group(0) @binding(0) var panel_tex: texture_2d<f32>;
@group(0) @binding(1) var panel_samp: sampler;

@fragment
fn fs_overlay(in: VsOut) -> @location(0) vec4<f32> {
    // Straight alpha out; the pipeline's blend state does the compositing.
    return textureSample(panel_tex, panel_samp, in.uv);
}
