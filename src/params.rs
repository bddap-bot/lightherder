//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a keyboard does.

use crate::affine::Framing;

/// The colour controls on one monitor's front panel, in the order an analog
/// signal meets them: chroma decode, video amplifier, phosphor. One monitor
/// for now; the multi-monitor stage gives each its own.
///
/// None of these is the loop gain wearing a hat. The gain is a per-channel
/// multiply of the light coming *back*, and is what puts any chroma into a
/// white seed's trail at all; these turn the chroma that is already there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour {
    /// Phase of the chroma subcarrier, in radians. Turning it a little each
    /// pass walks a feedback trail through the spectrum.
    pub hue: f32,
    /// Amplitude of the chroma subcarrier. 0 is monochrome; above 1 pushes
    /// colours past the gamut the camera handed over.
    pub saturation: f32,
    /// Black level: light added to every texel, which no gain can do. This is
    /// the knob that lifts a dying loop back off the floor.
    pub brightness: f32,
    /// Gain about mid-grey, which is what gives it a fixed point the loop
    /// gain has not got. See the shader for why that is the distinction that
    /// matters.
    pub contrast: f32,
    /// Phosphor transfer exponent. Above 1 the dark end is crushed and the
    /// trails thin out; below 1 they lift and smear.
    pub gamma: f32,
}

/// FCC NTSC luma and the two colour-difference axes. Row 0 is luma; rows 1
/// and 2 are the real and imaginary parts of one chroma subcarrier, which is
/// what lets hue be a phase.
const DECODE: [[f64; 3]; 3] = [
    [0.299, 0.587, 0.114],
    [0.5959, -0.2746, -0.3213],
    [0.2115, -0.5227, 0.3112],
];

/// Its exact inverse, to more digits than anyone publishes. The published
/// four-figure version is off by 1e-4, which sounds like nothing until it
/// runs inside a loop that feeds itself: a hundred passes of it costs a fifth
/// of the blue channel.
const ENCODE: [[f64; 3]; 3] = [
    [1.0, 0.9560502263958943, 0.6207549413271234],
    [1.0, -0.27205234368892417, -0.6472057134551779],
    [1.0, -1.1067043153243328, 1.704421283696311],
];

impl Colour {
    /// Every stage off, so the pass writes back what the camera gave it.
    pub const NEUTRAL: Colour = Colour {
        hue: 0.0,
        saturation: 1.0,
        brightness: 0.0,
        contrast: 1.0,
        gamma: 1.0,
    };

    /// The 3x3 the shader multiplies RGB by: decode, turn the chroma by hue
    /// and scale it by saturation, encode back. Indexed `m[row][col]`.
    ///
    /// Composed here, in f64, once a frame — not left as three steps in the
    /// shader. Chained in f32 per fragment the three matrices leave a
    /// ten-thousandth of the signal behind on a value near half scale, which
    /// is under half a step of the loop's half-float storage until it is not,
    /// and then the loop ratchets one step down per pass. Composed first, the
    /// neutral case is exactly the identity and there is nothing to ratchet.
    pub fn chroma_matrix(&self) -> [[f32; 3]; 3] {
        let (sin, cos) = (self.hue as f64).sin_cos();
        let saturation = self.saturation as f64;
        let (turn, lift) = (saturation * cos, saturation * sin);
        std::array::from_fn(|row| {
            // Turning and scaling the subcarrier is one complex multiply, so
            // it folds into the pair of chroma weights this row encodes with.
            let (i, q) = (ENCODE[row][1], ENCODE[row][2]);
            let (i, q) = (i * turn + q * lift, q * turn - i * lift);
            std::array::from_fn(|col| {
                (DECODE[0][col] + i * DECODE[1][col] + q * DECODE[2][col]) as f32
            })
        })
    }
}

