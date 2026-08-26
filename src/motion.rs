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

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::params::{Focus, Knob, Limit, Side};

/// The swing over one cycle.
///
/// Two, and they are the two the stage is named for: a sine is the wobble, a
/// ramp is the turn. A triangle is a sine with corners and was left out; a
/// square is a genuinely different gesture — a switcher stuttering rather
/// than a knob moving — and will be a third arm the day it is wanted, not a
/// flavour shipped ahead of a reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    #[default]
    Sine,
    Ramp,
}

impl Shape {
    pub const ALL: [Shape; 2] = [Shape::Sine, Shape::Ramp];

    /// The one name a shape has, on the terminal and on disk alike — its
    /// serde is this function and its inverse, the same way [`Knob`]'s is.
    pub const fn name(self) -> &'static str {
        match self {
            Shape::Sine => "sine",
            Shape::Ramp => "ramp",
        }
    }

    pub fn from_name(name: &str) -> Option<Shape> {
        Shape::ALL.into_iter().find(|shape| shape.name() == name)
    }

    /// The swing at `phase` cycles into the cycle, in `[-1, 1]`.
    fn at(self, phase: f64) -> f64 {
        match self {
            Shape::Sine => (phase * std::f64::consts::TAU).sin(),
            Shape::Ramp => 2.0 * phase - 1.0,
        }
    }

    /// Where in the cycle this shape passes through nothing, on its way up.
    /// [`Lfo::restart`] seats a fresh swing here, so switching one on moves
    /// the knob from where the hand left it rather than jumping to wherever
    /// a cycle that has been running since startup happens to be.
    const fn rest(self) -> f32 {
        match self {
            Shape::Sine => 0.0,
            // A ramp runs -1 to 1, so it is halfway through that it is at
            // rest — and it is only from full depth on a wrapping knob that
            // the step back at the cycle's end is invisible.
            Shape::Ramp => 0.5,
        }
    }
}

impl Serialize for Shape {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Shape, D::Error> {
        let name = String::deserialize(deserializer)?;
        Shape::from_name(&name).ok_or_else(|| {
            let known = Shape::ALL.map(Shape::name).join(", ");
            serde::de::Error::custom(format!("no shape called {name:?}; there are {known}"))
        })
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
    /// A hundred seconds a cycle. Slower than this is a knob that is not
    /// moving, which is what the automation's off switch is for — so this is
    /// a floor rather than a rail the rate can rest against.
    pub const SLOWEST: f32 = 0.01;

    /// Half the sixty frames a second the instrument is drawn at: past
    /// Nyquist an LFO does not go faster, it goes somewhere else.
    pub const FASTEST: f32 = 30.0;

    /// A ratio per key press, not a step: rates worth playing span three
    /// decades, and one linear step is either useless at the slow end or
    /// unreachable at the fast one.
    pub const RATE_RATIO: f32 = 1.06;

    /// Where a freshly switched-on swing starts: slow enough to read as
    /// motion rather than flicker.
    const FRESH_RATE: f32 = 0.05;

    /// Presses from silent to the widest depth the knob allows.
    const DEPTH_STEPS: f32 = 64.0;

    /// Freshly switched on: a quarter of the widest depth this knob allows —
    /// obvious at a glance without swinging a feedback loop straight to black
    /// or to white. The caller seats the phase with [`Lfo::restart`].
    pub fn new(knob: Knob, focus: Focus, shape: Shape) -> Lfo {
        Lfo {
            knob,
            focus: focus.narrowed(knob),
            shape,
            rate: Lfo::FRESH_RATE,
            depth: Lfo::depth_limit(knob) / 4.0,
            phase: 0.0,
        }
    }

    /// Whether two LFOs drive the same value. Both focuses are narrowed —
    /// `Lfo::new` does it and `config::validate` refuses anything else — so
    /// this is the whole comparison.
    pub fn same_target(&self, other: &Lfo) -> bool {
        self.knob == other.knob && self.focus == other.focus
    }

