//! The keyboard mapped onto the knobs. One table per shape, driving both the
//! lookup and the printed help, so the two cannot drift apart.

use winit::keyboard::KeyCode;

use crate::params::{Knob, Node};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Put a knob at a value outright, which a key cannot do and a
    /// fader does by standing somewhere. Absolute, not a position, so
    /// where the ends of a fader are is the surface's business and not
    /// the instrument's.
    Set(Knob, f32),
    Nudge(Knob, f32),
    /// Put the knobs' focus on one node of one kind outright, by its place
    /// in the graph. A select rather than a step: a hand that means "that
    /// one" should not have to walk past the ones it does not mean.
    Focus(Node, usize),
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

/// How many nodes of a kind have a key of their own, and so how deep any
/// graph may go: eight, because that is the keypad and a control surface's
/// channel strips alike. A node past this has no key and no button, which is
/// what [`crate::config::validate`] refuses a graph for.
pub const KEYED_NODES: usize = 8;

/// The nodes a key reaches outright, one kind per modifier — a camera bare,
/// the monitor of the same number under `shift`, the input under `ctrl`. The
/// numeric keypad because it is already numbered the way the graph is,
/// because it is the last block of eight the board has left, and because
/// these are physical key codes — so a board with NumLock off still sends
/// them. A slip onto one moves the focus and nothing else on the glass.
///
/// Modifiers rather than three blocks of eight keys: the board has no second
/// block, let alone a third, and the three kinds are the same question asked
/// of the graph's three sides — which is the shape a modifier has, not the
/// shape three unrelated tables have.
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
    // much of the first input, the focused monitor shows. On the spare
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

/// What a node key held under these modifiers names. Ctrl beats shift so the
/// pair held together is one answer rather than an order the hand has to
/// know.
pub const fn node_of(shift: bool, ctrl: bool) -> Node {
    match (ctrl, shift) {
        (true, _) => Node::Input,
        (false, true) => Node::Monitor,
        (false, false) => Node::Camera,
    }
}

/// `None` for the camera, which is the bare key. The absence is what makes
/// [`binding`] one search over [`Node::ALL`] rather than a second table of
/// the kinds that do carry a word.
pub(crate) const fn prefix(node: Node) -> Option<&'static str> {
    match node {
        Node::Camera => None,
        Node::Monitor => Some("shift "),
        Node::Input => Some("ctrl "),
    }
}

/// How a MIDI map spells each key that focuses a `node`, in the key table's
/// order and no longer than it. The one place a modifier is written into a
/// label, so a surface built from this and a key press cannot disagree — and
/// a row taken from it cannot run past the keys.
pub fn node_labels(node: Node) -> impl Iterator<Item = String> {
    let modifier = prefix(node).unwrap_or_default();
    NODE_KEYS
        .iter()
        .map(move |(_, key)| format!("{modifier}{key}"))
}