/// Everything the feedback pass needs to know for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub framing: Framing,
    /// Per-channel gain applied once per pass. With the seed off, below 1.0
    /// the image dies out and above 1.0 it blooms; with the seed on the loop
    /// settles instead, brighter the closer the gain is to 1.0. The channels
    /// differ to colour the trails.
    pub loop_gain: [f32; 3],
    /// Brightness of the seed spot, which is the only thing keeping a
    /// sub-unity loop alive.
    pub seed_brightness: f32,
    pub colour: Colour,
}

impl Default for Params {
    /// A framing that already moves: the camera pulls back a little and turns
    /// a little each pass, so the seed leaves a spiral of shrinking copies
    /// behind it. Gains sit just under unity, spread across the channels so
    /// the trail cools from white to blue as it winds in.
    fn default() -> Self {
        Params {
            framing: Framing {
                zoom: 0.994,
                rotation: 0.05,
                translate: [0.0, 0.0],
            },
            loop_gain: [0.980, 0.986, 0.992],
            seed_brightness: 0.10,
            // The colour stage starts out of the way. What it does is seen by
            // turning one knob against a loop that already works, and the
            // spiral this default renders is that loop.
            colour: Colour::NEUTRAL,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knob {
    Zoom,
    Rotation,
    TranslateX,
    TranslateY,
    /// All three channels at once, rigidly.
    Gain,
    GainR,
    GainG,
    GainB,
    Seed,
    Hue,
    Saturation,
    Brightness,
    Contrast,
    Gamma,
}

/// What a knob does when it runs out of room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Limit {
    Clamp(f32, f32),
    Wrap,
}

impl Knob {
    /// Every `for knob in ALL` test is silently vacuous for a knob missing
    /// from this list, including the ones that exist to catch omissions.
    pub const ALL: [Knob; 14] = [
        Knob::Zoom,
        Knob::Rotation,
        Knob::TranslateX,
        Knob::TranslateY,
        Knob::Gain,
        Knob::GainR,
        Knob::GainG,
        Knob::GainB,
        Knob::Seed,
        Knob::Hue,
        Knob::Saturation,
        Knob::Brightness,
        Knob::Contrast,
        Knob::Gamma,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Knob::Zoom => "zoom",
            Knob::Rotation => "rotation",
            Knob::TranslateX => "pan x",
            Knob::TranslateY => "pan y",
            Knob::Gain => "loop gain",
            Knob::GainR => "loop gain, red",
            Knob::GainG => "loop gain, green",
            Knob::GainB => "loop gain, blue",
            Knob::Seed => "seed",
            Knob::Hue => "hue",
            Knob::Saturation => "saturation",
            Knob::Brightness => "brightness",
            Knob::Contrast => "contrast",
            Knob::Gamma => "gamma",
        }
    }

    /// One key press worth of this knob. Spelled out rather than defaulted,
    /// so a knob added later cannot quietly inherit a step nobody chose.
    pub const fn increment(self) -> f32 {
        match self {
            // A full turn of the subcarrier is a gesture rather than a trim,
            // and at the default step it would take three thousand presses.
            Knob::Hue => 0.02,
            // Coarse enough to see: a thousandth of a radian, or of a decade
            // of phosphor curve, is imperceptible.
            Knob::Rotation | Knob::Seed | Knob::Saturation | Knob::Contrast | Knob::Gamma => 0.005,
            Knob::Zoom
            | Knob::TranslateX
            | Knob::TranslateY
            | Knob::Gain
            | Knob::GainR
            | Knob::GainG
            | Knob::GainB
            | Knob::Brightness => 0.002,
        }
    }

    pub const fn limit(self) -> Limit {
        match self {
            // Zero would divide by zero in the sampling transform.
            Knob::Zoom => Limit::Clamp(0.25, 4.0),
            // Spinning one way for long enough must not run the number away.
            Knob::Rotation => Limit::Wrap,
            Knob::TranslateX | Knob::TranslateY => Limit::Clamp(-1.0, 1.0),
            Knob::Gain | Knob::GainR | Knob::GainG | Knob::GainB => Limit::Clamp(0.0, 1.2),
            Knob::Seed => Limit::Clamp(0.0, 1.0),
            // A phase: it comes back round instead of running away.
            Knob::Hue => Limit::Wrap,
            Knob::Saturation | Knob::Contrast => Limit::Clamp(0.0, 4.0),
            // Potent inside a loop, so the rails are close: a tenth of a unit
            // added every pass floods the monitor to white in under a second.
            Knob::Brightness => Limit::Clamp(-0.5, 0.5),
            // Zero would flatten every level to 1.0, and a phosphor curve
            // worth playing lives nowhere near either rail.
            Knob::Gamma => Limit::Clamp(0.25, 4.0),
        }
    }
}

impl Params {
    pub fn nudge(&mut self, knob: Knob, delta: f32) {
        // The rigid gain knob is the one that is not a single value: clamp its
        // step once against the tightest channel, so hitting the rail slides
        // all three together instead of flattening the colour offsets.
        if knob == Knob::Gain {
            let step = self.rigid_gain_step(delta);
            for channel in [Knob::GainR, Knob::GainG, Knob::GainB] {
                self.nudge(channel, step);
            }
            return;
        }
        let field = self.knob_mut(knob).expect("only Gain has no single value");
        *field = match knob.limit() {
            Limit::Clamp(low, high) => (*field + delta).clamp(low, high),
            Limit::Wrap => wrap_pi(*field + delta),
        };
    }

