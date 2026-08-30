//! The keyboard mapped onto the knobs. One table per shape, driving both the
//! lookup and the printed help, so the two cannot drift apart.

use winit::keyboard::KeyCode;

use crate::params::Knob;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Put a knob at a value outright, which a key cannot do and a
    /// fader does by standing somewhere. Absolute, not a position, so
    /// where the ends of a fader are is the surface's business and not
    /// the instrument's.
    Set(Knob, f32),
    Nudge(Knob, f32),
    /// Put the camera knobs' focus on one camera outright, by its place in
    /// the graph. A select rather than a step: a hand that means "that one"
    /// should not have to walk past the ones it does not mean.
    FocusCamera(usize),
    FocusMonitor(usize),
    Reset,
    /// Put the last knob that moved back to its identity, and nothing else.
    /// Named by having been turned rather than by a control of its own: the
    /// instrument has two dozen of them and no display to point at one with,
    /// and the knob a hand wants back is the knob that hand was just on.
    ResetLastKnob,
    /// Swap the focused monitor's seed for the other kind: a white blob on
    /// the glass, or dark glass holding only what the switcher paints on it.
    /// A button and not a knob, because the two are not two settings of one
    /// thing — and the dark rig's level is already played on the switcher.
    Seed,
    /// Blank every monitor, so the loops restart from the seeds alone.
    Clear,
    /// Which way a press moves the tempo — passes a second, the speed the
    /// piece plays at. How far, and the range it moves inside, are
    /// [`crate::tempo`]'s.
    Tempo(crate::tempo::Step),
    /// Show the focused monitor on the whole display, or go back to the
    /// tiled bank. A latch and not a select — see [`crate::app`], where the
    /// monitor it shows is the focus and not an index of its own.
    Solo,
    /// Show or hide the controls overlay drawn over the picture.
    Overlay,
    /// Write what the display is showing to a file.
    Screencap,
    /// Record the display for as long as the control is held down.
    Record(Edge),
    Quit,
}

/// Which way a control is moving. Only the ones a hand *holds* have two
/// edges — every other binding on the panel is a press and nothing else,
/// which is why this rides on the one action that reads it rather than
/// beside every action that does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Down,
    Up,
}

/// What letting go of the control that pressed `action` does, and `None` for
/// a binding that is a press and nothing else. The one place a release means
/// anything, so a key and a button cannot disagree about which controls are
/// held.
pub fn released(action: Action) -> Option<Action> {
    match action {
        Action::Record(Edge::Down) => Some(Action::Record(Edge::Up)),
        _ => None,
    }
}

/// How many nodes of each side of the focus have a key of their own. Eight
/// because that is the keypad, and because it is a control surface's channel
/// strips. It is also how far into a graph the focus reaches, the select
/// being the only way to move it — a node past the eighth still plays, it
/// just plays at whatever the config left its knobs on.
pub(crate) const KEYED_NODES: usize = 8;

