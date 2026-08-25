// One camera watching one monitor. `fs_camera` writes the monitor's next
// frame; `fs_present` copies the monitor to the window.

struct Uniforms {
    // Rows of the uv -> uv map from a texel being written to the texel the
    // camera saw there. See affine::sample_transform.
    row0: vec4<f32>,
    row1: vec4<f32>,
    // rgb: per-channel loop gain. a: seed brightness.
    gain: vec4<f32>,
    // xy: seed centre in uv. zw: seed radii in uv, already aspect-corrected.
    seed: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> VsOut {
    // One oversized triangle covers the target without a vertex buffer.
    let c = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(c * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(c.x, 1.0 - c.y);
    return out;
}

@fragment
fn fs_camera(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec3<f32>(in.uv, 1.0);
    let src_uv = vec2<f32>(dot(u.row0.xyz, p), dot(u.row1.xyz, p));

    // Past the monitor's edge the camera sees an unlit room, but a clamped
    // sampler returns the border texel there, which would smear across the
    // frame. The sample is taken unconditionally so control flow stays
    // uniform, then thrown away.
    let inside = all(src_uv >= vec2<f32>(0.0)) && all(src_uv <= vec2<f32>(1.0));
    let sampled = textureSample(src_tex, src_samp, src_uv).rgb * u.gain.rgb;
    let fed_back = select(vec3<f32>(0.0), sampled, inside);

    let d = length((in.uv - u.seed.xy) / u.seed.zw);
    let seed = u.gain.a * exp(-d * d);

    return vec4<f32>(fed_back + vec3<f32>(seed), 1.0);
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(src_tex, src_samp, in.uv).rgb, 1.0);
}
