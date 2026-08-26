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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use web_time::Instant;

use crate::keys::{action_for_label, labels, Action};
use crate::params::{Focus, Knob, Limit, Params};

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
    /// A key, spelled the way the printed help spells it — `"p"`, `"space"`,
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
    /// The eight faders are the focused **monitor**: its front panel, and then
    /// how much of the focused camera it is showing. The eight rotaries above
    /// them are the focused **camera**: where it is pointed, how much light it
    /// hands back, and what its signal path does on the way. So the left hand
    /// works one monitor, the right hand one camera, and the top fader is the
    /// switcher crosspoint that joins the two the hands are on.
    ///
    /// Eight knobs are deliberately not here — the surface has sixteen
    /// controls and they are taken. The three per-channel gain offsets and
    /// the bloom radius are trims of knobs that are on the surface; the
    /// keyer's four wait for a hand that keys more than it bleeds and swaps
    /// this map for its own. They all stay on the keys.
    pub(crate) fn nano_kontrol2() -> Map {
        Map {
            device: "nanoKONTROL".into(),
            fader: vec![
                fader(0, Knob::Seed),
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
            // A "shift " that changes nothing. Every key but the slots
            // ignores it, so a binding that writes one is asking for
            // something the instrument will not do — and a performer finding
            // that out mid-set is worse than a line at startup.
            if let Some(bare) = b.key.strip_prefix("shift ") {
                if action_for_label(bare) == action_for_label(&b.key) {
                    return Err(format!(
                        "cc {}: {:?} is {bare:?} — only the preset slots read shift",
                        b.cc, b.key
                    ));
                }
            }
            if action_for_label(&b.key).is_none() {
                let known: Vec<&str> = labels().collect();
                return Err(format!(
                    "cc {}: no key called {:?}; there are {}, each also with a \"shift \" in front",
                    b.cc,
                    b.key,
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
}

const fn transport(cc: u8, name: &'static str, row: u8, col: u8) -> TransportButton {
    TransportButton { cc, name, row, col }
}

/// The transport strip as the device has it: track prev/next on top, cycle
/// beside the three marker buttons, and the tape row underneath.
pub(crate) const TRANSPORT: &[TransportButton] = &[
    transport(58, "track prev", 0, 0),
    transport(59, "track next", 0, 1),
    transport(46, "cycle", 1, 0),
    transport(60, "marker set", 1, 1),
    transport(61, "marker prev", 1, 2),
    transport(62, "marker next", 1, 3),
    transport(43, "rewind", 2, 0),
    transport(44, "forward", 2, 1),
    transport(42, "stop", 2, 2),
    transport(41, "play", 2, 3),
    transport(45, "record", 2, 4),
];

pub(crate) fn spot(cc: u8) -> Option<Spot> {
    let block = |first: u8| (cc >= first && cc < first + 8).then(|| cc - first);
    if let Some(i) = block(0) {
        return Some(Spot::Fader(i));
    }
    if let Some(i) = block(16) {
        return Some(Spot::Rotary(i));
    }
    if let Some(i) = block(32) {
        return Some(Spot::S(i));
    }
    if let Some(i) = block(48) {
        return Some(Spot::M(i));
    }
    if let Some(i) = block(64) {
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

/// The nanoKONTROL2's buttons, in the three rows and the transport strip it
/// has them in.
fn nano_buttons() -> Vec<Button> {
    let mut out = Vec::new();
    // A channel strip's three buttons are the three things a performer does
    // to the thing that strip stands for, in order of how much they commit:
    // Solo points the knobs at camera n, Mute plays slot n back, Record
    // writes over it. Solo selects because that is what a hand off a mixer
    // reaches for it to do — the press that started this was S1 meaning
    // "camera 1" and getting a preset over the live panel — and Record
    // stores because it is the button you have to mean, the asymmetry shift
    // makes on the function keys.
    //
    // Spelled off the key tables rather than beside them, so a label is
    // written once and a rebound key moves the button with it.
    for (camera, (_, key)) in crate::keys::CAMERA_KEYS.iter().enumerate() {
        out.push(button(32 + camera as u8, *key));
    }
    for (slot, (_, key)) in crate::keys::SLOT_KEYS.iter().enumerate() {
        out.push(button(48 + slot as u8, *key));
        out.push(button(64 + slot as u8, format!("shift {key}")));
    }
    out.extend([
        // The markers, holding what the strips displaced: which monitor the
        // faders are on, and the two that act on the whole instrument. No
        // step-to-the-next-camera any more — the Solo row reaches every
        // camera the surface has a strip for, and two ways to the same one
        // is one too many. `n` keeps it on the keyboard for a graph deeper
        // than the surface.
        button(60, "m"),
        button(61, "space"),
        button(62, "r"),
        // Transport, where the automation belongs. Nothing is bound to quit.
        button(41, "p"),
        button(43, "7"),
        button(44, "8"),
        button(58, "9"),
        button(59, "0"),
        // Cycle shows and hides the overlay that explains all of the above —
        // the one button whose job survives not knowing what any button does.
        button(46, "`"),
    ]);
    out
}

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
}

impl Stream {
    /// Feed one byte in, and push out any control change it completed.
    fn push(&mut self, byte: u8, out: &mut Vec<ControlChange>) {
        match byte {
            // Real time. Interleaved anywhere, even between a control number
            // and its value, and it does not disturb running status.
            0xF8..=0xFF => {}
            // System exclusive and system common. No running status survives
            // one, which is the whole of what a scene dump needs: its payload
            // is data bytes with no status in force, and the branch below
            // drops those. Nothing has to know a dump is being read.
            0xF0..=0xF7 => self.status = 0,
            0x80..=0xEF => {
                self.status = byte;
                self.have = 0;
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
                    out.push(ControlChange {
                        control: self.data[0],
                        value: self.data[1],
                    });
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
/// to be standing — twenty knobs' worth of that on a hot-plug, mid-piece,
/// with the headroom fader slamming a monitor to white. So a fader does not
/// take its knob over until it has passed through where the knob already is,
/// and then keeps it until it is let go of: an unplug, a recall, or the focus
/// moving to a node whose knobs are somewhere else entirely.
#[derive(Clone, Copy, Debug, Default)]
struct Pickup {
    caught: bool,
    was: Option<f32>,
}

/// One step of a 7-bit control. A fader that cannot land exactly on its
/// knob's value has to catch it anyway, or a knob between two steps is one no
/// fader can ever reach.
const STEP: f32 = 1.0 / 127.0;

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
}

impl Midi {
    /// The one door: a `Midi` cannot exist over a map the instrument would
    /// refuse, so nothing downstream has to handle one.
    pub fn new(map: Map) -> Result<Midi, String> {
        map.validate()?;
        Ok(Midi {
            action: map
                .button
                .iter()
                .map(|b| action_for_label(&b.key).expect("validate checked every label"))
                .collect(),
            pickup: vec![Pickup::default(); map.fader.len()],
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

    /// Let go of every fader's grip on its knob, because the knobs moved
    /// without the faders moving with them — a recall, a reset, or the focus
    /// stepping to a node whose knobs are somewhere else. Without this the
    /// next fader brushed throws its knob to wherever the fader is standing,
    /// which is the recall undone one knob at a time.
    pub fn release(&mut self) {
        for pickup in &mut self.pickup {
            *pickup = Pickup::default();
        }
    }

    fn drop_port(&mut self) {
        self.port = None;
        self.release();
        self.held.iter_mut().for_each(|held| *held = false);
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
        for path in find(&self.snd, &cards, &self.map.device) {
            match open(&path) {
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
    pub fn action_for(
        &mut self,
        message: ControlChange,
        params: &Params,
        focus: Focus,
    ) -> Option<Action> {
        if let Some(i) = self.map.fader.iter().position(|f| f.cc == message.control) {
            let knob = self.map.fader[i].knob;
            let fader = f32::from(message.value) / 127.0;
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

/// Open the device and read it on a thread, decoding as it goes.
///
/// A thread rather than a non-blocking read polled from the frame loop: a
/// blocking `read` on a device nobody is touching costs nothing, and it is
/// the same `read` that reports the unplug. The channel is unbounded because
/// a button press dropped for backpressure is a press that did not happen,
/// and a surface sends a kilobyte a second at its very worst.
fn open(path: &Path) -> Result<Port, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
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
                for cc in out.drain(..) {
                    if tx.send(cc).is_err() {
                        return;
                    }
                }
            }
        })
        .map_err(|e| format!("{}: {e}", name.display()))?;
    Ok(Port { path: name, rx })
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
        // The names are pinned above; the grid is the other fact the table
        // carries, and a swapped row or column draws the overlay's caption
        // on a neighbouring button.
        let at = |cc| match spot(cc) {
            Some(Spot::Transport(t)) => (t.row, t.col),
            other => panic!("cc {cc}: {other:?}"),
        };
        assert_eq!(at(58), (0, 0));
        assert_eq!(at(59), (0, 1));
        assert_eq!(at(46), (1, 0));
        assert_eq!(at(62), (1, 3));
        assert_eq!(at(43), (2, 0));
        assert_eq!(at(41), (2, 3));
        assert_eq!(at(45), (2, 4));
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

    fn decode(bytes: &[u8]) -> Vec<ControlChange> {
        let mut stream = Stream::default();
        let mut out = Vec::new();
        for byte in bytes {
            stream.push(*byte, &mut out);
        }
        out
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
        let mut bytes = cc(1, 9);
        bytes.extend([0xF0, 0x42, 0x40, 0x00, 0x01, 0x13, 0x00, 0x7F, 0xF7]);
        // And no status survives the dump, so the pair after it belongs to
        // nothing rather than to the fader that moved before it.
        bytes.extend([0x02, 0x05]);
        assert_eq!(decode(&bytes), [message(1, 9)]);
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
        // whole. Read wrongly it is CC 32, which is the recall button for
        // slot 1: a surface's touch strip would be recalling presets.
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
                fader(0, Knob::Seed),
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
        let cameras: Vec<Button> = (1..=8).map(|n| button(31 + n, format!("num{n}"))).collect();
        assert_eq!(map.button[..8], cameras[..]);
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
                button(60, "m"),
                button(61, "space"),
                button(62, "r"),
                button(41, "p"),
                button(43, "7"),
                button(44, "8"),
                button(58, "9"),
                button(59, "0"),
                button(46, "`"),
            ]
        );
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
            ]
        );
        // Nothing is bound to quit: a slipped finger on a control surface
        // must not be able to stop the instrument.
        assert!(!map
            .button
            .iter()
            .any(|b| action_for_label(&b.key) == Some(Action::Quit)));
        // And every control the map names is one the surface has. A row is
        // eight wide, so a key table grown past eight would otherwise walk
        // its block's last button off into the numbers between the rows.
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
        map.button.push(button(0, "r"));
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
        // Sweep the seed fader down through where the seed is standing, so it
        // catches, and then take it back up: the knob follows.
        assert_eq!(feed(&mut midi, &params, &cc(0, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(0, 0)).len(), 1);
        assert_eq!(feed(&mut midi, &params, &cc(0, 127)).len(), 1);
        // Unplug. The fader is still standing at the top and the knob is at
        // the top with it, but the next surface plugged in is a fader in an
        // unknown place — so the grip does not survive.
        midi.drop_port();
        assert_eq!(feed(&mut midi, &params, &cc(0, 64)), []);
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
        // CC 60 is "marker set", bound to "m".
        assert_eq!(
            feed(&mut midi, &params, &cc(60, 127)),
            [Action::NextMonitor]
        );
        // Held: a surface that repeats while a finger is on it must not walk
        // the focus round the graph.
        assert_eq!(feed(&mut midi, &params, &cc(60, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(60, 0)), []);
        assert_eq!(
            feed(&mut midi, &params, &cc(60, 127)),
            [Action::NextMonitor]
        );
    }

    #[test]
    fn the_buttons_reach_the_cameras_the_slots_and_the_automation() {
        let (mut midi, params) = surface();
        // One from each row, at both ends of the strip: the three rows are
        // three eight-wide blocks of control numbers and a block written
        // from the wrong first number lands whole on the wrong row.
        assert_eq!(feed(&mut midi, &params, &cc(32, 127)), [Action::Camera(0)]);
        assert_eq!(feed(&mut midi, &params, &cc(39, 127)), [Action::Camera(7)]);
        assert_eq!(feed(&mut midi, &params, &cc(48, 127)), [Action::Recall(0)]);
        assert_eq!(feed(&mut midi, &params, &cc(55, 127)), [Action::Recall(7)]);
        assert_eq!(feed(&mut midi, &params, &cc(64, 127)), [Action::Store(0)]);
        assert_eq!(feed(&mut midi, &params, &cc(71, 127)), [Action::Store(7)]);
        assert_eq!(feed(&mut midi, &params, &cc(41, 127)), [Action::Motion]);
        assert_eq!(
            feed(&mut midi, &params, &cc(44, 127)),
            [Action::MotionRate(1.0)]
        );
        assert_eq!(
            feed(&mut midi, &params, &cc(59, 127)),
            [Action::MotionDepth(1.0)]
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
        assert_eq!(
            feed(&mut midi, &params, &[0xB9, 60, 127]),
            [Action::NextMonitor]
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
        // routinely arrives as two. It ends where the seed already is, so the
        // pickup catches it on the last message and not before.
        pipe.write_all(&[0xB0, 0x00, 0x7F, 0x00]).unwrap();
        pipe.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        pipe.write_all(&[0x40, 0x00, 0x00]).unwrap();
        pipe.flush().unwrap();

        let acted = wait_for(&mut midi, &params);
        assert!(
            matches!(acted[..], [Action::Set(Knob::Seed, v)] if v.abs() < 1e-6),
            "{acted:?}"
        );
        // A button held down at the moment the cable comes out.
        pipe.write_all(&[0xB0, 60, 127]).unwrap();
        pipe.flush().unwrap();
        assert_eq!(wait_for(&mut midi, &params), [Action::NextMonitor]);

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
            [Action::NextMonitor],
            "the surface did not come back"
        );
        // And every fader starts again from wherever the knob is: this one
        // owned the seed before the unplug.
        pipe.write_all(&[0xB0, 0x00, 0x7F]).unwrap();
        pipe.flush().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(drain(&mut midi, &params).is_empty(), "the grip survived");
        drop(pipe);
        std::fs::remove_dir_all(&dir).unwrap();
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