    /// The value a knob turns, for the knobs that are a single number.
    fn knob_mut(&mut self, knob: Knob) -> Option<&mut f32> {
        match knob {
            Knob::Zoom => Some(&mut self.framing.zoom),
            Knob::Rotation => Some(&mut self.framing.rotation),
            Knob::TranslateX => Some(&mut self.framing.translate[0]),
            Knob::TranslateY => Some(&mut self.framing.translate[1]),
            Knob::GainR => Some(&mut self.loop_gain[0]),
            Knob::GainG => Some(&mut self.loop_gain[1]),
            Knob::GainB => Some(&mut self.loop_gain[2]),
            Knob::Seed => Some(&mut self.seed_brightness),
            Knob::Hue => Some(&mut self.colour.hue),
            Knob::Saturation => Some(&mut self.colour.saturation),
            Knob::Brightness => Some(&mut self.colour.brightness),
            Knob::Contrast => Some(&mut self.colour.contrast),
            Knob::Gamma => Some(&mut self.colour.gamma),
            Knob::Gain => None,
        }
    }

    fn rigid_gain_step(&self, delta: f32) -> f32 {
        let Limit::Clamp(low, high) = Knob::Gain.limit() else {
            unreachable!("gain clamps")
        };
        let headroom = self
            .loop_gain
            .iter()
            .map(|c| if delta >= 0.0 { high - c } else { c - low })
            .fold(f32::INFINITY, f32::min)
            .max(0.0);
        delta.abs().min(headroom) * delta.signum()
    }