pub fn action_for(key: KeyCode, node: Node) -> Option<Action> {
    if let Some(index) = NODE_KEYS.iter().position(|(bound, _)| *bound == key) {
        return Some(Action::Focus(node, index));
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
    Node { node: Node, index: usize },
    Axis { axis: &'static Axis, up: bool },
    Command(&'static Command),
}

/// Exactly as [`action_for`] reads the physical key, so a label cannot reach
/// what a key press cannot.
fn binding(label: &str) -> Option<Binding> {
    let (node, bare) = Node::ALL
        .into_iter()
        .find_map(|node| Some((node, label.strip_prefix(prefix(node)?)?)))
        .unwrap_or((Node::Camera, label));
    if let Some(index) = NODE_KEYS.iter().position(|(_, bound)| *bound == bare) {
        return Some(Binding::Node { node, index });
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
        .chain(command_labels())
        .chain(NODE_KEYS.iter().map(|(_, label)| *label))
}

/// The keys that do one thing each. Named apart from the rest of the
/// vocabulary because the surface is held to them: a command with no button
/// on the board is one nobody plays.
pub fn command_labels() -> impl Iterator<Item = &'static str> {
    COMMANDS.iter().map(|c| c.label)
}

/// `None` for a label no table claims, which is how a hand-written MIDI map
/// is caught at load rather than in the middle of a performance.
pub fn action_for_label(label: &str) -> Option<Action> {
    Some(match binding(label)? {
        Binding::Node { node, index } => Action::Focus(node, index),
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
        Binding::Node { node, index } => format!("focus {} {}", node.name(), index + 1),
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
        Binding::Node { node, index } => format!("{} {}", node.short(), index + 1),
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
    for node in Node::ALL {
        let modifier = prefix(node).unwrap_or_default();
        let keys = format!("{modifier}{first} / {last}");
        out.push_str(&format!(
            "  {keys:<12} focus {} 1 to {nodes}\n",
            node.name()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{Focus, Params};

    /// Every node key under every modifier, which is the whole node
    /// vocabulary a map or a hand may reach.
    fn every_node_label() -> Vec<(Node, usize, String)> {
        Node::ALL
            .into_iter()
            .flat_map(|node| {
                node_labels(node)
                    .enumerate()
                    .map(move |(i, l)| (node, i, l))
            })
            .collect()
    }

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
            let Some(Action::Nudge(down_knob, down)) = action_for(axis.down.0, Node::Camera) else {
                panic!("{:?} should nudge", axis.down)
            };
            let Some(Action::Nudge(up_knob, up)) = action_for(axis.up.0, Node::Camera) else {
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
        let Some(Action::Nudge(knob, delta)) = action_for(KeyCode::Equal, Node::Camera) else {
            panic!("= should nudge a knob")
        };
        p.nudge(knob, delta, Focus::default());
        let zoom = p.cameras[0].framing.zoom;
        assert!((zoom - (before + Knob::Zoom.increment())).abs() < 1e-6);
    }

    #[test]
    fn the_commands_do_what_they_say() {
        assert_eq!(
            action_for(KeyCode::Space, Node::Camera),
            Some(Action::Clear)
        );
        assert_eq!(action_for(KeyCode::KeyR, Node::Camera), Some(Action::Reset));
        assert_eq!(
            action_for(KeyCode::Escape, Node::Camera),
            Some(Action::Quit)
        );
        assert_eq!(
            action_for(KeyCode::Semicolon, Node::Camera),
            Some(Action::Seed)
        );
        assert_eq!(action_for(KeyCode::Enter, Node::Camera), Some(Action::Solo));
        assert_eq!(action_for(KeyCode::F12, Node::Camera), None);
        assert_eq!(action_for_label("f12"), None);
    }

    #[test]
    fn a_modifier_is_the_node_keys_business_and_nobody_else_s() {
        // A held modifier must not make a knob key mean something else: on a
        // physical layout one is held for all sorts of reasons. The one
        // table that does read them reads them as the same shape — another
        // side of the focus — and never as a different value of one thing.
        for key in every_key() {
            let reads_them = NODE_KEYS.iter().any(|(bound, _)| *bound == key);
            let bare = action_for(key, Node::Camera);
            for node in Node::ALL.into_iter().filter(|n| prefix(*n).is_some()) {
                match reads_them {
                    true => assert_ne!(action_for(key, node), bare, "{key:?} {node:?}"),
                    false => assert_eq!(action_for(key, node), bare, "{key:?} {node:?}"),
                }
            }
        }
        // And the two modifiers do not collapse onto each other: three keys
        // sharing one action would be a surface that cannot reach a kind.
        assert_ne!(
            action_for(NODE_KEYS[0].0, Node::Monitor),
            action_for(NODE_KEYS[0].0, Node::Input)
        );
        assert_eq!(node_of(false, false), Node::Camera);
        assert_eq!(node_of(true, false), Node::Monitor);
        assert_eq!(node_of(false, true), Node::Input);
        assert_eq!(node_of(true, true), Node::Input);
    }

    #[test]
    fn a_node_key_names_one_kind_per_modifier() {
        for (node, index, label) in every_node_label() {
            let key = NODE_KEYS[index].0;
            assert_eq!(action_for(key, node), Some(Action::Focus(node, index)));
            // And the same through the label, which is the whole of what a
            // MIDI map may say — a button that reaches one kind and not
            // another is a surface with a vocabulary the keys have not got.
            assert_eq!(action_for_label(&label), Some(Action::Focus(node, index)));
            assert_eq!(
                describes(&label).unwrap(),
                format!("focus {} {}", node.name(), index + 1)
            );
            assert_eq!(
                short(&label).unwrap(),
                format!("{} {}", node.short(), index + 1)
            );
        }
        // The list stops at the keys, which is what stops a surface being
        // built for a node nothing could focus.
        assert_eq!(node_labels(Node::Camera).count(), KEYED_NODES);
    }

    #[test]
    fn the_new_commands_are_on_the_keys_their_labels_name() {
        // Against literal key codes, because the label and the code are two
        // different facts and `the_keys_and_the_labels_agree` reads each
        // command's own code — so it is true whatever code that is, and a
        // binding moved to another key would still agree with itself.
        assert_eq!(
            action_for(KeyCode::Backspace, Node::Camera),
            Some(Action::ResetLastKnob)
        );
        assert_eq!(action_for(KeyCode::KeyR, Node::Camera), Some(Action::Reset));
        for node in Node::ALL {
            assert_eq!(
                action_for(KeyCode::Numpad1, node),
                Some(Action::Focus(node, 0))
            );
        }
    }

    #[test]
    fn the_capture_controls_are_a_press_and_a_hold() {
        assert_eq!(
            action_for(KeyCode::F7, Node::Camera),
            Some(Action::Screencap)
        );
        assert_eq!(
            action_for(KeyCode::F8, Node::Camera),
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
        // The header, and one line per kind the node keys share.
        assert_eq!(
            help.lines().count(),
            AXES.len() + COMMANDS.len() + 1 + Node::ALL.len()
        );
        // Each kind's line names its own modifier in the key range rather
        // than only in the words: three identical ranges meaning three
        // different things is a card that teaches the wrong gesture.
        for node in Node::ALL {
            let line = help
                .lines()
                .find(|line| line.contains(&format!("focus {} 1 to 8", node.name())))
                .unwrap_or_else(|| panic!("no {} line", node.name()));
            assert!(
                line.contains(&node_labels(node).next().unwrap()),
                "{}: {line}",
                node.name()
            );
        }
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
        // label, and every label under each modifier, since a map may write
        // one.
        for label in labels() {
            for label in [
                label.to_string(),
                format!("shift {label}"),
                format!("ctrl {label}"),
            ] {
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
            for label in [
                label.to_string(),
                format!("shift {label}"),
                format!("ctrl {label}"),
            ] {
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
        assert_eq!(
            action_for(KeyCode::Backquote, Node::Camera),
            Some(Action::Overlay)
        );
        assert_eq!(
            action_for(KeyCode::Backquote, Node::Monitor),
            Some(Action::Overlay)
        );
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

        // A modifier in front of a key that does not read one is that key;
        // the MIDI map refuses such a binding rather than letting it look
        // like it means something.
        assert_eq!(action_for_label("shift r"), Some(Action::Reset));
        assert_eq!(action_for_label("ctrl r"), Some(Action::Reset));
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
                assert_eq!(action_for_label(label), action_for(key, Node::Camera));
            }
        }
        for c in COMMANDS {
            assert_eq!(action_for_label(c.label), action_for(c.key, Node::Camera));
        }
        for (key, label) in NODE_KEYS {
            assert_eq!(action_for_label(label), action_for(key, Node::Camera));
        }
    }

    #[test]
    fn a_node_key_selects_the_node_it_is_numbered_for() {
        // Numbered from one on the key and from zero in the graph.
        for (camera, (key, _)) in NODE_KEYS.iter().enumerate() {
            assert_eq!(
                action_for(*key, Node::Camera),
                Some(Action::Focus(Node::Camera, camera))
            );
        }
        let third = NODE_KEYS[2].1;
        assert_eq!(describes(third).as_deref(), Some("focus camera 3"));
        assert_eq!(short(third).as_deref(), Some("cam 3"));
        assert_eq!(
            describes(&format!("ctrl {third}")).as_deref(),
            Some("focus input 3")
        );
        assert_eq!(short(&format!("ctrl {third}")).as_deref(), Some("in 3"));
    }
}
