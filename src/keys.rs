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
    /// Put the camera knobs' focus on one camera outright, by its place in
    /// the graph. A select rather than a step: a hand that means "that one"
    /// should not have to walk past the ones it does not mean.
    FocusCamera(usize),
    /// The same for the monitor half of the focus. It had only the step for
    /// a long while, which left the eight faders' node the one thing on the
    /// instrument a hand could not point at outright.
    FocusMonitor(usize),
    Reset,
    /// Put the last knob that moved back to its identity, and nothing else.
    /// Named by having been turned rather than by a control of its own: the
    /// instrument has two dozen of them and no display to point at one with,
    /// and the knob a hand wants back is the knob that hand was just on.
    ResetLastKnob,
    /// Turn the surface's fine mode on or off. A latch rather than a held
    /// modifier: the device's buttons are momentary and every binding here
    /// is a key press, so a mode is a press that stays — and the panel
    /// lights the button that is holding it.
    Fine,
    /// Write the whole panel to a preset slot.
    Store(usize),
    /// Play a preset slot back.
    Recall(usize),
    /// Swap the focused monitor's seed for the other kind: a white blob on
    /// the glass, or dark glass holding only what the switcher paints on it.
    /// A button and not a knob, because the two are not two settings of one
    /// thing — and the dark rig's level is already played on the switcher.
    Seed,
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
pub(crate) const SLOT_KEYS: [(KeyCode, &str); SLOTS] = [
    (KeyCode::F1, "f1"),
    (KeyCode::F2, "f2"),
    (KeyCode::F3, "f3"),
    (KeyCode::F4, "f4"),
    (KeyCode::F5, "f5"),
    (KeyCode::F6, "f6"),
    (KeyCode::F7, "f7"),
    (KeyCode::F8, "f8"),
];

/// How many nodes of each side of the focus have a key of their own. Eight
/// because that is the keypad, and because it is a control surface's channel
/// strips — a graph may hold more nodes than either, and `n` and `m` are what
/// walk to those.
pub(crate) const KEYED_NODES: usize = 8;

