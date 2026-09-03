// The feedback graph's passes. `fs_camera` writes one monitor's next frame
// from its taps into the bank of previous frames; `fs_present` copies one
// monitor to a viewport of the window.

// One flattened edge of the graph: a camera's view of one source monitor,
// scaled by everything between them. See feedback::Tap.
struct Tap {
    // Rows of the uv -> uv map from a texel being written to the texel the
    // camera saw there, the router output's mirror composed in. See
    // affine::sample_transform.
    row0: vec4<f32>,
    row1: vec4<f32>,
    // rgb: routing x splitter x gain, per channel. a: source layer.
    weight: vec4<f32>,
    // The switcher's keyer: x luma threshold, y softness, zw padding.
    key: vec4<f32>,
};

struct Uniforms {
    // Decodes RGB to luma and chroma, turns the chroma by hue and scales it
    // by saturation, and encodes back. See params::Colour::chroma_matrix.
    chroma: mat3x3<f32>,
    // x: brightness. y: contrast. z: the amplifier's headroom, handed over
    // rather than written here so the crate has one copy. w: padding.
    levels: vec4<f32>,
    // x: tap count. y: this monitor's own layer, for fs_present. z: the
    // first bank layer past `lower`. w: the first bank layer of `upper`.
    info: vec4<f32>,
    // x: the unsharp mask, the front panel's sharpness knob. yzw: padding.
    analog: vec4<f32>,
    // xyz: FCC NTSC luma, handed over rather than written here so the crate
    // has one copy of it.
    luma: vec4<f32>,
    // As long as feedback::MAX_TAPS, which is every camera through every
    // monitor plus the seed. A second spelling of that number, held to it by
    // the `min_binding_size` the pipeline is built with: a mismatch is a
    // validation error at startup rather than a wrong read.
    taps: array<Tap, 16>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
// The bank in two runs of layers, split round the slab a camera pass is
// drawing to: a pass may not sample the layers it writes, and a view is one
// run. A pass that writes none of the bank binds the whole of it as `lower`.
@group(0) @binding(1) var lower: texture_2d_array<f32>;
@group(0) @binding(2) var upper: texture_2d_array<f32>;
@group(0) @binding(3) var src_samp: sampler;

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
// decode, video amplifier, its rails.
fn front_panel(rgb: vec3<f32>) -> vec3<f32> {
    let decoded = u.chroma * rgb;

    // A gain about mid-grey, which is algebraically a gain plus a lift. What
    // makes it worth its own knob is the fixed point: the loop gain is a gain
    // about black applied before the seed is added, so no setting of it holds
    // mid-grey still.
    let amplified = (decoded - vec3<f32>(0.5)) * u.levels.y + vec3<f32>(0.5 + u.levels.x);

    // Where the amplifier runs out of rails: untouched below half the
    // headroom h, then bending asymptotically onto h. The two arms meet at
    // h/2 in both value and slope, so nothing kinks at the knee. This is what
    // makes an overdriven loop settle into a structure rather than clip the
    // monitor to flat white — the half-float target has headroom the eye
    // never gets to see. Both arms are evaluated: the divide by a channel at
    // zero gives an infinity the select discards.
    let h = u.levels.z;
    let limited = select(
        h - h * h / (4.0 * amplified),
        amplified,
        amplified < vec3<f32>(0.5 * h),
    );

    // A phosphor emits no negative light, so the floor here is physics.
    return max(limited, vec3<f32>(0.0));
}

// What one camera sees of one source monitor at one point — the sampling,
// not `Camera::look`, whose splitter weights are already folded into
// `tap.weight`. Past a monitor's
// edge the camera sees an unlit room, but a clamped sampler returns the
// border texel there, which would smear across the frame.
//
// textureSampleLevel, not textureSample, throughout this shader: the monitor
// textures have a single mip level, so the derivatives textureSample computes
// go nowhere.
fn seen_at(uv: vec2<f32>, layer: i32) -> vec3<f32> {
    var rgb: vec3<f32>;
    if layer < i32(u.info.z) {
        rgb = textureSampleLevel(lower, src_samp, uv, layer, 0.0).rgb;
    } else {
        rgb = textureSampleLevel(upper, src_samp, uv, layer - i32(u.info.w), 0.0).rgb;
    }
    return select(vec3<f32>(0.0), rgb, inside(uv));
}

fn inside(uv: vec2<f32>) -> bool {
    return all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
}

// One arm of the sharpness mask: the source at uv, or the centre again
// past the source's edge. The room beyond is dark, and a mask that saw it
// would add light along the border — which the loop multiplies.
fn arm(uv: vec2<f32>, layer: i32, centre: vec3<f32>) -> vec3<f32> {
    return select(centre, seen_at(uv, layer), inside(uv));
}

@fragment
fn fs_camera(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec3<f32>(in.uv, 1.0);

    var fed_back = vec3<f32>(0.0);
    let count = u32(u.info.x);
    for (var t = 0u; t < count; t++) {
        let tap = u.taps[t];
        let src_uv = vec2<f32>(dot(tap.row0.xyz, p), dot(tap.row1.xyz, p));
        let layer = i32(tap.weight.a);
        let raw = seen_at(src_uv, layer);
        var signal = raw;

        // The monitor's sharpness, an unsharp mask a texel wide on the signal
        // the switcher hands it. That signal is summed per fragment and never
        // re-read, so the mask is taken per tap from the bank texels, the
        // neighbours' keyer verdicts taken as the centre's. The arms are
        // summed in pairs so four equal samples come back as exactly the
        // centre.
        // Skipped at rest — four reads a texel on every tap of every monitor,
        // for nothing — and past the source's edge, where the centre is the
        // dark room and an arm that lands back inside would cut a dark rim
        // into whatever else lights the fragment.
        let sharpness = u.analog.x;
        if sharpness > 0.0 && inside(src_uv) {
            let texel = 1.0 / vec2<f32>(textureDimensions(lower));
            let across = vec2<f32>(tap.row0.x, tap.row1.x) * texel.x;
            let down = vec2<f32>(tap.row0.y, tap.row1.y) * texel.y;
            let blurred = 0.25
                * ((arm(src_uv + across, layer, raw) + arm(src_uv - across, layer, raw))
                    + (arm(src_uv + down, layer, raw) + arm(src_uv - down, layer, raw)));
            signal += sharpness * (raw - blurred);
        }

        // The keyer, judged on the centre sample: what it gates is the
        // switcher's whole hand-over, the way the gain scales it. It passes
        // everything at or above its threshold and finishes cutting one
        // softness below it, so a threshold of zero is exactly inert —
        // inside a loop, "almost passes" is a ratchet. The epsilon keeps
        // smoothstep's edges apart at zero softness, where it would
        // otherwise divide by nothing.
        let soft = max(tap.key.y, 1e-4);
        let alpha = smoothstep(tap.key.x - soft, tap.key.x, dot(u.luma.xyz, raw));
        fed_back += signal * tap.weight.rgb * alpha;
    }

    return vec4<f32>(front_panel(fed_back), 1.0);
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(
        textureSampleLevel(lower, src_samp, in.uv, i32(u.info.y), 0.0).rgb,
        1.0,
    );
}