/// The nodes a key reaches outright: a camera bare, and the monitor of the
/// same number with `shift` in front. The numeric keypad because it is
/// already numbered the way the graph is, because it is the last block of
/// eight the board has left, and because these are physical key codes — so a
/// board with NumLock off still sends them. A slip onto one moves the focus
/// and nothing else on the glass.
///
/// `shift` for the monitor rather than a second block of eight keys: the
/// board has no second block, and the camera and the monitor are the same
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
    // The switcher's crosspoints: how much of the focused camera, and how
    // much of the focused input, the focused monitor shows. On the spare
    // keys at the right edge, away from the knobs, because they are the two
    // controls that act on a pair of nodes rather than on either of them.
    axis(Knob::Route, KeyCode::Slash, "/", KeyCode::Backslash, "\\"),
    // The send on the far end of the digits, clear of the gain channels at
    // the near end: it is a level like they are, and the two rows' right
    // edge is where the other crosspoint lives too.
    axis(Knob::Send, KeyCode::Digit9, "9", KeyCode::Digit0, "0"),
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
    // The tempo, on the two digits the gain channels and the send left
    // between them: the one control that acts on the whole piece rather than
    // on a node of the graph, and the digits are where the other levels
    // already are.
    cmd(
        KeyCode::Digit7,
        "7",
        Action::Tempo(crate::tempo::Step::Slower),
        "slow the piece down (four presses halve the rate)",
        "rate -",
    ),
    cmd(
        KeyCode::Digit8,
        "8",
        Action::Tempo(crate::tempo::Step::Faster),
        "speed the piece up (four presses double the rate)",
        "rate +",
    ),
    // Enter, and not f12: this table is printed as the web build's own
    // legend, and every browser swallows f12 for its debugger.
    cmd(
        KeyCode::Enter,
        "enter",
        Action::Solo,
        "the focused monitor on the whole display, or the tiled bank",
        "solo",
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
    // The capture pair, on two function keys clear of the ones a browser
    // takes for itself: this table is printed as the web build's own legend,
    // and reload, the address bar and the debugger are the neighbours.
    cmd(
        KeyCode::F7,
        "f7",
        Action::Screencap,
        "write what the display is showing to a file",
        "snap",
    ),
    cmd(
        KeyCode::F8,
        "f8",
        Action::Record(Edge::Down),
        "record the display for as long as this is held down",
        "record",
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

pub fn action_for(key: KeyCode, shift: bool) -> Option<Action> {
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
    Node { node: usize, shift: bool },
    Axis { axis: &'static Axis, up: bool },
    Command(&'static Command),
}

/// Exactly as [`action_for`] reads the physical key, so a label cannot reach
/// what a key press cannot.
fn binding(label: &str) -> Option<Binding> {
    let (shift, bare) = match label.strip_prefix("shift ") {
        Some(rest) => (true, rest),
        None => (false, label),
    };
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
}

/// The keys that do one thing each, and what each one does. The surface is
/// held to this: a command with no button on the board is one nobody plays.
#[cfg(test)]
pub(crate) fn commands() -> impl Iterator<Item = (&'static str, Action)> {
    COMMANDS.iter().map(|c| (c.label, c.action))
}

/// `None` for a label no table claims, which is how a hand-written MIDI map
/// is caught at load rather than in the middle of a performance.
pub fn action_for_label(label: &str) -> Option<Action> {
    Some(match binding(label)? {
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
        assert_eq!(action_for(KeyCode::Space, false), Some(Action::Clear));
        assert_eq!(action_for(KeyCode::KeyR, false), Some(Action::Reset));
        assert_eq!(action_for(KeyCode::Escape, false), Some(Action::Quit));
        assert_eq!(action_for(KeyCode::Semicolon, false), Some(Action::Seed));
        assert_eq!(action_for(KeyCode::Enter, false), Some(Action::Solo));
        // A key no table claims, which is also how a hand-written MIDI map
        // naming one is caught at load.
        assert_eq!(action_for(KeyCode::F12, false), None);
        assert_eq!(action_for_label("f12"), None);
    }

    #[test]
    fn shift_is_the_node_keys_business_and_nobody_else_s() {
        // Held shift must not make a knob key mean something else: on a
        // physical layout it is held for all sorts of reasons. The one table
        // that does read it reads it as the same shape — the other side of
        // the focus — and never as a different value of the same thing.
        for key in every_key() {
            let reads_shift = NODE_KEYS.iter().any(|(bound, _)| *bound == key);
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
    fn the_capture_controls_are_a_press_and_a_hold() {
        assert_eq!(action_for(KeyCode::F7, false), Some(Action::Screencap));
        assert_eq!(
            action_for(KeyCode::F8, false),
            Some(Action::Record(Edge::Down))
        );
        assert_eq!(action_for_label("f7"), Some(Action::Screencap));
        assert_eq!(action_for_label("f8"), Some(Action::Record(Edge::Down)));
        assert_eq!(
            released(Action::Record(Edge::Down)),
            Some(Action::Record(Edge::Up))
        );
        // And letting go is the recording's alone: every other binding is a
        // press, so a release of one must reach nothing rather than firing
        // it a second time.
        for label in labels() {
            let action = action_for_label(label).expect("every label resolves");
            assert_eq!(
                released(action).is_some(),
                action == Action::Record(Edge::Down),
                "{label}"
            );
        }
    }

    #[test]
    fn the_help_names_every_binding() {
        let help = help();
        // The header and the two lines the node keys share.
        assert_eq!(help.lines().count(), AXES.len() + COMMANDS.len() + 3);
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
        assert_eq!(action_for(KeyCode::Backquote, true), Some(Action::Overlay));
        assert_eq!(action_for_label("`"), Some(Action::Overlay));
    }

    #[test]
    fn a_label_reaches_the_same_action_the_key_does() {
        assert_eq!(action_for_label("r"), Some(Action::Reset));
        assert_eq!(action_for_label("space"), Some(Action::Clear));
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
        assert_eq!(labels.len(), AXES.len() * 2 + COMMANDS.len() + KEYED_NODES);
        let help = help();
        // The camera keys print as one range line, its ends only, so the six
        // in the middle are named by those ends rather than each in turn.
        let named: Vec<&str> = labels[..labels.len() - KEYED_NODES]
            .iter()
            .copied()
            .chain([NODE_KEYS[0].1, NODE_KEYS[KEYED_NODES - 1].1])
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
