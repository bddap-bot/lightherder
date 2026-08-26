//! The keyboard mapped onto the knobs. One table per shape, driving both the
//! lookup and the printed help, so the two cannot drift apart.

use winit::keyboard::KeyCode;

use crate::params::Knob;
use crate::slots::SLOTS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nudge(Knob, f32),
    /// Move the camera knobs' focus to the next camera in the graph.
    NextCamera,
    /// Move the monitor knobs' focus to the next monitor.
    NextMonitor,
    /// Switch the automation on the last knob turned through its states: off,
    /// a sine, a ramp, off.
    Motion,
    /// Move that automation's rate, in presses.
    MotionRate(f32),
    /// Move its depth, in presses.
    MotionDepth(f32),
    Reset,
    /// Write the whole panel to a preset slot.
    Store(usize),
    /// Play a preset slot back.
    Recall(usize),
    /// Blank every monitor, so the loops restart from the seeds alone.
    Clear,
    Quit,
}

/// The preset slots. Function keys because they are the only block of eight
/// that is not already a knob, and because a slip onto one mid-performance
/// should not be a knob moving.
const SLOT_KEYS: [(KeyCode, &str); SLOTS] = [
    (KeyCode::F1, "f1"),
    (KeyCode::F2, "f2"),
    (KeyCode::F3, "f3"),
    (KeyCode::F4, "f4"),
    (KeyCode::F5, "f5"),
    (KeyCode::F6, "f6"),
    (KeyCode::F7, "f7"),
    (KeyCode::F8, "f8"),
];

