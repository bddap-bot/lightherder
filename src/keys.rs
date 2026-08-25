//! The keyboard mapped onto the knobs. One table per shape, driving both the
//! lookup and the printed help, so the two cannot drift apart.

use winit::keyboard::KeyCode;

use crate::params::Knob;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nudge(Knob, f32),
    /// Move the camera knobs' focus to the next camera in the graph.
    NextCamera,
    /// Move the monitor knobs' focus to the next monitor.
    NextMonitor,
    Reset,
    /// Blank every monitor, so the loops restart from the seeds alone.
    Clear,
    Quit,
}

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

pub fn action_for(key: KeyCode) -> Option<Action> {
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
            let Some(Action::Nudge(down_knob, down)) = action_for(axis.down.0) else {
                panic!("{:?} should nudge", axis.down)
            };
            let Some(Action::Nudge(up_knob, up)) = action_for(axis.up.0) else {
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
        let Some(Action::Nudge(knob, delta)) = action_for(KeyCode::Equal) else {
            panic!("= should nudge a knob")
        };
        p.nudge(knob, delta, Focus::default());
        let zoom = p.cameras[0].framing.zoom;
        assert!((zoom - (before + Knob::Zoom.increment())).abs() < 1e-6);
    }

    #[test]
    fn the_commands_do_what_they_say() {
        assert_eq!(action_for(KeyCode::KeyN), Some(Action::NextCamera));
        assert_eq!(action_for(KeyCode::KeyM), Some(Action::NextMonitor));
        assert_eq!(action_for(KeyCode::Space), Some(Action::Clear));
        assert_eq!(action_for(KeyCode::KeyR), Some(Action::Reset));
        assert_eq!(action_for(KeyCode::Escape), Some(Action::Quit));
        // A key no table claims.
        assert_eq!(action_for(KeyCode::F1), None);
    }

    #[test]
    fn the_help_names_every_binding() {
        let help = help();
        assert_eq!(help.lines().count(), AXES.len() + COMMANDS.len() + 1);
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
