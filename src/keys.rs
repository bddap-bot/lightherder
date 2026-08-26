//! The keyboard mapped onto the knobs. One table per shape, driving both the
//! lookup and the printed help, so the two cannot drift apart.

use winit::keyboard::KeyCode;

use crate::params::Knob;
use crate::slots::SLOTS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Put a knob at a value outright, which a key cannot do and a
    /// fader does by standing somewhere. Absolute, not a position, so
    /// where the ends of a fader are is the surface's business and not
    /// the instrument's.
    Set(Knob, f32),
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
    /// Cover the display, or stop covering it.
    Fullscreen,
    /// Show or hide the controls overlay drawn over the picture.
    Overlay,
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
    // The keyer, on what a nearly full board has left: the last two letters
    // for the level a performer hunts, and the nav cluster for the trims
    // that get set once per backdrop.
    axis(Knob::KeyThreshold, KeyCode::KeyB, "b", KeyCode::KeyL, "l"),
    axis(Knob::KeySoftness, KeyCode::F9, "f9", KeyCode::F10, "f10"),
    axis(Knob::KeyHue, KeyCode::Home, "home", KeyCode::End, "end"),
    axis(
        Knob::KeyTolerance,
        KeyCode::PageDown,
        "pgdn",
        KeyCode::PageUp,
        "pgup",
    ),
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
    // The switcher's crosspoint: how much of the focused camera the focused
    // monitor shows. On the two spare keys at the right edge, away from the
    // knobs, because it is the one control that acts on the pair of nodes
    // rather than on either of them.
    axis(Knob::Route, KeyCode::Slash, "/", KeyCode::Backslash, "\\"),
];

/// A key that does one thing, and the two ways the instrument writes it
/// down. `what` is the printed help's full wording; `short` is the on-screen
/// overlay's caption — at most two words, which is the ceiling for
/// text on the panel. They live side by side so the two cannot name the same
/// action a table apart.
struct Command {
    key: KeyCode,
    label: &'static str,
    action: Action,
    what: &'static str,
    short: &'static str,
}

const COMMANDS: &[Command] = &[
    // The automation, on the last knob turned rather than on a knob of its
    // own: twenty knobs would otherwise need twenty switches, and the
    // knob a performer just swept is the one they want to set moving.
    cmd(
        KeyCode::KeyP,
        "p",
        Action::Motion,
        "the last knob turned: off / sine / ramp",
        "motion",
    ),
    cmd(
        KeyCode::Digit7,
        "7",
        Action::MotionRate(-1.0),
        "its rate, slower",
        "rate -",
    ),
    cmd(
        KeyCode::Digit8,
        "8",
        Action::MotionRate(1.0),
        "its rate, faster",
        "rate +",
    ),
    cmd(
        KeyCode::Digit9,
        "9",
        Action::MotionDepth(-1.0),
        "its swing, narrower",
        "swing -",
    ),
    cmd(
        KeyCode::Digit0,
        "0",
        Action::MotionDepth(1.0),
        "its swing, wider",
        "swing +",
    ),
    cmd(
        KeyCode::KeyN,
        "n",
        Action::NextCamera,
        "focus the next camera",
        "cam >",
    ),
    cmd(
        KeyCode::KeyM,
        "m",
        Action::NextMonitor,
        "focus the next monitor",
        "mon >",
    ),
    cmd(
        KeyCode::Space,
        "space",
        Action::Clear,
        "blank every monitor",
        "blank",
    ),
    cmd(
        KeyCode::KeyR,
        "r",
        Action::Reset,
        "reset every knob",
        "reset",
    ),
    cmd(
        KeyCode::F11,
        "f11",
        Action::Fullscreen,
        "cover the display, or stop",
        "fullscreen",
    ),
    // Backquote because it is the traditional lid over a console, and the
    // last unclaimed key in easy reach of a resting hand.
    cmd(
        KeyCode::Backquote,
        "`",
        Action::Overlay,
        "the controls overlay, on or off",
        "help",
    ),
    cmd(KeyCode::Escape, "esc", Action::Quit, "quit", "quit"),
];

const fn cmd(
    key: KeyCode,
    label: &'static str,
    action: Action,
    what: &'static str,
    short: &'static str,
) -> Command {
    Command {
        key,
        label,
        action,
        what,
        short,
    }
}

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
    COMMANDS.iter().find(|c| c.key == key).map(|c| c.action)
}

