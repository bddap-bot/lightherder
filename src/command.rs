//! What a control on the surface does. One table, driving both the lookup a
//! `midi.toml` is resolved against and the card the instrument prints, so the
//! two cannot drift apart.

use crate::params::{Knob, Node};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Put a knob at a value outright, which is what a fader does by standing
    /// somewhere. Absolute, not a position, so where the ends of a fader are
    /// is the surface's business and not the instrument's.
    Set(Knob, f32),
    /// Put the knobs' focus on one node of one kind outright, by its place
    /// in the graph. A select rather than a step: a hand that means "that
    /// one" should not have to walk past the ones it does not mean.
    ///
    /// The index is in range for the graph the map was validated against —
    /// [`crate::midi::Map::validate`] refuses a binding past it — so nothing
    /// downstream checks it a second time.
    Focus(Node, usize),
    Reset,
    /// Put the last knob that moved back to its identity, and nothing else.
    /// Named by having been turned rather than by a control of its own: the
    /// instrument has a panel of them and no display to point at one with,
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
    Overlay,
    Screencap,
    Record(Edge),
    /// The switcher's foot pedal. Held rather than latched because the trap
    /// is the flick back.
    Cut(Edge),
    /// Turn the faders and rotaries over to their other page of knobs.
    Page,
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

pub fn released(action: Action) -> Option<Action> {
    match action {
        Action::Record(Edge::Down) => Some(Action::Record(Edge::Up)),
        Action::Cut(Edge::Down) => Some(Action::Cut(Edge::Up)),
        _ => None,
    }
}

struct Command {
    name: &'static str,
    action: Action,
    what: &'static str,
}

const COMMANDS: &[Command] = &[
    cmd("blank", Action::Clear, "blank every monitor"),
    cmd("reset", Action::Reset, "reset every knob"),
    cmd(
        "seed",
        Action::Seed,
        "the focused monitor's seed: a white blob or dark glass",
    ),
    cmd(
        "reset 1",
        Action::ResetLastKnob,
        "reset the last knob turned",
    ),
    cmd(
        "rate -",
        Action::Tempo(crate::tempo::Step::Slower),
        "slow the piece down (four presses halve the rate)",
    ),
    cmd(
        "rate +",
        Action::Tempo(crate::tempo::Step::Faster),
        "speed the piece up (four presses double the rate)",
    ),
    cmd(
        "solo",
        Action::Solo,
        "the focused monitor on the whole display, or the tiled bank",
    ),
    cmd("help", Action::Overlay, "the controls overlay, on or off"),
    cmd(
        "snap",
        Action::Screencap,
        "write what the display is showing to a file",
    ),
    cmd(
        "record",
        Action::Record(Edge::Down),
        "record the display for as long as this is held down",
    ),
    cmd(
        "cut",
        Action::Cut(Edge::Down),
        "the focused monitor shows only the focused input (or camera) while this is held down",
    ),
    cmd(
        "page",
        Action::Page,
        "the faders and rotaries on their other page of knobs; lit on page 2",
    ),
];

const fn cmd(name: &'static str, action: Action, what: &'static str) -> Command {
    Command { name, action, what }
}

/// The one place a kind and a number are written into a name. It stops at the
/// most of that kind a graph may legally hold, so every name it mints is one
/// a `midi.toml` can actually bind.
pub fn select_names(node: Node) -> impl Iterator<Item = String> {
    (1..=crate::config::cap(node)).map(move |i| format!("{} {i}", node.short()))
}

/// The one walk behind [`action_for_name`] and [`describes`], so what a name
/// reaches cannot depend on which of the two asked.
enum Binding {
    Select { node: Node, index: usize },
    Command(&'static Command),
}

fn binding(name: &str) -> Option<Binding> {
    for node in Node::ALL {
        if let Some(index) = select_names(node).position(|select| select == name) {
            return Some(Binding::Select { node, index });
        }
    }
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .map(Binding::Command)
}

/// Every name a `midi.toml` may bind a button to, in the order the card
/// prints them.
pub fn names() -> impl Iterator<Item = String> {
    Node::ALL
        .into_iter()
        .flat_map(select_names)
        .chain(command_names().map(String::from))
}

/// The commands that are not a select. Named apart from the rest of the
/// vocabulary because the surface is held to them: a command with no button
/// on the board is one nobody plays.
pub fn command_names() -> impl Iterator<Item = &'static str> {
    COMMANDS.iter().map(|c| c.name)
}

