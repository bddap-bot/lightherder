//! The control surface: a Korg nanoKONTROL2 read off ALSA on a thread of its
//! own, its messages turned into [`Action`]s. It is the whole of what plays
//! this instrument — there is no keyboard.
//!
//! Two kinds of control, because a surface has two kinds of thing on it. A
//! fader or a rotary names a [`Knob`] and turns it by how far it has moved,
//! never to where it stands — the README says why. A button sends that it
//! was pushed, so it names an [`Action`].
//!
//! ALSA raw MIDI and no library: a USB controller is `/dev/snd/midiC<card>D0`
//! and reading it gives the wire bytes. Nothing here needs the sequencer's
//! routing or its timestamps — the instrument acts on a message when it
//! arrives — so libasound would be a dependency bought for a `File::open`.
//!
//! The same node is opened a second time for writing, which is what lights
//! the focused camera's button — see [`crate::lamps`]. A second open rather
//! than one read-write handle: a raw MIDI node's two directions are separate
//! substreams, so the read this instrument has always depended on — the one
//! that reports the unplug by ending — is left exactly as it was, and a
//! surface whose output will not open is still a surface that plays.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use web_time::Instant;

use crate::affine::Axis;
use crate::command::{Action, Edge};
use crate::lamps::{lamp, Lamplight, Lamps};
use crate::params::{Focus, Knob, Limit, Node, Params};

/// Where ALSA puts its character devices.
const DEV_SND: &str = "/dev/snd";

/// The one file that says which card is which. `/dev/snd` names a card by
/// number and nothing else, so a surface cannot be recognised without it.
const CARDS: &str = "/proc/asound/cards";

/// Matched, case-insensitively, against the lines of [`CARDS`]: the first
/// sound card whose line contains it is the surface. A substring because the
/// line carries the driver and the bus as well as the name.
pub(crate) const DEVICE: &str = "nanoKONTROL";

/// How often an absent surface is looked for. Hot-plug with no netlink
/// socket and no inotify: one small `/proc` read and a handful of stats once a
/// second cost nothing next to a frame, and a second is faster than a hand can
/// plug a cable in and reach the faders.
const RESCAN: Duration = Duration::from_secs(1);

/// The value at and above which a button counts as pushed. Everything sends 0
/// and 127, and the edge detect below is what makes a press one press — so the
/// halfway line is here for its distance from both ends, where neither noise
/// nor a slow release can cross it twice.
const PUSHED: u8 = 64;

/// One control change, and the whole of the MIDI a control surface sends that
/// this instrument reads. The channel nibble is not kept: every channel is
/// listened to, so there is nowhere for it to be read, and a field written and
/// never read is a filter somebody will later think exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlChange {
    control: u8,
    value: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fader {
    pub(crate) cc: u8,
    pub(crate) knob: Knob,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Button {
    pub(crate) cc: u8,
    pub(crate) action: Action,
}

const fn fader(cc: u8, knob: Knob) -> Fader {
    Fader { cc, knob }
}

const fn button(cc: u8, action: Action) -> Button {
    Button { cc, action }
}

/// The eight faders are the left hand's: the focused monitor's front panel,
/// then the focused switcher's period and its crossfade — the lever the
/// piece is played on, on the fader nearest the hand that is already on the
/// select rows. The rotaries above them are the right hand's: where the rig
/// stands on its shaft, how late the focused camera's cable is, and then the
/// focused monitor's frame rate, the one router-output setting a knob turns.
/// Twelve handles on sixteen controls, so there is no second page and the
/// four rotaries past the fourth are dead.
pub(crate) const FADERS: [Fader; 12] = [
    fader(0, Knob::Hue),
    fader(1, Knob::Saturation),
    fader(2, Knob::Brightness),
    fader(3, Knob::Contrast),
    fader(4, Knob::Temperature),
    fader(5, Knob::Sharpness),
    fader(6, Knob::Period),
    fader(7, Knob::Switcher),
    fader(16, Knob::Zoom),
    fader(17, Knob::Rotation),
    fader(18, Knob::Delay),
    fader(19, Knob::FrameRate),
];

/// Where a control number sits on the panel: the one copy of the device's
/// physical facts, which the overlay draws off.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Spot {
    Fader(u8),
    Rotary(u8),
    S(u8),
    M(u8),
    R(u8),
    Transport(&'static TransportButton),
}

/// One button of the transport strip, and where it sits in the strip's grid
/// of three rows.
#[derive(Debug)]
pub(crate) struct TransportButton {
    cc: u8,
    pub(crate) row: u8,
    pub(crate) col: u8,
    /// The name the silkscreen prints above this button's group, for the
    /// buttons that are in one. Carried on the buttons rather than as a
    /// second table of columns, so a label spans wherever this table puts
    /// the buttons it names and the two cannot drift apart.
    pub(crate) group: Option<&'static str>,
}

const fn transport(cc: u8, row: u8, col: u8, group: Option<&'static str>) -> TransportButton {
    TransportButton {
        cc,
        row,
        col,
        group,
    }
}

/// The transport strip as the device has it: the track pair on top; cycle
/// alone at the left of the middle row with the marker three set apart over
/// the last three columns; the tape row of five underneath. TRACK and
/// MARKER are printed above the groups they name.
pub(crate) const TRANSPORT: &[TransportButton] = &[
    transport(58, 0, 0, Some("TRACK")),
    transport(59, 0, 1, Some("TRACK")),
    transport(46, 1, 0, None),
    transport(60, 1, 2, Some("MARKER")),
    transport(61, 1, 3, Some("MARKER")),
    transport(62, 1, 4, Some("MARKER")),
    transport(43, 2, 0, None),
    transport(44, 2, 1, None),
    transport(42, 2, 2, None),
    transport(41, 2, 3, None),
    transport(45, 2, 4, None),
];

/// The control number the first strip's control carries, one per block. The
/// panel is eight strips wide and each block is that eight in a run, so a
/// block is named by where it starts.
const FADER_ROW: u8 = 0;
const ROTARY_ROW: u8 = 16;
const S_ROW: u8 = 32;
const M_ROW: u8 = 48;
const R_ROW: u8 = 64;

/// How many channel strips the panel has, and so how wide a select row is
/// and how deep any graph may go: a node past this would have no button.
pub(crate) const STRIPS: usize = 8;

const _: () = assert!(
    crate::rig::count(Node::Camera) <= STRIPS
        && crate::rig::count(Node::Monitor) <= STRIPS
        && crate::rig::count(Node::Switcher) <= STRIPS,
    "a count past the strips would name selects no button can carry"
);

/// The tails of the Record and Solo rows, which the rig leaves dead — the
/// switchers and the cameras stop short of them — so they are the select
/// buttons no rig can claim, and these seven cost the transport nothing.
pub(crate) const SELECT: u8 = R_ROW + STRIPS as u8 - 1;
pub(crate) const FLIP_X: u8 = SELECT - 2;
pub(crate) const FLIP_Y: u8 = SELECT - 1;
pub(crate) const REVERSE: u8 = FLIP_X - 1;
const _: () = assert!(crate::rig::count(Node::Switcher) as u8 + R_ROW <= REVERSE);
pub(crate) const CLUTCH: u8 = S_ROW + STRIPS as u8 - 1;
pub(crate) const COARSER: u8 = CLUTCH - 1;
pub(crate) const FINER: u8 = COARSER - 1;
const _: () = assert!(crate::rig::count(Node::Camera) as u8 + S_ROW <= FINER);

pub(crate) fn spot(cc: u8) -> Option<Spot> {
    let block = |first: u8| (cc >= first && cc < first + STRIPS as u8).then(|| cc - first);
    if let Some(i) = block(FADER_ROW) {
        return Some(Spot::Fader(i));
    }
    if let Some(i) = block(ROTARY_ROW) {
        return Some(Spot::Rotary(i));
    }
    if let Some(i) = block(S_ROW) {
        return Some(Spot::S(i));
    }
    if let Some(i) = block(M_ROW) {
        return Some(Spot::M(i));
    }
    if let Some(i) = block(R_ROW) {
        return Some(Spot::R(i));
    }
    TRANSPORT.iter().find(|t| t.cc == cc).map(Spot::Transport)
}

/// Which of the three select rows a kind of node is on. Solo selects because
/// that is what a hand off a mixer reaches for it to do; the other two rows
/// follow it downward in the order the light travels — the cameras that film
/// the glass, the glass, then what arrives from outside.
#[cfg(test)]
pub(crate) const fn row_of(node: Node) -> u8 {
    match node {
        Node::Camera => S_ROW,
        Node::Monitor => M_ROW,
        Node::Switcher => R_ROW,
    }
}

/// The three select rows, each exactly as wide as its kind of node — the
/// buttons past a kind's count are dead — then the transport strip and the
/// rows' tails. The tape row's left half is the reset ladder, in the order
/// of how much it takes back: rewind puts the last knob turned back, stop
/// puts the whole panel back. Cycle shows and hides the overlay that
/// explains all of the above — the one button whose job survives not
/// knowing what any button does. Marker set takes a still of the display,
/// and record records it for as long as a hand stays on it. The clutch is
/// the corner, findable by feel while the other hand is on the fader it is
/// freeing. The track pair and play are dead.
pub(crate) const BUTTONS: [Button; 27] = [
    button(S_ROW, Action::Focus(Node::Camera, 0)),
    button(S_ROW + 1, Action::Focus(Node::Camera, 1)),
    button(S_ROW + 2, Action::Focus(Node::Camera, 2)),
    button(M_ROW, Action::Focus(Node::Monitor, 0)),
    button(M_ROW + 1, Action::Focus(Node::Monitor, 1)),
    button(M_ROW + 2, Action::Focus(Node::Monitor, 2)),
    button(M_ROW + 3, Action::Focus(Node::Monitor, 3)),
    button(M_ROW + 4, Action::Focus(Node::Monitor, 4)),
    button(R_ROW, Action::Focus(Node::Switcher, 0)),
    button(R_ROW + 1, Action::Focus(Node::Switcher, 1)),
    button(R_ROW + 2, Action::Focus(Node::Switcher, 2)),
    button(R_ROW + 3, Action::Focus(Node::Switcher, 3)),
    button(62, Action::Clear),
    button(43, Action::ResetLastKnob),
    button(42, Action::Reset),
    button(46, Action::Overlay),
    button(44, Action::Solo),
    button(60, Action::Screencap),
    button(45, Action::Record(Edge::Down)),
    button(61, Action::Cut(Edge::Down)),
    button(REVERSE, Action::Reverse),
    button(FLIP_X, Action::Flip(Axis::X)),
    button(FLIP_Y, Action::Flip(Axis::Y)),
    button(SELECT, Action::Select),
    button(FINER, Action::Finer),
    button(COARSER, Action::Coarser),
    button(CLUTCH, Action::Clutch(Edge::Down)),
];

/// Every control number a button answers to, which is the whole of what the
/// surface may ever be told to light.
fn lamps() -> Lamplight {
    BUTTONS.iter().fold(0, |mask, b| mask | lamp(b.cc))
}

/// What the lamps say that the focus alone cannot. The caller owns every
/// one of them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shown {
    pub flipped: [bool; 2],
    /// Whether the focused monitor is on its switcher's program rather than
    /// on its own camera direct — the one bit the select button turns, and a
    /// latch with no lamp on a fullscreen display is a footgun.
    pub program: bool,
    pub overlay: bool,
    pub solo: bool,
}

