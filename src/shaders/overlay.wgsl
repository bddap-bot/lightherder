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

struct Arrows {
    count: vec4<u32>,
    line: vec4<f32>,
    segments: array<vec4<f32>, 40>,
    shares: array<vec4<f32>, 40>,
};

@group(0) @binding(2) var<uniform> arrows: Arrows;

fn cover(p: vec2<f32>, s: vec4<f32>, widen: f32) -> f32 {
    let a = s.xy;
    let ab = s.zw - a;
    let len = length(ab);
    let d = ab / len;
    let ap = p - a;
    let t = dot(ap, d);
    let perp = abs(ap.x * d.y - ap.y * d.x);
    let half = arrows.line.x * 0.5 + widen;
    let head = arrows.line.y;
    let shaft = step(0.0, t) * step(t, len - head) * (1.0 - smoothstep(-0.5, 0.5, perp - half));
    let u = (len - t) / head;
    let tip = step(0.0, u) * step(u, 1.0)
        * (1.0 - smoothstep(-0.5, 0.5, perp - (arrows.line.z * u + widen)));
    return max(shaft, tip);
}

@fragment
fn fs_arrows(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy;
    var inner = 0.0;
    var outer = 0.0;
    for (var i = 0u; i < arrows.count.x; i++) {
        let share = arrows.shares[i].x;
        inner = max(inner, cover(p, arrows.segments[i], 0.0) * share);
        outer = max(outer, cover(p, arrows.segments[i], 1.0) * share);
    }
    return vec4<f32>(vec3<f32>(inner / max(outer, 1e-4)), outer);
}