/// A knob and the two keys that turn it. Physical key positions, so the
/// labels assume a US layout.
struct Axis {
    knob: Knob,
    down: (KeyCode, &'static str),
    up: (KeyCode, &'static str),
}

const AXES: &[Axis] = &[
    axis(Knob::Zoom, KeyCode::Minus, "-", KeyCode::Equal, "="),
    axis(Knob::Rotation, KeyCode::Comma, ",", KeyCode::Period, "."),
    axis(
        Knob::TranslateX,
        KeyCode::ArrowLeft,
        "left",
        KeyCode::ArrowRight,
        "right",
    ),
    axis(
        Knob::TranslateY,
        KeyCode::ArrowDown,
        "down",
        KeyCode::ArrowUp,
        "up",
    ),
    axis(
        Knob::Gain,
        KeyCode::BracketLeft,
        "[",
        KeyCode::BracketRight,
        "]",
    ),
    axis(Knob::GainR, KeyCode::Digit1, "1", KeyCode::Digit2, "2"),
    axis(Knob::GainG, KeyCode::Digit3, "3", KeyCode::Digit4, "4"),
    axis(Knob::GainB, KeyCode::Digit5, "5", KeyCode::Digit6, "6"),
    // The path's character sits under the right hand, next to nothing else:
    // these are the knobs a performer sweeps while watching, not trimming.
    axis(Knob::Bloom, KeyCode::KeyG, "g", KeyCode::KeyH, "h"),
    axis(Knob::BloomRadius, KeyCode::KeyJ, "j", KeyCode::KeyK, "k"),
    axis(Knob::ChromaBleed, KeyCode::KeyY, "y", KeyCode::KeyU, "u"),
    axis(Knob::Noise, KeyCode::KeyI, "i", KeyCode::KeyO, "o"),
    axis(Knob::Seed, KeyCode::Semicolon, ";", KeyCode::Quote, "'"),
    // The colour stage gets the left hand, kept together so a performer can
    // sweep the front panel without looking.
    axis(Knob::Hue, KeyCode::KeyA, "a", KeyCode::KeyS, "s"),
    axis(Knob::Saturation, KeyCode::KeyD, "d", KeyCode::KeyF, "f"),
    axis(Knob::Brightness, KeyCode::KeyZ, "z", KeyCode::KeyX, "x"),
    axis(Knob::Contrast, KeyCode::KeyC, "c", KeyCode::KeyV, "v"),
    axis(Knob::Gamma, KeyCode::KeyQ, "q", KeyCode::KeyW, "w"),
    // The amplifier's rail, beside the phosphor curve it feeds.
    axis(Knob::Headroom, KeyCode::KeyE, "e", KeyCode::KeyT, "t"),
];

const COMMANDS: &[(KeyCode, &str, Action, &str)] = &[
    // The automation, on the last knob turned rather than on a knob of its
    // own: nineteen knobs would otherwise need nineteen switches, and the
    // knob a performer just swept is the one they want to set moving.
    (
        KeyCode::KeyP,
        "p",
        Action::Motion,
        "the last knob turned: off / sine / ramp",
    ),
    (
        KeyCode::Digit7,
        "7",
        Action::MotionRate(-1.0),
        "its rate, slower",
    ),
    (
        KeyCode::Digit8,
        "8",
        Action::MotionRate(1.0),
        "its rate, faster",
    ),
    (
        KeyCode::Digit9,
        "9",
        Action::MotionDepth(-1.0),
        "its swing, narrower",
    ),
    (
        KeyCode::Digit0,
        "0",
        Action::MotionDepth(1.0),
        "its swing, wider",
    ),
    (
        KeyCode::KeyN,
        "n",
        Action::NextCamera,
        "focus the next camera",
    ),
    (
        KeyCode::KeyM,
        "m",
        Action::NextMonitor,
        "focus the next monitor",
    ),
    (
        KeyCode::Space,
        "space",
        Action::Clear,
        "blank every monitor",
    ),
    (KeyCode::KeyR, "r", Action::Reset, "reset every knob"),
    (KeyCode::Escape, "esc", Action::Quit, "quit"),
];

const fn axis(
    knob: Knob,
    down: KeyCode,
    down_label: &'static str,
    up: KeyCode,
    up_label: &'static str,
) -> Axis {
    Axis {
        knob,
        down: (down, down_label),
        up: (up, up_label),
    }
}

/// `shift` is read by the slot keys and nothing else: recall is one press
/// and store is the press you have to mean, because storing over a slot
/// during a performance cannot be undone and recalling can.
pub fn action_for(key: KeyCode, shift: bool) -> Option<Action> {
    if let Some(slot) = SLOT_KEYS.iter().position(|(bound, _)| *bound == key) {
        return Some(if shift {
            Action::Store(slot)
        } else {
            Action::Recall(slot)
        });
    }
    for axis in AXES {
        if axis.down.0 == key {
            return Some(Action::Nudge(axis.knob, -axis.knob.increment()));
        }
        if axis.up.0 == key {
            return Some(Action::Nudge(axis.knob, axis.knob.increment()));
        }
    }
    COMMANDS
        .iter()
        .find(|(bound, _, _, _)| *bound == key)
        .map(|(_, _, action, _)| *action)
}

pub fn help() -> String {
    let mut out = String::from("keys (US layout positions)\n");
    for axis in AXES {
        let keys = format!("{} / {}", axis.down.1, axis.up.1);
        out.push_str(&format!("  {keys:<12} {} down / up\n", axis.knob.name()));
    }
    for (_, label, _, what) in COMMANDS {
        out.push_str(&format!("  {label:<12} {what}\n"));
    }
    // Off the table's own labels, like the two above it: a slot rebound to a
    // different key must not leave the help naming the old one.
    let (first, last) = (SLOT_KEYS[0].1, SLOT_KEYS[SLOTS - 1].1);
    let keys = format!("{first} / {last}");
    out.push_str(&format!("  {keys:<12} recall preset slot 1 to {SLOTS}\n"));
    let keys = format!("shift {first} / {last}");
    out.push_str(&format!("  {keys:<12} store preset slot 1 to {SLOTS}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{Focus, Params};

    fn every_key() -> Vec<KeyCode> {
        AXES.iter()
            .flat_map(|a| [a.down.0, a.up.0])
            .chain(COMMANDS.iter().map(|(key, _, _, _)| *key))
            .chain(SLOT_KEYS.iter().map(|(key, _)| *key))
            .collect()
    }

    #[test]
    fn no_key_is_bound_twice() {
        let keys = every_key();
        for (i, key) in keys.iter().enumerate() {
            assert!(!keys[..i].contains(key), "{key:?} is bound twice");
        }
    }

    #[test]
    fn every_knob_has_an_axis() {
        for knob in Knob::ALL {
            assert!(AXES.iter().any(|a| a.knob == knob), "{knob:?} has no keys");
        }
    }

    #[test]
    fn the_two_keys_of_an_axis_push_it_opposite_ways() {
        for axis in AXES {
            let Some(Action::Nudge(down_knob, down)) = action_for(axis.down.0, false) else {
                panic!("{:?} should nudge", axis.down)
            };
            let Some(Action::Nudge(up_knob, up)) = action_for(axis.up.0, false) else {
                panic!("{:?} should nudge", axis.up)
            };
            assert_eq!(down_knob, axis.knob);
            assert_eq!(up_knob, axis.knob);
            assert!(down < 0.0 && up > 0.0, "{:?}: {down} and {up}", axis.knob);
        }
    }

    #[test]
    fn a_key_press_reaches_the_value_it_names() {
        let mut p = Params::default();
        let before = p.cameras[0].framing.zoom;
        let Some(Action::Nudge(knob, delta)) = action_for(KeyCode::Equal, false) else {
            panic!("= should nudge a knob")
        };
        p.nudge(knob, delta, Focus::default());
        let zoom = p.cameras[0].framing.zoom;
        assert!((zoom - (before + Knob::Zoom.increment())).abs() < 1e-6);
    }

    #[test]
    fn the_commands_do_what_they_say() {
        assert_eq!(action_for(KeyCode::KeyN, false), Some(Action::NextCamera));
        assert_eq!(action_for(KeyCode::KeyM, false), Some(Action::NextMonitor));
        assert_eq!(action_for(KeyCode::Space, false), Some(Action::Clear));
        assert_eq!(action_for(KeyCode::KeyR, false), Some(Action::Reset));
        assert_eq!(action_for(KeyCode::Escape, false), Some(Action::Quit));
        // A key no table claims — F9, because the slots stop at F8.
        assert_eq!(action_for(KeyCode::F9, false), None);
    }

    #[test]
    fn the_slot_keys_recall_and_shift_stores() {
        for (slot, (key, label)) in SLOT_KEYS.iter().enumerate() {
            assert_eq!(action_for(*key, false), Some(Action::Recall(slot)));
            assert_eq!(action_for(*key, true), Some(Action::Store(slot)));
            assert!(help().contains(label) || (1..SLOTS - 1).contains(&slot));
        }
    }

    #[test]
    fn shift_is_the_slot_keys_business_and_nobody_else_s() {
        // Held shift must not make a knob key mean something else: on a
        // physical layout it is held for all sorts of reasons.
        for key in every_key() {
            if SLOT_KEYS.iter().any(|(bound, _)| *bound == key) {
                continue;
            }
            assert_eq!(action_for(key, true), action_for(key, false), "{key:?}");
        }
    }

    #[test]
    fn the_automation_keys_are_a_switch_and_two_pairs() {
        assert_eq!(action_for(KeyCode::KeyP, false), Some(Action::Motion));
        // The pairs push opposite ways, same as an axis — they are not axes
        // only because there is no knob to hang them on until one is running.
        assert_eq!(
            action_for(KeyCode::Digit7, false),
            Some(Action::MotionRate(-1.0))
        );
        assert_eq!(
            action_for(KeyCode::Digit8, false),
            Some(Action::MotionRate(1.0))
        );
        assert_eq!(
            action_for(KeyCode::Digit9, false),
            Some(Action::MotionDepth(-1.0))
        );
        assert_eq!(
            action_for(KeyCode::Digit0, false),
            Some(Action::MotionDepth(1.0))
        );
    }

    #[test]
    fn the_help_names_every_binding() {
        let help = help();
        // The header, and the two lines the eight slot keys share.
        assert_eq!(help.lines().count(), AXES.len() + COMMANDS.len() + 3);
        assert!(help.contains("recall preset slot") && help.contains("store preset slot"));
        for axis in AXES {
            assert!(help.contains(axis.down.1), "{} missing", axis.down.1);
            assert!(help.contains(axis.up.1), "{} missing", axis.up.1);
            assert!(
                help.contains(axis.knob.name()),
                "{} missing",
                axis.knob.name()
            );
        }
    }
}