/// One thing off the wire. A knob or a button is a control change; a system
/// exclusive frame is how the surface answers a question about itself, which
/// is the whole of the conversation that puts its lights under the host.
enum Message {
    Control(ControlChange),
    Sysex(Vec<u8>),
}

/// The longest system exclusive frame worth keeping. A scene dump is 402
/// bytes and nothing else is asked for, so this is where a frame that has
/// lost its end — a cable pulled mid-message — stops growing.
const SYSEX_MAX: usize = 512;

/// The MIDI byte stream, decoded as it arrives.
///
/// A stream, not a parser over a buffer, because a `read` lands wherever it
/// lands: a three-byte message routinely arrives as two reads. Running
/// status — a message with the status byte left off, which is how a surface
/// sends a fader sweep — is the reason this cannot be done a message at a
/// time either.
#[derive(Default)]
struct Stream {
    /// The status byte in force, or 0 for none.
    status: u8,
    data: [u8; 2],
    have: usize,
    /// The system exclusive frame being collected, its leading `F0` and all.
    /// `None` between frames, which is nearly always.
    sysex: Option<Vec<u8>>,
}

impl Stream {
    /// Feed one byte in, and push out any message it completed.
    fn push(&mut self, byte: u8, out: &mut Vec<Message>) {
        match byte {
            // Real time. Interleaved anywhere, even between a control number
            // and its value or inside a system exclusive frame, and it
            // disturbs neither.
            0xF8..=0xFF => {}
            // No running status survives a system message, so the data bytes
            // of a frame cannot be read as knob moves however this ends.
            0xF0 => {
                self.status = 0;
                self.sysex = Some(vec![0xF0]);
            }
            0xF7 => {
                self.status = 0;
                if let Some(mut frame) = self.sysex.take() {
                    frame.push(0xF7);
                    out.push(Message::Sysex(frame));
                }
            }
            // System common, and any channel message: either ends a frame
            // that never got its `F7`, which is what a surface unplugged
            // mid-dump leaves behind.
            0xF1..=0xF6 => {
                self.status = 0;
                self.sysex = None;
            }
            0x80..=0xEF => {
                self.status = byte;
                self.have = 0;
                self.sysex = None;
            }
            _ if self.sysex.is_some() => {
                let frame = self.sysex.as_mut().expect("just checked");
                frame.push(byte);
                // Longer than anything this asks for: dropped here rather
                // than grown until a frame with no end is the whole of
                // memory. The bytes after it are data under no status, which
                // nothing decodes.
                if frame.len() > SYSEX_MAX {
                    self.sysex = None;
                }
            }
            _ if self.status == 0 => {}
            _ => {
                self.data[self.have] = byte;
                self.have += 1;
                // Two, for every channel message this pairs up. Program
                // change and channel pressure carry one, so their data pairs
                // up wrongly here — and cannot be mistaken for a knob move
                // anyway, because a control change only ever arrives under a
                // 0xB0, and running status of a program change is another
                // program change.
                if self.have < 2 {
                    return;
                }
                self.have = 0;
                if self.status & 0xF0 == 0xB0 {
                    out.push(Message::Control(ControlChange {
                        control: self.data[0],
                        value: self.data[1],
                    }));
                }
            }
        }
    }
}

/// How much of a continuous knob's travel one full throw moves: a power
/// of two from the whole travel down to a sixteenth, a quarter to start.
/// A count knob runs its whole count over the throw and does not listen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Precision {
    halvings: u8,
}

impl Precision {
    const FINEST: u8 = 4;
    pub const DEFAULT: Precision = Precision { halvings: 2 };

