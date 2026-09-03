//! The control surface: a MIDI device read off ALSA on a thread of its own,
//! its messages turned into [`Action`]s. It is the whole of what plays this
//! instrument — there is no keyboard.
//!
//! Two kinds of control, because a surface has two kinds of thing on it. A
//! fader or a rotary names a [`Knob`] and turns it by how far it has moved,
//! never to where it stands — the README says why. A button sends that it
//! was pushed, so it names a command by the two words [`crate::command`]
//! spells it with.
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

use serde::Deserialize;
use web_time::Instant;

use crate::affine::Axis;
use crate::command::{action_for_name, names, Action, Edge};
use crate::lamps::{lamp, Lamplight, Lamps};
use crate::params::{Focus, Knob, Limit, Node, Params};

/// Where ALSA puts its character devices.
const DEV_SND: &str = "/dev/snd";

/// The one file that says which card is which. `/dev/snd` names a card by
/// number and nothing else, so a surface cannot be recognised without it.
const CARDS: &str = "/proc/asound/cards";

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

/// What the surface's controls are wired to.
///
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    /// Matched, case-insensitively, against the lines of [`CARDS`]: the
    /// first sound card whose line contains it is the surface. A substring
    /// because the line carries the driver and the bus as well as the name.
    pub device: String,
    /// Continuous controls. Every channel is listened to: a surface with a
    /// global MIDI channel setting should work whatever it is set to, and
    /// nothing else is going to be plugged into this instrument.
    #[serde(default)]
    pub fader: Vec<Fader>,
    /// Buttons, by the command each one runs.
    #[serde(default)]
    pub button: Vec<Button>,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fader {
    pub(crate) cc: u8,
    pub(crate) knob: Knob,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
    pub(crate) cc: u8,
    /// A command, spelled the way the panel captions it — `"reset"`,
    /// `"blank"`, `"mon 1"`.
    pub(crate) command: String,
}

const fn fader(cc: u8, knob: Knob) -> Fader {
    Fader { cc, knob }
}

/// A relative `XDG_CONFIG_HOME` is ignored the way the spec says to.
pub fn map_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("lightherder")
        .join("midi.toml")
}

impl Map {
    /// The factory CC layout of a Korg nanoKONTROL2, which is what this
    /// instrument is played from.
    ///
    /// The eight faders are the left hand's: the focused monitor's front
    /// panel, then the focused switcher's period and its crossfade — the
    /// lever the piece is played on, on the fader nearest the hand that is
    /// already on the select rows. The rotaries above them are the right
    /// hand's: the focused camera — where it stands on its shaft and how
    /// late its cable is — and then the focused monitor's frame rate, the
    /// one router-output setting a knob turns. Twelve handles on sixteen
    /// controls, so there is no second page and the four rotaries past the
    /// fourth are dead — free for a `midi.toml`, and no hand throws one by
    /// accident. The select rows are as wide as the rig — see
    /// [`nano_buttons`].
    pub(crate) fn nano_kontrol2() -> Map {
        Map {
            device: "nanoKONTROL".into(),
            fader: [
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
            .into(),
            button: nano_buttons(),
        }
    }

    /// The surface as it is actually mapped, one control a line, for the
    /// card a performer keeps beside the instrument. Generated from the map
    /// in force rather than written out beside it, so a `midi.toml` that
    /// moves a knob moves it on the card too.
    pub fn card(&self) -> String {
        let mut out = format!(
            "surface: the first card named {:?}, on any MIDI channel\n",
            self.device
        );
        for f in &self.fader {
            let control = silkscreen(&self.device, f.cc);
            out.push_str(&format!("  {control:<12} {}\n", f.knob.name()));
        }
        for b in &self.button {
            let control = silkscreen(&self.device, b.cc);
            // The name as well as the sentence: the name is what a
            // `midi.toml` writes, so a performer reading the card has read
            // the vocabulary too.
            let what = crate::command::describes(&b.command).unwrap_or_default();
            out.push_str(&format!("  {control:<12} {what} ({})\n", b.command));
        }
        out
    }

    /// The performer's map if there is one, the factory layout if there is
    /// not. A file that is there and will not parse is an error rather than
    /// a silent fall back to the default: a surface that quietly plays the
    /// wrong knobs is worse than one that will not start.
    pub fn load(path: &Path) -> Result<Map, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // No file, or nowhere a file could be — a browser has no
            // filesystem at all, and "there is no map" is the same answer.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::Unsupported
                ) =>
            {
                return Ok(Map::nano_kontrol2())
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let map: Map = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        map.validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(map)
    }

    /// Everything about a map that has to be true before a note of it is
    /// played, checked once at load — the surface is read inside the frame
    /// loop and there is nothing to report an error to there.
    fn validate(&self) -> Result<(), String> {
        if self.device.is_empty() {
            return Err("device is empty, which every card's line contains".into());
        }
        let mut seen: Vec<u8> = Vec::new();
        let faders = self.fader.iter().map(|f| f.cc);
        for cc in faders.chain(self.button.iter().map(|b| b.cc)) {
            if cc > 127 {
                return Err(format!("cc {cc} is not a control number; they stop at 127"));
            }
            if seen.contains(&cc) {
                return Err(format!("cc {cc} is bound twice"));
            }
            seen.push(cc);
        }
        for b in &self.button {
            if action_for_name(&b.command).is_none() {
                let known: Vec<String> = names().collect();
                return Err(format!(
                    "cc {}: no command called {:?}; there are {}",
                    b.cc,
                    b.command,
                    known.join(", ")
                ));
            }
        }
        Ok(())
    }
}

/// Where a control number sits on a nanoKONTROL2's panel: the one copy of
/// the device's physical facts. [`silkscreen`] names a spot and the overlay
/// places one, both off this table, so the card and the picture cannot call
/// the same number two different controls.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Spot {
    Fader(u8),
    Rotary(u8),
    S(u8),
    M(u8),
    R(u8),
    Transport(&'static TransportButton),
}

/// One button of the transport strip: what is printed on it, and where it
/// sits in the strip's grid of three rows.
#[derive(Debug)]
pub(crate) struct TransportButton {
    cc: u8,
    pub(crate) name: &'static str,
    pub(crate) row: u8,
    pub(crate) col: u8,
    /// The name the silkscreen prints above this button's group, for the
    /// buttons that are in one. Carried on the buttons rather than as a
    /// second table of columns, so a label spans wherever this table puts
    /// the buttons it names and the two cannot drift apart.
    pub(crate) group: Option<&'static str>,
}