/// The nodes a key reaches outright: a camera bare, and the monitor of the
/// same number with `shift` in front. The numeric keypad because it is
/// already numbered the way the graph is, because it is the last block of
/// eight the board has left, and because these are physical key codes — so a
/// board with NumLock off still sends them. A slip onto one moves the focus
/// and nothing else on the glass.
///
/// `shift` for the monitor rather than a second block of eight keys: the
/// board has no second block, and the two halves of the focus are the same
/// question asked of the two sides of the graph — which is the shape a
/// modifier has, not the shape two unrelated tables have.
pub(crate) const NODE_KEYS: [(KeyCode, &str); KEYED_NODES] = [
    (KeyCode::Numpad1, "num1"),
    (KeyCode::Numpad2, "num2"),
    (KeyCode::Numpad3, "num3"),
    (KeyCode::Numpad4, "num4"),
    (KeyCode::Numpad5, "num5"),
    (KeyCode::Numpad6, "num6"),
    (KeyCode::Numpad7, "num7"),
    (KeyCode::Numpad8, "num8"),
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
    // ";" because the seed has always been under this finger, and the seed
    // is what it still is. One key rather than a pair: there is no level
    // between the two rigs for a second one to walk through.
    cmd(
        KeyCode::Semicolon,
        ";",
        Action::Seed,
        "the focused monitor's seed: a white blob or dark glass",
        "seed",
    ),
    // Backspace, because what it does to a knob is what it does to a
    // character: takes back the last one.
    cmd(
        KeyCode::Backspace,
        "backspace",
        Action::ResetLastKnob,
        "reset the last knob turned",
        "reset 1",
    ),
    // Tab, for stepping the whole surface down to the keys' own step. Off on
    // the left with nothing else near it: a mode left on by a slip is worse
    // than one that takes a reach to find.
    cmd(
        KeyCode::Tab,
        "tab",
        Action::Fine,
        "fine mode for the surface's knobs, on or off",
        "fine",
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

/// `shift` is read by two tables and no others. On a slot key it is the
/// difference between recall and store: both are irreversible — a recall
/// walks over a live panel nothing has stored — but a hand mid-piece reaches
/// for a slot far more often than it writes one, and the modifier is the
/// only thing a keyboard has to tell the two apart. On a node key it is
/// which side of the focus is meant, camera bare and monitor shifted.
pub fn action_for(key: KeyCode, shift: bool) -> Option<Action> {
    if let Some(slot) = SLOT_KEYS.iter().position(|(bound, _)| *bound == key) {
        return Some(if shift {
            Action::Store(slot)
        } else {
            Action::Recall(slot)
        });
    }
    if let Some(node) = NODE_KEYS.iter().position(|(bound, _)| *bound == key) {
        return Some(if shift {
            Action::FocusMonitor(node)
        } else {
            Action::FocusCamera(node)
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
    Node { node: usize, shift: bool },
    Axis { axis: &'static Axis, up: bool },
    Command(&'static Command),
}

/// A leading `"shift "` is stripped for every table, and the two that read
/// it keep it — exactly as [`action_for`] reads the physical key.
fn binding(label: &str) -> Option<Binding> {
    let (shift, bare) = match label.strip_prefix("shift ") {
        Some(rest) => (true, rest),
        None => (false, label),
    };
    if let Some(slot) = SLOT_KEYS.iter().position(|(_, bound)| *bound == bare) {
        return Some(Binding::Slot { slot, shift });
    }
    if let Some(node) = NODE_KEYS.iter().position(|(_, bound)| *bound == bare) {
        return Some(Binding::Node { node, shift });
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
        .chain(NODE_KEYS.iter().map(|(_, label)| *label))
        .chain(SLOT_KEYS.iter().map(|(_, label)| *label))
}

/// What the key spelled `label` does, `"shift f1"` included. `None` for a
/// label no table claims, which is how a hand-written MIDI map is caught at
/// load rather than in the middle of a performance.
pub fn action_for_label(label: &str) -> Option<Action> {
    Some(match binding(label)? {
        Binding::Slot { slot, shift: true } => Action::Store(slot),
        Binding::Slot { slot, shift: false } => Action::Recall(slot),
        Binding::Node { node, shift: true } => Action::FocusMonitor(node),
        Binding::Node { node, shift: false } => Action::FocusCamera(node),
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
        Binding::Node { node, shift } => {
            let side = if shift { "monitor" } else { "camera" };
            format!("focus {side} {}", node + 1)
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
        Binding::Node { node, shift } => {
            let side = if shift { "mon" } else { "cam" };
            format!("{side} {}", node + 1)
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
    // Off the tables' own labels, like the two above them: a key rebound
    // must not leave the help naming the old one.
    let nodes = NODE_KEYS.len();
    let (first, last) = (NODE_KEYS[0].1, NODE_KEYS[nodes - 1].1);
    let keys = format!("{first} / {last}");
    out.push_str(&format!("  {keys:<12} focus camera 1 to {nodes}\n"));
    let keys = format!("shift {first} / {last}");
    out.push_str(&format!("  {keys:<12} focus monitor 1 to {nodes}\n"));
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
            .chain(NODE_KEYS.iter().map(|(key, _)| *key))
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
        assert_eq!(action_for(KeyCode::Semicolon, false), Some(Action::Seed));
        // The seed's other key went with its fader, and free means free: a
        // key still resolving to the knob that was deleted would be a
        // vocabulary the instrument no longer has.
        assert_eq!(action_for(KeyCode::Quote, false), None);
        assert_eq!(action_for_label("'"), None);
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
    fn shift_is_the_slot_and_node_keys_business_and_nobody_else_s() {
        // Held shift must not make a knob key mean something else: on a
        // physical layout it is held for all sorts of reasons. The two
        // tables that do read it read it as the same shape — a second thing
        // the same block of keys names — and never as a different value of
        // the same thing.
        for key in every_key() {
            let reads_shift = SLOT_KEYS.iter().any(|(bound, _)| *bound == key)
                || NODE_KEYS.iter().any(|(bound, _)| *bound == key);
            if !reads_shift {
                assert_eq!(action_for(key, true), action_for(key, false), "{key:?}");
                continue;
            }
            assert_ne!(action_for(key, true), action_for(key, false), "{key:?}");
        }
    }

    #[test]
    fn a_node_key_is_a_camera_bare_and_a_monitor_shifted() {
        for (node, (key, label)) in NODE_KEYS.iter().enumerate() {
            assert_eq!(action_for(*key, false), Some(Action::FocusCamera(node)));
            assert_eq!(action_for(*key, true), Some(Action::FocusMonitor(node)));
            // And the same through the label, which is the whole of what a
            // MIDI map may say — a button that reaches one and not the other
            // is a surface with a vocabulary the keys have not got.
            assert_eq!(action_for_label(label), Some(Action::FocusCamera(node)));
            assert_eq!(
                action_for_label(&format!("shift {label}")),
                Some(Action::FocusMonitor(node))
            );
            assert_eq!(
                describes(label).unwrap(),
                format!("focus camera {}", node + 1)
            );
            assert_eq!(
                describes(&format!("shift {label}")).unwrap(),
                format!("focus monitor {}", node + 1)
            );
            assert_eq!(short(label).unwrap(), format!("cam {}", node + 1));
            assert_eq!(
                short(&format!("shift {label}")).unwrap(),
                format!("mon {}", node + 1)
            );
        }
    }

    #[test]
    fn the_new_commands_are_on_the_keys_their_labels_name() {
        // Against literal key codes, because the label and the code are two
        // different facts and `the_keys_and_the_labels_agree` reads each
        // command's own code — so it is true whatever code that is, and a
        // binding moved to another key would still agree with itself.
        assert_eq!(
            action_for(KeyCode::Backspace, false),
            Some(Action::ResetLastKnob)
        );
        assert_eq!(action_for(KeyCode::Tab, false), Some(Action::Fine));
        assert_eq!(action_for(KeyCode::KeyR, false), Some(Action::Reset));
        assert_eq!(
            action_for(KeyCode::Numpad1, false),
            Some(Action::FocusCamera(0))
        );
        assert_eq!(
            action_for(KeyCode::Numpad1, true),
            Some(Action::FocusMonitor(0))
        );
    }

    #[test]
    fn the_help_names_every_binding() {
        let help = help();
        // The header, the two lines the node keys share, and the two the
        // slot keys share.
        assert_eq!(help.lines().count(), AXES.len() + COMMANDS.len() + 5);
        assert!(help.contains("recall preset slot") && help.contains("store preset slot"));
        assert!(help.contains("focus camera 1 to 8") && help.contains("focus monitor 1 to 8"));
        // And that the monitor half needs shift, in the key range rather
        // than only in the words: two identical ranges meaning two different
        // things is a card that teaches the wrong gesture.
        let monitors = help
            .lines()
            .find(|line| line.contains("focus monitor"))
            .expect("no monitor line");
        assert!(
            monitors.contains(&format!("shift {}", NODE_KEYS[0].1)),
            "{monitors}"
        );
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
        assert_eq!(describes("r").as_deref(), Some("reset every knob"));
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
        assert_eq!(short("r").as_deref(), Some("reset"));
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
        assert_eq!(action_for_label("r"), Some(Action::Reset));
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
        assert_eq!(action_for_label("shift r"), Some(Action::Reset));
    }

    #[test]
    fn the_printed_help_and_the_label_list_are_the_same_list() {
        // `labels()` is the vocabulary a MIDI map may use and `help()` is
        // what the instrument prints; a label on one and not the other is a
        // binding a performer cannot discover or one the help lies about.
        let labels: Vec<&str> = labels().collect();
        assert_eq!(
            labels.len(),
            AXES.len() * 2 + COMMANDS.len() + KEYED_NODES + SLOTS
        );
        let help = help();
        // The camera keys and the slots each print as one range line, its
        // ends only, so the six in the middle of either are named by those
        // ends rather than each in turn.
        let named: Vec<&str> = labels[..labels.len() - KEYED_NODES - SLOTS]
            .iter()
            .copied()
            .chain([
                NODE_KEYS[0].1,
                NODE_KEYS[KEYED_NODES - 1].1,
                SLOT_KEYS[0].1,
                SLOT_KEYS[SLOTS - 1].1,
            ])
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
        for (key, label) in NODE_KEYS {
            assert_eq!(action_for_label(label), action_for(key, false));
        }
    }

    #[test]
    fn a_camera_key_selects_the_camera_it_is_numbered_for() {
        // Numbered from one on the key and from zero in the graph.
        for (camera, (key, _)) in NODE_KEYS.iter().enumerate() {
            assert_eq!(action_for(*key, false), Some(Action::FocusCamera(camera)));
        }
        let third = NODE_KEYS[2].1;
        assert_eq!(describes(third).as_deref(), Some("focus camera 3"));
        assert_eq!(short(third).as_deref(), Some("cam 3"));
    }
}