    pub fn gain(self) -> f32 {
        1.0 / f32::from(1u8 << self.halvings)
    }

    fn finer(self) -> Precision {
        Precision {
            halvings: (self.halvings + 1).min(Self::FINEST),
        }
    }

    fn coarser(self) -> Precision {
        Precision {
            halvings: self.halvings.saturating_sub(1),
        }
    }
}

impl std::fmt::Display for Precision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "1/{}", 1u8 << self.halvings)
    }
}

/// One control change, for a test in another module to play into the
/// instrument. The fields stay private so that nothing outside this module
/// can invent a message the decoder would never have produced.
#[cfg(test)]
pub(crate) fn change(control: u8, value: u8) -> ControlChange {
    ControlChange { control, value }
}

/// The surface, connected or not.
pub struct Midi {
    /// Where ALSA is looked for. Constant in the instrument; a parameter so
    /// that discovery, the open and the decode can be run against a directory
    /// the tests wrote.
    snd: PathBuf,
    cards: PathBuf,
    port: Option<Port>,
    /// Where each control was last seen, by control number rather than by
    /// binding: a page turn puts another knob under the same fader, and the
    /// fader has not moved.
    standing: [Option<u8>; 128],
    /// What a whole-number knob has been turned by and not yet paid, within
    /// half a step either way.
    owed: [f32; Knob::ALL.len()],
    precision: Precision,
    /// Which buttons are being held, by control number. A button is acted on
    /// when it goes down, so a surface whose buttons latch — the
    /// nanoKONTROL2 can be set either way — plays every other press. Cleared
    /// only by a release passing through [`Midi::action_for`], which is why
    /// an unplug hands the caller one for every button.
    held: [bool; 128],
    next_scan: Instant,
    /// The last thing that went wrong looking for the surface, so a device
    /// that is there and will not open is said once rather than sixty times a
    /// second — and a *different* failure is still said.
    complaint: Option<String>,
}

struct Port {
    path: PathBuf,
    rx: Receiver<ControlChange>,
    /// The surface's lights, when its output opened. `None` is a surface
    /// that plays and does not light.
    lamps: Option<Lamps>,
}

/// The device end of the surface [`Midi::plug_in_a_test_surface`] plugs in.
#[cfg(test)]
pub(crate) struct TestSurface {
    pub(crate) wire: crate::lamps::Wire,
    /// The surface's controls. Held rather than dropped even by a test that
    /// touches none of them, because a dropped sender is exactly what
    /// [`Midi::poll`] reads as the cable coming out.
    controls: std::sync::mpsc::Sender<ControlChange>,
}

#[cfg(test)]
impl TestSurface {
    pub(crate) fn send(&self, control: u8, value: u8) {
        self.controls.send(change(control, value)).unwrap();
    }

    pub(crate) fn press(&self, control: u8) {
        self.send(control, 127);
    }

    pub(crate) fn release(&self, control: u8) {
        self.send(control, 0);
    }
}

impl Default for Midi {
    fn default() -> Midi {
        Midi {
            standing: [None; 128],
            owed: [0.0; Knob::ALL.len()],
            precision: Precision::DEFAULT,
            held: [false; 128],
            snd: PathBuf::from(DEV_SND),
            cards: PathBuf::from(CARDS),
            port: None,
            next_scan: Instant::now(),
            complaint: None,
        }
    }
}

impl Midi {
    pub fn precision(&self) -> Precision {
        self.precision
    }

    pub fn finer(&mut self) {
        self.precision = self.precision.finer();
    }

    pub fn forgive(&mut self, knobs: impl IntoIterator<Item = Knob>) {
        for knob in knobs {
            self.owed[knob as usize] = 0.0;
        }
    }

    pub fn coarser(&mut self) {
        self.precision = self.precision.coarser();
    }

    #[cfg(test)]
    pub(crate) fn standing(&self, cc: u8) -> Option<u8> {
        self.standing[usize::from(cc)]
    }

    fn clutched(&self) -> bool {
        self.held[usize::from(CLUTCH)]
    }

    /// Look somewhere other than the real ALSA for the surface. Tests only.
    #[cfg(test)]
    fn looking_in(mut self, snd: PathBuf, cards: PathBuf) -> Midi {
        self.snd = snd;
        self.cards = cards;
        self
    }

    /// Plug in a surface a test drives, and hand back the device end of its
    /// wire. Real lights on a real file descriptor: the only stand-in is the
    /// socket where the device node would be, so what a redraw asks the
    /// panel for is observable as the bytes it puts on it.
    ///
    /// A port and not just lamps, because the call site under test — the
    /// redraw's [`Midi::show`] — writes nothing unless there is a surface,
    /// and the port is what says there is one.
    #[cfg(test)]
    pub(crate) fn plug_in_a_test_surface(&mut self) -> TestSurface {
        let (lamps, wire) = crate::lamps::over_a_socket(lamps());
        let (controls, rx) = std::sync::mpsc::channel();
        self.port = Some(Port {
            path: PathBuf::from("a test's surface"),
            rx,
            lamps: Some(lamps),
        });
        TestSurface { wire, controls }
    }

