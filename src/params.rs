//! The knobs on the instrument, and the keys wired to them.

use crate::affine::Framing;
use winit::keyboard::KeyCode;

/// Radius of the seed spot, in screen units where the monitor is 1.0 tall.
pub const SEED_RADIUS: f32 = 0.06;

/// Everything the feedback pass needs to know for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub framing: Framing,
    /// Per-channel gain applied once per pass. Below 1.0 the image dies out,
    /// above 1.0 it blooms; the channels differ to colour the trails.
    pub decay: [f32; 3],
    /// Brightness of the spot injected at the centre, which is the only thing
    /// keeping a decaying loop alive.
    pub seed_gain: f32,
}

impl Default for Params {
    /// A framing that already oscillates: a slight magnification and turn per
    /// pass, gain just under unity, seed on.
    fn default() -> Self {
        Params {
            framing: Framing {
                zoom: 1.02,
                rotation: 0.01,
                translate: [0.0, 0.0],
            },
            decay: [0.97, 0.98, 0.99],
            seed_gain: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knob {
    Zoom,
    Rotation,
    TranslateX,
    TranslateY,
    Decay,
    DecayR,
    DecayG,
    DecayB,
    Seed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nudge(Knob, f32),
    Reset,
    /// Blank the monitor, so the loop restarts from the seed alone.
    Clear,
    Quit,
}

impl Knob {
    /// One key press worth of this knob. Zoom and rotation are far more
    /// sensitive than the rest: near unity they decide whether the loop
    /// spirals in, out or stands still.
    pub fn step(self) -> f32 {
        match self {
            Knob::Zoom => 0.002,
            Knob::Rotation => 0.005,
            Knob::TranslateX | Knob::TranslateY => 0.002,
            Knob::Decay | Knob::DecayR | Knob::DecayG | Knob::DecayB => 0.005,
            Knob::Seed => 0.05,
        }
    }
}

pub fn action_for(key: KeyCode) -> Option<Action> {
    let nudge = |knob: Knob, sign: f32| Some(Action::Nudge(knob, sign * knob.step()));
    match key {
        KeyCode::Minus => nudge(Knob::Zoom, -1.0),
        KeyCode::Equal => nudge(Knob::Zoom, 1.0),
        KeyCode::Comma => nudge(Knob::Rotation, -1.0),
        KeyCode::Period => nudge(Knob::Rotation, 1.0),
        KeyCode::ArrowLeft => nudge(Knob::TranslateX, -1.0),
        KeyCode::ArrowRight => nudge(Knob::TranslateX, 1.0),
        KeyCode::ArrowDown => nudge(Knob::TranslateY, -1.0),
        KeyCode::ArrowUp => nudge(Knob::TranslateY, 1.0),
        KeyCode::BracketLeft => nudge(Knob::Decay, -1.0),
        KeyCode::BracketRight => nudge(Knob::Decay, 1.0),
        KeyCode::Digit1 => nudge(Knob::DecayR, -1.0),
        KeyCode::Digit2 => nudge(Knob::DecayR, 1.0),
        KeyCode::Digit3 => nudge(Knob::DecayG, -1.0),
        KeyCode::Digit4 => nudge(Knob::DecayG, 1.0),
        KeyCode::Digit5 => nudge(Knob::DecayB, -1.0),
        KeyCode::Digit6 => nudge(Knob::DecayB, 1.0),
        KeyCode::Semicolon => nudge(Knob::Seed, -1.0),
        KeyCode::Quote => nudge(Knob::Seed, 1.0),
        KeyCode::Space => Some(Action::Clear),
        KeyCode::KeyR => Some(Action::Reset),
        KeyCode::Escape => Some(Action::Quit),
        _ => None,
    }
}

impl Params {
    pub fn nudge(&mut self, knob: Knob, delta: f32) {
        match knob {
            Knob::Zoom => self.framing.zoom = (self.framing.zoom + delta).clamp(0.25, 4.0),
            Knob::Rotation => self.framing.rotation = wrap_pi(self.framing.rotation + delta),
            Knob::TranslateX => {
                self.framing.translate[0] = (self.framing.translate[0] + delta).clamp(-1.0, 1.0)
            }
            Knob::TranslateY => {
                self.framing.translate[1] = (self.framing.translate[1] + delta).clamp(-1.0, 1.0)
            }
            Knob::Decay => {
                for c in &mut self.decay {
                    *c = (*c + delta).clamp(0.0, 1.5);
                }
            }
            Knob::DecayR => self.decay[0] = (self.decay[0] + delta).clamp(0.0, 1.5),
            Knob::DecayG => self.decay[1] = (self.decay[1] + delta).clamp(0.0, 1.5),
            Knob::DecayB => self.decay[2] = (self.decay[2] + delta).clamp(0.0, 1.5),
            Knob::Seed => self.seed_gain = (self.seed_gain + delta).clamp(0.0, 2.0),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "zoom {:.3}  rot {:+.3}  pan {:+.3},{:+.3}  decay {:.3},{:.3},{:.3}  seed {:.2}",
            self.framing.zoom,
            self.framing.rotation,
            self.framing.translate[0],
            self.framing.translate[1],
            self.decay[0],
            self.decay[1],
            self.decay[2],
            self.seed_gain,
        )
    }
}

/// Into `(-pi, pi]`, so a knob spun in one direction never runs away.
fn wrap_pi(radians: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let wrapped = (PI - radians).rem_euclid(TAU);
    PI - wrapped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn knobs_clamp_at_their_limits() {
        let mut p = Params::default();
        for _ in 0..10_000 {
            p.nudge(Knob::Zoom, 1.0);
            p.nudge(Knob::Decay, 1.0);
            p.nudge(Knob::Seed, 1.0);
            p.nudge(Knob::TranslateX, 1.0);
        }
        assert_eq!(p.framing.zoom, 4.0);
        assert_eq!(p.decay, [1.5; 3]);
        assert_eq!(p.seed_gain, 2.0);
        assert_eq!(p.framing.translate[0], 1.0);

        for _ in 0..10_000 {
            p.nudge(Knob::Zoom, -1.0);
            p.nudge(Knob::Decay, -1.0);
            p.nudge(Knob::Seed, -1.0);
            p.nudge(Knob::TranslateX, -1.0);
        }
        assert_eq!(p.framing.zoom, 0.25);
        assert_eq!(p.decay, [0.0; 3]);
        assert_eq!(p.seed_gain, 0.0);
        assert_eq!(p.framing.translate[0], -1.0);
    }

    #[test]
    fn rotation_wraps_instead_of_running_away() {
        let mut p = Params::default();
        p.framing.rotation = 0.0;
        for _ in 0..10_000 {
            p.nudge(Knob::Rotation, 0.5);
            assert!(p.framing.rotation > -PI && p.framing.rotation <= PI);
        }
        // 10000 * 0.5 = 5000 rad, which is 5000 - 796 * TAU away from zero.
        let expected = wrap_pi(5000.0);
        assert!(
            (p.framing.rotation - expected).abs() < 1e-2,
            "{}",
            p.framing.rotation
        );
    }

    #[test]
    fn wrap_pi_keeps_the_boundaries_it_promises() {
        assert!((wrap_pi(PI) - PI).abs() < 1e-6);
        assert!((wrap_pi(-PI) - PI).abs() < 1e-6);
        assert!((wrap_pi(0.0)).abs() < 1e-6);
        assert!((wrap_pi(PI + 0.1) - (-PI + 0.1)).abs() < 1e-5);
    }

    #[test]
    fn a_channel_knob_moves_only_its_channel() {
        let mut p = Params::default();
        let before = p.decay;
        p.nudge(Knob::DecayG, 0.1);
        assert_eq!(p.decay[0], before[0]);
        assert_eq!(p.decay[2], before[2]);
        assert!((p.decay[1] - (before[1] + 0.1)).abs() < 1e-6);
    }

    #[test]
    fn keys_reach_the_knobs_they_are_labelled_with() {
        assert_eq!(
            action_for(KeyCode::Equal),
            Some(Action::Nudge(Knob::Zoom, Knob::Zoom.step()))
        );
        assert_eq!(
            action_for(KeyCode::Minus),
            Some(Action::Nudge(Knob::Zoom, -Knob::Zoom.step()))
        );
        assert_eq!(
            action_for(KeyCode::ArrowUp),
            Some(Action::Nudge(Knob::TranslateY, Knob::TranslateY.step()))
        );
        assert_eq!(action_for(KeyCode::Space), Some(Action::Clear));
        assert_eq!(action_for(KeyCode::KeyR), Some(Action::Reset));
        assert_eq!(action_for(KeyCode::Escape), Some(Action::Quit));
        assert_eq!(action_for(KeyCode::KeyQ), None);
    }

    #[test]
    fn a_key_press_reaches_the_value_it_names() {
        let mut p = Params::default();
        let before = p.framing.zoom;
        let Some(Action::Nudge(knob, delta)) = action_for(KeyCode::Equal) else {
            panic!("Equal should nudge a knob")
        };
        p.nudge(knob, delta);
        assert!((p.framing.zoom - (before + Knob::Zoom.step())).abs() < 1e-6);
    }
}