/// `None` for a name no table claims, which is how a hand-written `midi.toml`
/// is caught at load rather than in the middle of a performance.
pub fn action_for_name(name: &str) -> Option<Action> {
    Some(match binding(name)? {
        Binding::Select { node, index } => Action::Focus(node, index),
        Binding::Command(c) => c.action,
    })
}

/// What the command called `name` does, in the words the printed card uses.
pub fn describes(name: &str) -> Option<String> {
    Some(match binding(name)? {
        Binding::Select { node, index } => format!("focus {} {}", node.name(), index + 1),
        Binding::Command(c) => c.what.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_reaches_exactly_one_action() {
        let names: Vec<String> = names().collect();
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name} names two bindings");
            assert!(action_for_name(name).is_some(), "{name}");
            assert!(!describes(name).unwrap().is_empty(), "{name}");
        }
        assert_eq!(action_for_name("wiggle"), None);
        assert_eq!(describes("wiggle"), None);
    }

    #[test]
    fn a_select_names_its_kind_and_its_place() {
        for node in Node::ALL {
            for (index, name) in select_names(node).enumerate() {
                assert_eq!(action_for_name(&name), Some(Action::Focus(node, index)));
                assert_eq!(
                    describes(&name).unwrap(),
                    format!("focus {} {}", node.name(), index + 1)
                );
            }
            assert_eq!(select_names(node).count(), crate::config::cap(node));
        }
        // The one place the actual words are pinned: every other select test
        // builds its expectation out of `Node::short` and `Node::name`, so a
        // rename would move expectation and reality together. Numbered from
        // one on the button and from zero in the graph.
        assert_eq!(
            action_for_name("cam 3"),
            Some(Action::Focus(Node::Camera, 2))
        );
        assert_eq!(describes("mon 3").as_deref(), Some("focus monitor 3"));
        assert_eq!(action_for_name("in 1"), Some(Action::Focus(Node::Input, 0)));
        // And it stops where the graph does: a fifth input is refused at
        // load, so a name for one is a binding the loader would reject and a
        // line in the refusal's own list of what to write instead.
        assert_eq!(action_for_name("in 5"), None);
        assert_eq!(action_for_name("cam 9"), None);
    }

    #[test]
    fn the_commands_do_what_they_say() {
        assert_eq!(action_for_name("blank"), Some(Action::Clear));
        assert_eq!(action_for_name("reset"), Some(Action::Reset));
        assert_eq!(action_for_name("seed"), Some(Action::Seed));
        assert_eq!(action_for_name("solo"), Some(Action::Solo));
        assert_eq!(action_for_name("help"), Some(Action::Overlay));
        assert_eq!(action_for_name("snap"), Some(Action::Screencap));
        assert_eq!(action_for_name("record"), Some(Action::Record(Edge::Down)));
        assert_eq!(action_for_name("cut"), Some(Action::Cut(Edge::Down)));
        assert_eq!(action_for_name("page"), Some(Action::Page));
        assert_eq!(describes("reset").as_deref(), Some("reset every knob"));
    }

    #[test]
    fn letting_go_reaches_only_the_held_commands() {
        assert_eq!(
            released(Action::Record(Edge::Down)),
            Some(Action::Record(Edge::Up))
        );
        assert_eq!(
            released(Action::Cut(Edge::Down)),
            Some(Action::Cut(Edge::Up))
        );
        // Every other binding is a press, so a release of one must reach
        // nothing rather than firing it a second time.
        for name in names() {
            let action = action_for_name(&name).expect("every name resolves");
            assert_eq!(
                released(action).is_some(),
                matches!(action, Action::Record(Edge::Down) | Action::Cut(Edge::Down)),
                "{name}"
            );
        }
    }

    #[test]
    fn a_name_is_two_words_at_most() {
        // The ceiling for text on the panel, held where the names are
        // written: a name is what the overlay captions its button with.
        for name in names() {
            assert!(name.split_whitespace().count() <= 2, "{name:?}");
        }
    }
}
