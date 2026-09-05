//! What a control on the surface does.

use crate::affine::Axis;
use crate::params::{Knob, Node};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Turn a knob by this much, in the knob's own units. A delta and not a
    /// position, so where a fader stands is the surface's business and not
    /// the instrument's.
    Turn(Knob, f32),
    /// Put the knobs' focus on one node of one kind outright, by its place
    /// in the graph. A select rather than a step: a hand that means "that
    /// one" should not have to walk past the ones it does not mean.
    ///
    /// The index is in range for the rig: the select rows are built as wide
    /// as it, so nothing downstream checks it a second time.
    Focus(Node, usize),
    Reset,
    /// Put the last knob that moved back to its identity, and nothing else.
    /// Named by having been turned rather than by a control of its own: the
    /// instrument has a panel of them and no display to point at one with,
    /// and the knob a hand wants back is the knob that hand was just on.
    ResetLastKnob,
    /// Blank every monitor, so the loops restart from the seed alone.
    Clear,
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
    /// A press, since pressing it again is the reverse of the reverse.
    Reverse,
    /// The focused monitor's router select: its own camera direct, or its
    /// switcher's program. A latch with a lamp, since one of the two is what
    /// the monitor is on and nothing else says which. The rotating monitor
    /// has no select, and the button is dead on it.
    Select,
    /// A latch on a button with a lamp, not a knob: a flip is on or off. On
    /// the focused monitor, because the rig mirrors a router output and not
    /// a camera.
    Flip(Axis),
    /// Halve or double what a full throw of a fader moves.
    Finer,
    Coarser,
    /// While held, every fader and rotary is inert, so a hand can bring one
    /// back from a rail it has hit.
    Clutch(Edge),
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
        Action::Clutch(Edge::Down) => Some(Action::Clutch(Edge::Up)),
        _ => None,
    }
}

impl Action {
    /// What the overlay captions a control with: two words at most.
    pub(crate) fn caption(self) -> String {
        match self {
            Action::Turn(knob, _) => knob.name().into(),
            Action::Focus(node, index) => format!("{} {}", node.short(), index + 1),
            Action::Reset => "reset".into(),
            Action::ResetLastKnob => "reset 1".into(),
            Action::Clear => "blank".into(),
            Action::Solo => "solo".into(),
            Action::Overlay => "help".into(),
            Action::Screencap => "snap".into(),
            Action::Record(_) => "record".into(),
            Action::Cut(_) => "cut".into(),
            Action::Reverse => "reverse".into(),
            Action::Select => "select".into(),
            Action::Flip(Axis::X) => "flip x".into(),
            Action::Flip(Axis::Y) => "flip y".into(),
            Action::Finer => "precision -".into(),
            Action::Coarser => "precision +".into(),
            Action::Clutch(_) => "clutch".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_select_names_its_kind_and_its_place() {
        assert_eq!(Action::Focus(Node::Camera, 2).caption(), "cam 3");
        assert_eq!(Action::Focus(Node::Monitor, 0).caption(), "mon 1");
        assert_eq!(Action::Focus(Node::Switcher, 3).caption(), "sw 4");
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
        assert_eq!(
            released(Action::Clutch(Edge::Down)),
            Some(Action::Clutch(Edge::Up))
        );
        for action in [
            Action::Reset,
            Action::ResetLastKnob,
            Action::Clear,
            Action::Solo,
            Action::Overlay,
            Action::Screencap,
            Action::Reverse,
            Action::Select,
            Action::Flip(Axis::X),
            Action::Finer,
            Action::Coarser,
            Action::Focus(Node::Camera, 0),
            Action::Record(Edge::Up),
            Action::Cut(Edge::Up),
            Action::Clutch(Edge::Up),
        ] {
            assert_eq!(released(action), None, "{action:?}");
        }
    }
}
