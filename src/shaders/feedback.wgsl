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
    // Source-uv steps for one bloom radius across (xy) and down (zw) the
    // camera's image. See feedback::Tap.
    halo: vec4<f32>,
    // xy: source-uv step for one chroma-bleed offset along the scanline.
    // z: the fraction of the light the lens scatters. w: padding.
    bleed: vec4<f32>,
    // The camera's keyer: x luma threshold, y softness, z chroma tolerance,
    // w padding.
    key: vec4<f32>,
    // xyz: the RGB row measuring the key colour in a sample; all zero when
    // the chroma key is off. See feedback::Tap.
    keyvec: vec4<f32>,
};

struct Uniforms {
    // xy: blob centre in uv. zw: blob radii in uv, already aspect-corrected.
    blob: vec4<f32>,
    // Decodes RGB to luma and chroma, turns the chroma by hue and scales it
    // by saturation, and encodes back. See params::Colour::chroma_matrix.
    chroma: mat3x3<f32>,
    // x: brightness. y: contrast. z: phosphor gamma. w: the blob's brightness.
    levels: vec4<f32>,
    // x: tap count. y: this monitor's own layer, for fs_present. z: the
    // first bank layer past `lower`. w: the first bank layer of `upper`.
    info: vec4<f32>,
    // x: grain amplitude. y: the amplifier's headroom. z: frame counter.
    analog: vec4<f32>,
    // xyz: FCC NTSC luma, handed over rather than written here so the crate
    // has one copy of it. The weights sum to one, so adding the same amount
    // to all three channels moves luma by exactly that and leaves the chroma
    // subcarrier untouched — which is how the bleed puts one signal's colour
    // on another's detail without a matrix.
    luma: vec4<f32>,
    taps: array<Tap, 32>,
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
    let h = u.analog.y;
    let limited = select(
        h - h * h / (4.0 * amplified),
        amplified,
        amplified < vec3<f32>(0.5 * h),
    );

    // A phosphor emits no negative light, and pow() of a negative is not a
    // number, so the floor here is physics and hygiene at once.
    return pow(max(limited, vec3<f32>(0.0)), vec3<f32>(u.levels.z));
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
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    if !inside {
        return vec3<f32>(0.0);
    }
    if layer < i32(u.info.z) {
        return textureSampleLevel(lower, src_samp, uv, layer, 0.0).rgb;
    }
    return textureSampleLevel(upper, src_samp, uv, layer - i32(u.info.w), 0.0).rgb;
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
        let raw = seen_at(src_uv, layer);

        // The lens scatters some of the light into a ring instead of
        // focusing it. `mix`, not an add: a term that adds light is a term
        // the loop multiplies. A four-point ring is a crude halo, but the
        // loop turns and rescales it every pass, which is what fills it in.
        //
        // Applied per tap, and that costs nothing: every stage below is
        // affine in the samples, and a camera's taps share one affine and one
        // set of offsets, so blooming a splitter's two sources separately and
        // blooming their blend give the same answer exactly.
        let bloom = tap.bleed.z;
        var halo = raw;
        if bloom > 0.0 {
            halo = 0.25
                * (seen_at(src_uv + tap.halo.xy, layer)
                    + seen_at(src_uv - tap.halo.xy, layer)
                    + seen_at(src_uv + tap.halo.zw, layer)
                    + seen_at(src_uv - tap.halo.zw, layer));
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
                (raw + seen_at(src_uv + tap.bleed.xy, layer) + seen_at(src_uv - tap.bleed.xy, layer))
                    / 3.0,
                halo,
                bloom,
            );
            signal = smeared + vec3<f32>(dot(u.luma.xyz, lens) - dot(u.luma.xyz, smeared));
        }

        // The keyer, judged on the centre sample: what it gates is the
        // path's whole hand-over, lens and bleed included, the way the gain
        // scales it. The luma key passes everything at or above its
        // threshold and finishes cutting one softness below it, so a
        // threshold of zero is exactly inert — inside a loop, "almost
        // passes" is a ratchet. The chroma key is its mirror: a sample
        // carrying more of the key colour than the tolerance is cut. The
        // epsilon keeps smoothstep's edges apart at zero softness, where it
        // would otherwise divide by nothing.
        let soft = max(tap.key.y, 1e-4);
        let alpha = smoothstep(tap.key.x - soft, tap.key.x, dot(u.luma.xyz, raw))
            * (1.0 - smoothstep(tap.key.z, tap.key.z + soft, dot(tap.keyvec.xyz, raw)));
        fed_back += signal * tap.weight.rgb * alpha;
    }

    // Grain from the sensors and cables feeding this monitor, in before the
    // front panel because that is where it joins the signal. Monochrome: it
    // is luma noise, and the chroma knobs do nothing to grey.
    let grain = u.analog.x * grain_at(vec2<u32>(in.pos.xy), u32(u.analog.z));

    let d = length((in.uv - u.blob.xy) / u.blob.zw);
    let blob = u.levels.a * exp(-d * d);

    // The knobs are on the monitor, not on the cameras, so they colour
    // everything the monitor displays — the white blob included.
    return vec4<f32>(front_panel(fed_back + vec3<f32>(blob + grain)), 1.0);
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(
        textureSampleLevel(lower, src_samp, in.uv, i32(u.info.y), 0.0).rgb,
        1.0,
    );
}
