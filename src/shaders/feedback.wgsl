// The feedback graph's passes. `fs_camera` writes one monitor's next frame
// from its taps into the bank of previous frames; `fs_record` puts a delay
// unit's picture on its line; `fs_present` copies one monitor to a viewport
// of the window.

// One flattened edge of the graph: a camera's view of one source monitor,
// scaled by everything between them. See feedback::Tap.
struct Tap {
    // Rows of the uv -> uv map from a texel being written to the texel the
    // camera saw there, the router output's mirror composed in. See
    // affine::sample_transform.
    row0: vec4<f32>,
    row1: vec4<f32>,
    // rgb: splitter x gain, per channel. a: source layer.
    weight: vec4<f32>,
};

// One switcher on the monitor's chain. See feedback::Stage.
struct Stage {
    // x: one past the last tap of its In1. yzw: padding.
    in1: vec4<f32>,
    // x: the crossfade. y: the clip, 0 for the key off. z: the edge's width.
    // w: 1 on a chroma key, 0 on a luma key.
    key: vec4<f32>,
};

struct Uniforms {
    // Decodes RGB to luma and chroma, turns the chroma by hue and scales it
    // by saturation, and encodes back. See params::Colour::chroma_matrix.
    chroma: mat3x3<f32>,
    // x: brightness. y: contrast. z: the amplifier's headroom, handed over
    // rather than written here so the crate has one copy. w: padding.
    levels: vec4<f32>,
    // x: the taps that land on the program before any switcher runs. y: this
    // monitor's own layer, for fs_present. z: the first bank layer past
    // `lower`. w: the first bank layer of `upper`.
    info: vec4<f32>,
    // x: the unsharp mask, the front panel's sharpness knob. yzw: padding.
    analog: vec4<f32>,
    // xyz: the keys' measures, FCC NTSC luma and a blue screen's share,
    // handed over rather than written here so the crate has one copy of
    // each.
    luma: vec4<f32>,
    blue: vec4<f32>,
    // x: how many of `stages` run. yzw: padding.
    chain: vec4<f32>,
    // As long as feedback::STAGES, and `taps` as feedback::MAX_TAPS, which is
    // every camera through every monitor plus the seed. Second spellings of
    // those numbers: a static assertion beside each fails the build if the
    // Rust side grows, since wgpu catches only the other direction.
    stages: array<Stage, 4>,
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

// What one camera sees of one source layer at one point — the sampling,
// not `Camera::look`, whose splitter weights are already folded into the
// tap's weight — and in alpha whether it is a monitor it sees there. Past a
// layer's edge the camera sees an unlit room, but a clamped sampler returns
// the border texel there, which would smear across the frame. A monitor is
// opaque across its face; a delay unit's picture keeps where its camera saw
// one.
//
// textureSampleLevel, not textureSample, throughout this shader: the monitor
// textures have a single mip level, so the derivatives textureSample computes
// go nowhere.
fn seen_at(uv: vec2<f32>, layer: i32) -> vec4<f32> {
    var texel: vec4<f32>;
    if layer < i32(u.info.z) {
        texel = textureSampleLevel(lower, src_samp, uv, layer, 0.0);
    } else {
        texel = textureSampleLevel(upper, src_samp, uv, layer - i32(u.info.w), 0.0);
    }
    return select(vec4<f32>(0.0), texel, inside(uv));
}

fn inside(uv: vec2<f32>) -> bool {
    return all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
}

// One arm of the sharpness mask: the source at uv, or the centre again
// where no monitor was seen. The room beyond is dark, and a mask that saw
// it would add light along the border — which the loop multiplies.
fn arm(uv: vec2<f32>, layer: i32, centre: vec3<f32>) -> vec3<f32> {
    let seen = seen_at(uv, layer);
    return select(centre, seen.rgb, seen.a > 0.5);
}

// How much of In2 a switcher's key passes at this texel, judged on the
// picture In2 is handed: its luma, or one less its share of a blue screen.
// It passes everything at or above the clip and finishes cutting one edge
// below, and a clip of zero is the key off, skipped outright so that it is
// exactly inert — inside a loop, "almost passes" is a ratchet.
fn passed(stage: Stage, two: vec3<f32>) -> f32 {
    let clip = stage.key.y;
    if clip <= 0.0 {
        return 1.0;
    }
    var measured = dot(u.luma.xyz, two);
    if stage.key.w > 0.5 {
        measured = 1.0 - dot(u.blue.xyz, two);
    }
    return smoothstep(clip - stage.key.z, clip, measured);
}

// What a run of taps lands on one switcher input: the picture that arrives,
// which is what a key judges; what the monitor shows of it, the sharpness
// mask included, since that is the monitor's front panel and not the
// switcher's; and whether any of the taps saw a monitor.
struct Landed {
    arrived: vec3<f32>,
    shown: vec3<f32>,
    covered: bool,
};

fn landed(first: u32, end: u32, p: vec3<f32>) -> Landed {
    var landed = Landed(vec3<f32>(0.0), vec3<f32>(0.0), false);
    for (var t = first; t < end; t++) {
        let tap = u.taps[t];
        let src_uv = vec2<f32>(dot(tap.row0.xyz, p), dot(tap.row1.xyz, p));
        let layer = i32(tap.weight.a);
        let seen = seen_at(src_uv, layer);
        let raw = seen.rgb;
        var signal = raw;

        // The monitor's sharpness, an unsharp mask a texel wide on the signal
        // the switcher hands it. That signal is summed per fragment and never
        // re-read, so the mask is taken per tap from the bank texels. The
        // arms are summed in pairs so four equal samples come back as
        // exactly the centre.
        // Skipped at rest — four reads a texel on every tap of every monitor,
        // for nothing — and where the centre saw no monitor, which is the
        // dark room, and an arm that lands on one would cut a dark rim into
        // whatever else lights the fragment.
        let sharpness = u.analog.x;
        if sharpness > 0.0 && seen.a > 0.5 {
            let texel = 1.0 / vec2<f32>(textureDimensions(lower));
            let across = vec2<f32>(tap.row0.x, tap.row1.x) * texel.x;
            let down = vec2<f32>(tap.row0.y, tap.row1.y) * texel.y;
            let blurred = 0.25
                * ((arm(src_uv + across, layer, raw) + arm(src_uv - across, layer, raw))
                    + (arm(src_uv + down, layer, raw) + arm(src_uv - down, layer, raw)));
            signal += sharpness * (raw - blurred);
        }

        landed.arrived += raw * tap.weight.rgb;
        landed.shown += signal * tap.weight.rgb;
        landed.covered = landed.covered || seen.a > 0.5;
    }
    return landed;
}

// Every tap sampled for the texel at `p`, run through the monitor's chain of
// switchers from the deepest out, each keying the program so far — its In2
// — over its In1; and in alpha whether any tap saw a monitor there.
fn gathered(p: vec3<f32>) -> vec4<f32> {
    var t = u32(u.info.x);
    var program = landed(0u, t, p);
    for (var s = 0u; s < u32(u.chain.x); s++) {
        let stage = u.stages[s];
        let end = u32(stage.in1.x);
        let in1 = landed(t, end, p);
        t = end;
        let level = stage.key.x * passed(stage, program.arrived);
        program = Landed(
            mix(in1.arrived, program.arrived, level),
            mix(in1.shown, program.shown, level),
            in1.covered || program.covered,
        );
    }
    return vec4<f32>(program.shown, select(0.0, 1.0, program.covered));
}

@fragment
fn fs_camera(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(front_panel(gathered(vec3<f32>(in.uv, 1.0)).rgb), 1.0);
}

// One picture onto a delay unit's line: its camera's view through the glass
// and the framing, before the cable, the key or any front panel.
@fragment
fn fs_record(in: VsOut) -> @location(0) vec4<f32> {
    return gathered(vec3<f32>(in.uv, 1.0));
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(
        textureSampleLevel(lower, src_samp, in.uv, i32(u.info.y), 0.0).rgb,
        1.0,
    );
}

@fragment
fn fs_mark(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
