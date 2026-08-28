//! The control surface: a MIDI device read off ALSA on a thread of its own,
//! its messages turned into the very same [`Action`]s the keyboard makes.
//!
//! Two kinds of control, because a surface has two kinds of thing on it. A
//! fader or a rotary sends where it *is*, so it names a [`Knob`] and sets it
//! absolutely. A button sends that it was pushed, so it names a **key** — by
//! the label [`crate::keys::help`] prints — and does whatever that key does.
//! Naming a key rather than an action of its own is what keeps the surface
//! from growing a second vocabulary alongside the keyboard's: everything a
//! button can do is on the help the instrument already prints, and a binding
//! added to the keys is reachable from the panel the same day.
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

use serde::{Deserialize, Serialize};
use web_time::Instant;

use crate::keys::{action_for_label, labels, Action};
use crate::lamps::{lamp, Lamplight, Lamps};
use crate::params::{Focus, Knob, Limit, Params, Seed};

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
/// Loaded from `midi.toml` beside the preset slots when there is one, and
/// [`Map::nano_kontrol2`] when there is not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Buttons, by the key each one presses.
    #[serde(default)]
    pub button: Vec<Button>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fader {
    pub(crate) cc: u8,
    pub(crate) knob: Knob,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
    pub(crate) cc: u8,
    /// A key, spelled the way the printed help spells it — `"r"`, `"space"`,
    /// `"shift f1"`.
    pub(crate) key: String,
}

const fn fader(cc: u8, knob: Knob) -> Fader {
    Fader { cc, knob }
}

impl Map {
    /// The factory CC layout of a Korg nanoKONTROL2, which is what this
    /// instrument is played from.
    ///
    /// Faders 2 to 8 are the focused **monitor**: its front panel, and then
    /// how much of the focused camera it is showing. The eight rotaries above
    /// them are the focused **camera**: where it is pointed, how much light it
    /// hands back, and what its signal path does on the way. So the left hand
    /// works one monitor, the right hand one camera, and the last fader is the
    /// switcher crosspoint that joins the two the hands are on.
    ///
    /// Fader 1 is bound to nothing. There is nothing left worth throwing at
    /// it: an unbound fader is a control a `midi.toml` can claim, and one
    /// nobody's hand throws by accident.
    ///
    /// Nine knobs are deliberately not here — the surface has fifteen
    /// bound controls and they are taken. The three per-channel gain offsets
    /// and the bloom radius are trims of knobs that are on the surface; the
    /// keyer's four wait for a hand that keys more than it bleeds and swaps
    /// this map for its own; and the switcher's send is on a graph only when
    /// that graph has an input, which is not what a fixed fader is for. They
    /// all stay on the keys.
    pub(crate) fn nano_kontrol2() -> Map {
        Map {
            device: "nanoKONTROL".into(),
            fader: vec![
                fader(1, Knob::Hue),
                fader(2, Knob::Saturation),
                fader(3, Knob::Brightness),
                fader(4, Knob::Contrast),
                fader(5, Knob::Gamma),
                fader(6, Knob::Headroom),
                fader(7, Knob::Route),
                fader(16, Knob::Zoom),
                fader(17, Knob::Rotation),
                fader(18, Knob::TranslateX),
                fader(19, Knob::TranslateY),
                fader(20, Knob::Gain),
                fader(21, Knob::Bloom),
                fader(22, Knob::ChromaBleed),
                fader(23, Knob::Noise),
            ],
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
            // The key is named as well as what it does: a button *is* that
            // key, so a performer who has learnt one has learnt the other.
            let what = crate::keys::describes(&b.key).unwrap_or_default();
            out.push_str(&format!("  {control:<12} {what} ({})\n", b.key));
        }
        out
    }

    /// Where the map is kept: beside the preset slots, because both are the
    /// performer's own configuration of one instrument.
    fn path(dir: &Path) -> PathBuf {
        dir.join("midi.toml")
    }

