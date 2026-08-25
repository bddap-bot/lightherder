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
    // xy: the chroma subcarrier as a phasor, hue in its phase and saturation
    // in its length. z: brightness. w: contrast.
    colour: vec4<f32>,
    // x: phosphor gamma. The rest is what a uniform member's 16-byte
    // alignment costs; there is no missing term.
    phosphor: vec4<f32>,
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

// The monitor's front panel, in the order an analog signal meets it: chroma
// decode, video amplifier, phosphor.
fn analog_colour(rgb: vec3<f32>) -> vec3<f32> {
    // NTSC luma and colour-difference axes. Working here rather than in RGB is
    // what makes hue a phase: the two chroma axes are the real and imaginary
    // parts of one subcarrier, so turning it is a complex multiply and luma
    // comes out untouched.
    let yiq = vec3<f32>(
        dot(rgb, vec3<f32>(0.299, 0.587, 0.114)),
        dot(rgb, vec3<f32>(0.5959, -0.2746, -0.3213)),
        dot(rgb, vec3<f32>(0.2115, -0.5227, 0.3112)),
    );
    let iq = vec2<f32>(
        yiq.y * u.colour.x - yiq.z * u.colour.y,
        yiq.y * u.colour.y + yiq.z * u.colour.x,
    );
    let decoded = vec3<f32>(
        yiq.x + dot(iq, vec2<f32>(0.9563, 0.6210)),
        yiq.x + dot(iq, vec2<f32>(-0.2721, -0.6474)),
        yiq.x + dot(iq, vec2<f32>(-1.1070, 1.7046)),
    );

    // Contrast pivots about mid-grey. Pivoting about black instead would make
    // it a second loop gain, which is a knob this instrument already has.
    let amplified = (decoded - vec3<f32>(0.5)) * u.colour.w + vec3<f32>(0.5 + u.colour.z);

    // A phosphor emits no negative light, and pow() of a negative is not a
    // number, so the floor here is physics and hygiene at once. Nothing
    // clamps the top: the loop keeps its half-float headroom.
    return pow(max(amplified, vec3<f32>(0.0)), vec3<f32>(u.phosphor.x));
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

    // The knobs are on the monitor, not on the camera, so they colour
    // everything the monitor displays — the seed spot included.
    return vec4<f32>(analog_colour(fed_back + vec3<f32>(seed)), 1.0);
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(src_tex, src_samp, in.uv).rgb, 1.0);
}
