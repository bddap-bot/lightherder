//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a keyboard does.

use crate::affine::Framing;
use core::ops::RangeInclusive;

/// Everything the feedback pass needs to know for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub framing: Framing,
    /// Per-channel gain applied once per pass. Below 1.0 the image dies out,
    /// above 1.0 it blooms; the channels differ to colour the trails.
    pub loop_gain: [f32; 3],
    /// Brightness of the spot injected at the centre, which is the only thing
    /// keeping a sub-unity loop alive.
    pub seed_brightness: f32,
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
}

impl Knob {
    pub const ALL: [Knob; 9] = [
        Knob::Zoom,
        Knob::Rotation,
        Knob::TranslateX,
        Knob::TranslateY,
        Knob::Gain,
        Knob::GainR,
        Knob::GainG,
        Knob::GainB,
        Knob::Seed,
    ];

    /// One key press worth of this knob. Zoom and gain are the sensitive
    /// ones: a few thousandths decides whether the loop collapses, stands
    /// still or runs away.
    pub const fn increment(self) -> f32 {
        match self {
            Knob::Rotation => 0.005,
            _ => 0.002,
        }
    }

    /// Hard limits, or `None` for a knob that wraps rather than stopping.
    pub fn range(self) -> Option<RangeInclusive<f32>> {
        match self {
            // Zero would divide by zero in the sampling transform.
            Knob::Zoom => Some(0.25..=4.0),
            Knob::Rotation => None,
            Knob::TranslateX | Knob::TranslateY => Some(-1.0..=1.0),
            Knob::Gain | Knob::GainR | Knob::GainG | Knob::GainB => Some(0.0..=1.2),
            Knob::Seed => Some(0.0..=0.25),
        }
    }
}

impl Params {
    pub fn nudge(&mut self, knob: Knob, delta: f32) {
        match knob {
            Knob::Rotation => self.framing.rotation = wrap_pi(self.framing.rotation + delta),
            // Clamp the step once against the tightest channel, so hitting the
            // rail slides all three together instead of flattening the colour
            // offsets the user dialled in.
            Knob::Gain => {
                let step = self.rigid_gain_step(delta);
                for channel in [Knob::GainR, Knob::GainG, Knob::GainB] {
                    self.nudge(channel, step);
                }
            }
            knob => {
                if let (Some(field), Some(range)) = (self.knob_mut(knob), knob.range()) {
                    *field = (*field + delta).clamp(*range.start(), *range.end());
                }
            }
        }
    }

    /// The value a knob turns, for the knobs that are a single number.
    fn knob_mut(&mut self, knob: Knob) -> Option<&mut f32> {
        match knob {
            Knob::Zoom => Some(&mut self.framing.zoom),
            Knob::TranslateX => Some(&mut self.framing.translate[0]),
            Knob::TranslateY => Some(&mut self.framing.translate[1]),
            Knob::GainR => Some(&mut self.loop_gain[0]),
            Knob::GainG => Some(&mut self.loop_gain[1]),
            Knob::GainB => Some(&mut self.loop_gain[2]),
            Knob::Seed => Some(&mut self.seed_brightness),
            Knob::Rotation | Knob::Gain => None,
        }
    }

    fn rigid_gain_step(&self, delta: f32) -> f32 {
        let range = Knob::Gain.range().expect("gain is bounded");
        let headroom = self
            .loop_gain
            .iter()
            .map(|c| {
                if delta >= 0.0 {
                    range.end() - c
                } else {
                    c - range.start()
                }
            })
            .fold(f32::INFINITY, f32::min)
            .max(0.0);
        delta.abs().min(headroom) * delta.signum()
    }

    pub fn describe(&self) -> String {
        format!(
            "zoom {:.3}  rot {:+.3}  pan {:+.3},{:+.3}  gain {:.3},{:.3},{:.3}  seed {:.3}",
            self.framing.zoom,
            self.framing.rotation,
            self.framing.translate[0],
            self.framing.translate[1],
            self.loop_gain[0],
            self.loop_gain[1],
            self.loop_gain[2],
            self.seed_brightness,
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
        assert_eq!(p.seed_brightness, 0.25);
        assert_eq!(p.framing.translate, [1.0, 1.0]);

        for _ in 0..10_000 {
            for knob in Knob::ALL {
                p.nudge(knob, -1.0);
            }
        }
        assert_eq!(p.framing.zoom, 0.25);
        assert_eq!(p.loop_gain, [0.0; 3]);
        assert_eq!(p.seed_brightness, 0.0);
        assert_eq!(p.framing.translate, [-1.0, -1.0]);
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