    /// Seat the phase so the swing passes through nothing at `seconds`.
    /// Without this, switching an LFO on joins a cycle that has been running
    /// since startup at whatever point it has reached, and the knob jumps by
    /// up to the full depth on the next frame.
    pub fn restart(&mut self, seconds: f64) {
        self.set_phase(f64::from(self.shape.rest()) - self.rate as f64 * seconds);
    }

    /// `cycles` folded into one cycle. `rem_euclid` can round up to exactly
    /// 1.0 for an argument just under zero, and a phase is a position
    /// *within* the cycle — `config::validate` says so.
    fn set_phase(&mut self, cycles: f64) {
        self.phase = (cycles.rem_euclid(1.0) as f32).min(1.0 - f32::EPSILON);
    }

    /// The offset this adds to its knob `seconds` after the instrument
    /// started. In f64 because the phase is a rate times a time: at the 30 Hz
    /// ceiling, f32 seconds have lost half a cycle's worth of resolution
    /// inside a day, and a performance is not the only thing that runs.
    pub fn offset(&self, seconds: f64) -> f32 {
        let phase = (self.rate as f64 * seconds + self.phase as f64).rem_euclid(1.0);
        (self.depth as f64 * self.shape.at(phase)) as f32
    }

    /// The widest depth that means anything on `knob`: its whole travel, or a
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

