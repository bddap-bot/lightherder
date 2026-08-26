//! Automation: the knobs that turn themselves.
//!
//! There is no separate kinetics here. A camera on a motor is its rotation
//! swept through a full turn, which is a ramp at full depth on the rotation
//! knob — so "continuous camera rotation" and "an LFO on any parameter" are
//! one mechanism, and the instrument has one of it rather than two that drift.
//!
//! An LFO does not own its knob. It is an offset added to whatever the hand
//! left there, recomputed from the stored value every frame. That is what
//! keeps a swing from compounding, keeps a saved preset the knobs rather than
//! wherever the swing happened to be when it was written, and leaves the keys
//! that turn a knob still turning it while it moves.

use serde::{Deserialize, Serialize};

use crate::params::{Focus, Knob, Limit};

/// The swing over one cycle.
///
/// Two, and they are the two the stage is named for: a sine is the wobble, a
/// ramp is the turn. A triangle is a sine with corners and was left out; a
/// square is a genuinely different gesture — a switcher stuttering rather
/// than a knob moving — and will be a third arm the day it is wanted, not a
/// flavour shipped ahead of a reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Sine,
    Ramp,
}

impl Shape {
    pub const ALL: [Shape; 2] = [Shape::Sine, Shape::Ramp];

    pub const fn name(self) -> &'static str {
        match self {
            Shape::Sine => "sine",
            Shape::Ramp => "ramp",
        }
    }

    /// The swing at `phase` cycles into the cycle, in `[-1, 1]`.
    fn at(self, phase: f64) -> f64 {
        match self {
            Shape::Sine => (phase * std::f64::consts::TAU).sin(),
            // Ends where it began, one turn on, which is why a ramp on a
            // wrapping knob is a continuous rotation rather than a sweep with
            // a jump in it.
            Shape::Ramp => 2.0 * phase - 1.0,
        }
    }
}

/// One knob turning itself: how far it swings, how fast, and where in the
/// cycle it was when the instrument started.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lfo {
    pub knob: Knob,
    /// Which node's knob. Only the half `knob` reads may be set — see
    /// [`Focus::narrowed`], and `config::validate` holds the line — so that
    /// the same automation is found by the same lookup however the focus has
    /// been walked since.
    #[serde(default)]
    pub focus: Focus,
    #[serde(default)]
    pub shape: Shape,
    /// Cycles per second.
    pub rate: f32,
    /// Half the swing, in the knob's own units. The knob's limit still
    /// applies, so a swing wider than the knob's travel flattens against it
    /// rather than running away.
    pub depth: f32,
    /// Where in the cycle this is at time zero, in cycles. Two LFOs at one
    /// rate and a quarter cycle apart is how a pan becomes a circle.
    #[serde(default)]
    pub phase: f32,
}

impl Lfo {
    /// Nothing faster than half the frame rate: past Nyquist an LFO does not
    /// go faster, it goes somewhere else. Zero is a legal rate — a frozen
    /// swing is a plain offset — but the keys never produce one, since
    /// turning the automation off is what the off switch is for.
    pub const RATE: Limit = Limit::Clamp(0.0, 30.0);

    /// A hundred seconds a cycle, and the floor the rate keys work up from.
    pub const SLOWEST: f32 = 0.01;

    /// A ratio per key press, not a step: rates worth playing span three
    /// decades, and one linear step is either useless at the slow end or
    /// unreachable at the fast one.
    pub const RATE_RATIO: f32 = 1.06;

    /// Presses from silent to the widest swing the knob has.
    const DEPTH_STEPS: f32 = 64.0;

    /// Freshly switched on: slow enough to read as motion rather than
    /// flicker, and a quarter of the knob's travel — obvious at a glance
    /// without swinging a feedback loop straight to black or to white.
    pub fn new(knob: Knob, focus: Focus, shape: Shape) -> Lfo {
        Lfo {
            knob,
            focus: focus.narrowed(knob),
            shape,
            rate: 0.05,
            depth: Lfo::depth_limit(knob) / 4.0,
            phase: 0.0,
        }
    }

    /// The offset this adds to its knob `seconds` after the instrument
    /// started. In f64 because a performance is long: an hour into it, f32
    /// seconds have lost the resolution a 30 Hz cycle is made of.
    pub fn offset(&self, seconds: f64) -> f32 {
        let phase = (self.rate as f64 * seconds + self.phase as f64).rem_euclid(1.0);
        (self.depth as f64 * self.shape.at(phase)) as f32
    }

    /// The widest swing that means anything on `knob`: its whole travel, or a
    /// half turn for a knob that wraps, where a ramp then makes exactly one
    /// revolution per cycle. Read from the knob's own limit rather than
    /// tabulated again, so a knob whose range moves takes its automation with
    /// it.
    pub fn depth_limit(knob: Knob) -> f32 {
        match knob.limit() {
            Limit::Clamp(low, high) => high - low,
            Limit::Wrap => std::f32::consts::PI,
        }
    }