    pub fn describe(&self) -> String {
        format!(
            "zoom {:.3}  rot {:+.3}  pan {:+.3},{:+.3}  gain {:.3},{:.3},{:.3}  seed {:.3}  \
             hue {:+.3}  sat {:.3}  bright {:+.3}  contrast {:.3}  gamma {:.3}",
            self.framing.zoom,
            self.framing.rotation,
            self.framing.translate[0],
            self.framing.translate[1],
            self.loop_gain[0],
            self.loop_gain[1],
            self.loop_gain[2],
            self.seed_brightness,
            self.colour.hue,
            self.colour.saturation,
            self.colour.brightness,
            self.colour.contrast,
            self.colour.gamma,
        )
    }
}

/// Into `(-pi, pi]`, so a knob spun in one direction never runs away.
fn wrap_pi(radians: f32) -> f32 {
    use core::f32::consts::{PI, TAU};
    let wrapped = (PI - radians).rem_euclid(TAU);
    PI - wrapped
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::PI;

    #[test]
    fn every_knob_moves_something() {
        for knob in Knob::ALL {
            let mut p = Params::default();
            p.nudge(knob, 0.01);
            assert_ne!(p, Params::default(), "{knob:?} did nothing");
        }
    }

    #[test]
    fn knobs_stop_at_their_limits() {
        let mut p = Params::default();
        for _ in 0..10_000 {
            for knob in Knob::ALL {
                p.nudge(knob, 1.0);
            }
        }
        assert_eq!(p.framing.zoom, 4.0);
        assert_eq!(p.loop_gain, [1.2; 3]);
        assert_eq!(p.seed_brightness, 1.0);
        assert_eq!(p.framing.translate, [1.0, 1.0]);
        assert_eq!(p.colour.saturation, 4.0);
        assert_eq!(p.colour.brightness, 0.5);
        assert_eq!(p.colour.contrast, 4.0);
        assert_eq!(p.colour.gamma, 4.0);

        for _ in 0..10_000 {
            for knob in Knob::ALL {
                p.nudge(knob, -1.0);
            }
        }
        assert_eq!(p.framing.zoom, 0.25);
        assert_eq!(p.loop_gain, [0.0; 3]);
        assert_eq!(p.seed_brightness, 0.0);
        assert_eq!(p.framing.translate, [-1.0, -1.0]);
        assert_eq!(p.colour.saturation, 0.0);
        assert_eq!(p.colour.brightness, -0.5);
        assert_eq!(p.colour.contrast, 0.0);
        assert_eq!(p.colour.gamma, 0.25);
    }
    #[test]
    fn the_rigid_gain_knob_moves_the_way_it_is_pushed() {
        let mut p = Params::default();
        let before = p.loop_gain;
        p.nudge(Knob::Gain, -0.01);
        for (after, before) in p.loop_gain.iter().zip(before) {
            assert!(*after < before, "down should lower {before}, got {after}");
        }
        p.nudge(Knob::Gain, 0.02);
        for (after, before) in p.loop_gain.iter().zip(before) {
            assert!(*after > before, "up should raise {before}, got {after}");
        }
    }
    #[test]
    fn the_rigid_gain_knob_keeps_its_colour_offsets_at_the_rail() {
        let mut p = Params::default();
        let spread = [
            p.loop_gain[1] - p.loop_gain[0],
            p.loop_gain[2] - p.loop_gain[1],
        ];
        for _ in 0..10_000 {
            p.nudge(Knob::Gain, 0.01);
        }
        assert_eq!(
            p.loop_gain[2], 1.2,
            "the leading channel should reach the top"
        );
        assert!((p.loop_gain[1] - p.loop_gain[0] - spread[0]).abs() < 1e-4);
        assert!((p.loop_gain[2] - p.loop_gain[1] - spread[1]).abs() < 1e-4);
    }

    #[test]
    fn rotation_wraps_instead_of_running_away() {
        let mut p = Params::default();
        p.framing.rotation = 0.0;
        for _ in 0..10_000 {
            p.nudge(Knob::Rotation, 0.5);
            assert!(p.framing.rotation > -PI && p.framing.rotation <= PI);
        }
        assert!((p.framing.rotation - wrap_pi(5000.0)).abs() < 1e-2);
    }

    #[test]
    fn wrap_pi_keeps_the_boundaries_it_promises() {
        assert!((wrap_pi(PI) - PI).abs() < 1e-6);
        assert!((wrap_pi(-PI) - PI).abs() < 1e-6);
        assert!(wrap_pi(0.0).abs() < 1e-6);
        assert!((wrap_pi(PI + 0.1) - (-PI + 0.1)).abs() < 1e-5);
    }

    #[test]
    fn a_channel_knob_moves_only_its_channel() {
        let mut p = Params::default();
        let before = p.loop_gain;
        p.nudge(Knob::GainG, 0.1);
        assert_eq!(p.loop_gain[0], before[0]);
        assert_eq!(p.loop_gain[2], before[2]);
        assert!((p.loop_gain[1] - (before[1] + 0.1)).abs() < 1e-6);
    }

    #[test]
    fn the_log_line_shows_every_knob() {
        // The log line is the only readout the instrument has, so a knob
        // missing from it is a knob that cannot be played.
        for knob in Knob::ALL {
            let mut p = Params::default();
            let before = p.describe();
            p.nudge(knob, 0.05);
            assert_ne!(
                p.describe(),
                before,
                "{} is not in the log line",
                knob.name()
            );
        }
    }

    /// Settings that between them exercise both signs of the phase, a
    /// saturation under 1 and one over.
    const SOME_COLOURS: [Colour; 4] = [
        Colour::NEUTRAL,
        Colour {
            hue: 1.1,
            saturation: 0.3,
            ..Colour::NEUTRAL
        },
        Colour {
            hue: -2.0,
            saturation: 2.5,
            ..Colour::NEUTRAL
        },
        Colour {
            saturation: 0.0,
            ..Colour::NEUTRAL
        },
    ];

    #[test]
    fn the_encode_matrix_is_the_decode_matrix_inverted() {
        // Transcribed rather than computed, and it is the only reason the
        // neutral matrix comes out identity, so it gets checked.
        for (row, weights) in ENCODE.iter().enumerate() {
            for col in 0..3 {
                let product: f64 = weights
                    .iter()
                    .zip(DECODE)
                    .map(|(weight, axis)| weight * axis[col])
                    .sum();
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (product - expected).abs() < 1e-12,
                    "row {row} times column {col} is {product}, not {expected}"
                );
            }
        }
    }

