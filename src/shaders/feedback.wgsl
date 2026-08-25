// The feedback graph's passes. `fs_camera` writes one monitor's next frame
// from its taps into the bank of previous frames; `fs_present` copies one
// monitor to a viewport of the window.

// One flattened edge of the graph: a camera's view of one source monitor,
// scaled by everything between them. See feedback::Tap.
struct Tap {
    // Rows of the uv -> uv map from a texel being written to the texel the
    // camera saw there. See affine::sample_transform.
    row0: vec4<f32>,
    row1: vec4<f32>,
    // rgb: routing x splitter x gain, per channel. a: source layer.
    weight: vec4<f32>,
    // Source-uv steps for one bloom radius across (xy) and up (zw) the
    // camera's image. See feedback::Tap.
    halo: vec4<f32>,
    // xy: source-uv step for one chroma-bleed offset along the scanline.
    // z: the fraction of the light the lens scatters. w: padding.
    bleed: vec4<f32>,
};

struct Uniforms {
    // xy: seed centre in uv. zw: seed radii in uv, already aspect-corrected.
    seed: vec4<f32>,
    // Decodes RGB to luma and chroma, turns the chroma by hue and scales it
    // by saturation, and encodes back. See params::Colour::chroma_matrix.
    chroma: mat3x3<f32>,
    // x: brightness. y: contrast. z: phosphor gamma. w: seed brightness.
    levels: vec4<f32>,
    // x: tap count. y: this monitor's own layer, for fs_present.
    info: vec4<f32>,
    // x: grain amplitude. y: the amplifier's headroom. z: frame counter.
    character: vec4<f32>,
    taps: array<Tap, 32>,
};

// FCC NTSC luma. The weights sum to one, so adding the same amount to all
// three channels moves luma by exactly that and leaves the chroma subcarrier
// untouched — which is how the bleed puts one signal's colour on another's
// detail without a matrix. Same row 0 as params::DECODE.
const LUMA = vec3<f32>(0.299, 0.587, 0.114);

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d_array<f32>;
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
// decode, video amplifier, its rails, phosphor.
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
    let h = u.character.y;
    let limited = select(
        h - h * h / (4.0 * amplified),
        amplified,
        amplified < vec3<f32>(0.5 * h),
    );

    // A phosphor emits no negative light, and pow() of a negative is not a
    // number, so the floor here is physics and hygiene at once.
    return pow(max(limited, vec3<f32>(0.0)), vec3<f32>(u.levels.z));
}

// What one camera sees of one source monitor at one point. Past a monitor's
// edge the camera sees an unlit room, but a clamped sampler returns the
// border texel there, which would smear across the frame.
//
// textureSampleLevel, not textureSample, throughout this shader: the monitor
// textures have a single mip level, so the derivatives textureSample computes
// go nowhere — and not needing them is what lets the character samples below
// be skipped on a path that has none.
fn look(uv: vec2<f32>, layer: i32) -> vec3<f32> {
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    return select(
        vec3<f32>(0.0),
        textureSampleLevel(src_tex, src_samp, uv, layer, 0.0).rgb,
        inside,
    );
}

// A cheap integer hash. The grain has to differ at every texel and every
// frame and be the same on every run; nothing else about it matters.
fn grain_at(pixel: vec2<u32>, frame: u32) -> f32 {
    var h = pixel.x * 374761393u + pixel.y * 668265263u + frame * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h) * (2.0 / 4294967296.0) - 1.0;
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
        let raw = look(src_uv, layer);

        // The lens scatters some of the light into a ring instead of
        // focusing it. `mix`, not an add: a term that adds light is a term
        // the loop multiplies. A four-point ring is a crude halo, but the
        // loop turns and rescales it every pass, which is what fills it in.
        //
        // Applied per tap, so a camera looking through a splitter blooms each
        // monitor separately rather than their blend. The two differ only
        // where one source is dark and the other is not: everywhere else the
        // scatter is linear and the sum comes out the same.
        let bloom = tap.bleed.z;
        var halo = raw;
        if bloom > 0.0 {
            halo = 0.25
                * (look(src_uv + tap.halo.xy, layer)
                    + look(src_uv - tap.halo.xy, layer)
                    + look(src_uv + tap.halo.zw, layer)
                    + look(src_uv - tap.halo.zw, layer));
        }
        let lens = mix(raw, halo, bloom);

        // Composite chroma has a fraction of luma's bandwidth, so the colour
        // arrives smeared along the scanline while the detail does not: keep
        // this point's luma, take the neighbourhood's colour. The same halo
        // serves all three samples — it varies slowly by construction, which
        // is what makes it a halo — so the lens is not re-run per sample.
        var signal = lens;
        if any(tap.bleed.xy != vec2<f32>(0.0)) {
            let smeared = mix(
                (raw + look(src_uv + tap.bleed.xy, layer) + look(src_uv - tap.bleed.xy, layer))
                    / 3.0,
                halo,
                bloom,
            );
            signal = smeared + vec3<f32>(dot(LUMA, lens) - dot(LUMA, smeared));
        }
        fed_back += signal * tap.weight.rgb;
    }

    // Grain from the sensors and cables feeding this monitor, in before the
    // front panel because that is where it joins the signal. Monochrome: it
    // is luma noise, and the chroma knobs do nothing to grey.
    let grain = u.character.x * grain_at(vec2<u32>(in.pos.xy), u32(u.character.z));

    let d = length((in.uv - u.seed.xy) / u.seed.zw);
    let seed = u.levels.a * exp(-d * d);

    // The knobs are on the monitor, not on the cameras, so they colour
    // everything the monitor displays — the seed spot included.
    return vec4<f32>(front_panel(fed_back + vec3<f32>(seed + grain)), 1.0);
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(
        textureSampleLevel(src_tex, src_samp, in.uv, i32(u.info.y), 0.0).rgb,
        1.0,
    );
}