    /// Move the rate by `steps` presses, geometrically.
    pub fn scale_rate(&mut self, steps: f32) {
        let Limit::Clamp(_, fastest) = Lfo::RATE else {
            unreachable!("the rate clamps")
        };
        let rate = self.rate.max(Lfo::SLOWEST) * Lfo::RATE_RATIO.powf(steps);
        self.rate = rate.clamp(Lfo::SLOWEST, fastest);
    }

    /// Move the depth by `steps` presses, as a fraction of this knob's own
    /// travel — so one press means the same amount of swing on a knob that
    /// runs 0 to 4 as on one that runs 0 to 0.25.
    pub fn nudge_depth(&mut self, steps: f32) {
        let most = Lfo::depth_limit(self.knob);
        self.depth = (self.depth + steps * most / Lfo::DEPTH_STEPS).clamp(0.0, most);
    }

    /// One line of the instrument's readout.
    pub fn describe(&self) -> String {
        let node = if self.knob.is_camera() {
            format!("cam {}", self.focus.camera + 1)
        } else {
            format!("mon {}", self.focus.monitor + 1)
        };
        format!(
            "motion: {} {} {:.3} Hz +-{:.3} on {node}",
            self.knob.name(),
            self.shape.name(),
            self.rate,
            self.depth,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Params;

    #[test]
    fn a_shape_stays_inside_the_swing_it_is_given() {
        for shape in Shape::ALL {
            for i in 0..1000 {
                let at = shape.at(i as f64 / 1000.0);
                assert!((-1.0..=1.0).contains(&at), "{shape:?} at {i}: {at}");
            }
        }
    }

    #[test]
    fn a_sine_is_centred_and_a_ramp_climbs() {
        // A cycle of the sine sums to nothing, so the knob it drives is on
        // average where the hand left it. The ramp is what a motor does:
        // strictly increasing across the cycle, ending one full swing on.
        let steps = 1000;
        let mean: f64 = (0..steps)
            .map(|i| Shape::Sine.at(i as f64 / steps as f64))
            .sum::<f64>()
            / steps as f64;
        assert!(mean.abs() < 1e-9, "the sine is off centre by {mean}");

        let mut last = f64::NEG_INFINITY;
        for i in 0..steps {
            let at = Shape::Ramp.at(i as f64 / steps as f64);
            assert!(at > last, "the ramp went back at {i}");
            last = at;
        }
        assert!((Shape::Ramp.at(0.0) - -1.0).abs() < 1e-12);
        assert!((Shape::Ramp.at(1.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_shape_spells_itself_the_way_it_is_documented() {
        // The name in the readout and the name in a config file are the same
        // name, and the README writes them literally.
        for shape in Shape::ALL {
            let text = toml::to_string(&Lfo::new(Knob::Hue, Focus::default(), shape)).unwrap();
            assert!(
                text.contains(&format!("shape = \"{}\"", shape.name())),
                "{shape:?} writes itself as {text}"
            );
        }
    }

    #[test]
    fn an_lfo_repeats_at_its_rate() {
        let mut lfo = Lfo::new(Knob::Hue, Focus::default(), Shape::Sine);
        lfo.rate = 0.25;
        for i in 0..20 {
            let t = i as f64 * 0.37;
            let a = lfo.offset(t);
            let b = lfo.offset(t + 4.0);
            assert!((a - b).abs() < 1e-5, "at {t}: {a} then {b}");
        }
    }

    #[test]
    fn phase_offsets_two_lfos_against_each_other() {
        // A quarter cycle apart on two axes is a circle, which is the one
        // thing `phase` exists for.
        let mut x = Lfo::new(Knob::TranslateX, Focus::default(), Shape::Sine);
        x.rate = 0.5;
        let mut y = x;
        y.knob = Knob::TranslateY;
        y.phase = 0.25;
        for i in 0..50 {
            let t = i as f64 * 0.13;
            let (a, b) = (x.offset(t) / x.depth, y.offset(t) / y.depth);
            assert!(
                (a * a + b * b - 1.0).abs() < 1e-4,
                "at {t}: {a},{b} is not on the circle"
            );
        }
    }

    #[test]
    fn a_frozen_lfo_is_a_plain_offset() {
        let mut lfo = Lfo::new(Knob::Zoom, Focus::default(), Shape::Sine);
        lfo.rate = 0.0;
        lfo.phase = 0.25;
        for i in 0..10 {
            assert!((lfo.offset(i as f64 * 100.0) - lfo.depth).abs() < 1e-5);
        }
    }

    #[test]
    fn the_rate_keys_reach_both_ends_and_stop() {
        let Limit::Clamp(_, fastest) = Lfo::RATE else {
            unreachable!()
        };
        let mut lfo = Lfo::new(Knob::Hue, Focus::default(), Shape::Sine);
        for _ in 0..1000 {
            lfo.scale_rate(1.0);
        }
        assert_eq!(lfo.rate, fastest);
        for _ in 0..1000 {
            lfo.scale_rate(-1.0);
        }
        assert_eq!(lfo.rate, Lfo::SLOWEST);
        // And the trip either way is a performer's worth of presses, not a
        // thousand: this is the whole reason the step is a ratio.
        let mut lfo = Lfo::new(Knob::Hue, Focus::default(), Shape::Sine);
        let mut presses = 0;
        while lfo.rate < 5.0 {
            lfo.scale_rate(1.0);
            presses += 1;
        }
        assert!(presses < 100, "{presses} presses to reach 5 Hz");
    }

    #[test]
    fn the_depth_keys_reach_both_ends_and_stop() {
        for knob in Knob::ALL {
            let most = Lfo::depth_limit(knob);
            let mut lfo = Lfo::new(knob, Focus::default(), Shape::Sine);
            for _ in 0..1000 {
                lfo.nudge_depth(1.0);
            }
            assert_eq!(lfo.depth, most, "{knob:?} did not reach its widest");
            for _ in 0..1000 {
                lfo.nudge_depth(-1.0);
            }
            assert_eq!(lfo.depth, 0.0, "{knob:?} did not reach silence");
        }
    }

    #[test]
    fn a_full_depth_ramp_on_the_rotation_knob_is_one_turn_a_cycle() {
        // The stage's headline claim, and the reason there is no separate
        // kinetics: over one cycle the camera's rotation covers exactly one
        // revolution, in even steps, and closes.
        use std::f32::consts::{PI, TAU};
        let mut params = crate::config::single();
        params.cameras[0].framing.rotation = 0.0;
        params.motion = vec![Lfo {
            depth: PI,
            rate: 1.0,
            ..Lfo::new(Knob::Rotation, Focus::default(), Shape::Ramp)
        }];
        let angle = |t: f64| params.modulated(t).cameras[0].framing.rotation;
        let steps = 360;
        let mut turned = 0.0;
        for i in 0..steps {
            let (from, to) = (angle(i as f64 / 360.0), angle((i + 1) as f64 / 360.0));
            // The shortest way round, which is the only way to measure a
            // wrapping angle without reading its wrap as a jump.
            let step = (to - from + PI).rem_euclid(TAU) - PI;
            assert!(step > 0.0, "step {i} went backwards: {step}");
            assert!(
                (step - TAU / steps as f32).abs() < 1e-3,
                "step {i} is {step}, not even"
            );
            turned += step;
        }
        assert!((turned - TAU).abs() < 1e-2, "one cycle turned {turned}");
        assert!(
            (angle(1.0) - angle(0.0)).abs() < 1e-4,
            "the turn does not close"
        );
    }

    #[test]
    fn automation_offsets_the_knobs_instead_of_owning_them() {
        // The property the whole design rests on: the stored value is
        // untouched, and the swing is measured from it rather than from
        // wherever the last frame left it.
        let mut params = crate::config::single();
        params.monitors[0].colour.hue = 0.4;
        params.motion = vec![Lfo::new(Knob::Hue, Focus::default(), Shape::Sine)];
        let depth = params.motion[0].depth;
        for i in 0..10_000 {
            let hue = params.modulated(i as f64 * 1.0 / 60.0).monitors[0]
                .colour
                .hue;
            assert!((hue - 0.4).abs() <= depth + 1e-4, "drifted to {hue}");
        }
        assert_eq!(params.monitors[0].colour.hue, 0.4, "the stored knob moved");
    }

    #[test]
    fn a_graph_with_no_automation_is_handed_over_untouched() {
        let params = Params::default();
        assert!(matches!(
            params.modulated(12.5),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn stacked_lfos_on_one_knob_sum() {
        // Nothing forbids two, because two offsets on one knob is a beat
        // rather than a conflict — the keys drive the first, a config may
        // stack more.
        let mut params = crate::config::single();
        params.monitors[0].colour.brightness = 0.0;
        let one = Lfo {
            rate: 0.0,
            phase: 0.25,
            depth: 0.1,
            ..Lfo::new(Knob::Brightness, Focus::default(), Shape::Sine)
        };
        params.motion = vec![one, one];
        let brightness = params.modulated(3.0).monitors[0].colour.brightness;
        assert!((brightness - 0.2).abs() < 1e-5, "{brightness}");
    }

    #[test]
    fn the_swing_is_bounded_by_the_knob_it_drives() {
        // Depth is clamped to the knob's travel, and the nudge clamps again,
        // so no automation can put a value somewhere a hand could not.
        for knob in Knob::ALL {
            let mut params = crate::config::insanity();
            let mut lfo = Lfo::new(knob, Focus::default(), Shape::Sine);
            lfo.depth = Lfo::depth_limit(knob);
            lfo.rate = 7.0;
            params.motion = vec![lfo];
            for i in 0..500 {
                let at = params.modulated(i as f64 / 60.0);
                crate::config::validate(&at)
                    .unwrap_or_else(|e| panic!("{knob:?} drove the graph out of shape: {e}"));
            }
        }
    }
}