    #[test]
    fn the_neutral_chroma_matrix_is_exactly_the_identity() {
        // Nearly is not enough. The stage runs on every pass of a loop that
        // feeds itself, so a residual does not stay a residual.
        for (row, weights) in Colour::NEUTRAL.chroma_matrix().iter().enumerate() {
            for (col, weight) in weights.iter().enumerate() {
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (weight - expected).abs() < 1e-9,
                    "m[{row}][{col}] is {weight}"
                );
            }
        }
    }

    #[test]
    fn the_chroma_matrix_holds_grey_and_luma_whatever_the_knobs_say() {
        // Grey has no chroma to turn, so every row sums to one; and the knobs
        // move light between the channels without changing how much there is,
        // so luma survives them. Both hold for every setting, which is what
        // makes hue a phase rather than a mixing knob.
        for colour in SOME_COLOURS {
            let m = colour.chroma_matrix();
            for row in m {
                let grey: f32 = row.iter().sum();
                assert!((grey - 1.0).abs() < 1e-5, "{colour:?}: row {row:?}");
            }
            for (col, luma) in DECODE[0].iter().enumerate() {
                let out: f32 = m
                    .iter()
                    .zip(DECODE[0])
                    .map(|(row, weight)| weight as f32 * row[col])
                    .sum();
                assert!(
                    (out - *luma as f32).abs() < 1e-5,
                    "{colour:?}: luma weight {col} became {out}"
                );
            }
        }
    }

    #[test]
    fn saturation_at_zero_leaves_luma_and_nothing_else() {
        let m = Colour {
            saturation: 0.0,
            ..Colour::NEUTRAL
        }
        .chroma_matrix();
        for row in m {
            for (weight, luma) in row.iter().zip(DECODE[0]) {
                assert!((weight - luma as f32).abs() < 1e-5, "row {row:?}");
            }
        }
    }

    #[test]
    fn hue_wraps_instead_of_running_away() {
        let mut p = Params::default();
        for _ in 0..10_000 {
            p.nudge(Knob::Hue, 0.5);
            assert!(p.colour.hue > -PI && p.colour.hue <= PI);
        }
    }

    #[test]
    fn the_default_loop_is_contracting() {
        // A gain at or above 1.0 blooms without bound. The image is what
        // finally clips, which only the GPU tests can see, but a runaway
        // default is visible from here.
        for gain in Params::default().loop_gain {
            assert!(gain < 1.0, "default gain {gain} never settles");
            assert!(
                gain > 0.9,
                "default gain {gain} leaves no trail worth seeing"
            );
        }
    }
}
