//! The keyboard mapped onto the knobs. One table drives both the lookup and
//! the printed help, so the two cannot drift apart.

use winit::keyboard::KeyCode;

use crate::params::Knob;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Nudge(Knob, f32),
    Reset,
    /// Blank the monitor, so the loop restarts from the seed alone.
    Clear,
    Quit,
}

/// Physical key positions, so the labels below assume a US layout.
const BINDINGS: &[(KeyCode, &str, Action)] = &[
    (
        KeyCode::Minus,
        "-",
        Action::Nudge(Knob::Zoom, -Knob::Zoom.increment()),
    ),
    (
        KeyCode::Equal,
        "=",
        Action::Nudge(Knob::Zoom, Knob::Zoom.increment()),
    ),
    (
        KeyCode::Comma,
        ",",
        Action::Nudge(Knob::Rotation, -Knob::Rotation.increment()),
    ),
    (
        KeyCode::Period,
        ".",
        Action::Nudge(Knob::Rotation, Knob::Rotation.increment()),
    ),
    (
        KeyCode::ArrowLeft,
        "left",
        Action::Nudge(Knob::TranslateX, -Knob::TranslateX.increment()),
    ),
    (
        KeyCode::ArrowRight,
        "right",
        Action::Nudge(Knob::TranslateX, Knob::TranslateX.increment()),
    ),
    (
        KeyCode::ArrowDown,
        "down",
        Action::Nudge(Knob::TranslateY, -Knob::TranslateY.increment()),
    ),
    (
        KeyCode::ArrowUp,
        "up",
        Action::Nudge(Knob::TranslateY, Knob::TranslateY.increment()),
    ),
    (
        KeyCode::BracketLeft,
        "[",
        Action::Nudge(Knob::Gain, -Knob::Gain.increment()),
    ),
    (
        KeyCode::BracketRight,
        "]",
        Action::Nudge(Knob::Gain, Knob::Gain.increment()),
    ),
    (
        KeyCode::Digit1,
        "1",
        Action::Nudge(Knob::GainR, -Knob::GainR.increment()),
    ),
    (
        KeyCode::Digit2,
        "2",
        Action::Nudge(Knob::GainR, Knob::GainR.increment()),
    ),
    (
        KeyCode::Digit3,
        "3",
        Action::Nudge(Knob::GainG, -Knob::GainG.increment()),
    ),
    (
        KeyCode::Digit4,
        "4",
        Action::Nudge(Knob::GainG, Knob::GainG.increment()),
    ),
    (
        KeyCode::Digit5,
        "5",
        Action::Nudge(Knob::GainB, -Knob::GainB.increment()),
    ),
    (
        KeyCode::Digit6,
        "6",
        Action::Nudge(Knob::GainB, Knob::GainB.increment()),
    ),
    (
        KeyCode::Semicolon,
        ";",
        Action::Nudge(Knob::Seed, -Knob::Seed.increment()),
    ),
    (
        KeyCode::Quote,
        "'",
        Action::Nudge(Knob::Seed, Knob::Seed.increment()),
    ),
    (KeyCode::Space, "space", Action::Clear),
    (KeyCode::KeyR, "r", Action::Reset),
    (KeyCode::Escape, "esc", Action::Quit),
];

pub fn action_for(key: KeyCode) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|(bound, _, _)| *bound == key)
        .map(|(_, _, action)| *action)
}

pub fn help() -> String {
    let mut out = String::from("keys (US layout positions)\n");
    for (_, label, action) in BINDINGS {
        out.push_str(&format!("  {label:<8} {}\n", describe(*action)));
    }
    out
}

fn describe(action: Action) -> String {
    match action {
        Action::Nudge(knob, delta) => {
            let direction = if delta < 0.0 { "down" } else { "up" };
            let what = match knob {
                Knob::Zoom => "zoom",
                Knob::Rotation => "rotation",
                Knob::TranslateX => "pan x",
                Knob::TranslateY => "pan y",
                Knob::Gain => "loop gain, all channels",
                Knob::GainR => "loop gain, red",
                Knob::GainG => "loop gain, green",
                Knob::GainB => "loop gain, blue",
                Knob::Seed => "seed brightness",
            };
            format!("{what} {direction}")
        }
        Action::Reset => "reset every knob".to_string(),
        Action::Clear => "blank the monitor".to_string(),
        Action::Quit => "quit".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Params;

    #[test]
    fn no_key_is_bound_twice() {
        for (i, (key, _, _)) in BINDINGS.iter().enumerate() {
            assert!(
                !BINDINGS[..i].iter().any(|(other, _, _)| other == key),
                "{key:?} is bound twice"
            );
        }
    }

    #[test]
    fn every_knob_has_a_key_in_both_directions() {
        for knob in Knob::ALL {
            let deltas: Vec<f32> = BINDINGS
                .iter()
                .filter_map(|(_, _, action)| match action {
                    Action::Nudge(bound, delta) if *bound == knob => Some(*delta),
                    _ => None,
                })
                .collect();
            assert!(deltas.iter().any(|d| *d > 0.0), "{knob:?} cannot go up");
            assert!(deltas.iter().any(|d| *d < 0.0), "{knob:?} cannot go down");
        }
    }

    #[test]
    fn a_key_press_reaches_the_value_it_names() {
        let mut p = Params::default();
        let before = p.framing.zoom;
        let Some(Action::Nudge(knob, delta)) = action_for(KeyCode::Equal) else {
            panic!("= should nudge a knob")
        };
        p.nudge(knob, delta);
        assert!((p.framing.zoom - (before + Knob::Zoom.increment())).abs() < 1e-6);
    }

    #[test]
    fn the_other_keys_do_what_they_say() {
        assert_eq!(action_for(KeyCode::Space), Some(Action::Clear));
        assert_eq!(action_for(KeyCode::KeyR), Some(Action::Reset));
        assert_eq!(action_for(KeyCode::Escape), Some(Action::Quit));
        assert_eq!(action_for(KeyCode::KeyQ), None);
    }

    #[test]
    fn the_help_names_every_binding() {
        let help = help();
        assert_eq!(help.lines().count(), BINDINGS.len() + 1);
        for (_, label, _) in BINDINGS {
            assert!(help.contains(label), "{label} missing from help");
        }
    }
}
