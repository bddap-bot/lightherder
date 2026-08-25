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
    /// Gain about mid-grey. Deliberately not about black: a gain about black
    /// is what the loop gain already is, and a second one would be the same
    /// knob twice.
    pub contrast: f32,
    /// Phosphor transfer exponent. Above 1 the dark end is crushed and the
    /// trails thin out; below 1 they lift and smear.
    pub gamma: f32,
}

impl Colour {
    /// Every stage off, so the pass writes back what the camera gave it.
    pub const NEUTRAL: Colour = Colour {
        hue: 0.0,
        saturation: 1.0,
        brightness: 0.0,
        contrast: 1.0,
        gamma: 1.0,
    };

    /// The chroma subcarrier as a phasor: hue is its phase and saturation its
    /// amplitude. Scaling and rotating a 2D vector commute, so one complex
    /// multiply on `(I, Q)` is both knobs, and the shader never recomputes a
    /// uniform's sine per fragment.
    pub fn chroma_phasor(&self) -> [f32; 2] {
        let (sin, cos) = self.hue.sin_cos();
        [self.saturation * cos, self.saturation * sin]
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
            Knob::Seed => "seed brightness",
            Knob::Hue => "hue",
            Knob::Saturation => "saturation",
            Knob::Brightness => "brightness",
            Knob::Contrast => "contrast",
            Knob::Gamma => "gamma",
        }
    }

    /// One key press worth of this knob. Rotation is the coarse one: a
    /// thousandth of a radian per press would be imperceptible.
    pub const fn increment(self) -> f32 {
        match self {
            // Hue is the coarse one: a full turn of the subcarrier is a
            // gesture rather than a trim, and at the default step it would
            // take three thousand presses.
            Knob::Hue => 0.02,
            Knob::Rotation | Knob::Seed => 0.005,
            Knob::Saturation | Knob::Contrast | Knob::Gamma => 0.005,
            _ => 0.002,
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

    #[test]
    fn the_neutral_colour_leaves_the_chroma_alone() {
        // 1 + 0i: the complex multiply the shader does with this is identity,
        // which is what makes the whole stage inert at its defaults.
        let phasor = Colour::NEUTRAL.chroma_phasor();
        assert!(
            (phasor[0] - 1.0).abs() < 1e-6 && phasor[1].abs() < 1e-6,
            "{phasor:?}"
        );
    }

    #[test]
    fn the_chroma_phasor_is_hue_by_saturation() {
        let colour = Colour {
            hue: core::f32::consts::FRAC_PI_2,
            saturation: 0.5,
            ..Colour::NEUTRAL
        };
        // A quarter turn puts all of it on the imaginary axis, and the
        // saturation is its length.
        let [re, im] = colour.chroma_phasor();
        assert!(re.abs() < 1e-6, "re {re}");
        assert!((im - 0.5).abs() < 1e-6, "im {im}");
        assert!((re.hypot(im) - colour.saturation).abs() < 1e-6);
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