const fn transport(
    cc: u8,
    name: &'static str,
    row: u8,
    col: u8,
    group: Option<&'static str>,
) -> TransportButton {
    TransportButton {
        cc,
        name,
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
    transport(58, "track prev", 0, 0, Some("TRACK")),
    transport(59, "track next", 0, 1, Some("TRACK")),
    transport(46, "cycle", 1, 0, None),
    transport(60, "marker set", 1, 2, Some("MARKER")),
    transport(61, "marker prev", 1, 3, Some("MARKER")),
    transport(62, "marker next", 1, 4, Some("MARKER")),
    transport(43, "rewind", 2, 0, None),
    transport(44, "forward", 2, 1, None),
    transport(42, "stop", 2, 2, None),
    transport(41, "play", 2, 3, None),
    transport(45, "record", 2, 4, None),
];

/// The control number the first strip's control carries, one per block. The
/// panel is eight strips wide and each block is that eight in a run, so a
/// block is named by where it starts.
const FADERS: u8 = 0;
const ROTARIES: u8 = 16;
const S_ROW: u8 = 32;
const M_ROW: u8 = 48;
const R_ROW: u8 = 64;

/// How many buttons a select row has, and so how deep any graph may go: the
/// panel is eight channel strips wide, so a node past this would have no
/// button.
pub const ROW_BUTTONS: usize = 8;

const _: () = assert!(
    crate::rig::count(Node::Camera) <= ROW_BUTTONS
        && crate::rig::count(Node::Monitor) <= ROW_BUTTONS
        && crate::rig::count(Node::Switcher) <= ROW_BUTTONS,
    "a count past the select row would name selects no button can carry"
);

/// The tails of the Record and Solo rows, which the rig leaves dead — the
/// switchers and the cameras stop short of them — so they are the select
/// buttons no rig can claim, and these seven cost the transport nothing.
pub(crate) const SELECT: u8 = R_ROW + ROW_BUTTONS as u8 - 1;
pub(crate) const FLIP_X: u8 = SELECT - 2;
pub(crate) const FLIP_Y: u8 = SELECT - 1;
pub(crate) const REVERSE: u8 = FLIP_X - 1;
const _: () = assert!(crate::rig::count(Node::Switcher) as u8 + R_ROW <= REVERSE);
pub(crate) const CLUTCH: u8 = S_ROW + ROW_BUTTONS as u8 - 1;
pub(crate) const COARSER: u8 = CLUTCH - 1;
pub(crate) const FINER: u8 = COARSER - 1;
const _: () = assert!(crate::rig::count(Node::Camera) as u8 + S_ROW <= FINER);

pub(crate) fn spot(cc: u8) -> Option<Spot> {
    let block = |first: u8| (cc >= first && cc < first + ROW_BUTTONS as u8).then(|| cc - first);
    if let Some(i) = block(FADERS) {
        return Some(Spot::Fader(i));
    }
    if let Some(i) = block(ROTARIES) {
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

/// What is printed beside control `cc` on the surface named `device`. Off the
/// nanoKONTROL2's factory CC layout, the same one [`Map::nano_kontrol2`] is
/// written against — so a map that moves a knob to another control still
/// names the control a hand reaches for.
///
/// Any other surface gets numbers: these names are one instrument's
/// silkscreen, and a card that invents a control a performer cannot find is
/// worse than one that prints the number they can. A number no silkscreen
/// claims prints as itself for the same reason.
pub fn silkscreen(device: &str, cc: u8) -> String {
    if !nano_kontrol2(device) {
        return format!("cc {cc}");
    }
    match spot(cc) {
        Some(Spot::Fader(i)) => format!("fader {}", i + 1),
        Some(Spot::Rotary(i)) => format!("rotary {}", i + 1),
        Some(Spot::S(i)) => format!("S{}", i + 1),
        Some(Spot::M(i)) => format!("M{}", i + 1),
        Some(Spot::R(i)) => format!("R{}", i + 1),
        Some(Spot::Transport(t)) => t.name.into(),
        None => format!("cc {cc}"),
    }
}

/// Whether `device` is the surface whose silkscreen and physical layout this
/// crate knows. One test, shared by the card's names and the overlay's
/// geometry: the two must agree on which surface they are drawing.
pub(crate) fn nano_kontrol2(device: &str) -> bool {
    device.to_lowercase().contains("nanokontrol")
}

fn button(cc: u8, command: impl Into<String>) -> Button {
    Button {
        cc,
        command: command.into(),
    }
}

/// Which of the three select rows a kind of node is on. Solo selects because
/// that is what a hand off a mixer reaches for it to do; the other two rows
/// follow it downward in the order the light travels — the cameras that film
/// the glass, the glass, then what arrives from outside.
pub(crate) const fn row_of(node: Node) -> u8 {
    match node {
        Node::Camera => S_ROW,
        Node::Monitor => M_ROW,
        Node::Switcher => R_ROW,
    }
}

/// The nanoKONTROL2's buttons for the rig: the three select rows, and the
/// transport strip.
///
/// **A row is exactly as wide as its kind of node.** The buttons past a
/// kind's count are dead: dark, silent, and free for a `midi.toml` to claim.
///
/// Off [`crate::command`]'s own names rather than a second copy of them, so a
/// row cannot run past the buttons and a renamed command takes its button
/// with it.
fn nano_buttons() -> Vec<Button> {
    let mut out = Vec::new();
    for node in Node::ALL {
        for (index, name) in crate::command::select_names(node).enumerate() {
            out.push(button(row_of(node) + index as u8, name));
        }
    }
    out.extend([
        button(62, "blank"),
        // The tape row's left half is the reset ladder, in the order of how
        // much it takes back: rewind puts the last knob turned back, stop
        // puts the whole panel back.
        button(43, "reset 1"),
        button(42, "reset"),
        // Cycle shows and hides the overlay that explains all of the above —
        // the one button whose job survives not knowing what any button does.
        button(46, "help"),
        button(44, "solo"),
        // The tempo, on the one pair the silkscreen groups. A step and not a
        // fader, so `--rate` stays where the piece starts rather than being
        // thrown to wherever a cap was left standing.
        button(58, "rate -"),
        button(59, "rate +"),
        // The capture pair, on the two buttons whose silkscreen already says
        // what they do: marker set takes a still of the display, and record
        // records it for as long as a hand stays on it.
        button(60, "snap"),
        button(45, "record"),
        button(61, "cut"),
        button(REVERSE, "reverse"),
        button(FLIP_X, "flip x"),
        button(FLIP_Y, "flip y"),
        button(SELECT, "select"),
        button(FINER, "precision -"),
        button(COARSER, "precision +"),
        // The corner, findable by feel while the other hand is on the fader
        // it is freeing.
        button(CLUTCH, "clutch"),
    ]);
    out
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

/// How much of a knob's travel one full throw of a fader moves: a power of
/// two from the whole travel down to a sixteenth. The ladder is given,
/// and a quarter is where it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Precision {
    halvings: u8,
}

impl Precision {
    const FINEST: u8 = 4;
    pub const DEFAULT: Precision = Precision { halvings: 2 };

    /// The fraction of the travel a full throw covers.
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
    map: Map,
    /// The button labels resolved once. `map.button` is the file's spelling
    /// and this is what a press does; `Midi::new` is the only door and it
    /// refuses a map with a label no key answers to, so the frame loop
    /// neither looks a string up nor has an `Option` to swallow.
    action: Vec<Action>,
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
    /// One per entry of `map.button`: whether it is being held. A button is
    /// acted on when it goes down, so a surface whose buttons latch — the
    /// nanoKONTROL2 can be set either way — plays every other press. Cleared
    /// only by a release passing through [`Midi::action_for`], which is why
    /// an unplug hands the caller one for every button.
    held: Vec<bool>,
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

impl Midi {
    /// The one door: a `Midi` cannot exist over a map the instrument would
    /// refuse, so nothing downstream has to handle one.
    pub fn new(map: Map) -> Result<Midi, String> {
        map.validate()?;
        let action: Vec<Action> = map
            .button
            .iter()
            .map(|b| action_for_name(&b.command).expect("validate checked every command"))
            .collect();
        Ok(Midi {
            action,
            standing: [None; 128],
            owed: [0.0; Knob::ALL.len()],
            precision: Precision::DEFAULT,
            held: vec![false; map.button.len()],
            map,
            snd: PathBuf::from(DEV_SND),
            cards: PathBuf::from(CARDS),
            port: None,
            next_scan: Instant::now(),
            complaint: None,
        })
    }

    /// The map in force, which is what the on-screen overlay draws: the
    /// overlay must show the surface as it is actually wired, not as the
    /// factory shipped it.
    pub fn map(&self) -> &Map {
        &self.map
    }

    pub fn precision(&self) -> Precision {
        self.precision
    }

    pub fn finer(&mut self) {
        self.precision = self.precision.finer();
    }

    pub fn coarser(&mut self) {
        self.precision = self.precision.coarser();
    }

    /// Whether a hand is holding the clutch, read off the buttons themselves
    /// so that there is no second flag to fall out of step with them.
    fn clutched(&self) -> bool {
        self.action
            .iter()
            .zip(&self.held)
            .any(|(action, held)| *held && *action == Action::Clutch(Edge::Down))
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
        let buttons = self.map.button.iter().fold(0, |mask, b| mask | lamp(b.cc));
        let (lamps, wire) = crate::lamps::over_a_socket(buttons);
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
            messages.extend(self.map.button.iter().map(|b| ControlChange {
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

    /// The lamp of the first button whose press is `action`, and nothing at
    /// all when no button of the map in force does it.
    ///
    /// One question — "which button *is* this action?" — asked of the map in
    /// force rather than of the factory layout, so a `midi.toml` that moves a
    /// binding moves its lamp with it. Every property the panel needs falls
    /// out of that one lookup rather than being arranged: the first button
    /// wins, a kind the graph has none of lights nothing rather than the
    /// nearest button, and a mode the map binds nowhere lights nothing
    /// rather than the wrong thing. All three are the same `None`.
    fn lamp_of(&self, action: Action) -> Lamplight {
        self.action
            .iter()
            .zip(&self.map.button)
            .find(|(bound, _)| **bound == action)
            .map_or(0, |(_, button)| lamp(button.cc))
    }

    /// The panel [`Midi::show`] would ask for, apart from whether there is a
    /// surface to ask.
    fn wanted(&self, focus: Focus, shown: Shown) -> Lamplight {
        let when = |on: bool, action| if on { self.lamp_of(action) } else { 0 };
        let mut want = Node::ALL.into_iter().fold(0, |want, node| {
            want | self.lamp_of(Action::Focus(node, focus.at(node)))
        }) | when(shown.overlay, Action::Overlay)
            | when(shown.solo, Action::Solo)
            | when(shown.program, Action::Select);
        for axis in Axis::ALL {
            want |= when(shown.flipped[axis as usize], Action::Flip(axis));
        }
        for (button, held) in self.map.button.iter().zip(&self.held) {
            if *held {
                want |= lamp(button.cc);
            }
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
        // Every control number a button of the map answers to, which is the
        // whole of what the surface may ever be told to light.
        let buttons = self.map.button.iter().fold(0, |mask, b| mask | lamp(b.cc));
        let mut last = None;
        for path in find(&self.snd, &cards, &self.map.device) {
            match open(&path, buttons) {
                Ok(port) => {
                    log::info!("surface: {} on {}", self.map.device, port.path.display());
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
        if let Some(fader) = self.map.fader.iter().find(|f| f.cc == message.control) {
            let steps = f32::from(message.value) - f32::from(from?);
            if self.clutched() {
                return None;
            }
            let limit = fader.knob.limit(params);
            let by = steps / 127.0 * self.precision.gain() * limit.travel();
            let paid = match limit {
                Limit::Whole(_) => {
                    let owed = &mut self.owed[fader.knob as usize];
                    *owed += by;
                    let paid = owed.round();
                    *owed -= paid;
                    paid
                }
                Limit::Clamp(..) | Limit::Ratio(..) | Limit::Wrap => by,
            };
            return (paid != 0.0).then_some(Action::Turn(fader.knob, paid));
        }
        let i = self
            .map
            .button
            .iter()
            .position(|b| b.cc == message.control)?;
        let down = message.value >= PUSHED;
        let was = std::mem::replace(&mut self.held[i], down);
        match (down, was) {
            (true, false) => Some(self.action[i]),
            (false, true) => crate::command::released(self.action[i]),
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

/// Every raw MIDI endpoint of every card whose line names `device`, in the
/// order to try them. All of them, not the first: two cards can match one
/// name, and the lowest endpoint of a card can be the one that is busy or
/// output-only — so the caller opens down the list rather than committing to
/// a candidate it cannot get past.
fn find<'a>(
    snd: &'a Path,
    cards_text: &'a str,
    device: &str,
) -> impl Iterator<Item = PathBuf> + 'a {
    let wanted = device.to_lowercase();
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
    fn the_silkscreen_is_the_one_printed_on_a_nano_kontrol2() {
        // The four blocks and both ends of each, since an off-by-one in
        // either the first number or the width mislabels every control on
        // the card and nothing on the surface would say so.
        let nano = |cc| silkscreen("nanoKONTROL", cc);
        assert_eq!(nano(0), "fader 1");
        assert_eq!(nano(7), "fader 8");
        assert_eq!(nano(16), "rotary 1");
        assert_eq!(nano(23), "rotary 8");
        assert_eq!(nano(32), "S1");
        assert_eq!(nano(39), "S8");
        assert_eq!(nano(48), "M1");
        assert_eq!(nano(55), "M8");
        assert_eq!(nano(64), "R1");
        assert_eq!(nano(71), "R8");
        assert_eq!(nano(41), "play");
        assert_eq!(nano(43), "rewind");
        assert_eq!(nano(59), "track next");
        // The gaps between the blocks are numbers, not the nearest name.
        assert_eq!(nano(8), "cc 8");
        assert_eq!(nano(15), "cc 15");
        assert_eq!(nano(72), "cc 72");
        // And another surface entirely gets numbers throughout: these names
        // are one instrument's, and a card naming a control the performer
        // cannot find is worse than one naming the number they can.
        assert_eq!(silkscreen("Launchpad", 0), "cc 0");
        assert_eq!(silkscreen("Launchpad", 41), "cc 41");
    }

    #[test]
    fn the_transport_grid_is_arranged_the_way_the_device_is() {
        // The names are pinned above; the geometry is the rest of what the
        // table carries, and the overlay draws its left cluster off it —
        // so a swapped row, column or group prints a caption over a button
        // the performer's hand will not be on. Whole rather than sampled:
        // the row nobody thought to check is the one that drifts.
        assert_eq!(
            TRANSPORT
                .iter()
                .map(|t| (t.cc, t.row, t.col, t.group))
                .collect::<Vec<_>>(),
            [
                (58, 0, 0, Some("TRACK")),
                (59, 0, 1, Some("TRACK")),
                (46, 1, 0, None),
                // Cycle sits alone: the markers start two columns right of
                // it rather than beside it, and column 1 of that row is
                // bare on the surface.
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
    fn the_card_names_every_binding_in_the_map() {
        let map = Map::nano_kontrol2();
        let card = map.card();
        // A line for the device and one for each control, no more and no
        // fewer: a card that quietly leaves a fader off is a fader the
        // performer does not know they have.
        assert_eq!(card.lines().count(), 1 + map.fader.len() + map.button.len());
        for f in &map.fader {
            let line = format!("  {:<12} {}", silkscreen(&map.device, f.cc), f.knob.name());
            assert!(card.contains(&line), "missing: {line}");
        }
        for b in &map.button {
            let what = crate::command::describes(&b.command).expect("every command is described");
            let line = format!(
                "  {:<12} {what} ({})",
                silkscreen(&map.device, b.cc),
                b.command
            );
            assert!(card.contains(&line), "missing: {line}");
        }
        assert!(
            card.contains("nanoKONTROL"),
            "the card does not name the surface"
        );
        assert!(
            card.contains(
                "  marker prev  the focused switcher runs to its other source while this is held down (cut)"
            ),
            "the cut is not on marker prev"
        );
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
        // One status byte and then pairs: a surface drops the status on every
        // message after the first of a sweep. The control is not zero, so a
        // decoder that latched the pair's first byte from the run's first
        // message would show.
        let mut bytes = cc(5, 1);
        bytes.extend([5, 2, 5, 3]);
        assert_eq!(
            decode(&bytes),
            [message(5, 1), message(5, 2), message(5, 3)]
        );
    }

    #[test]
    fn a_clock_byte_in_the_middle_of_a_message_is_not_part_of_it() {
        // Real time is interleaved anywhere, including between a control
        // number and its value, and must not disturb what it lands in.
        let bytes = [0xB0, 0x07, 0xF8, 0x40, 0xFE, 0x07, 0x41];
        assert_eq!(decode(&bytes), [message(7, 0x40), message(7, 0x41)]);
    }

    #[test]
    fn a_scene_dump_is_not_a_hundred_knob_moves() {
        // Sysex holds arbitrary 7-bit data, and the nanoKONTROL2 sends one on
        // request. It has to arrive with running status in force, which is
        // the only state in which its payload could be read as knob moves —
        // a dump on its own is data under no status, which nothing decodes.
        let dump = [0xF0, 0x42, 0x40, 0x00, 0x01, 0x13, 0x00, 0x7F, 0xF7];
        let mut bytes = cc(1, 9);
        bytes.extend(dump);
        // And no status survives the dump, so the pair after it belongs to
        // nothing rather than to the fader that moved before it.
        bytes.extend([0x02, 0x05]);
        assert_eq!(decode(&bytes), [message(1, 9)]);
        // The frame itself comes out whole, `F0` and `F7` and all: it is the
        // surface's answer about itself, and the lights are read out of it.
        assert_eq!(sysex(&bytes), [dump.to_vec()]);
    }

    #[test]
    fn a_frame_that_never_ends_is_dropped_rather_than_kept() {
        // A cable pulled mid-dump, and then the surface plugged back in and
        // played. Without a cap the frame grows for as long as the
        // instrument runs; without an end on a status byte the knob moves
        // after it are swallowed by it.
        let mut bytes = vec![0xF0, 0x42];
        bytes.extend(std::iter::repeat_n(0x00, SYSEX_MAX * 2));
        bytes.extend(cc(7, 64));
        assert_eq!(decode(&bytes), [message(7, 64)]);
        assert!(sysex(&bytes).is_empty(), "an endless frame came out");

        // A frame cut short by the next status byte rather than by length.
        let mut cut = vec![0xF0, 0x42, 0x40];
        cut.extend(cc(7, 65));
        assert_eq!(decode(&cut), [message(7, 65)]);
        assert!(sysex(&cut).is_empty(), "a frame with no end came out");

        // And one exactly at the cap still arrives: a scene dump is 402
        // bytes and a cap that clipped it would be a handshake that never
        // completes.
        let mut whole = vec![0xF0];
        whole.extend(std::iter::repeat_n(0x01, SYSEX_MAX - 2));
        whole.push(0xF7);
        assert_eq!(whole.len(), SYSEX_MAX);
        assert_eq!(sysex(&whole), [whole.clone()]);
    }

    #[test]
    fn a_real_time_byte_inside_a_frame_is_not_part_of_it() {
        // Clock is interleaved anywhere at all, and a scene dump takes long
        // enough on the wire to catch several.
        let bytes = [0xF0, 0x42, 0xF8, 0x40, 0xFE, 0xF7];
        assert_eq!(sysex(&bytes), [vec![0xF0, 0x42, 0x40, 0xF7]]);
    }

    /// The focus a test means, spelled out — `Focus::default()` says nothing
    /// about which index is which, and a swapped pair would still compile.
    fn at(camera: usize, monitor: usize) -> Focus {
        Focus {
            camera,
            monitor,
            switcher: 0,
        }
    }

    #[test]
    fn the_lamp_of_each_node_is_the_button_that_selects_it() {
        // Off the map, so the factory rows and a `midi.toml` that moved them
        // both light the button a hand actually reaches for. One row per
        // kind, so a focus lights one lamp on each of the three.
        let midi = Midi::new(Map::nano_kontrol2()).unwrap();
        for index in 0..crate::rig::count(Node::Camera) {
            let focus = Focus {
                camera: index,
                monitor: index,
                switcher: index,
            };
            assert_eq!(
                midi.wanted(focus, Shown::default()),
                crate::lamps::lamp(S_ROW + index as u8)
                    | crate::lamps::lamp(M_ROW + index as u8)
                    | crate::lamps::lamp(R_ROW + index as u8),
                "node {index}"
            );
        }

        // A map that moves the select rows takes the lights with it, and one
        // that binds only some of them lights only those.
        let mut map = Map::nano_kontrol2();
        map.button
            .retain(|b| !matches!(action_for_name(&b.command), Some(Action::Focus(..))));
        map.button.push(button(90, "cam 2"));
        map.button.push(button(91, "mon 3"));
        let midi = Midi::new(map).unwrap();
        assert_eq!(
            midi.wanted(at(1, 2), Shown::default()),
            crate::lamps::lamp(90) | crate::lamps::lamp(91)
        );
        // A node no button of the map names has no lamp rather than the
        // nearest one: a graph may run deeper than the surface, and the same
        // `None` answers both.
        assert_eq!(
            midi.wanted(at(0, 2), Shown::default()),
            crate::lamps::lamp(91)
        );
        assert_eq!(
            midi.wanted(at(1, 0), Shown::default()),
            crate::lamps::lamp(90)
        );
        assert_eq!(midi.wanted(at(7, 7), Shown::default()), 0);

        // The first button that names a node wins, rather than the last.
        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "cam 1"));
        let midi = Midi::new(map).unwrap();
        assert_eq!(
            midi.wanted(at(0, 0), Shown::default()),
            crate::lamps::lamp(S_ROW) | crate::lamps::lamp(M_ROW) | crate::lamps::lamp(R_ROW)
        );
    }

    #[test]
    fn the_panel_is_the_focused_pair_and_whatever_is_under_a_finger() {
        // Taking the surface's LED mode takes every button's light, so a held
        // button has to be lit here or it goes dark for good — and the focus
        // lamps have to survive alongside it rather than instead of it.
        let (mut midi, params) = surface();
        // Camera 3 is S3, monitor 2 is M2 and the first input is R1: one
        // lamp per row, all lit.
        let focus = at(2, 1);
        let pair = crate::lamps::lamp(S_ROW + 2)
            | crate::lamps::lamp(M_ROW + 1)
            | crate::lamps::lamp(R_ROW);
        assert_eq!(midi.wanted(focus, Shown::default()), pair);
        // A finger down on a button whose light nothing else would give
        // back: all three lamps, not one.
        assert_eq!(feed(&mut midi, &params, &cc(62, 127)), [Action::Clear]);
        assert_eq!(
            midi.wanted(focus, Shown::default()),
            pair | crate::lamps::lamp(62)
        );
        // And out again when the finger comes off, which is a message that
        // does nothing else at all.
        assert_eq!(feed(&mut midi, &params, &cc(62, 0)), []);
        assert_eq!(midi.wanted(focus, Shown::default()), pair);
        // A node no row reaches lights nothing of its own, one kind at a
        // time so none can cover for another.
        let switcher = crate::lamps::lamp(R_ROW);
        assert_eq!(
            midi.wanted(at(99, 1), Shown::default()),
            crate::lamps::lamp(M_ROW + 1) | switcher
        );
        assert_eq!(
            midi.wanted(at(2, 99), Shown::default()),
            crate::lamps::lamp(S_ROW + 2) | switcher
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
        // The one thing that says a mode is on to a performer looking at a
        // fullscreen display. Off the button's *action*, so a `midi.toml`
        // that moves a mode moves its lamp with it.
        let midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let focus = at(0, 0);
        let base = midi.wanted(focus, Shown::default());
        assert_eq!(
            midi.wanted(
                focus,
                Shown {
                    overlay: true,
                    ..Shown::default()
                }
            ) & !base,
            crate::lamps::lamp(46),
            "the overlay is cycle"
        );
        assert_eq!(
            midi.wanted(
                focus,
                Shown {
                    solo: true,
                    ..Shown::default()
                }
            ) & !base,
            crate::lamps::lamp(44),
            "the display's solo is forward"
        );

        // A map that binds the mode's key nowhere lights nothing extra,
        // rather than the button that number used to be.
        let mut map = Map::nano_kontrol2();
        map.button.retain(|b| b.cc != 46);
        let midi = Midi::new(map).unwrap();
        let base = midi.wanted(focus, Shown::default());
        assert_eq!(
            midi.wanted(
                focus,
                Shown {
                    overlay: true,
                    ..Shown::default()
                }
            ),
            base
        );
    }

    #[test]
    fn the_select_lamp_says_which_input_the_focused_monitor_is_on() {
        // The panel is the only thing that says it: which of the two a
        // monitor is on is a question a hand asks before it presses, and a
        // latch that answers only by changing the picture answers too late.
        let midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let focus = at(0, 0);
        let base = midi.wanted(focus, Shown::default());
        assert_eq!(
            midi.wanted(
                focus,
                Shown {
                    program: true,
                    ..Shown::default()
                }
            ) & !base,
            crate::lamps::lamp(SELECT),
            "the select is the record row's last button on the factory map"
        );
        // And a map that binds the select nowhere lights nothing extra.
        let mut map = Map::nano_kontrol2();
        map.button.retain(|b| b.cc != SELECT);
        let midi = Midi::new(map).unwrap();
        let base = midi.wanted(focus, Shown::default());
        assert_eq!(
            midi.wanted(
                focus,
                Shown {
                    program: true,
                    ..Shown::default()
                }
            ),
            base
        );
    }

    #[test]
    fn only_a_control_change_comes_out() {
        // A surface sends notes and a pitch bend too, and none of them is a
        // knob. Their data bytes look exactly like a control change's, so the
        // status they arrived under is the only thing keeping them out.
        let mut bytes = vec![0x90, 0x40, 0x7F]; // note on
        bytes.extend([0xC0, 0x05]); // program change
        bytes.extend([0xE0, 0x00, 0x40]); // pitch bend
        bytes.extend(cc(3, 11));
        assert_eq!(decode(&bytes), [message(3, 11)]);
    }

    #[test]
    fn a_message_this_decoder_pairs_up_wrongly_does_not_eat_the_next_one() {
        // Channel pressure carries one data byte where this pairs up two, so
        // it is left half-finished — and the control change straight after it,
        // with nothing in between to absorb the odd byte, must still arrive
        // whole. Read wrongly it is CC 32, which is S1: a surface's touch
        // strip would be throwing the camera knobs onto camera 1.
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

    /// A directory of this test's own. The suite runs in one process, so the
    /// pid alone would have every test here sharing one.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lightherder-midi-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn found(dir: &Path, cards: &str, device: &str) -> Vec<PathBuf> {
        find(dir, cards, device).collect()
    }

    #[test]
    fn the_surface_is_found_by_name_among_the_cards_that_are_not_it() {
        let dir = scratch("find");
        // The card numbers in the fixture are 1, 2 and 5 — not 0, 1, 2 — so a
        // search that took a line's place in the file instead of the number
        // printed on it lands on the wrong card.
        for card in [1, 2, 5] {
            std::fs::write(dir.join(format!("midiC{card}D0")), "").unwrap();
        }
        assert_eq!(
            found(&dir, SAMPLE_CARDS, "nanoKONTROL"),
            [dir.join("midiC5D0")]
        );
        // Case, because a card's line spells it however the vendor did.
        assert_eq!(
            found(&dir, SAMPLE_CARDS, "nanokontrol2"),
            [dir.join("midiC5D0")]
        );
        assert!(found(&dir, SAMPLE_CARDS, "Launchpad").is_empty());
        // Named, but not plugged in: the card is gone from the file too.
        assert!(found(&dir, " 1 [NVidia ]: x\n", "nanoKONTROL").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_card_with_no_raw_midi_device_is_not_a_surface() {
        // Every card is in the file whether or not it has a MIDI endpoint.
        let dir = scratch("no-device");
        assert!(found(&dir, SAMPLE_CARDS, "nanoKONTROL").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_endpoint_of_every_matching_card_is_offered_in_turn() {
        // The lowest endpoint of a card can be output-only or busy, and two
        // cards can answer to one name — so discovery hands over candidates
        // rather than committing to one it cannot get past.
        let dir = scratch("candidates");
        std::fs::write(dir.join("midiC5D3"), "").unwrap();
        assert_eq!(
            found(&dir, SAMPLE_CARDS, "nanoKONTROL"),
            [dir.join("midiC5D3")],
            "an endpoint above D0 is still the surface"
        );
        // Two cards matching a loose name, the first of them without a node.
        let two = format!("{SAMPLE_CARDS} 6 [nanoKONTROL2_1 ]: USB-Audio - nanoKONTROL2\n");
        std::fs::write(dir.join("midiC6D0"), "").unwrap();
        assert_eq!(
            found(&dir, &two, "nanoKONTROL"),
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
    fn the_factory_map_is_wired_the_way_the_readme_says() {
        // Every pair, literally. Coverage alone let Hue and Brightness swap
        // CCs, and "blank" and "reset" swap buttons — a surface whose
        // silkscreen lies, and every behaviour test still green.
        let map = Map::nano_kontrol2();
        assert_eq!(
            map.fader,
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

        // The three select rows, one kind of node each and each as wide as
        // the graph. Written out rather than generated, so a row that slides
        // by one — or two kinds landing on one row — fails here rather than
        // under a hand mid-piece. This map is built for [`full`], so every
        // row runs its whole width.
        assert_eq!(
            map.button[..12],
            [
                button(32, "cam 1"),
                button(33, "cam 2"),
                button(34, "cam 3"),
                button(48, "mon 1"),
                button(49, "mon 2"),
                button(50, "mon 3"),
                button(51, "mon 4"),
                button(52, "mon 5"),
                button(64, "sw 1"),
                button(65, "sw 2"),
                button(66, "sw 3"),
                button(67, "sw 4"),
            ]
        );
        assert_eq!(
            map.button[12..],
            [
                button(62, "blank"),
                button(43, "reset 1"),
                button(42, "reset"),
                button(46, "help"),
                button(44, "solo"),
                button(58, "rate -"),
                button(59, "rate +"),
                button(60, "snap"),
                button(45, "record"),
                button(61, "cut"),
                button(68, "reverse"),
                button(69, "flip x"),
                button(70, "flip y"),
                button(71, "select"),
                button(37, "precision -"),
                button(38, "precision +"),
                button(39, "clutch"),
            ]
        );
        // The four rotaries past the fourth are dead: twelve handles on
        // sixteen controls, so nothing is owed a second page.
        for cc in 20..24 {
            assert!(!map.fader.iter().any(|f| f.cc == cc), "rotary cc {cc}");
        }
    }

    #[test]
    fn every_select_row_is_exactly_as_wide_as_its_kind() {
        // A button is owed to equipment: the rig has three cameras, five
        // monitors and four switchers, and the buttons past each are dead.
        let selects = |map: &Map| -> Vec<(u8, String)> {
            map.button
                .iter()
                .filter(|b| matches!(action_for_name(&b.command), Some(Action::Focus(..))))
                .map(|b| (b.cc, b.command.clone()))
                .collect()
        };
        assert_eq!(
            selects(&Map::nano_kontrol2()),
            [
                (32, "cam 1".to_string()),
                (33, "cam 2".to_string()),
                (34, "cam 3".to_string()),
                (48, "mon 1".to_string()),
                (49, "mon 2".to_string()),
                (50, "mon 3".to_string()),
                (51, "mon 4".to_string()),
                (52, "mon 5".to_string()),
                (64, "sw 1".to_string()),
                (65, "sw 2".to_string()),
                (66, "sw 3".to_string()),
                (67, "sw 4".to_string()),
            ]
        );
        // The transport strip is not a select row, so it is whole beside
        // them — a row rule that swallowed it would take the blank, the
        // resets and the overlay toggle with it.
        let map = Map::nano_kontrol2();
        for name in crate::command::command_names() {
            assert!(
                map.button.iter().any(|b| b.command == name),
                "{name} left the board with the select rows"
            );
        }
    }

    #[test]
    fn the_factory_map_covers_the_surface_it_names() {
        let map = Map::nano_kontrol2();
        // Naming the ones that are missing is what makes a knob added later
        // show up as a failure rather than as a knob nobody can reach.
        let missing: Vec<&str> = Knob::ALL
            .into_iter()
            .filter(|knob| !map.fader.iter().any(|f| f.knob == *knob))
            .map(Knob::name)
            .collect();
        assert_eq!(missing, [""; 0]);
        // And every control the map names is one the surface has. The rows
        // are eight-wide blocks of control numbers, so a select row grown
        // past eight walks off the end of its block into numbers no button
        // is printed beside.
        for cc in map
            .fader
            .iter()
            .map(|f| f.cc)
            .chain(map.button.iter().map(|b| b.cc))
        {
            assert!(spot(cc).is_some(), "cc {cc} is nowhere on the panel");
        }
        Midi::new(map).unwrap();
    }

    #[test]
    fn every_command_has_a_button() {
        // The board is the whole instrument, so a command with no button on
        // it is one nobody can play — there is no keyboard to carve one out
        // onto.
        let map = Map::nano_kontrol2();
        let missing: Vec<&str> = crate::command::command_names()
            .filter(|name| !map.button.iter().any(|b| b.command == *name))
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn a_map_that_would_play_the_wrong_thing_is_refused() {
        // Within one list...
        let mut map = Map::nano_kontrol2();
        map.button.push(button(S_ROW, "reset"));
        assert!(map.validate().unwrap_err().contains("bound twice"));

        // ...and across the two, which matters more: `action_for` looks at
        // the faders first, so a button sharing a fader's number is silently
        // dead rather than ambiguous.
        let mut map = Map::nano_kontrol2();
        map.button.push(button(1, "reset"));
        assert!(map.validate().unwrap_err().contains("bound twice"));

        let mut map = Map::nano_kontrol2();
        map.fader.push(fader(200, Knob::Hue));
        let why = map.validate().unwrap_err();
        assert!(why.contains("200") && why.contains("127"), "{why}");

        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "wiggle"));
        let why = map.validate().unwrap_err();
        assert!(why.contains("wiggle"), "{why}");
        // The error lists what a command may be, because a config file is
        // written by hand and there are thirty-odd of them.
        assert!(why.contains("blank") && why.contains("cam 1"), "{why}");

        let mut map = Map::nano_kontrol2();
        map.device = String::new();
        assert!(map.validate().is_err());

        // And the one door refuses all of it, so a `Midi` over a bad map is
        // unconstructable rather than quietly inert.
        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "wiggle"));
        assert!(Midi::new(map).is_err());
    }

    #[test]
    fn a_map_file_is_read_the_way_it_is_written() {
        // A literal file at the literal name the README documents — not a
        // round trip, which agrees with itself whatever serde was told these
        // fields are called and wherever `map_path` decides to look.
        let dir = scratch("map-file");
        assert_eq!(map_path().file_name().unwrap(), "midi.toml");
        std::fs::write(
            dir.join("midi.toml"),
            "device = \"nanoKONTROL\"\n\
             [[fader]]\ncc = 0\nknob = \"hue\"\n\
             [[button]]\ncc = 41\ncommand = \"mon 2\"\n",
        )
        .unwrap();
        let map = Map::load(&dir.join("midi.toml")).unwrap();
        assert_eq!(map.fader, [fader(0, Knob::Hue)]);
        assert_eq!(map.button, [button(41, "mon 2")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_flip_button_is_lit_while_the_focused_camera_is_mirrored_that_way() {
        let (midi, _) = surface();
        let focus = at(2, 1);
        let base = midi.wanted(focus, Shown::default());
        let x = crate::lamps::lamp(FLIP_X);
        let y = crate::lamps::lamp(FLIP_Y);
        assert_eq!(base & (x | y), 0);
        let lit = |flipped| {
            midi.wanted(
                focus,
                Shown {
                    flipped,
                    ..Shown::default()
                },
            ) & !base
        };
        assert_eq!(lit([true, false]), x);
        assert_eq!(lit([false, true]), y);
        assert_eq!(lit([true, true]), x | y);
    }

    #[test]
    fn no_map_file_is_the_factory_map_and_a_broken_one_is_an_error() {
        let dir = scratch("map-absent");
        let path = dir.join("midi.toml");
        assert_eq!(Map::load(&path).unwrap(), Map::nano_kontrol2());
        std::fs::write(
            dir.join("midi.toml"),
            "device = \"x\"\n[[fader]]\ncc = 0\nknob = \"wobble\"\n",
        )
        .unwrap();
        let why = Map::load(&path).unwrap_err();
        assert!(why.contains("wobble"), "{why}");
        // A file that parses but would misplay is caught by the same door.
        std::fs::write(
            dir.join("midi.toml"),
            "device = \"x\"\n[[button]]\ncc = 1\ncommand = \"nope\"\n",
        )
        .unwrap();
        assert!(Map::load(&path).unwrap_err().contains("nope"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn surface() -> (Midi, Params) {
        (
            Midi::new(Map::nano_kontrol2()).unwrap(),
            crate::config::instrument(),
        )
    }

    fn feed(midi: &mut Midi, params: &Params, bytes: &[u8]) -> Vec<Action> {
        decode(bytes)
            .into_iter()
            .filter_map(|m| midi.action_for(m, params))
            .collect()
    }

    /// What one message on fader 3 — saturation, a travel of 4 — turns.
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
        // The first message says only where the fader is: nothing has moved
        // yet, so nothing turns — whatever the knob reads, wherever the fader
        // was left. That is what makes a hot-plug throw nothing.
        assert_eq!(turned(&mut midi, &params, 127), None);
        // From there a full throw is a quarter of the travel, by default.
        let by = turned(&mut midi, &params, 0).unwrap();
        assert!((by + 1.0).abs() < 1e-6, "{by}");
        let by = turned(&mut midi, &params, 127).unwrap();
        assert!((by - 1.0).abs() < 1e-6, "{by}");
        // One step is one 127th of that, whichever way.
        let by = turned(&mut midi, &params, 126).unwrap();
        assert!((by + 1.0 / 127.0).abs() < 1e-6, "{by}");
        // A fader standing still turns nothing.
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
        // The fader is brought back from its rail under the clutch, and the
        // rotaries are as still as the faders.
        assert_eq!(turned(&mut midi, &params, 64), None);
        assert_eq!(turned(&mut midi, &params, 0), None);
        assert_eq!(feed(&mut midi, &params, &cc(16, 50)), []);
        assert_eq!(feed(&mut midi, &params, &cc(16, 90)), []);
        assert_eq!(
            feed(&mut midi, &params, &cc(CLUTCH, 0)),
            [Action::Clutch(Edge::Up)]
        );
        // Let go, the fader turns on from where the clutch left it, by how
        // far it moved since — not by the whole way it was carried. The zoom
        // is a ratio, so the travel a code is a 127th of is the rails' log.
        let by = turned(&mut midi, &params, 10).unwrap();
        assert!((by - 10.0 / 127.0).abs() < 1e-6, "{by}");
        assert!(matches!(
            feed(&mut midi, &params, &cc(16, 91))[..],
            [Action::Turn(Knob::Zoom, by)] if (by - 16f32.ln() / 4.0 / 127.0).abs() < 1e-6
        ));
        // And the clutch is lit while a hand is on it.
        let lit = |midi: &Midi| midi.wanted(at(0, 0), Shown::default()) & lamp(CLUTCH) != 0;
        assert!(!lit(&midi));
        feed(&mut midi, &params, &cc(CLUTCH, 127));
        assert!(lit(&midi));
        feed(&mut midi, &params, &cc(CLUTCH, 0));
        assert!(!lit(&midi));
    }

    #[test]
    fn a_whole_frame_knob_is_turned_a_frame_at_a_time() {
        // The delay counts frames, and at a quarter a full throw over a
        // reach of four is one frame: the knob ticks over at the half, like
        // a detent, and the fader is never more than half a frame in credit.
        let mut params = crate::config::instrument();
        params.delay = 4;
        let mut midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let delay = |midi: &mut Midi, value: u8| match feed(midi, &params, &cc(18, value))[..] {
            [] => None,
            [Action::Turn(Knob::Delay, by)] => Some(by),
            ref other => panic!("{other:?}"),
        };
        assert_eq!(delay(&mut midi, 0), None);
        assert_eq!(delay(&mut midi, 63), None);
        assert_eq!(delay(&mut midi, 64), Some(1.0));
        assert_eq!(delay(&mut midi, 127), None);
        assert_eq!(delay(&mut midi, 0), Some(-1.0));
        midi.coarser();
        midi.coarser();
        assert_eq!(delay(&mut midi, 127), Some(4.0));
    }

    #[test]
    fn an_unplug_throws_nothing() {
        let (mut midi, params) = surface();
        assert_eq!(turned(&mut midi, &params, 40), None);
        // Unplugged, nothing can vouch for where the next surface's faders
        // stand, so its first word is where and not how far.
        midi.drop_port();
        assert_eq!(turned(&mut midi, &params, 0), None);
        assert!((turned(&mut midi, &params, 127).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn an_unknown_command_is_refused_with_the_list_of_the_real_ones() {
        // A `midi.toml` is written by hand, so a typo has to say what the
        // vocabulary is rather than only that this was not in it.
        let refuse = |name: &str| {
            Midi::new(Map {
                device: "x".into(),
                fader: Vec::new(),
                button: vec![button(90, name)],
            })
            .err()
            .expect("a map the instrument would refuse")
        };
        for name in ["shift num9", "cam 9", "zzz"] {
            let why = refuse(name);
            assert!(why.contains("no command called"), "{name}: {why}");
            assert!(
                why.contains("cam 1"),
                "{name} was not given the list: {why}"
            );
            assert!(
                why.contains("blank"),
                "{name} was not given the list: {why}"
            );
        }
    }

    #[test]
    fn unplugging_the_surface_lets_go_of_every_button_a_finger_was_on() {
        // A held mode must end with the surface holding it, and end the way
        // a release would have — after whatever the surface had already
        // sent — rather than leaving the instrument cut or recording with
        // nothing to let go of it, and the next press swallowed.
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
            [
                Action::Record(crate::command::Edge::Down),
                Action::Cut(crate::command::Edge::Down)
            ]
        );
        surface.press(60);
        drop(surface);
        assert_eq!(
            play(&mut midi),
            [
                Action::Screencap,
                Action::Record(crate::command::Edge::Up),
                Action::Cut(crate::command::Edge::Up)
            ]
        );
        assert!(midi.held.iter().all(|held| !held));
    }

    #[test]
    fn a_button_acts_when_it_goes_down_and_not_when_it_comes_up() {
        let (mut midi, params) = surface();
        // The select is a latch, so a surface that repeats while a finger
        // rests on it would flip a monitor back and forth under a hand that
        // pressed once.
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), [Action::Select]);
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(SELECT, 127)), [Action::Select]);
    }

    #[test]
    fn the_capture_buttons_are_a_press_and_a_hold() {
        use crate::command::Edge;
        let (mut midi, params) = surface();
        // Marker set is a press like every other button: a still, and
        // nothing at all on the way up.
        assert_eq!(feed(&mut midi, &params, &cc(60, 127)), [Action::Screencap]);
        assert_eq!(feed(&mut midi, &params, &cc(60, 0)), []);
        // Record is the one button both edges of which reach the
        // instrument, so a recording lasts exactly as long as the finger.
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
        // Both ends of every row: the rows are three eight-wide blocks of
        // control numbers, and a block written from the wrong first number
        // lands whole on the wrong row.
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
                    node.name()
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
        // The transport strip: rewind puts one knob back, stop the whole
        // panel, cycle lifts the overlay, the track pair moves the tempo.
        assert_eq!(
            feed(&mut midi, &params, &cc(43, 127)),
            [Action::ResetLastKnob]
        );
        assert_eq!(feed(&mut midi, &params, &cc(42, 127)), [Action::Reset]);
        assert_eq!(feed(&mut midi, &params, &cc(46, 127)), [Action::Overlay]);
        assert_eq!(
            feed(&mut midi, &params, &cc(58, 127)),
            [Action::Tempo(crate::tempo::Step::Slower)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(59, 127)),
            [Action::Tempo(crate::tempo::Step::Faster)]
        );
    }

    #[test]
    fn a_control_nothing_is_bound_to_does_nothing() {
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(100, 127)), []);
    }

    #[test]
    fn a_surface_on_another_channel_is_still_the_surface() {
        // Not just decoded — *played*. A channel filter anywhere would leave
        // a nanoKONTROL2 set to channel 10 dead, and the decoder test alone
        // cannot see one added in `action_for`.
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &[0xB9, 62, 127]), [Action::Clear]);
        assert_eq!(decode(&[0xB9, 0x07, 0x64]), [message(7, 0x64)]);
    }

    #[test]
    fn the_whole_path_runs_off_a_device_that_appears_and_goes_away_and_comes_back() {
        // Discovery, the open, the thread, the decode and the map, driven by
        // bytes down a pipe that is not there when the instrument starts —
        // which is what hot-plug is.
        let dir = scratch("hotplug");
        let cards = dir.join("cards");
        std::fs::write(&cards, SAMPLE_CARDS).unwrap();
        let mut midi = Midi::new(Map::nano_kontrol2())
            .unwrap()
            .looking_in(dir.clone(), cards);
        let params = Params::default();

        // Nothing plugged in: no device node, and no waiting for one.
        assert!(drain(&mut midi, &params).is_empty());

        let node = dir.join("midiC5D0");
        let mut pipe = plug(&node);
        // A sweep, in running status, split across two writes mid-message —
        // a `read` lands wherever it lands, and a three-byte message
        // routinely arrives as two. The first message only places the
        // fader; the two after it turn saturation down by a quarter of its
        // travel and back up by half of that.
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
        // A button held down at the moment the cable comes out.
        pipe.write_all(&[0xB0, 62, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(wait_for(&mut midi, &params), [Action::Clear]);

        // Unplug: the last writer goes, so the reader's `read` returns 0.
        drop(pipe);
        std::fs::remove_file(&node).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while midi.port.is_some() && Instant::now() < deadline {
            drain(&mut midi, &params);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(midi.port.is_none(), "the surface never went away");

        // Plug it back in. It has to be looked for again — and the button
        // that was down when the cable went must count as a fresh press.
        let mut pipe = plug(&node);
        pipe.write_all(&[0xB0, 62, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(
            wait_for(&mut midi, &params),
            [Action::Clear],
            "the surface did not come back"
        );
        // And a fader's first word is where it is, not how far it came:
        // where this one stood before the unplug is nothing to it now.
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
        // The app test that this lamp survives a redraw needs a GPU, because
        // an `App` holds one — and it skips where there is no adapter. This
        // is the half of that claim which needs nothing but a descriptor, so
        // a machine with no display still fails a lamp that never leaves the
        // instrument.
        let mut midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let mut surface = midi.plug_in_a_test_surface();
        surface.wire.handshake(0);
        // One row per kind: the first of each is S1, M1 and R1, which are
        // controls 32, 48 and 64.
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

    /// A fifo standing in for the device node, opened read *and* write so
    /// neither end blocks waiting for the other — a reader's open returns at
    /// once because this handle is already a writer, and dropping it is the
    /// unplug. A write-only writer thread would deadlock the suite rather
    /// than fail it if it ever failed to arrive.
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

    /// Poll until the surface has said something, or give up. The device is
    /// read on a thread, so how many frames it takes is not ours to say.
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