/// A label resolved against the three tables: the one walk behind
/// [`action_for_label`], [`describes`] and [`short`], so their wordings can
/// differ but what a label reaches cannot.
enum Binding {
    Slot { slot: usize, shift: bool },
    Axis { axis: &'static Axis, up: bool },
    Command(&'static Command),
}

/// A leading `"shift "` is stripped for every table but only the slots keep
/// it, exactly as [`action_for`] reads the physical key.
fn binding(label: &str) -> Option<Binding> {
    let (shift, bare) = match label.strip_prefix("shift ") {
        Some(rest) => (true, rest),
        None => (false, label),
    };
    if let Some(slot) = SLOT_KEYS.iter().position(|(_, bound)| *bound == bare) {
        return Some(Binding::Slot { slot, shift });
    }
    for axis in AXES {
        if axis.down.1 == bare {
            return Some(Binding::Axis { axis, up: false });
        }
        if axis.up.1 == bare {
            return Some(Binding::Axis { axis, up: true });
        }
    }
    COMMANDS
        .iter()
        .find(|c| c.label == bare)
        .map(Binding::Command)
}

/// Every key label the help prints, in the order it prints them.
///
/// The control surface names a button's job by the key it presses, so this
/// is also the whole vocabulary a MIDI map may use — and a binding added to
/// the tables above is reachable from the panel the same day, with nothing
/// to keep in step.
pub fn labels() -> impl Iterator<Item = &'static str> {
    AXES.iter()
        .flat_map(|axis| [axis.down.1, axis.up.1])
        .chain(COMMANDS.iter().map(|c| c.label))
        .chain(SLOT_KEYS.iter().map(|(_, label)| *label))
}

/// What the key spelled `label` does, `"shift f1"` included. `None` for a
/// label no table claims, which is how a hand-written MIDI map is caught at
/// load rather than in the middle of a performance.
pub fn action_for_label(label: &str) -> Option<Action> {
    Some(match binding(label)? {
        Binding::Slot { slot, shift: true } => Action::Store(slot),
        Binding::Slot { slot, shift: false } => Action::Recall(slot),
        Binding::Axis { axis, up } => {
            let step = axis.knob.increment();
            Action::Nudge(axis.knob, if up { step } else { -step })
        }
        Binding::Command(c) => c.action,
    })
}

/// What the key spelled `label` does, in the words the help uses. The
/// surface prints this against the control a button sits on, so a button and
/// the key it presses cannot be described two different ways.
pub fn describes(label: &str) -> Option<String> {
    Some(match binding(label)? {
        Binding::Slot { slot, shift } => {
            let what = if shift { "store" } else { "recall" };
            format!("{what} preset slot {}", slot + 1)
        }
        Binding::Axis { axis, up } => {
            format!("{} {}", axis.knob.name(), if up { "up" } else { "down" })
        }
        Binding::Command(c) => c.what.to_string(),
    })
}

/// What the key spelled `label` does, in the two words the on-screen overlay
/// has room for. The same resolution as [`describes`], worded for a caption
/// — so a control cannot be captioned one thing on the overlay and another
/// on the card. `None` for a label no table claims.
pub fn short(label: &str) -> Option<String> {
    Some(match binding(label)? {
        Binding::Slot { slot, shift } => {
            let what = if shift { "save" } else { "slot" };
            format!("{what} {}", slot + 1)
        }
        Binding::Axis { axis, up } => {
            format!("{} {}", axis.knob.name(), if up { "+" } else { "-" })
        }
        Binding::Command(c) => c.short.to_string(),
    })
}

pub fn help() -> String {
    let mut out = String::from("keys (US layout positions)\n");
    for axis in AXES {
        let keys = format!("{} / {}", axis.down.1, axis.up.1);
        out.push_str(&format!("  {keys:<12} {} down / up\n", axis.knob.name()));
    }
    for c in COMMANDS {
        out.push_str(&format!("  {:<12} {}\n", c.label, c.what));
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
            .chain(COMMANDS.iter().map(|c| c.key))
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
        // A key no table claims.
        assert_eq!(action_for(KeyCode::F12, false), None);
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

    #[test]
    fn every_label_names_exactly_one_key() {
        // The MIDI map addresses a key by its label, so two keys sharing one
        // would make a button's job depend on table order.
        let labels: Vec<&str> = labels().collect();
        for (i, label) in labels.iter().enumerate() {
            assert!(!labels[..i].contains(label), "{label} labels two keys");
        }
        // And every one of them resolves, which is what the map's loader
        // checks a hand-written binding against.
        for label in labels {
            assert!(action_for_label(label).is_some(), "{label}");
        }
    }

    #[test]
    fn every_label_is_described() {
        // `Map::card` prints a control by what its key does, so a label with
        // no description is a blank line on the performer's card. Every
        // label, and every label under a shift, since the map may write one.
        for label in labels() {
            for label in [label.to_string(), format!("shift {label}")] {
                let what = describes(&label).unwrap_or_else(|| panic!("{label}: no description"));
                assert!(!what.is_empty(), "{label}: an empty description");
            }
        }
        // And it says what the key does, not what its neighbour does.
        assert_eq!(describes("f3").as_deref(), Some("recall preset slot 3"));
        assert_eq!(
            describes("shift f3").as_deref(),
            Some("store preset slot 3")
        );
        assert_eq!(describes("=").as_deref(), Some("zoom up"));
        assert_eq!(describes("-").as_deref(), Some("zoom down"));
        assert_eq!(
            describes("p").as_deref(),
            Some("the last knob turned: off / sine / ramp")
        );
        assert_eq!(describes("wiggle"), None);
    }

    #[test]
    fn every_label_has_a_short_caption_and_it_names_the_same_action() {
        // The overlay captions a button by its key's short; a label with no
        // short is a lit control with nothing written on it. Two words at
        // most, which is the ceiling for text on the panel — the
        // axes are exempt only where the knob's own name already spends two,
        // and the sign rides along.
        for label in labels() {
            for label in [label.to_string(), format!("shift {label}")] {
                let short = short(&label).unwrap_or_else(|| panic!("{label}: no caption"));
                assert!(!short.is_empty(), "{label}: an empty caption");
            }
        }
        assert_eq!(short("f3").as_deref(), Some("slot 3"));
        assert_eq!(short("shift f3").as_deref(), Some("save 3"));
        assert_eq!(short("=").as_deref(), Some("zoom +"));
        assert_eq!(short("-").as_deref(), Some("zoom -"));
        assert_eq!(short("space").as_deref(), Some("blank"));
        assert_eq!(short("p").as_deref(), Some("motion"));
        assert_eq!(short("`").as_deref(), Some("help"));
        assert_eq!(short("wiggle"), None);
    }

    #[test]
    fn the_overlay_toggle_is_reachable_from_keyboard_and_label_alike() {
        assert_eq!(action_for(KeyCode::Backquote, false), Some(Action::Overlay));
        // Held shift must not hide the help from a hand that happens to be
        // storing a slot with the other.
        assert_eq!(action_for(KeyCode::Backquote, true), Some(Action::Overlay));
        assert_eq!(action_for_label("`"), Some(Action::Overlay));
    }

    #[test]
    fn a_label_reaches_the_same_action_the_key_does() {
        assert_eq!(action_for_label("p"), Some(Action::Motion));
        assert_eq!(action_for_label("space"), Some(Action::Clear));
        assert_eq!(action_for_label("f3"), Some(Action::Recall(2)));
        assert_eq!(action_for_label("shift f3"), Some(Action::Store(2)));
        assert_eq!(
            action_for_label("="),
            Some(Action::Nudge(Knob::Zoom, Knob::Zoom.increment()))
        );
        assert_eq!(action_for_label("wiggle"), None);

        // "shift" in front of a key that does not read it is that key; the
        // MIDI map refuses such a binding rather than letting it look like
        // it means something.
        assert_eq!(action_for_label("shift p"), Some(Action::Motion));
    }

    #[test]
    fn the_printed_help_and_the_label_list_are_the_same_list() {
        // `labels()` is the vocabulary a MIDI map may use and `help()` is
        // what the instrument prints; a label on one and not the other is a
        // binding a performer cannot discover or one the help lies about.
        let labels: Vec<&str> = labels().collect();
        assert_eq!(labels.len(), AXES.len() * 2 + COMMANDS.len() + SLOTS);
        let help = help();
        // The slots print as one range line, "f1 / f8", so the six in the
        // middle are named by the ends rather than each in turn.
        let named: Vec<&str> = labels[..labels.len() - SLOTS]
            .iter()
            .copied()
            .chain([SLOT_KEYS[0].1, SLOT_KEYS[SLOTS - 1].1])
            .collect();
        for label in named {
            assert!(help.contains(label), "{label} is not in the help");
        }
        // Every label reaches the action its key does, all forty-odd of them
        // rather than the handful spelled out above.
        for axis in AXES {
            for (key, label) in [axis.down, axis.up] {
                assert_eq!(action_for_label(label), action_for(key, false));
            }
        }
        for c in COMMANDS {
            assert_eq!(action_for_label(c.label), action_for(c.key, false));
        }
        for (key, label) in SLOT_KEYS {
            assert_eq!(action_for_label(label), action_for(key, false));
            assert_eq!(
                action_for_label(&format!("shift {label}")),
                action_for(key, true)
            );
        }
    }
}