    /// The performer's map if there is one, the factory layout if there is
    /// not. A file that is there and will not parse is an error rather than
    /// a silent fall back to the default: a surface that quietly plays the
    /// wrong knobs is worse than one that will not start.
    pub fn load(dir: &Path) -> Result<Map, String> {
        let path = Map::path(dir);
        let text = match std::fs::read_to_string(&path) {
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
        for cc in self
            .fader
            .iter()
            .map(|f| f.cc)
            .chain(self.button.iter().map(|b| b.cc))
        {
            if cc > 127 {
                return Err(format!("cc {cc} is not a control number; they stop at 127"));
            }
            if seen.contains(&cc) {
                return Err(format!("cc {cc} is bound twice"));
            }
            seen.push(cc);
        }
        for b in &self.button {
            // A key nothing answers to, first: an unknown label makes *both*
            // readings of it `None`, so the no-op-shift check below would
            // otherwise fire on "shift num9" and report a shift that does
            // nothing rather than a key that does not exist — naming the
            // wrong fault, and printing no list of the ones that do.
            if action_for_label(&b.key).is_none() {
                let known: Vec<&str> = labels().collect();
                return Err(format!(
                    "cc {}: no key called {:?}; there are {}, each also with a \"shift \" in front",
                    b.cc,
                    b.key,
                    known.join(", ")
                ));
            }
            // A "shift " that changes nothing. Only the slot keys and the
            // node keys read it, so a binding that writes one anywhere else
            // is asking for something the instrument will not do — and a
            // performer finding that out mid-set is worse than a line at
            // startup. Asked of the tables rather than named here, so a key
            // that starts reading shift needs no second edit.
            if let Some(bare) = b.key.strip_prefix("shift ") {
                if action_for_label(bare) == action_for_label(&b.key) {
                    return Err(format!(
                        "cc {}: {:?} is {bare:?} — that key does not read shift",
                        b.cc, b.key
                    ));
                }
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
/// block is named by where it starts. [`nano_buttons`] writes its three
/// button rows off the three below, so the map and the panel cannot
/// disagree about which row a number is on.
const FADERS: u8 = 0;
const ROTARIES: u8 = 16;
const S_ROW: u8 = 32;
const M_ROW: u8 = 48;
const R_ROW: u8 = 64;

pub(crate) fn spot(cc: u8) -> Option<Spot> {
    let block = |first: u8| (cc >= first && cc < first + 8).then(|| cc - first);
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

fn button(cc: u8, key: impl Into<String>) -> Button {
    Button {
        cc,
        key: key.into(),
    }
}

/// How many nodes each half of the Solo row selects. A fact about the
/// **surface**, not about any graph: the row is eight wide and there are two
/// hands, so each half is four. The graph is why the split is affordable —
/// nothing anyone plays has more than four cameras or four monitors, while
/// eight buttons spent on the camera half left the *monitor* half, the node
/// under all eight faders, with no outright address at all — but a deeper
/// graph is reached the way a graph deeper than the surface always was, by
/// the `n` and `m` steps.
const NODES_PER_HALF: usize = 4;

/// The row is written out of the key table, so a half wider than the table
/// would index past its end while building the factory map — at startup, on
/// every run, rather than here.
const _: () = assert!(2 * NODES_PER_HALF <= crate::keys::KEYED_NODES);

/// The nanoKONTROL2's buttons, in the three rows and the transport strip it
/// has them in.
fn nano_buttons() -> Vec<Button> {
    let mut out = Vec::new();
    // The Solo row points the knobs at a node: the left four at the camera
    // the rotaries turn, the right four at the monitor the faders turn. Solo
    // selects because that is what a hand off a mixer reaches for it to do,
    // and the row splits down the middle because the surface's two hands do.
    //
    // Off the key tables rather than beside them, so a label is written once
    // and a rebound key takes its button with it.
    for node in 0..NODES_PER_HALF {
        let (_, key) = crate::keys::NODE_KEYS[node];
        out.push(button(S_ROW + node as u8, key));
        out.push(button(
            S_ROW + (NODES_PER_HALF + node) as u8,
            format!("shift {key}"),
        ));
    }
    // Mute plays slot n back, Record writes over it — the two lower rows in
    // the order of how much they commit. Nothing here is guarded: a single
    // press on either is irreversible, and the only thing standing between
    // them is which row a hand lands on.
    for (slot, (_, key)) in crate::keys::SLOT_KEYS.iter().enumerate() {
        out.push(button(M_ROW + slot as u8, *key));
        out.push(button(R_ROW + slot as u8, format!("shift {key}")));
    }
    out.extend([
        // The markers: the two that step the focus, and the one that blanks
        // the glass. The steps stay even though the Solo row selects,
        // because a graph may run deeper than the half-row each side has.
        button(60, "n"),
        button(61, "m"),
        button(62, "space"),
        // The tape row's left half is the reset ladder, in the order of how
        // much it takes back: rewind puts the last knob turned back, stop
        // puts the whole panel back.
        button(43, "backspace"),
        button(42, "r"),
        // Cycle shows and hides the overlay that explains all of the above —
        // the one button whose job survives not knowing what any button does.
        button(46, "`"),
        // Solo: the focused monitor on the whole display. On forward, which
        // record's reason for staying unbound never covered — this is the
        // press on the surface that takes back the least, changing what is
        // *looked* at and nothing the graph is doing, and the same button
        // puts the bank back.
        button(44, "enter"),
        // The seed, on play: what a monitor's loop starts from, on the
        // button whose silkscreen says start. It lights while the focused
        // monitor has a blob on the glass, so the panel says which of the
        // two rigs that monitor is — a toggle with no indicator on a
        // fullscreen display being the footgun fine mode makes the case
        // about.
        button(41, ";"),
        // Fine mode, up on the track pair with nothing else near it: a latch
        // left on by a slip is worse than one that takes a reach to find,
        // and this is the only button on the surface a hand has to *mean*.
        button(58, "tab"),
    ]);
    // Record and track-next are left unbound on purpose. Record above all:
    // it is the one button a blind slip should not find, and the surface has
    // no want of another irreversible press.
    out
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

/// Where a fader is, 0 at the bottom and 1 at the top, as the value its knob
/// would take. The travel is the knob's own, read off [`Limit::ends`] — a
/// phase's is one full turn, and this is not the place that decides how long
/// a turn is.
fn value_at(knob: Knob, position: f32) -> f32 {
    let (low, high) = knob.limit().ends();
    low + position * (high - low)
}

/// The inverse: where a fader would have to stand for the knob to read
/// `value`. Always inside the travel, because a knob is only ever set by
/// `Params::nudge` or loaded through `config::validate` and both hold it
/// inside the very same [`Limit`] — the two ends of a fader are the two ends
/// of the knob, with nothing past either.
fn position_of(knob: Knob, value: f32) -> f32 {
    let (low, high) = knob.limit().ends();
    (value - low) / (high - low)
}

/// One fader's grip on its knob.
///
/// A fader sends where it is, so the first one touched after the surface is
/// plugged in would otherwise throw its knob to wherever the fader happens
/// to be standing — a whole panel's worth of that on a hot-plug, mid-piece,
/// with the headroom fader slamming a monitor to white. So a fader does not
/// take its knob over until it has passed through where the knob already is,
/// and then keeps it until [`Midi::release`] lets go, which names the times
/// that happens.
#[derive(Clone, Copy, Debug, Default)]
struct Pickup {
    caught: bool,
    was: Option<f32>,
}

/// One step of a 7-bit control. A fader that cannot land exactly on its
/// knob's value has to catch it anyway, or a knob between two steps is one no
/// fader can ever reach.
const STEP: f32 = 1.0 / 127.0;

/// How much of a knob's travel a whole sweep of its control covers in fine
/// mode. A sixteenth, so the 128 steps a 7-bit control has land 2032 of them
/// across the travel instead of 127 — and a sweep still moves the knob far
/// enough to be a trim rather than a nudge.
///
/// One number for every knob rather than the keys own per-knob step. A step
/// chosen to be *visible* under a repeating key is not the same quantity as
/// "finer than the fader", and on four knobs it is not even the same sign:
/// the bloom radius, the chroma bleed, the grain and the key softness all run
/// 0 to 0.25, where a fader step is 0.00197 and the key step is 0.002 — so
/// gearing off the keys would have made fine mode a *coarsen* mode on them.
const FINE_GEARING: f32 = 1.0 / 16.0;

impl Pickup {
    /// Whether this move of the fader reaches its knob, which is at `value`.
    fn catches(&mut self, fader: f32, knob: Knob, value: f32) -> bool {
        let was = self.was.replace(fader).unwrap_or(fader);
        let reaches = |target: f32| {
            let (from, to) = (was - target, fader - target);
            from.min(to) <= STEP && from.max(to) >= -STEP
        };
        let at = position_of(knob, value);
        self.caught |= match knob.limit() {
            // A phase's two fader ends are the same angle, so a knob sitting
            // on the seam is reachable from either — and `wrap_pi` puts it
            // there exactly, at +PI, which is position 1.0 while the fader
            // that produced it is standing at 0.0.
            Limit::Wrap => reaches(at - 1.0) || reaches(at) || reaches(at + 1.0),
            Limit::Clamp(..) => reaches(at),
        };
        self.caught
    }

    /// How far the control moved since the message before it, as a fraction
    /// of its travel — the whole of what fine mode reads off a fader.
    ///
    /// `None` for the first message after a release, which is the one that
    /// says where the fader is standing: a move is two positions and there
    /// is only one.
    ///
    /// Nothing can be thrown by a read that only says how far something
    /// moved, so this waits for nothing — but the grip goes, because it was
    /// a claim about where the fader *stands* and the fader has just walked
    /// away from the knob while turning it a sixteenth as far. Dropped here,
    /// on the one control that moved, rather than across the panel when the
    /// mode is turned on or off: a fader nobody touched is holding a knob
    /// that has not moved either.
    fn moved(&mut self, fader: f32) -> Option<f32> {
        let was = self.was.replace(fader)?;
        self.caught = false;
        Some(fader - was)
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
    /// One per entry of `map.fader`, in the same order.
    pickup: Vec<Pickup>,
    /// Whether the controls are read as how far they moved rather than as
    /// where they are — see [`Midi::toggle_fine`]. Here rather than in the
    /// caller because it is one fact about how this surface is read, and a
    /// copy kept beside the one that decides is a copy to keep in step.
    fine: bool,
    /// One per entry of `map.button`: whether it is being held. A button is
    /// acted on when it goes down, so a surface whose buttons latch — the
    /// nanoKONTROL2 can be set either way — plays every other press.
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
    /// The surface's keys. Held rather than dropped even by a test that says
    /// nothing on them, because a dropped sender is exactly what
    /// [`Midi::poll`] reads as the cable coming out.
    keys: std::sync::mpsc::Sender<ControlChange>,
}

#[cfg(test)]
impl TestSurface {
    /// A button pushed, the way the reader thread reports one.
    pub(crate) fn press(&self, control: u8) {
        self.keys.send(change(control, 127)).unwrap();
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
            .map(|b| action_for_label(&b.key).expect("validate checked every label"))
            .collect();
        Ok(Midi {
            action,
            pickup: vec![Pickup::default(); map.fader.len()],
            fine: false,
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
        let (keys, rx) = std::sync::mpsc::channel();
        self.port = Some(Port {
            path: PathBuf::from("a test's surface"),
            rx,
            lamps: Some(lamps),
        });
        TestSurface { wire, keys }
    }

    /// Every message the surface has sent since the last call, and the
    /// connecting and disconnecting around them. Called once a frame; never
    /// blocks, and never waits on a device that is not plugged in.
    ///
    /// Messages rather than actions, so the caller can turn each one into an
    /// action against the panel the one before it left — a fader and a slot
    /// button inside one frame is a real two-handed gesture, and a whole
    /// batch decided against one snapshot has the fader dragging a knob out
    /// of the preset the button just recalled.
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
        // returned; what the unplug takes is the state, not the backlog.
        if gone {
            self.drop_port();
        }
        messages
    }

    /// Light the panel for `focus`: the select button of the camera the
    /// rotaries turn and of the monitor the faders turn, every button a
    /// finger is on, and the button holding each latched mode that is on.
    ///
    /// The held ones are not decoration. Taking the surface's LED mode takes
    /// every button's light at once — see [`crate::lamps`] — so a button that
    /// lit itself under a finger has to be lit here or it goes dark for good,
    /// and the Record row that overwrites a preset slot is on that list.
    ///
    /// Said again every redraw rather than at each of the several places the
    /// focus moves: this is the one call that cannot miss one, and it also
    /// catches a surface plugged in halfway through a piece, where no focus
    /// change follows to light it. Saying the same panel again costs nothing
    /// on the wire.
    ///
    /// `seed`, `overlay` and `solo` are what the caller owns — one fact
    /// about the focused monitor and two latched modes. Fine mode is this
    /// type's own, and asking for it back to hand it straight in would be
    /// the answer taking a trip through the question.
    pub fn show(&self, focus: Focus, seed: Seed, overlay: bool, solo: bool) {
        if let Some(lamps) = self.port.as_ref().and_then(|port| port.lamps.as_ref()) {
            lamps.show(self.wanted(focus, seed, overlay, solo));
        }
    }

    /// The lamp of the first button whose press is `action`, and nothing at
    /// all when no button of the map in force does it.
    ///
    /// One question — "which button *is* this action?" — asked of the map in
    /// force rather than of the factory layout, so a `midi.toml` that moves a
    /// binding moves its lamp with it. Every property the panel needs falls
    /// out of that one lookup rather than being arranged: the first button
    /// wins, a node past the end of the select row lights nothing rather than
    /// the nearest button, and a mode the map binds nowhere lights nothing
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
    fn wanted(&self, focus: Focus, seed: Seed, overlay: bool, solo: bool) -> Lamplight {
        let when = |on: bool, action| if on { self.lamp_of(action) } else { 0 };
        let mut want = self.lamp_of(Action::FocusCamera(focus.camera))
            | self.lamp_of(Action::FocusMonitor(focus.monitor))
            | when(self.fine, Action::Fine)
            | when(seed.lit(), Action::Seed)
            | when(overlay, Action::Overlay)
            | when(solo, Action::Solo);
        for (button, held) in self.map.button.iter().zip(&self.held) {
            if *held {
                want |= lamp(button.cc);
            }
        }
        want
    }

    /// Let go of every fader's grip on its knob. Three times the knobs move
    /// without the faders moving with them — a recall, a reset, and the
    /// focus landing on a node whose knobs are somewhere else — and one
    /// where the faders move without the knobs: an unplug, after which
    /// nothing can vouch for where a fader is standing. Without this the
    /// next fader brushed throws its knob to wherever the fader is standing,
    /// which is the recall undone one knob at a time.
    pub fn release(&mut self) {
        for pickup in &mut self.pickup {
            *pickup = Pickup::default();
        }
    }

    /// Turn fine mode on or off, and say which it now is.
    ///
    /// Nothing is let go here. A fader only goes stale by being *moved*
    /// under a relative read, and that is exactly where [`Pickup::moved`]
    /// drops its grip — so a fader nobody touched in fine mode keeps the one
    /// it had, and a one-knob trim does not charge the whole panel a pickup
    /// sweep. Which is the same answer [`Midi::release_knob`] gives.
    pub fn toggle_fine(&mut self) -> bool {
        self.fine = !self.fine;
        self.fine
    }

    /// Let go of just the controls that hold `knob` — what a reset of one
    /// knob owes, where [`Midi::release`] would charge the whole panel a
    /// pickup sweep for it. Everything the full release says applies here to
    /// this one knob: it has moved without its fader moving with it, so a
    /// fader left caught would throw it back on the next touch.
    ///
    /// By the *field*, not by the knob, and off the same
    /// [`Knob::shares_a_field_with`] the reset itself reads: the rigid gain
    /// and its three channels write the same three floats, so a reset of one
    /// leaves a fader on the other holding a knob that has just moved — and
    /// that fader is a rigid one, which throws all three channels at once.
    pub fn release_knob(&mut self, knob: Knob) {
        for (pickup, fader) in self.pickup.iter_mut().zip(&self.map.fader) {
            if knob.shares_a_field_with(fader.knob) {
                *pickup = Pickup::default();
            }
        }
    }

    /// Let go of the surface. Note what is *not* here: the buttons are not
    /// released, because `poll` hands back the messages that were already in
    /// hand and the caller is about to press them — a release here would be
    /// undone by the same batch, leaving a button held by nothing, its lamp
    /// lit and its next press swallowed. [`Midi::connect`] does it instead,
    /// where nothing can arrive between the release and the first message.
    fn drop_port(&mut self) {
        self.port = None;
        self.release();
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
                    // Whatever was under a finger when the last cable came
                    // out is not under one now — see [`Midi::drop_port`] for
                    // why the release waits until here.
                    self.held.iter_mut().for_each(|held| *held = false);
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
    pub fn action_for(
        &mut self,
        message: ControlChange,
        params: &Params,
        focus: Focus,
    ) -> Option<Action> {
        if let Some(i) = self.map.fader.iter().position(|f| f.cc == message.control) {
            let knob = self.map.fader[i].knob;
            let fader = f32::from(message.value) / 127.0;
            // Fine mode reads the control as how far it *moved* rather than
            // where it is, geared down by [`FINE_GEARING`]. A `Nudge` and not a
            // `Set`, which is what keeps the promise the coarse path makes:
            // the rails, the wrap and the rigid three-channel gain are the
            // key press's, so there is still nowhere fine mode can put a
            // knob that a hand could not.
            if self.fine {
                let moved = self.pickup[i].moved(fader)?;
                let (low, high) = knob.limit().ends();
                return Some(Action::Nudge(knob, moved * (high - low) * FINE_GEARING));
            }
            return self.pickup[i]
                .catches(fader, knob, params.knob(knob, focus))
                .then(|| Action::Set(knob, value_at(knob, fader)));
        }
        let i = self
            .map
            .button
            .iter()
            .position(|b| b.cc == message.control)?;
        let down = message.value >= PUSHED;
        let pressed = down && !self.held[i];
        self.held[i] = down;
        pressed.then_some(self.action[i])
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
            let what = crate::keys::describes(&b.key).expect("every key is described");
            let line = format!("  {:<12} {what} ({})", silkscreen(&map.device, b.cc), b.key);
            assert!(card.contains(&line), "missing: {line}");
        }
        assert!(
            card.contains("nanoKONTROL"),
            "the card does not name the surface"
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
            input: 0,
        }
    }

    #[test]
    fn the_lamp_of_each_node_is_the_button_that_selects_it() {
        // Off the map, so the factory Solo row and a `midi.toml` that moved
        // it both light the button a hand actually reaches for. The row
        // splits down the middle: the left four are the cameras the rotaries
        // turn, the right four the monitors the faders turn.
        let midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let lit = |camera, monitor| midi.wanted(at(camera, monitor), Seed::Dark, false, false);
        for node in 0..4 {
            assert_eq!(
                lit(node, node),
                crate::lamps::lamp(32 + node as u8) | crate::lamps::lamp(36 + node as u8),
                "node {node}"
            );
        }

        // A map that moves the select row takes the lights with it, and one
        // that binds only some of the row lights only those.
        let mut map = Map::nano_kontrol2();
        map.button.retain(|b| !(32..40).contains(&b.cc));
        map.button.push(button(90, "num2"));
        map.button.push(button(91, "shift num3"));
        let midi = Midi::new(map).unwrap();
        assert_eq!(
            midi.wanted(at(1, 2), Seed::Dark, false, false),
            crate::lamps::lamp(90) | crate::lamps::lamp(91)
        );
        // A node no button of the map names has no lamp rather than the
        // nearest one: a graph may run deeper than the surface, and the same
        // `None` answers both.
        assert_eq!(
            midi.wanted(at(0, 2), Seed::Dark, false, false),
            crate::lamps::lamp(91)
        );
        assert_eq!(
            midi.wanted(at(1, 0), Seed::Dark, false, false),
            crate::lamps::lamp(90)
        );
        assert_eq!(midi.wanted(at(7, 7), Seed::Dark, false, false), 0);

        // The first button that names a node wins, rather than the last.
        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "num1"));
        let midi = Midi::new(map).unwrap();
        assert_eq!(
            midi.wanted(at(0, 0), Seed::Dark, false, false),
            crate::lamps::lamp(32) | crate::lamps::lamp(36)
        );
    }

    #[test]
    fn the_panel_is_the_focused_pair_and_whatever_is_under_a_finger() {
        // Taking the surface's LED mode takes every button's light, so a held
        // button has to be lit here or it goes dark for good — and the focus
        // lamps have to survive alongside it rather than instead of it.
        let (mut midi, params) = surface();
        // Camera 3 is S3 and monitor 2 is S6: one lamp per hand, both lit.
        let focus = at(2, 1);
        let pair = crate::lamps::lamp(34) | crate::lamps::lamp(37);
        assert_eq!(midi.wanted(focus, Seed::Dark, false, false), pair);
        // M3 goes down: recall slot 3, whose light nothing else would give
        // back. All three lamps, not one.
        assert_eq!(feed(&mut midi, &params, &cc(50, 127)), [Action::Recall(2)]);
        assert_eq!(
            midi.wanted(focus, Seed::Dark, false, false),
            pair | crate::lamps::lamp(50)
        );
        // And out again when the finger comes off, which is a message that
        // does nothing else at all.
        assert_eq!(feed(&mut midi, &params, &cc(50, 0)), []);
        assert_eq!(midi.wanted(focus, Seed::Dark, false, false), pair);
        // A node the select row does not reach lights nothing of its own,
        // one side at a time so neither can cover for the other.
        assert_eq!(
            midi.wanted(at(99, 1), Seed::Dark, false, false),
            crate::lamps::lamp(37)
        );
        assert_eq!(
            midi.wanted(at(2, 99), Seed::Dark, false, false),
            crate::lamps::lamp(34)
        );
        assert_eq!(midi.wanted(at(99, 99), Seed::Dark, false, false), 0);
    }

    #[test]
    fn a_latched_mode_lights_the_button_that_holds_it() {
        // The one thing that says a mode is on to a performer looking at a
        // fullscreen display. Off the button's *action*, so a `midi.toml`
        // that moves fine mode moves its lamp with it.
        let mut midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let focus = at(0, 0);
        let base = midi.wanted(focus, Seed::Dark, false, false);
        assert_eq!(
            midi.wanted(focus, Seed::Dark, true, false) & !base,
            crate::lamps::lamp(46),
            "the overlay is cycle"
        );
        assert_eq!(
            midi.wanted(focus, Seed::Dark, false, true) & !base,
            crate::lamps::lamp(44),
            "the display's solo is forward"
        );
        assert!(midi.toggle_fine());
        assert_eq!(
            midi.wanted(focus, Seed::Dark, false, false) & !base,
            crate::lamps::lamp(58),
            "fine mode is track-prev on the factory map"
        );
        assert!(!midi.toggle_fine());
        assert_eq!(midi.wanted(focus, Seed::Dark, false, false), base);

        // A map that binds the mode's key nowhere lights nothing extra,
        // rather than the button that number used to be.
        let mut map = Map::nano_kontrol2();
        map.button.retain(|b| b.cc != 58);
        let mut midi = Midi::new(map).unwrap();
        let base = midi.wanted(focus, Seed::Dark, false, false);
        assert!(midi.toggle_fine());
        assert_eq!(midi.wanted(focus, Seed::Dark, false, false), base);
    }

    #[test]
    fn the_seed_lamp_says_which_rig_the_focused_monitor_is() {
        // The panel is the only thing that says it: the two seeds look
        // nothing alike on the glass, but which one a monitor is *on* is a
        // question a hand asks before it presses, and a toggle that answers
        // it only by changing the picture answers it too late.
        let midi = Midi::new(Map::nano_kontrol2()).unwrap();
        let focus = at(0, 0);
        let base = midi.wanted(focus, Seed::Dark, false, false);
        assert_eq!(
            midi.wanted(focus, Seed::BLOB, false, false) & !base,
            crate::lamps::lamp(41),
            "the seed is play on the factory map"
        );
        // A blob of no light is not a blob. No config can load one — the
        // load refuses it precisely so the two rigs have one spelling each —
        // but the type can hold one, and a lamp reading the *variant* rather
        // than the light would light PLAY over a monitor drawing nothing.
        assert_eq!(midi.wanted(focus, Seed::WhiteBlob(0.0), false, false), base);
        // Any blob, not just the one the toggle brings back: a config may
        // name its own level and the button is still what it is on.
        assert_eq!(
            midi.wanted(focus, Seed::WhiteBlob(0.42), false, false),
            midi.wanted(focus, Seed::BLOB, false, false)
        );
        // And a map that binds the seed nowhere lights nothing extra.
        let mut map = Map::nano_kontrol2();
        map.button.retain(|b| b.cc != 41);
        let midi = Midi::new(map).unwrap();
        let base = midi.wanted(focus, Seed::Dark, false, false);
        assert_eq!(midi.wanted(focus, Seed::BLOB, false, false), base);
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
        // CCs, and "next monitor" and "reset" swap buttons — a surface whose
        // silkscreen lies, and every behaviour test still green.
        let map = Map::nano_kontrol2();
        assert_eq!(
            map.fader,
            [
                fader(1, Knob::Hue),
                fader(2, Knob::Saturation),
                fader(3, Knob::Brightness),
                fader(4, Knob::Contrast),
                fader(5, Knob::Gamma),
                fader(6, Knob::Headroom),
                fader(7, Knob::Route),
                fader(16, Knob::Zoom),
                fader(17, Knob::Rotation),
                fader(18, Knob::TranslateX),
                fader(19, Knob::TranslateY),
                fader(20, Knob::Gain),
                fader(21, Knob::Bloom),
                fader(22, Knob::ChromaBleed),
                fader(23, Knob::Noise),
            ]
        );
        // The select row, split down the middle: the left four strips are the
        // cameras the rotaries turn, the right four the monitors the faders
        // turn. Written out rather than generated, so a row that slides by
        // one — or a half that turns back into four more cameras — fails
        // here rather than under a hand mid-piece.
        let nodes: Vec<Button> = (1..=4)
            .flat_map(|n| {
                [
                    button(31 + n, format!("num{n}")),
                    button(35 + n, format!("shift num{n}")),
                ]
            })
            .collect();
        assert_eq!(map.button[..8], nodes[..]);
        let slots: Vec<Button> = (1..=8)
            .flat_map(|n| {
                [
                    button(47 + n, format!("f{n}")),
                    button(63 + n, format!("shift f{n}")),
                ]
            })
            .collect();
        assert_eq!(map.button[8..24], slots[..]);
        assert_eq!(
            map.button[24..],
            [
                button(60, "n"),
                button(61, "m"),
                button(62, "space"),
                button(43, "backspace"),
                button(42, "r"),
                button(46, "`"),
                button(44, "enter"),
                button(41, ";"),
                button(58, "tab"),
            ]
        );
        // The one fader the map leaves alone. A knob quietly wired onto it
        // later is a knob a hand finds by throwing it.
        assert!(!map.fader.iter().any(|f| f.cc == 0));
        // And the two the factory map leaves alone, record above all: a
        // button bound here is a button a blind slip can find.
        for cc in [45, 59] {
            assert!(
                !map.button.iter().any(|b| b.cc == cc),
                "{} is bound",
                silkscreen(&map.device, cc)
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
        assert_eq!(
            missing,
            [
                "loop gain, red",
                "loop gain, green",
                "loop gain, blue",
                "bloom radius",
                "key threshold",
                "key softness",
                "key hue",
                "key tolerance",
                "send",
            ]
        );
        // Nothing is bound to quit: a slipped finger on a control surface
        // must not be able to stop the instrument.
        assert!(!map
            .button
            .iter()
            .any(|b| action_for_label(&b.key) == Some(Action::Quit)));
        // And every control the map names is one the surface has. The rows
        // are eight-wide blocks of control numbers, so a key table grown
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
    fn a_map_that_would_play_the_wrong_thing_is_refused() {
        // Within one list...
        let mut map = Map::nano_kontrol2();
        map.button.push(button(48, "n"));
        assert!(map.validate().unwrap_err().contains("bound twice"));

        // ...and across the two, which matters more: `action_for` looks at
        // the faders first, so a button sharing a fader's number is silently
        // dead rather than ambiguous.
        let mut map = Map::nano_kontrol2();
        map.button.push(button(1, "r"));
        assert!(map.validate().unwrap_err().contains("bound twice"));

        let mut map = Map::nano_kontrol2();
        map.fader.push(fader(200, Knob::Hue));
        let why = map.validate().unwrap_err();
        assert!(why.contains("200") && why.contains("127"), "{why}");

        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "wiggle"));
        let why = map.validate().unwrap_err();
        assert!(why.contains("wiggle"), "{why}");
        // The error lists what a key may be, because a config file is written
        // by hand and there are forty of them.
        assert!(why.contains("space") && why.contains("f1"), "{why}");

        // A shift that does nothing: every key but the slots ignores it, so a
        // binding that writes one is saying something the instrument will not
        // do, and saying so is cheaper than a performer finding out.
        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "shift n"));
        let why = map.validate().unwrap_err();
        assert!(why.contains("shift"), "{why}");

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
        // keys are called and wherever `Map::path` decides to look.
        let dir = scratch("map-file");
        assert_eq!(Map::path(&dir).file_name().unwrap(), "midi.toml");
        std::fs::write(
            dir.join("midi.toml"),
            "device = \"nanoKONTROL\"\n\
             [[fader]]\ncc = 0\nknob = \"hue\"\n\
             [[button]]\ncc = 41\nkey = \"shift f2\"\n",
        )
        .unwrap();
        let map = Map::load(&dir).unwrap();
        assert_eq!(map.fader, [fader(0, Knob::Hue)]);
        assert_eq!(map.button, [button(41, "shift f2")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_map_file_is_the_factory_map_and_a_broken_one_is_an_error() {
        let dir = scratch("map-absent");
        assert_eq!(Map::load(&dir).unwrap(), Map::nano_kontrol2());
        std::fs::write(
            dir.join("midi.toml"),
            "device = \"x\"\n[[fader]]\ncc = 0\nknob = \"wobble\"\n",
        )
        .unwrap();
        let why = Map::load(&dir).unwrap_err();
        assert!(why.contains("wobble"), "{why}");
        // A file that parses but would misplay is caught by the same door.
        std::fs::write(
            dir.join("midi.toml"),
            "device = \"x\"\n[[button]]\ncc = 1\nkey = \"nope\"\n",
        )
        .unwrap();
        assert!(Map::load(&dir).unwrap_err().contains("nope"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_fader_spans_exactly_its_knob_s_travel() {
        for knob in Knob::ALL {
            let (low, high) = knob.limit().ends();
            // The ends and the middle: two points fix any straight line, and
            // the third is what a square law would miss.
            assert!((value_at(knob, 0.0) - low).abs() < 1e-6, "{}", knob.name());
            assert!((value_at(knob, 1.0) - high).abs() < 1e-6, "{}", knob.name());
            let middle = (low + high) / 2.0;
            assert!(
                (value_at(knob, 0.5) - middle).abs() < 1e-6,
                "{}: {} not {middle}",
                knob.name(),
                value_at(knob, 0.5)
            );
            for step in 0..=127 {
                let position = step as f32 / 127.0;
                let round = position_of(knob, value_at(knob, position));
                assert!((round - position).abs() < 1e-5, "{}", knob.name());
            }
        }
        // Against numbers worked out by hand rather than by the inverse, so a
        // curve and its own inverse cannot agree their way past this.
        assert!((position_of(Knob::Contrast, 1.0) - 0.25).abs() < 1e-6);
        assert!((value_at(Knob::Contrast, 0.75) - 3.0).abs() < 1e-6);
        // A phase makes one full revolution, ending where it set out.
        assert!(value_at(Knob::Hue, 0.5).abs() < 1e-6);
        assert!(
            (value_at(Knob::Hue, 1.0) - value_at(Knob::Hue, 0.0) - std::f32::consts::TAU).abs()
                < 1e-5
        );
    }

    fn surface() -> (Midi, Params) {
        (Midi::new(Map::nano_kontrol2()).unwrap(), Params::default())
    }

    fn feed_at(midi: &mut Midi, params: &Params, focus: Focus, bytes: &[u8]) -> Vec<Action> {
        decode(bytes)
            .into_iter()
            .filter_map(|m| midi.action_for(m, params, focus))
            .collect()
    }

    fn feed(midi: &mut Midi, params: &Params, bytes: &[u8]) -> Vec<Action> {
        feed_at(midi, params, Focus::default(), bytes)
    }

    #[test]
    fn a_fader_does_not_move_its_knob_until_it_reaches_it() {
        let (mut midi, params) = surface();
        // Contrast is fader 5 and sits at 1.0 in a range of 0 to 4, so a
        // quarter of the way up. Everything below that is the fader in the
        // wrong place, and must do nothing at all.
        assert_eq!(feed(&mut midi, &params, &cc(4, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 10)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 20)), []);
        // Sweeping past where the knob is catches it, and from then on the
        // fader is the knob wherever it goes.
        let caught = feed(&mut midi, &params, &cc(4, 40));
        assert!(
            matches!(caught[..], [Action::Set(Knob::Contrast, _)]),
            "{caught:?}"
        );
        assert!(matches!(
            feed(&mut midi, &params, &cc(4, 0))[..],
            [Action::Set(Knob::Contrast, v)] if v.abs() < 1e-6
        ));
        assert!(matches!(
            feed(&mut midi, &params, &cc(4, 127))[..],
            [Action::Set(Knob::Contrast, v)] if (v - 4.0).abs() < 1e-6
        ));
    }

    /// The nudge one `feed` produced, or a failure naming what came out
    /// instead. Every fine-mode assertion is about a delta, and a delta of
    /// two positions divided by 127 does not land on a round number.
    fn one_nudge(acted: &[Action], knob: Knob) -> f32 {
        match acted {
            [Action::Nudge(turned, delta)] if *turned == knob => *delta,
            other => panic!("wanted one nudge of {}, got {other:?}", knob.name()),
        }
    }

    #[test]
    fn fine_mode_reads_a_fader_as_how_far_it_moved() {
        let (mut midi, params) = surface();
        assert!(midi.toggle_fine());
        // The first message after the mode is entered says where the fader
        // is standing. A move is two positions and there is only one, so
        // nothing turns yet — however far the fader is from its knob.
        assert_eq!(feed(&mut midi, &params, &cc(4, 64)), []);
        // Contrast is fader 5, over a range of 0 to 4. Eight of the 127
        // steps, geared a sixteenth: 8/127 of the travel, of a quarter of
        // that range.
        let want = 8.0 / 127.0 * 4.0 / 16.0;
        let delta = one_nudge(&feed(&mut midi, &params, &cc(4, 72)), Knob::Contrast);
        assert!((delta - want).abs() < 1e-6, "{delta} is not {want}");
        // And down is the same distance the other way.
        let delta = one_nudge(&feed(&mut midi, &params, &cc(4, 64)), Knob::Contrast);
        assert!((delta + want).abs() < 1e-6, "{delta} is not {}", -want);
        // A whole sweep of the control covers a sixteenth of the travel,
        // which is the entire point: 2032 places to stand instead of 127.
        feed(&mut midi, &params, &cc(4, 0));
        let mut swept = 0.0;
        for value in 1..=127 {
            if let [Action::Nudge(Knob::Contrast, delta)] =
                feed(&mut midi, &params, &cc(4, value))[..]
            {
                swept += delta;
            }
        }
        assert!(
            (swept - 4.0 / 16.0).abs() < 1e-4,
            "a full sweep moved {swept}"
        );
    }

    #[test]
    fn fine_mode_does_not_wait_for_pickup_and_cannot_throw_a_knob() {
        // A fader at the bottom while contrast stands a quarter up. Coarse,
        // that fader does nothing until the sweep reaches the knob; fine, it
        // moves the knob at once — and only by how far it travelled, which
        // is the whole reason it is safe to skip the wait.
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(4, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 1)), []);
        assert!(midi.toggle_fine());
        let delta = one_nudge(&feed(&mut midi, &params, &cc(4, 2)), Knob::Contrast);
        let step = 4.0 / 127.0 / 16.0;
        assert!((delta - step).abs() < 1e-6, "{delta} is not one fine step");
    }

    #[test]
    fn only_the_faders_moved_in_fine_mode_let_go() {
        // A fine sweep walks a fader a long way from a knob it turned a
        // sixteenth as far, so a grip kept through it would throw that knob
        // on the next coarse touch. But that is true of the fader that
        // *moved* and of no other: letting go of the panel on the way in and
        // again on the way out would charge every other fader a pickup sweep
        // for a one-knob trim — the very cost `release_knob` exists to
        // refuse.
        let (mut midi, params) = surface();
        // Catch contrast and gamma, each by sweeping up from the bottom past
        // where its knob stands.
        for control in [4, 5] {
            assert_eq!(feed(&mut midi, &params, &cc(control, 0)), []);
            assert_eq!(feed(&mut midi, &params, &cc(control, 40)).len(), 1);
        }
        assert!(midi.toggle_fine());
        // Only contrast is touched in fine mode, and it walks to the top.
        assert_eq!(feed(&mut midi, &params, &cc(4, 127)).len(), 1);
        assert!(!midi.toggle_fine());
        // Contrast has to be swept back to its knob: coarse, the fader is at
        // the top while the knob is a quarter up, and a grip that survived
        // would slam contrast to 4.
        assert_eq!(feed(&mut midi, &params, &cc(4, 120)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 20)).len(), 1);
        // Gamma never moved and never let go — its fader and its knob still
        // agree, so there is nothing to re-take.
        assert_eq!(feed(&mut midi, &params, &cc(5, 120)).len(), 1);
    }

    #[test]
    fn a_reset_of_one_knob_lets_go_of_that_knob_alone() {
        // The whole-panel release would charge every other fader a pickup
        // sweep for one knob moving.
        let (mut midi, params) = surface();
        // Catch contrast (fader 5) and gamma (fader 6), each by sweeping up
        // from the bottom past where it stands.
        assert_eq!(feed(&mut midi, &params, &cc(4, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(5, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 40)).len(), 1);
        assert_eq!(feed(&mut midi, &params, &cc(5, 40)).len(), 1);
        midi.release_knob(Knob::Contrast);
        // Contrast waits for a sweep back to where its knob now is; gamma is
        // untouched and still follows its fader anywhere.
        assert_eq!(feed(&mut midi, &params, &cc(4, 120)), []);
        assert_eq!(feed(&mut midi, &params, &cc(5, 120)).len(), 1);
    }

    #[test]
    fn fine_mode_gears_the_whole_travel_and_not_just_its_top() {
        // Contrast runs 0 to 4, so a gearing that used the top of the travel
        // instead of its width would be right about contrast and wrong about
        // every knob whose bottom is not zero. Pan x runs -1 to 1: half the
        // step under `high` alone, and a phase would be halved too.
        let (mut midi, params) = surface();
        assert!(midi.toggle_fine());
        assert_eq!(feed(&mut midi, &params, &cc(18, 64)), []);
        let delta = one_nudge(&feed(&mut midi, &params, &cc(18, 72)), Knob::TranslateX);
        let want = 8.0 / 127.0 * 2.0 / 16.0;
        assert!((delta - want).abs() < 1e-6, "{delta} is not {want}");
    }

    #[test]
    fn each_fader_in_fine_mode_reads_its_own_travel() {
        // Two faders worked in turn. Each nudge is that fader's own move —
        // a shared or mis-indexed last position would make one the
        // *difference between two different faders*, which is unbounded.
        let (mut midi, params) = surface();
        assert!(midi.toggle_fine());
        assert_eq!(feed(&mut midi, &params, &cc(4, 10)), []);
        assert_eq!(feed(&mut midi, &params, &cc(5, 100)), []);
        let step = |range: f32, steps: f32| steps / 127.0 * range / 16.0;
        let delta = one_nudge(&feed(&mut midi, &params, &cc(4, 14)), Knob::Contrast);
        assert!((delta - step(4.0, 4.0)).abs() < 1e-6, "contrast {delta}");
        let delta = one_nudge(&feed(&mut midi, &params, &cc(5, 98)), Knob::Gamma);
        assert!((delta - step(3.75, -2.0)).abs() < 1e-6, "gamma {delta}");
        let delta = one_nudge(&feed(&mut midi, &params, &cc(4, 15)), Knob::Contrast);
        assert!(
            (delta - step(4.0, 1.0)).abs() < 1e-6,
            "contrast again {delta}"
        );
    }

    #[test]
    fn a_reset_of_one_knob_lets_go_of_every_control_over_the_same_field() {
        // The rigid gain and its three channels are the same three floats.
        // A reset of the rigid one with a channel fader still caught — or
        // the other way round — leaves that fader holding a knob that has
        // just moved, and the rigid one throws all three channels at once.
        let mut map = Map::nano_kontrol2();
        map.fader.push(fader(24, Knob::GainR));
        let mut midi = Midi::new(map).unwrap();
        let params = Params::default();
        // Catch both: rotary 5 is the rigid gain, cc 24 is red. Both knobs
        // sit near 0.986 in a range of 0 to 1.2, so a sweep from the bottom
        // takes them.
        for control in [20, 24] {
            assert_eq!(feed(&mut midi, &params, &cc(control, 0)), []);
            assert_eq!(feed(&mut midi, &params, &cc(control, 127)).len(), 1);
        }
        midi.release_knob(Knob::GainR);
        // Neither is holding anything now, so neither can throw the gain.
        assert_eq!(feed(&mut midi, &params, &cc(20, 60)), []);
        assert_eq!(feed(&mut midi, &params, &cc(24, 60)), []);
        // And the other direction, from a fresh grip.
        for control in [20, 24] {
            assert_eq!(feed(&mut midi, &params, &cc(control, 0)), []);
            assert_eq!(feed(&mut midi, &params, &cc(control, 127)).len(), 1);
        }
        midi.release_knob(Knob::Gain);
        assert_eq!(feed(&mut midi, &params, &cc(20, 60)), []);
        assert_eq!(feed(&mut midi, &params, &cc(24, 60)), []);
        // A knob that shares no field with either keeps its grip.
        assert_eq!(feed(&mut midi, &params, &cc(4, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 40)).len(), 1);
        midi.release_knob(Knob::Gain);
        assert_eq!(feed(&mut midi, &params, &cc(4, 100)).len(), 1);
    }

    #[test]
    fn a_released_control_forgets_where_it_was_standing_as_well() {
        // Pickup catches when the fader's *travel since the last message*
        // spans the knob. A release that dropped only the grip and kept the
        // last position would let one jump across the knob re-take it, which
        // is the reset undone by a fader that never swept anywhere.
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(4, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(4, 40)).len(), 1);
        midi.release_knob(Knob::Contrast);
        // Contrast stands a quarter up, at 31 of 127. One message from 40
        // down to 20 spans it — and must still do nothing, because after a
        // release there is no "was" for it to have spanned from.
        assert_eq!(feed(&mut midi, &params, &cc(4, 20)), []);
    }

    #[test]
    fn an_unknown_key_is_named_as_one_even_with_a_shift_in_front() {
        // Both readings of an unknown label are `None`, so a shift check
        // that ran first would find them equal and report a shift that does
        // nothing — naming the wrong fault, and printing no list of the keys
        // that do exist. `shift num9` is the typo this block now invites.
        let refuse = |key: &str| {
            Midi::new(Map {
                device: "x".into(),
                fader: Vec::new(),
                button: vec![button(90, key)],
            })
            .err()
            .expect("a map the instrument would refuse")
        };
        for key in ["shift num9", "shift zzz", "zzz"] {
            let why = refuse(key);
            assert!(why.contains("no key called"), "{key}: {why}");
            assert!(why.contains("num1"), "{key} was not given the list: {why}");
        }
        // And a shift that really does change nothing is still caught, with
        // the message that names *that* fault.
        let why = refuse("shift r");
        assert!(why.contains("does not read shift"), "{why}");
    }

    #[test]
    fn a_fader_standing_on_its_knob_catches_it_at_once() {
        // The other way a fader is caught. Contrast at 3.0 is three quarters
        // of the way up a range that is not 0..1, so a pickup comparing the
        // raw value against the fader's own 0..1 would not call this standing
        // on it.
        let (mut midi, mut params) = surface();
        params.monitors[0].colour.contrast = 3.0;
        assert!(matches!(
            feed(&mut midi, &params, &cc(4, 95))[..],
            [Action::Set(Knob::Contrast, _)]
        ));
        // A fader elsewhere on the same knob still has to sweep to it.
        let (mut midi, _) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(4, 10)), []);
    }

    #[test]
    fn a_phase_fader_catches_its_knob_at_either_end_of_the_turn() {
        // Hue wraps, so the fader's two ends are the same angle. `wrap_pi`
        // leaves a value at exactly +PI, which is the *top* of the fader's
        // travel — while the fader that produced it is standing at the
        // bottom. Compared on a line the fader is then dead for 126 of its
        // 128 positions.
        let (mut midi, mut params) = surface();
        params.monitors[0].colour.hue = std::f32::consts::PI;
        assert!(matches!(
            feed(&mut midi, &params, &cc(1, 0))[..],
            [Action::Set(Knob::Hue, _)]
        ));
        // And the same angle a hair the other side catches at the bottom too.
        let (mut midi, mut params) = surface();
        params.monitors[0].colour.hue = -std::f32::consts::PI + 0.001;
        assert!(matches!(
            feed(&mut midi, &params, &cc(1, 0))[..],
            [Action::Set(Knob::Hue, _)]
        ));
        // A hue in the middle of the turn is still not caught from an end.
        let (mut midi, mut params) = surface();
        params.monitors[0].colour.hue = 0.0;
        assert_eq!(feed(&mut midi, &params, &cc(1, 0)), []);
    }

    #[test]
    fn unplugging_the_surface_makes_every_fader_find_its_knob_again() {
        let (mut midi, params) = surface();
        // Sweep the saturation fader down through where the knob is
        // standing, so it catches, and then take it back up: the knob
        // follows.
        assert_eq!(feed(&mut midi, &params, &cc(2, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(2, 0)).len(), 1);
        assert_eq!(feed(&mut midi, &params, &cc(2, 127)).len(), 1);
        // Unplug. The fader is still standing at the top and the knob is at
        // the top with it, but the next surface plugged in is a fader in an
        // unknown place — so the grip does not survive.
        midi.drop_port();
        assert_eq!(feed(&mut midi, &params, &cc(2, 64)), []);
    }

    #[test]
    fn moving_the_focus_makes_every_fader_find_its_knob_again() {
        // The other way the knobs move without a fader moving with them, and
        // the reason `App::refocus` exists: the fader is holding *this*
        // node's knob, and the next node's is somewhere else.
        let mut params = crate::config::crossed();
        params.monitors[0].colour.contrast = 1.0;
        params.monitors[1].colour.contrast = 4.0;
        let first = Focus::default();
        let second = Focus {
            camera: 0,
            monitor: 1,
            input: 0,
        };
        let mut midi = Midi::new(Map::nano_kontrol2()).unwrap();
        assert_eq!(feed_at(&mut midi, &params, first, &cc(4, 0)).len(), 0);
        assert_eq!(feed_at(&mut midi, &params, first, &cc(4, 64)).len(), 1);
        midi.release();
        // Monitor 2's contrast is at the top; the fader is at half. Without
        // letting go it would drag that knob down to half on the next touch.
        assert_eq!(feed_at(&mut midi, &params, second, &cc(4, 70)), []);
    }

    #[test]
    fn a_button_acts_when_it_goes_down_and_not_when_it_comes_up() {
        let (mut midi, params) = surface();
        // CC 60 is "marker set", bound to "n".
        assert_eq!(feed(&mut midi, &params, &cc(60, 127)), [Action::NextCamera]);
        // Held: a surface that repeats while a finger is on it must not walk
        // the focus round the graph.
        assert_eq!(feed(&mut midi, &params, &cc(60, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(60, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(60, 127)), [Action::NextCamera]);
    }

    #[test]
    fn the_buttons_reach_the_nodes_the_slots_and_the_transport() {
        let (mut midi, params) = surface();
        // One from each row, at both ends of it: the rows are three
        // eight-wide blocks of control numbers, and a block written from the
        // wrong first number lands whole on the wrong row. Both ends of the
        // Solo row are also both *halves* of it — a split written the wrong
        // way round would still pass on cc 32 alone.
        assert_eq!(
            feed(&mut midi, &params, &cc(32, 127)),
            [Action::FocusCamera(0)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(35, 127)),
            [Action::FocusCamera(3)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(36, 127)),
            [Action::FocusMonitor(0)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(39, 127)),
            [Action::FocusMonitor(3)]
        );
        assert_eq!(feed(&mut midi, &params, &cc(48, 127)), [Action::Recall(0)]);
        assert_eq!(feed(&mut midi, &params, &cc(55, 127)), [Action::Recall(7)]);
        assert_eq!(feed(&mut midi, &params, &cc(64, 127)), [Action::Store(0)]);
        assert_eq!(feed(&mut midi, &params, &cc(71, 127)), [Action::Store(7)]);
        // The transport strip: rewind puts one knob back, stop the whole
        // panel, cycle lifts the overlay, track-prev latches fine mode.
        assert_eq!(
            feed(&mut midi, &params, &cc(43, 127)),
            [Action::ResetLastKnob]
        );
        assert_eq!(feed(&mut midi, &params, &cc(42, 127)), [Action::Reset]);
        assert_eq!(feed(&mut midi, &params, &cc(46, 127)), [Action::Overlay]);
        assert_eq!(feed(&mut midi, &params, &cc(58, 127)), [Action::Fine]);
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
        assert_eq!(
            feed(&mut midi, &params, &[0xB9, 60, 127]),
            [Action::NextCamera]
        );
        assert_eq!(decode(&[0xB9, 0x07, 0x64]), [message(7, 0x64)]);
    }

    #[test]
    fn a_fader_reads_the_knob_at_the_focus_the_keys_are_on() {
        // The panel under the hands is the focused monitor's, so the pickup
        // has to compare against that one and not against monitor 1.
        let mut params = crate::config::crossed();
        params.monitors[0].colour.saturation = 0.0;
        params.monitors[1].colour.saturation = 4.0;
        let far = Focus {
            camera: 0,
            monitor: 1,
            input: 0,
        };
        // Fader 3 is saturation. At the top it is monitor 2's value, so it
        // catches there...
        let mut midi = Midi::new(Map::nano_kontrol2()).unwrap();
        assert!(matches!(
            feed_at(&mut midi, &params, far, &cc(2, 127))[..],
            [Action::Set(Knob::Saturation, _)]
        ));
        // ...and would not have on monitor 1, whose saturation is at zero.
        let mut midi = Midi::new(Map::nano_kontrol2()).unwrap();
        assert_eq!(
            feed_at(&mut midi, &params, Focus::default(), &cc(2, 127)),
            []
        );
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
        // routinely arrives as two. It sweeps past where saturation is
        // standing on the last message and not before, which is where the
        // pickup catches it.
        pipe.write_all(&[0xB0, 0x02, 0x7F, 0x02]).unwrap();
        pipe.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        pipe.write_all(&[0x40, 0x02, 0x00]).unwrap();
        pipe.flush().unwrap();

        let acted = wait_for(&mut midi, &params);
        assert!(
            matches!(acted[..], [Action::Set(Knob::Saturation, v)] if v.abs() < 1e-6),
            "{acted:?}"
        );
        // A button held down at the moment the cable comes out.
        pipe.write_all(&[0xB0, 60, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(wait_for(&mut midi, &params), [Action::NextCamera]);

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
        pipe.write_all(&[0xB0, 60, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(
            wait_for(&mut midi, &params),
            [Action::NextCamera],
            "the surface did not come back"
        );
        // And every fader starts again from wherever the knob is: this one
        // owned the saturation before the unplug.
        pipe.write_all(&[0xB0, 0x02, 0x7F]).unwrap();
        pipe.flush().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(drain(&mut midi, &params).is_empty(), "the grip survived");
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
        // The Solo row splits down the middle, four nodes to a hand: camera
        // 1 is Solo 1 and monitor 1 is Solo 5, which are controls 32 and 36.
        midi.show(Focus::default(), Seed::Dark, false, false);
        assert!(
            surface.wire.panel_becomes(lamp(32) | lamp(36)),
            "the panel the instrument started on never reached the wire"
        );
        let moved = Focus {
            camera: 1,
            ..Focus::default()
        };
        midi.show(moved, Seed::Dark, false, false);
        assert!(
            surface.wire.panel_becomes(lamp(33) | lamp(36)),
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
            .filter_map(|m| midi.action_for(m, params, Focus::default()))
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