    /// Move the rate by `steps` presses, geometrically, holding the swing
    /// where it is at `seconds`. Without that the phase — which is measured
    /// from the instrument's start — lands somewhere else the moment the rate
    /// changes, and sweeping the rate key becomes a stutter rather than a
    /// speed-up.
    pub fn scale_rate(&mut self, steps: f32, seconds: f64) {
        let before = self.rate as f64 * seconds;
        self.rate = (self.rate * Lfo::RATE_RATIO.powf(steps)).clamp(Lfo::SLOWEST, Lfo::FASTEST);
        self.set_phase(self.phase as f64 + before - self.rate as f64 * seconds);
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
        let node = match self.knob.side() {
            Side::Camera => format!("cam {}", self.focus.camera + 1),
            Side::Monitor => format!("mon {}", self.focus.monitor + 1),
            Side::Edge => format!(
                "cam {} on mon {}",
                self.focus.camera + 1,
                self.focus.monitor + 1
            ),
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
        // average where the hand left it, and it reaches both rails on the
        // way. The ramp is what a motor does: strictly increasing across the
        // cycle, ending one full swing on.
        let steps = 1000;
        let sine: Vec<f64> = (0..steps)
            .map(|i| Shape::Sine.at(i as f64 / steps as f64))
            .collect();
        let mean: f64 = sine.iter().sum::<f64>() / steps as f64;
        assert!(mean.abs() < 1e-9, "the sine is off centre by {mean}");
        assert!((Shape::Sine.at(0.25) - 1.0).abs() < 1e-12);
        assert!((Shape::Sine.at(0.75) + 1.0).abs() < 1e-12);
        assert!(Shape::Sine.at(0.0).abs() < 1e-12);

        let mut last = f64::NEG_INFINITY;
        for i in 0..steps {
            let at = Shape::Ramp.at(i as f64 / steps as f64);
            assert!(at > last, "the ramp went back at {i}");
            last = at;
        }
        assert!((Shape::Ramp.at(0.0) - -1.0).abs() < 1e-12);
        assert!((Shape::Ramp.at(0.5)).abs() < 1e-12);
        assert!((Shape::Ramp.at(1.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_shape_is_the_same_name_on_disk_as_on_the_terminal() {
        for shape in Shape::ALL {
            assert_eq!(Shape::from_name(shape.name()), Some(shape));
            let written = toml::to_string(&Lfo::new(Knob::Hue, Focus::default(), shape)).unwrap();
            assert!(
                written.contains(&format!("shape = \"{}\"", shape.name())),
                "{shape:?} writes itself as {written}"
            );
        }
        assert_eq!(Shape::from_name("square"), None);
    }

    #[test]
    fn an_lfo_repeats_at_its_rate_and_not_at_some_other() {
        // Sampled at times that are whole cycles of *no other* rate in the
        // set, since a period of 1s agrees with a period of 4s wherever both
        // are whole — which is how a dropped rate term hides.
        for rate in [0.25f32, 1.0, 3.0] {
            let mut lfo = Lfo::new(Knob::Hue, Focus::default(), Shape::Sine);
            lfo.rate = rate;
            let cycle = f64::from(1.0 / rate);
            for i in 0..20 {
                let t = i as f64 * 0.37;
                let (a, b) = (lfo.offset(t), lfo.offset(t + cycle));
                assert!((a - b).abs() < 1e-5, "rate {rate} at {t}: {a} then {b}");
            }
            // And a quarter of its own cycle in it is somewhere else — which
            // is what fails if the rate is dropped and every LFO runs at 1 Hz.
            let quarter = lfo.offset(cycle / 4.0) - lfo.offset(0.0);
            assert!(quarter.abs() > 0.5 * lfo.depth, "rate {rate} is not moving");
        }
        // Two rates apart must not agree a quarter of a second in.
        let (mut slow, mut fast) = (
            Lfo::new(Knob::Hue, Focus::default(), Shape::Sine),
            Lfo::new(Knob::Hue, Focus::default(), Shape::Sine),
        );
        slow.rate = 0.25;
        fast.rate = 3.0;
        assert!((slow.offset(0.25) - fast.offset(0.25)).abs() > 1e-3);
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
    fn switching_one_on_does_not_move_the_knob_it_lands_on() {
        // The swing starts from where the hand left the knob. Without the
        // re-seat it joins a cycle that has been running since startup at
        // whatever point it has reached — up to a full depth away.
        for shape in Shape::ALL {
            for seconds in [0.0, 0.37, 12.5, 307.0, 4321.5] {
                let mut lfo = Lfo::new(Knob::Rotation, Focus::default(), shape);
                lfo.restart(seconds);
                assert!(
                    lfo.offset(seconds).abs() < 1e-4,
                    "{shape:?} at {seconds}s starts {} from rest",
                    lfo.offset(seconds)
                );
                assert!((0.0..1.0).contains(&lfo.phase), "phase {}", lfo.phase);
                // And it goes somewhere from there.
                let quarter = 0.25 / f64::from(lfo.rate);
                assert!(lfo.offset(seconds + quarter).abs() > 0.1 * lfo.depth);
            }
        }
    }

    #[test]
    fn the_rate_keys_reach_both_ends_and_stop() {
        let mut lfo = Lfo::new(Knob::Hue, Focus::default(), Shape::Sine);
        for _ in 0..1000 {
            lfo.scale_rate(1.0, 0.0);
        }
        assert_eq!(lfo.rate, Lfo::FASTEST);
        for _ in 0..1000 {
            lfo.scale_rate(-1.0, 0.0);
        }
        assert_eq!(lfo.rate, Lfo::SLOWEST);
        // And the trip either way is a performer's worth of presses, not a
        // thousand: this is the whole reason the step is a ratio.
        let mut lfo = Lfo::new(Knob::Hue, Focus::default(), Shape::Sine);
        let mut presses = 0;
        while lfo.rate < 5.0 {
            lfo.scale_rate(1.0, 0.0);
            presses += 1;
        }
        assert!(presses < 100, "{presses} presses to reach 5 Hz");
    }

    #[test]
    fn the_widest_depth_is_the_knob_s_own_travel() {
        // Spelled out rather than compared against the function that computes
        // it: every other check of a depth — the keys, `validate`, the fresh
        // quarter — reads `depth_limit`, so nothing else can notice it being
        // wrong.
        assert_eq!(Lfo::depth_limit(Knob::Zoom), 3.75); // 0.25 to 4.0
        assert_eq!(Lfo::depth_limit(Knob::Brightness), 1.0); // -0.5 to 0.5
        assert_eq!(Lfo::depth_limit(Knob::Gain), 1.2); // 0 to 1.2
        assert_eq!(Lfo::depth_limit(Knob::Rotation), std::f32::consts::PI);
        assert_eq!(Lfo::depth_limit(Knob::Hue), std::f32::consts::PI);
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
    fn an_lfo_drives_the_node_it_names_and_no_other() {
        // The read side of `focus`. `validate` has four poison cases for the
        // write side; without this, `modulated` could ignore the field
        // entirely and drive camera 1 for every LFO in the graph.
        let mut params = crate::config::crossed();
        let (a, b) = (
            params.cameras[0].framing.zoom,
            params.cameras[1].framing.zoom,
        );
        let mut lfo = Lfo::new(
            Knob::Zoom,
            Focus {
                camera: 1,
                monitor: 0,
            },
            Shape::Sine,
        );
        lfo.rate = 1.0;
        lfo.phase = 0.25;
        params.motion = vec![lfo];
        let at = params.modulated(0.0);
        assert_eq!(at.cameras[0].framing.zoom, a, "camera 1 moved");
        assert!((at.cameras[1].framing.zoom - (b + lfo.depth)).abs() < 1e-4);

        // Same for the monitor half of a focus.
        let mut params = crate::config::crossed();
        let (a, b) = (
            params.monitors[0].colour.brightness,
            params.monitors[1].colour.brightness,
        );
        let mut lfo = Lfo::new(
            Knob::Brightness,
            Focus {
                camera: 0,
                monitor: 1,
            },
            Shape::Sine,
        );
        lfo.rate = 1.0;
        lfo.phase = 0.25;
        lfo.depth = 0.2;
        params.motion = vec![lfo];
        let at = params.modulated(0.0);
        assert_eq!(at.monitors[0].colour.brightness, a, "monitor 1 moved");
        assert!((at.monitors[1].colour.brightness - (b + 0.2)).abs() < 1e-4);
    }

    #[test]
    fn stacked_lfos_on_one_knob_sum() {
        // Two offsets on one knob is a beat, not a fight. They are totalled
        // before anything is applied, because `nudge` clamps: applied one at
        // a time, one of them stops at a rail the sum would have cleared, and
        // which one depends on the order they sit in the list.
        let frozen = |depth, phase| Lfo {
            rate: 1.0,
            phase,
            depth,
            ..Lfo::new(Knob::Brightness, Focus::default(), Shape::Sine)
        };
        let mut params = crate::config::single();
        params.monitors[0].colour.brightness = 0.0;
        // Different depths, so applying one of them twice is not the same
        // answer as applying each once.
        params.motion = vec![frozen(0.1, 0.25), frozen(0.05, 0.25)];
        let at = params.modulated(0.0).monitors[0].colour.brightness;
        assert!((at - 0.15).abs() < 1e-5, "{at}");

        // At a rail, and in both orders: brightness clamps at 0.5, and
        // +0.4 then -0.1 applied in sequence stops at 0.5 and comes back to
        // 0.4, where the sum is 0.5.
        params.monitors[0].colour.brightness = 0.2;
        params.motion = vec![frozen(0.4, 0.25), frozen(0.1, 0.75)];
        let forwards = params.modulated(0.0).monitors[0].colour.brightness;
        params.motion.reverse();
        let backwards = params.modulated(0.0).monitors[0].colour.brightness;
        assert_eq!(forwards, backwards, "the order of the list changed it");
        assert!((forwards - 0.5).abs() < 1e-5, "{forwards}");
    }

    #[test]
    fn the_swing_is_bounded_by_the_knob_it_drives() {
        // Depth is clamped to the knob's travel, and the nudge clamps again,
        // so no automation can put a value somewhere a hand could not — which
        // matters because `Feedback::step` re-runs `validate` every frame.
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