    /// Every message the surface has sent since the last call, and the
    /// connecting and disconnecting around them. Called once a frame; never
    /// blocks, and never waits on a device that is not plugged in.
    ///
    /// Messages rather than actions, so the caller can turn each one into an
    /// action against the panel the one before it left — a fader and a
    /// button inside one frame is a real two-handed gesture, and a whole
    /// batch decided against one snapshot has the fader answering a panel
    /// the button has already moved.
    pub fn poll(&mut self) -> Vec<ControlChange> {
        self.connect();
        let mut messages = Vec::new();
        let mut gone = false;
        if let Some(port) = &self.port {
            loop {
                match port.rx.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        log::info!("surface: {} went away", port.path.display());
                        gone = true;
                        break;
                    }
                }
            }
        }
        // The messages already in hand are still this device's, and are
        // returned; what the unplug takes is the state, not the backlog. Then
        // every button is let go of, after that backlog, so a held mode ends
        // the way a release would have ended it rather than outliving the
        // surface that held it. Every button and not the held ones, because
        // the backlog may still hold a press; a release of a button nobody
        // is on is nothing.
        if gone {
            self.drop_port();
            messages.extend(BUTTONS.iter().map(|b| ControlChange {
                control: b.cc,
                value: 0,
            }));
        }
        messages
    }

    /// Light the panel for `focus`: the select button of the camera the
    /// rotaries turn and of the monitor the faders turn, every button a
    /// finger is on, and the button holding each latched mode that is on.
    ///
    /// The held ones are not decoration. Taking the surface's LED mode takes
    /// every button's light at once — see [`crate::lamps`] — so a button that
    /// lit itself under a finger has to be lit here or it goes dark for good.
    ///
    /// Said again every redraw rather than at each of the several places the
    /// focus moves: this is the one call that cannot miss one, and it also
    /// catches a surface plugged in halfway through a piece, where no focus
    /// change follows to light it. Saying the same panel again costs nothing
    /// on the wire.
    ///
    pub fn show(&self, focus: Focus, shown: Shown) {
        if let Some(lamps) = self.port.as_ref().and_then(|port| port.lamps.as_ref()) {
            lamps.show(self.wanted(focus, shown));
        }
    }

    /// The lamp of the button whose press is `action`, and nothing at all
    /// when no button does it: a node the rig has none of lights nothing
    /// rather than the nearest button.
    fn lamp_of(&self, action: Action) -> Lamplight {
        BUTTONS
            .iter()
            .find(|button| button.action == action)
            .map_or(0, |button| lamp(button.cc))
    }

    /// The panel [`Midi::show`] would ask for, apart from whether there is a
    /// surface to ask.
    pub(crate) fn wanted(&self, focus: Focus, shown: Shown) -> Lamplight {
        let when = |on: bool, action| if on { self.lamp_of(action) } else { 0 };
        let mut want = Node::ALL.into_iter().fold(0, |want, node| {
            want | self.lamp_of(Action::Focus(node, focus.at(node)))
        }) | when(shown.overlay, Action::Overlay)
            | when(shown.solo, Action::Solo)
            | when(shown.program, Action::Select);
        for axis in Axis::ALL {
            want |= when(shown.flipped[axis as usize], Action::Flip(axis));
        }
        for button in BUTTONS.iter().filter(|b| self.held[usize::from(b.cc)]) {
            want |= lamp(button.cc);
        }
        want
    }

    /// Let go of the surface. The buttons are not let go of here: `poll`
    /// hands back the messages that were already in hand and the caller is
    /// about to press them, so it appends the releases after that backlog.
    fn drop_port(&mut self) {
        self.port = None;
        // The next surface plugged in is standing wherever it was left.
        self.standing = [None; 128];
        // A fresh cable is a fresh chance for whatever went wrong last time
        // to have been the old one.
        self.complaint = None;
        self.next_scan = Instant::now() + RESCAN;
    }

    fn connect(&mut self) {
        if self.port.is_some() || Instant::now() < self.next_scan {
            return;
        }
        self.next_scan = Instant::now() + RESCAN;
        let cards = match std::fs::read_to_string(&self.cards) {
            Ok(cards) => cards,
            // Not a missing surface but a missing card list, which is a
            // different fault and would otherwise look like nothing plugged
            // in for the whole session.
            Err(e) => return self.complain(format!("{}: {e}", self.cards.display())),
        };
        let mut last = None;
        for path in find(&self.snd, &cards) {
            match open(&path, lamps()) {
                Ok(port) => {
                    log::info!("surface: {DEVICE} on {}", port.path.display());
                    self.complaint = None;
                    self.port = Some(port);
                    return;
                }
                // Kept and tried in turn rather than committed to: two cards
                // can match one name, and a card's lowest endpoint can be the
                // one that is busy or output-only.
                Err(why) => last = Some(why),
            }
        }
        if let Some(why) = last {
            self.complain(why);
        }
    }

    /// Say it once, and again only if it changes.
    fn complain(&mut self, why: String) {
        if self.complaint.as_deref() != Some(why.as_str()) {
            log::error!("surface: {why}");
            self.complaint = Some(why);
        }
    }

    /// What one control change does to the panel as it stands, if anything.
    pub fn action_for(&mut self, message: ControlChange, params: &Params) -> Option<Action> {
        // Where the control is, whatever it is bound to: a fader moved while
        // its page is hidden has still moved, and the knob it turns on the
        // other page must not be charged for that when the page comes back.
        let from = self.standing[usize::from(message.control)].replace(message.value);
        if let Some(fader) = FADERS.iter().find(|f| f.cc == message.control) {
            let steps = f32::from(message.value) - f32::from(from?);
            if self.clutched() {
                return None;
            }
            let limit = fader.knob.limit(params);
            let throw = steps / 127.0 * limit.travel();
            let paid = match limit {
                Limit::Whole(_) => {
                    let owed = &mut self.owed[fader.knob as usize];
                    *owed += throw;
                    let paid = owed.round();
                    *owed -= paid;
                    paid
                }
                Limit::Clamp(..) | Limit::Ratio(..) | Limit::Wrap => throw * self.precision.gain(),
            };
            return (paid != 0.0).then_some(Action::Turn(fader.knob, paid));
        }
        let button = BUTTONS.iter().find(|b| b.cc == message.control)?;
        let down = message.value >= PUSHED;
        let was = std::mem::replace(&mut self.held[usize::from(message.control)], down);
        match (down, was) {
            (true, false) => Some(button.action),
            (false, true) => crate::command::released(button.action),
            _ => None,
        }
    }
}

/// The card lines of [`CARDS`]: `" 2 [nanoKONTROL2  ]: USB-Audio - nanoKONTROL2"`.
/// Each card has a second, indented line as well, which has no number in
/// front and so is not one of these. The number is the card's, not its place
/// in the file — unloading a module leaves a gap.
fn cards(text: &str) -> impl Iterator<Item = (u32, &str)> {
    text.lines().filter_map(|line| {
        let index = line.split_whitespace().next()?.parse().ok()?;
        Some((index, line))
    })
}

/// Every raw MIDI endpoint of every card whose line names [`DEVICE`], in
/// the order to try them. All of them, not the first: two cards can match
/// one name, and the lowest endpoint of a card can be the one that is busy
/// or output-only — so the caller opens down the list rather than committing
/// to a candidate it cannot get past.
fn find<'a>(snd: &'a Path, cards_text: &'a str) -> impl Iterator<Item = PathBuf> + 'a {
    let wanted = DEVICE.to_lowercase();
    cards(cards_text)
        .filter(move |(_, line)| line.to_lowercase().contains(&wanted))
        .flat_map(|(card, _)| (0..8).map(move |d| (card, d)))
        .map(|(card, d)| snd.join(format!("midiC{card}D{d}")))
        .filter(|path| path.exists())
}

/// Open the device and read it on a thread, decoding as it goes — and open
/// it a second time for writing, which is what lights its buttons.
///
/// A thread rather than a non-blocking read polled from the frame loop: a
/// blocking `read` on a device nobody is touching costs nothing, and it is
/// the same `read` that reports the unplug. The channel is unbounded because
/// a button press dropped for backpressure is a press that did not happen,
/// and a surface sends a kilobyte a second at its very worst.
///
/// The write half is its own open, its own thread and its own failure: only
/// the read decides whether there is a surface at all. A node that will not
/// open for writing — its output substream already taken by something else —
/// is a surface that plays without lights, said once and then played.
fn open(path: &Path, buttons: Lamplight) -> Result<Port, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let lamps = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())
        .and_then(|out| Lamps::spawn(out, buttons));
    let lamps = match lamps {
        Ok(lamps) => Some(lamps),
        Err(why) => {
            log::warn!(
                "surface: {} has no lights for the instrument ({why}); its buttons light themselves",
                path.display()
            );
            None
        }
    };
    let frames = lamps.as_ref().map(Lamps::frames);
    let (tx, rx) = std::sync::mpsc::channel();
    let name = path.to_owned();
    std::thread::Builder::new()
        .name("midi".into())
        .spawn(move || {
            let mut stream = Stream::default();
            let mut buf = [0u8; 64];
            let mut out = Vec::new();
            loop {
                let read = match file.read(&mut buf) {
                    // The device is gone. Dropping `tx` is how the frame loop
                    // finds out, so there is nothing else to do about it.
                    Ok(0) => return,
                    Ok(read) => read,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                };
                for byte in &buf[..read] {
                    stream.push(*byte, &mut out);
                }
                for message in out.drain(..) {
                    match message {
                        Message::Control(cc) => {
                            if tx.send(cc).is_err() {
                                return;
                            }
                        }
                        Message::Sysex(frame) => {
                            if let Some(frames) = &frames {
                                frames.say(frame);
                            }
                        }
                    }
                }
            }
        })
        .map_err(|e| format!("{}: {e}", name.display()))?;
    Ok(Port {
        path: name,
        rx,
        lamps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_transport_grid_is_arranged_the_way_the_device_is() {
        assert_eq!(
            TRANSPORT
                .iter()
                .map(|t| (t.cc, t.row, t.col, t.group))
                .collect::<Vec<_>>(),
            [
                (58, 0, 0, Some("TRACK")),
                (59, 0, 1, Some("TRACK")),
                (46, 1, 0, None),
                (60, 1, 2, Some("MARKER")),
                (61, 1, 3, Some("MARKER")),
                (62, 1, 4, Some("MARKER")),
                (43, 2, 0, None),
                (44, 2, 1, None),
                (42, 2, 2, None),
                (41, 2, 3, None),
                (45, 2, 4, None),
            ],
        );
    }

    #[test]
    fn the_spots_are_the_four_blocks_and_the_strip() {
        let spot = |cc| spot(cc).map(|s| format!("{s:?}"));
        assert_eq!(spot(0).as_deref(), Some("Fader(0)"));
        assert_eq!(spot(7).as_deref(), Some("Fader(7)"));
        assert_eq!(spot(16).as_deref(), Some("Rotary(0)"));
        assert_eq!(spot(23).as_deref(), Some("Rotary(7)"));
        assert_eq!(spot(32).as_deref(), Some("S(0)"));
        assert_eq!(spot(39).as_deref(), Some("S(7)"));
        assert_eq!(spot(48).as_deref(), Some("M(0)"));
        assert_eq!(spot(55).as_deref(), Some("M(7)"));
        assert_eq!(spot(64).as_deref(), Some("R(0)"));
        assert_eq!(spot(71).as_deref(), Some("R(7)"));
        assert!(
            spot(41).unwrap().contains("row: 2, col: 3"),
            "{:?}",
            spot(41)
        );
        assert!(
            spot(59).unwrap().contains("row: 0, col: 1"),
            "{:?}",
            spot(59)
        );
        for gap in [8, 15, 24, 31, 40, 47, 56, 57, 63, 72, 127] {
            assert_eq!(spot(gap), None, "cc {gap}");
        }
    }

    fn cc(control: u8, value: u8) -> Vec<u8> {
        vec![0xB0, control, value]
    }

    fn messages(bytes: &[u8]) -> Vec<Message> {
        let mut stream = Stream::default();
        let mut out = Vec::new();
        for byte in bytes {
            stream.push(*byte, &mut out);
        }
        out
    }

    fn decode(bytes: &[u8]) -> Vec<ControlChange> {
        messages(bytes)
            .into_iter()
            .filter_map(|m| match m {
                Message::Control(cc) => Some(cc),
                Message::Sysex(_) => None,
            })
            .collect()
    }

    fn sysex(bytes: &[u8]) -> Vec<Vec<u8>> {
        messages(bytes)
            .into_iter()
            .filter_map(|m| match m {
                Message::Sysex(frame) => Some(frame),
                Message::Control(_) => None,
            })
            .collect()
    }

    fn message(control: u8, value: u8) -> ControlChange {
        ControlChange { control, value }
    }

    #[test]
    fn a_control_change_is_its_control_and_its_value() {
        assert_eq!(decode(&cc(7, 100)), [message(7, 100)]);
    }

    #[test]
    fn running_status_is_how_a_sweep_arrives() {
        let mut bytes = cc(5, 1);
        bytes.extend([5, 2, 5, 3]);
        assert_eq!(
            decode(&bytes),
            [message(5, 1), message(5, 2), message(5, 3)]
        );
    }

    #[test]
    fn a_clock_byte_in_the_middle_of_a_message_is_not_part_of_it() {
        let bytes = [0xB0, 0x07, 0xF8, 0x40, 0xFE, 0x07, 0x41];
        assert_eq!(decode(&bytes), [message(7, 0x40), message(7, 0x41)]);
    }

    #[test]
    fn a_scene_dump_is_not_a_hundred_knob_moves() {
        let dump = [0xF0, 0x42, 0x40, 0x00, 0x01, 0x13, 0x00, 0x7F, 0xF7];
        let mut bytes = cc(1, 9);
        bytes.extend(dump);
        bytes.extend([0x02, 0x05]);
        assert_eq!(decode(&bytes), [message(1, 9)]);
        assert_eq!(sysex(&bytes), [dump.to_vec()]);
    }

    #[test]
    fn a_frame_that_never_ends_is_dropped_rather_than_kept() {
        let mut bytes = vec![0xF0, 0x42];
        bytes.extend(std::iter::repeat_n(0x00, SYSEX_MAX * 2));
        bytes.extend(cc(7, 64));
        assert_eq!(decode(&bytes), [message(7, 64)]);
        assert!(sysex(&bytes).is_empty(), "an endless frame came out");

        let mut cut = vec![0xF0, 0x42, 0x40];
        cut.extend(cc(7, 65));
        assert_eq!(decode(&cut), [message(7, 65)]);
        assert!(sysex(&cut).is_empty(), "a frame with no end came out");

        let mut whole = vec![0xF0];
        whole.extend(std::iter::repeat_n(0x01, SYSEX_MAX - 2));
        whole.push(0xF7);
        assert_eq!(whole.len(), SYSEX_MAX);
        assert_eq!(sysex(&whole), [whole.clone()]);
    }

    #[test]
    fn a_real_time_byte_inside_a_frame_is_not_part_of_it() {
        let bytes = [0xF0, 0x42, 0xF8, 0x40, 0xFE, 0xF7];
        assert_eq!(sysex(&bytes), [vec![0xF0, 0x42, 0x40, 0xF7]]);
    }

    fn at(camera: usize, monitor: usize) -> Focus {
        Focus {
            camera,
            monitor,
            switcher: 0,
        }
    }

    #[test]
    fn the_lamp_of_each_node_is_the_button_that_selects_it() {
        let midi = Midi::default();
        for index in 0..crate::rig::count(Node::Camera) {
            let focus = Focus {
                camera: index,
                monitor: index,
                switcher: index,
            };
            assert_eq!(
                midi.wanted(focus, Shown::default()),
                lamp(S_ROW + index as u8) | lamp(M_ROW + index as u8) | lamp(R_ROW + index as u8),
                "node {index}"
            );
        }
    }

    #[test]
    fn the_panel_is_the_focused_pair_and_whatever_is_under_a_finger() {
        let (mut midi, params) = surface();
        let focus = at(2, 1);
        let pair = lamp(S_ROW + 2) | lamp(M_ROW + 1) | lamp(R_ROW);
        assert_eq!(midi.wanted(focus, Shown::default()), pair);
        assert_eq!(feed(&mut midi, &params, &cc(62, 127)), [Action::Clear]);
        assert_eq!(midi.wanted(focus, Shown::default()), pair | lamp(62));
        assert_eq!(feed(&mut midi, &params, &cc(62, 0)), []);
        assert_eq!(midi.wanted(focus, Shown::default()), pair);
        let switcher = lamp(R_ROW);
        assert_eq!(
            midi.wanted(at(99, 1), Shown::default()),
            lamp(M_ROW + 1) | switcher
        );
        assert_eq!(
            midi.wanted(at(2, 99), Shown::default()),
            lamp(S_ROW + 2) | switcher
        );
        assert_eq!(midi.wanted(at(99, 99), Shown::default()), switcher);
        let lost = Focus {
            camera: 99,
            monitor: 99,
            switcher: 99,
        };
        assert_eq!(midi.wanted(lost, Shown::default()), 0);
    }

    #[test]
    fn a_latched_mode_lights_the_button_that_holds_it() {
        let midi = Midi::default();
        let focus = at(0, 0);
        let base = midi.wanted(focus, Shown::default());
        let lit = |shown: Shown| midi.wanted(focus, shown) & !base;
        assert_eq!(
            lit(Shown {
                overlay: true,
                ..Shown::default()
            }),
            lamp(46),
            "the overlay is cycle"
        );
        assert_eq!(
            lit(Shown {
                solo: true,
                ..Shown::default()
            }),
            lamp(44),
            "the display's solo is forward"
        );
        assert_eq!(
            lit(Shown {
                program: true,
                ..Shown::default()
            }),
            lamp(SELECT),
            "the select is the record row's last button"
        );
        assert_eq!(
            lit(Shown {
                flipped: [true, false],
                ..Shown::default()
            }),
            lamp(FLIP_X)
        );
        assert_eq!(
            lit(Shown {
                flipped: [false, true],
                ..Shown::default()
            }),
            lamp(FLIP_Y)
        );
        assert_eq!(
            lit(Shown {
                flipped: [true, true],
                ..Shown::default()
            }),
            lamp(FLIP_X) | lamp(FLIP_Y)
        );
    }

    #[test]
    fn only_a_control_change_comes_out() {
        let mut bytes = vec![0x90, 0x40, 0x7F];
        bytes.extend([0xC0, 0x05]);
        bytes.extend([0xE0, 0x00, 0x40]);
        bytes.extend(cc(3, 11));
        assert_eq!(decode(&bytes), [message(3, 11)]);
    }

    #[test]
    fn a_message_this_decoder_pairs_up_wrongly_does_not_eat_the_next_one() {
        assert_eq!(decode(&[0xD0, 0x20, 0xB0, 0x07, 0x40]), [message(7, 0x40)]);
        assert_eq!(decode(&[0xC0, 0x05, 0xB0, 0x07, 0x41]), [message(7, 0x41)]);
    }

    const SAMPLE_CARDS: &str = "\
 1 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0xf7080000 irq 100
 2 [Generic        ]: HDA-Intel - HD-Audio Generic
                      HD-Audio Generic at 0xf7b00000 irq 102
 5 [nanoKONTROL2   ]: USB-Audio - nanoKONTROL2
                      KORG INC. nanoKONTROL2 at usb-0000:00:14.0-2, full speed
";

    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lightherder-midi-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn found(dir: &Path, cards: &str) -> Vec<PathBuf> {
        find(dir, cards).collect()
    }

    #[test]
    fn the_surface_is_found_by_name_among_the_cards_that_are_not_it() {
        let dir = scratch("find");
        for card in [1, 2, 5] {
            std::fs::write(dir.join(format!("midiC{card}D0")), "").unwrap();
        }
        assert_eq!(found(&dir, SAMPLE_CARDS), [dir.join("midiC5D0")]);
        assert_eq!(
            found(&dir, &SAMPLE_CARDS.to_uppercase()),
            [dir.join("midiC5D0")]
        );
        assert!(found(&dir, " 1 [NVidia ]: x\n 2 [Launchpad ]: y\n").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_card_with_no_raw_midi_device_is_not_a_surface() {
        let dir = scratch("no-device");
        assert!(found(&dir, SAMPLE_CARDS).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_endpoint_of_every_matching_card_is_offered_in_turn() {
        let dir = scratch("candidates");
        std::fs::write(dir.join("midiC5D3"), "").unwrap();
        assert_eq!(
            found(&dir, SAMPLE_CARDS),
            [dir.join("midiC5D3")],
            "an endpoint above D0 is still the surface"
        );
        let two = format!("{SAMPLE_CARDS} 6 [nanoKONTROL2_1 ]: USB-Audio - nanoKONTROL2\n");
        std::fs::write(dir.join("midiC6D0"), "").unwrap();
        assert_eq!(
            found(&dir, &two),
            [dir.join("midiC5D3"), dir.join("midiC6D0")]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_second_line_of_a_card_is_not_a_card() {
        let seen: Vec<u32> = cards(SAMPLE_CARDS).map(|(index, _)| index).collect();
        assert_eq!(seen, [1, 2, 5]);
    }

    #[test]
    fn the_panel_is_wired_the_way_the_readme_says() {
        assert_eq!(
            FADERS,
            [
                fader(0, Knob::Hue),
                fader(1, Knob::Saturation),
                fader(2, Knob::Brightness),
                fader(3, Knob::Contrast),
                fader(4, Knob::Temperature),
                fader(5, Knob::Sharpness),
                fader(6, Knob::Period),
                fader(7, Knob::Switcher),
                fader(16, Knob::Zoom),
                fader(17, Knob::Rotation),
                fader(18, Knob::Delay),
                fader(19, Knob::FrameRate),
            ]
        );
        assert_eq!(
            BUTTONS,
            [
                button(32, Action::Focus(Node::Camera, 0)),
                button(33, Action::Focus(Node::Camera, 1)),
                button(34, Action::Focus(Node::Camera, 2)),
                button(48, Action::Focus(Node::Monitor, 0)),
                button(49, Action::Focus(Node::Monitor, 1)),
                button(50, Action::Focus(Node::Monitor, 2)),
                button(51, Action::Focus(Node::Monitor, 3)),
                button(52, Action::Focus(Node::Monitor, 4)),
                button(64, Action::Focus(Node::Switcher, 0)),
                button(65, Action::Focus(Node::Switcher, 1)),
                button(66, Action::Focus(Node::Switcher, 2)),
                button(67, Action::Focus(Node::Switcher, 3)),
                button(62, Action::Clear),
                button(43, Action::ResetLastKnob),
                button(42, Action::Reset),
                button(46, Action::Overlay),
                button(44, Action::Solo),
                button(60, Action::Screencap),
                button(45, Action::Record(Edge::Down)),
                button(61, Action::Cut(Edge::Down)),
                button(68, Action::Reverse),
                button(69, Action::Flip(Axis::X)),
                button(70, Action::Flip(Axis::Y)),
                button(71, Action::Select),
                button(37, Action::Finer),
                button(38, Action::Coarser),
                button(39, Action::Clutch(Edge::Down)),
            ]
        );
        for cc in [20, 21, 22, 23, 41, 58, 59] {
            assert!(!FADERS.iter().any(|f| f.cc == cc), "cc {cc} is bound");
            assert!(!BUTTONS.iter().any(|b| b.cc == cc), "cc {cc} is bound");
        }
    }

    #[test]
    fn every_select_row_is_exactly_as_wide_as_its_kind() {
        for node in Node::ALL {
            let row: Vec<u8> = BUTTONS
                .iter()
                .filter(|b| matches!(b.action, Action::Focus(kind, _) if kind == node))
                .map(|b| b.cc)
                .collect();
            let want: Vec<u8> = (0..crate::rig::count(node))
                .map(|i| row_of(node) + i as u8)
                .collect();
            assert_eq!(row, want, "{}", node.short());
        }
    }

    #[test]
    fn every_press_has_exactly_one_button_and_every_knob_a_fader() {
        for action in [
            Action::Clear,
            Action::Reset,
            Action::ResetLastKnob,
            Action::Overlay,
            Action::Solo,
            Action::Screencap,
            Action::Record(Edge::Down),
            Action::Cut(Edge::Down),
            Action::Reverse,
            Action::Flip(Axis::X),
            Action::Flip(Axis::Y),
            Action::Select,
            Action::Finer,
            Action::Coarser,
            Action::Clutch(Edge::Down),
        ] {
            let on = BUTTONS.iter().filter(|b| b.action == action).count();
            assert_eq!(on, 1, "{action:?} is on {on} buttons");
        }
        let missing: Vec<&str> = Knob::ALL
            .into_iter()
            .filter(|knob| !FADERS.iter().any(|f| f.knob == *knob))
            .map(Knob::name)
            .collect();
        assert_eq!(missing, [""; 0]);
    }

    #[test]
    fn every_caption_is_two_words_at_most_and_names_one_control() {
        let mut captions: Vec<String> = FADERS
            .iter()
            .map(|f| f.knob.name().to_string())
            .chain(BUTTONS.iter().map(|b| b.action.caption()))
            .collect();
        for caption in &captions {
            assert!(caption.split_whitespace().count() <= 2, "{caption:?}");
        }
        let all = captions.len();
        captions.sort();
        captions.dedup();
        assert_eq!(captions.len(), all, "two controls share a caption");
    }

    #[test]
    fn every_bound_button_is_a_lamp_and_nothing_else_is() {
        for cc in 0..128u8 {
            assert_eq!(
                lamps() & lamp(cc) != 0,
                BUTTONS.iter().any(|b| b.cc == cc),
                "cc {cc}"
            );
        }
    }

    #[test]
    fn every_control_is_on_the_panel_once() {
        let mut ccs: Vec<u8> = FADERS
            .iter()
            .map(|f| f.cc)
            .chain(BUTTONS.iter().map(|b| b.cc))
            .collect();
        for cc in &ccs {
            assert!(spot(*cc).is_some(), "cc {cc} is nowhere on the panel");
        }
        let all = ccs.len();
        ccs.sort_unstable();
        ccs.dedup();
        assert_eq!(ccs.len(), all, "a control is bound twice");
    }

    fn surface() -> (Midi, Params) {
        (Midi::default(), crate::config::instrument())
    }

    fn feed(midi: &mut Midi, params: &Params, bytes: &[u8]) -> Vec<Action> {
        decode(bytes)
            .into_iter()
            .filter_map(|m| midi.action_for(m, params))
            .collect()
    }

    fn turned(midi: &mut Midi, params: &Params, value: u8) -> Option<f32> {
        match feed(midi, params, &cc(1, value))[..] {
            [] => None,
            [Action::Turn(Knob::Saturation, by)] => Some(by),
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_fader_turns_its_knob_by_how_far_it_moved_and_not_to_where_it_stands() {
        let (mut midi, params) = surface();
        assert_eq!(turned(&mut midi, &params, 127), None);
        let by = turned(&mut midi, &params, 0).unwrap();
        assert!((by + 1.0).abs() < 1e-6, "{by}");
        let by = turned(&mut midi, &params, 127).unwrap();
        assert!((by - 1.0).abs() < 1e-6, "{by}");
        let by = turned(&mut midi, &params, 126).unwrap();
        assert!((by + 1.0 / 127.0).abs() < 1e-6, "{by}");
        assert_eq!(turned(&mut midi, &params, 126), None);
    }

    #[test]
    fn the_precision_ladder_is_five_rungs_from_a_whole_travel_to_a_sixteenth() {
        let (mut midi, params) = surface();
        assert_eq!(midi.precision().to_string(), "1/4");
        let throw = |midi: &mut Midi| {
            turned(midi, &params, 0);
            turned(midi, &params, 127).unwrap() / 4.0
        };
        assert!((throw(&mut midi) - 0.25).abs() < 1e-6);
        for (press, want) in [
            (COARSER, "1/2"),
            (COARSER, "1/1"),
            (COARSER, "1/1"),
            (FINER, "1/2"),
            (FINER, "1/4"),
            (FINER, "1/8"),
            (FINER, "1/16"),
            (FINER, "1/16"),
        ] {
            match feed(&mut midi, &params, &cc(press, 127))[..] {
                [Action::Finer] => midi.finer(),
                [Action::Coarser] => midi.coarser(),
                ref other => panic!("{other:?}"),
            }
            feed(&mut midi, &params, &cc(press, 0));
            assert_eq!(midi.precision().to_string(), want);
            let fraction = throw(&mut midi);
            assert!(
                (fraction - midi.precision().gain()).abs() < 1e-6,
                "{want}: a full throw moved {fraction}"
            );
        }
    }

    #[test]
    fn the_clutch_holds_every_knob_still_and_lets_go_without_a_jump() {
        let (mut midi, params) = surface();
        assert_eq!(turned(&mut midi, &params, 127), None);
        assert_eq!(
            feed(&mut midi, &params, &cc(CLUTCH, 127)),
            [Action::Clutch(Edge::Down)]
        );
        assert_eq!(turned(&mut midi, &params, 64), None);
        assert_eq!(turned(&mut midi, &params, 0), None);
        assert_eq!(feed(&mut midi, &params, &cc(16, 50)), []);
        assert_eq!(feed(&mut midi, &params, &cc(16, 90)), []);
        assert_eq!(
            feed(&mut midi, &params, &cc(CLUTCH, 0)),
            [Action::Clutch(Edge::Up)]
        );
        let by = turned(&mut midi, &params, 10).unwrap();
        assert!((by - 10.0 / 127.0).abs() < 1e-6, "{by}");
        assert!(matches!(
            feed(&mut midi, &params, &cc(16, 91))[..],
            [Action::Turn(Knob::Zoom, by)] if (by - 16f32.ln() / 4.0 / 127.0).abs() < 1e-6
        ));
        let lit = |midi: &Midi| midi.wanted(at(0, 0), Shown::default()) & lamp(CLUTCH) != 0;
        assert!(!lit(&midi));
        feed(&mut midi, &params, &cc(CLUTCH, 127));
        assert!(lit(&midi));
        feed(&mut midi, &params, &cc(CLUTCH, 0));
        assert!(!lit(&midi));
    }

    #[test]
    fn a_count_knob_runs_its_whole_count_over_the_throw_a_step_at_a_time_deaf_to_the_precision() {
        let mut params = crate::config::instrument();
        params.delay = 4;
        let mut midi = Midi::default();
        let delay = |midi: &mut Midi, value: u8| match feed(midi, &params, &cc(18, value))[..] {
            [] => None,
            [Action::Turn(Knob::Delay, by)] => Some(by),
            ref other => panic!("{other:?}"),
        };
        assert_eq!(delay(&mut midi, 0), None);
        assert_eq!(delay(&mut midi, 15), None);
        assert_eq!(delay(&mut midi, 16), Some(1.0));
        assert_eq!(delay(&mut midi, 127), Some(3.0));
        midi.finer();
        assert_eq!(delay(&mut midi, 0), Some(-4.0));
        midi.coarser();
        midi.coarser();
        assert_eq!(delay(&mut midi, 127), Some(4.0));
    }

    #[test]
    fn rotaries_3_and_4_turn_delay_and_frame_rate_on_the_rig_as_launched() {
        let mut params = crate::config::instrument();
        let focus = Focus::default();
        let mut midi = Midi::default();
        for (control, knob) in [(18, Knob::Delay), (19, Knob::FrameRate)] {
            let before = params.knob(knob, focus);
            let mut wire = cc(control, 20);
            wire.extend(cc(control, 127));
            for action in feed(&mut midi, &params, &wire) {
                match action {
                    Action::Turn(turned, by) if turned == knob => params.nudge(knob, by, focus),
                    other => panic!("{other:?}"),
                }
            }
            let after = params.knob(knob, focus);
            assert!(after > before, "{knob:?}: {before} -> {after}");
            assert_ne!(knob.reads(after), knob.reads(before));
        }
        assert_eq!(params.cameras[0].delay, 2);
        assert_eq!(params.monitors[0].cadence, crate::params::Cadence::ALL[2]);
    }

    #[test]
    fn an_unplug_throws_nothing() {
        let (mut midi, params) = surface();
        assert_eq!(turned(&mut midi, &params, 40), None);
        midi.drop_port();
        assert_eq!(turned(&mut midi, &params, 0), None);
        assert!((turned(&mut midi, &params, 127).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unplugging_the_surface_lets_go_of_every_button_a_finger_was_on() {
        let (mut midi, params) = surface();
        let surface = midi.plug_in_a_test_surface();
        surface.press(45);
        surface.press(61);
        let play = |midi: &mut Midi| -> Vec<Action> {
            midi.poll()
                .into_iter()
                .filter_map(|m| midi.action_for(m, &params))
                .collect()
        };
        assert_eq!(
            play(&mut midi),
            [Action::Record(Edge::Down), Action::Cut(Edge::Down)]
        );
        surface.press(60);
        drop(surface);
        assert_eq!(
            play(&mut midi),
            [
                Action::Screencap,
                Action::Record(Edge::Up),
                Action::Cut(Edge::Up)
            ]
        );
        assert!(midi.held.iter().all(|held| !held));
    }

    #[test]
    fn a_button_acts_when_it_goes_down_and_not_when_it_comes_up() {
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), [Action::Select]);
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), [Action::Select]);
    }

    #[test]
    fn the_capture_buttons_are_a_press_and_a_hold() {
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(60, 127)), [Action::Screencap]);
        assert_eq!(feed(&mut midi, &params, &cc(60, 0)), []);
        assert_eq!(
            feed(&mut midi, &params, &cc(45, 127)),
            [Action::Record(Edge::Down)]
        );
        assert_eq!(feed(&mut midi, &params, &cc(45, 127)), []);
        assert_eq!(
            feed(&mut midi, &params, &cc(45, 0)),
            [Action::Record(Edge::Up)]
        );
        assert_eq!(feed(&mut midi, &params, &cc(45, 0)), []);
    }

    #[test]
    fn the_buttons_reach_the_nodes_and_the_transport() {
        let (mut midi, params) = surface();
        for (row, node, width) in [
            (S_ROW, Node::Camera, crate::rig::count(Node::Camera)),
            (M_ROW, Node::Monitor, crate::rig::count(Node::Monitor)),
            (R_ROW, Node::Switcher, crate::rig::count(Node::Switcher)),
        ] {
            for index in [0, width - 1] {
                assert_eq!(
                    feed(&mut midi, &params, &cc(row + index as u8, 127)),
                    [Action::Focus(node, index)],
                    "{} {index}",
                    node.short()
                );
            }
        }
        assert_eq!(
            feed(&mut midi, &params, &cc(REVERSE, 127)),
            [Action::Reverse]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(FLIP_X, 127)),
            [Action::Flip(Axis::X)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(FLIP_Y, 127)),
            [Action::Flip(Axis::Y)]
        );
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), [Action::Select]);
        assert_eq!(feed(&mut midi, &params, &cc(FINER, 127)), [Action::Finer]);
        assert_eq!(
            feed(&mut midi, &params, &cc(COARSER, 127)),
            [Action::Coarser]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(CLUTCH, 127)),
            [Action::Clutch(Edge::Down)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(43, 127)),
            [Action::ResetLastKnob]
        );
        assert_eq!(feed(&mut midi, &params, &cc(42, 127)), [Action::Reset]);
        assert_eq!(feed(&mut midi, &params, &cc(46, 127)), [Action::Overlay]);
    }

    #[test]
    fn a_dead_control_does_nothing() {
        let (mut midi, params) = surface();
        for dead in [20, 41, 58, 59, 100] {
            assert_eq!(feed(&mut midi, &params, &cc(dead, 127)), [], "cc {dead}");
            assert_eq!(feed(&mut midi, &params, &cc(dead, 0)), [], "cc {dead}");
        }
    }

    #[test]
    fn a_surface_on_another_channel_is_still_the_surface() {
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &[0xB9, 62, 127]), [Action::Clear]);
        assert_eq!(decode(&[0xB9, 0x07, 0x64]), [message(7, 0x64)]);
    }

    #[test]
    fn the_whole_path_runs_off_a_device_that_appears_and_goes_away_and_comes_back() {
        let dir = scratch("hotplug");
        let cards = dir.join("cards");
        std::fs::write(&cards, SAMPLE_CARDS).unwrap();
        let mut midi = Midi::default().looking_in(dir.clone(), cards);
        let params = Params::default();

        assert!(drain(&mut midi, &params).is_empty());

        let node = dir.join("midiC5D0");
        let mut pipe = plug(&node);
        pipe.write_all(&[0xB0, 0x01, 0x7F, 0x01]).unwrap();
        pipe.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        pipe.write_all(&[0x00, 0x01, 0x40]).unwrap();
        pipe.flush().unwrap();

        let acted = wait_for(&mut midi, &params);
        assert!(
            matches!(
                acted[..],
                [Action::Turn(Knob::Saturation, down), Action::Turn(Knob::Saturation, up)]
                    if (down + 1.0).abs() < 1e-6 && (up - 64.0 / 127.0).abs() < 1e-6
            ),
            "{acted:?}"
        );
        pipe.write_all(&[0xB0, 62, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(wait_for(&mut midi, &params), [Action::Clear]);

        drop(pipe);
        std::fs::remove_file(&node).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while midi.port.is_some() && Instant::now() < deadline {
            drain(&mut midi, &params);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(midi.port.is_none(), "the surface never went away");

        let mut pipe = plug(&node);
        pipe.write_all(&[0xB0, 62, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(
            wait_for(&mut midi, &params),
            [Action::Clear],
            "the surface did not come back"
        );
        pipe.write_all(&[0xB0, 0x02, 0x7F]).unwrap();
        pipe.flush().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            drain(&mut midi, &params).is_empty(),
            "the old position survived the unplug"
        );
        drop(pipe);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_focused_node_s_lamp_reaches_the_wire() {
        let mut midi = Midi::default();
        let mut surface = midi.plug_in_a_test_surface();
        surface.wire.handshake(0);
        let home = lamp(S_ROW) | lamp(M_ROW) | lamp(R_ROW);
        midi.show(Focus::default(), Shown::default());
        assert!(
            surface.wire.panel_becomes(home),
            "the panel the instrument started on never reached the wire"
        );
        let moved = Focus {
            camera: 1,
            ..Focus::default()
        };
        midi.show(moved, Shown::default());
        assert!(
            surface
                .wire
                .panel_becomes(lamp(S_ROW + 1) | lamp(M_ROW) | lamp(R_ROW)),
            "the lamp did not follow the focus onto camera 2"
        );
    }

    fn plug(node: &Path) -> std::fs::File {
        if !node.exists() {
            let status = std::process::Command::new("mkfifo")
                .arg(node)
                .status()
                .expect("mkfifo");
            assert!(status.success());
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .unwrap()
    }

    fn drain(midi: &mut Midi, params: &Params) -> Vec<Action> {
        midi.poll()
            .into_iter()
            .filter_map(|m| midi.action_for(m, params))
            .collect()
    }

    fn wait_for(midi: &mut Midi, params: &Params) -> Vec<Action> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let acted = drain(midi, params);
            if !acted.is_empty() {
                return acted;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Vec::new()
    }
}
