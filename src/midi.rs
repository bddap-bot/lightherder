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
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::keys::{action_for_label, labels, Action};
use crate::params::{Focus, Knob, Limit, Params};

/// Where ALSA puts its character devices.
pub const DEV_SND: &str = "/dev/snd";

/// The one file that says which card is which. `/dev/snd` names a card by
/// number and nothing else, so a surface cannot be recognised without it.
pub const CARDS: &str = "/proc/asound/cards";

/// How often an absent surface is looked for. Hot-plug with no netlink
/// socket and no inotify: a `read_dir` of six entries once a second costs
/// nothing next to a frame, and a second is faster than a hand can plug a
/// cable in and reach the faders.
const RESCAN: Duration = Duration::from_secs(1);

/// The value at and above which a button counts as pushed. Everything sends
/// 0 and 127; the halfway line is what keeps a surface that ramps in between
/// from firing twice.
const PUSHED: u8 = 64;

/// One control change: the whole of the MIDI a control surface sends, and
/// all this instrument reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cc {
    pub channel: u8,
    pub control: u8,
    pub value: u8,
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
    pub cc: u8,
    pub knob: Knob,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
    pub cc: u8,
    /// A key, spelled the way the printed help spells it — `"p"`, `"space"`,
    /// `"shift f1"`.
    pub key: String,
}

const fn fader(cc: u8, knob: Knob) -> Fader {
    Fader { cc, knob }
}

impl Map {
    /// The factory CC layout of a Korg nanoKONTROL2, which is what this
    /// instrument is played from.
    ///
    /// The eight faders are the focused monitor's front panel — the colour
    /// stage, its seed and its rail — with the loop gain on the last, since
    /// gain and brightness are played together. The eight rotaries above
    /// them are the focused camera: where it is pointed and what its signal
    /// path does to the light. So the left hand works one monitor and the
    /// right hand one camera, and the two focus buttons move which.
    ///
    /// The three per-channel gain trims are deliberately not here. There are
    /// nineteen knobs and sixteen controls, and those three are a trim on the
    /// rigid gain that is on a fader — set once, not swept.
    pub fn nano_kontrol2() -> Map {
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
                fader(7, Knob::Gain),
                fader(16, Knob::Zoom),
                fader(17, Knob::Rotation),
                fader(18, Knob::TranslateX),
                fader(19, Knob::TranslateY),
                fader(20, Knob::Bloom),
                fader(21, Knob::BloomRadius),
                fader(22, Knob::ChromaBleed),
                fader(23, Knob::Noise),
            ],
            button: nano_buttons(),
        }
    }

    /// Where the map is kept: beside the preset slots, because both are the
    /// performer's own configuration of one instrument.
    pub fn path(dir: &Path) -> PathBuf {
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::nano_kontrol2()),
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
    pub fn validate(&self) -> Result<(), String> {
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
    // Solo recalls a slot and Record stores one — the same asymmetry shift
    // makes on the function keys, in the row a hand reaches for and the row
    // it has to mean.
    for slot in 1..=crate::slots::SLOTS as u8 {
        out.push(button(31 + slot, format!("f{slot}")));
        out.push(button(63 + slot, format!("shift f{slot}")));
    }
    out.extend([
        // Mute: what the panel is pointed at, and what is on the glass.
        button(48, "n"),
        button(49, "m"),
        button(50, "space"),
        button(51, "r"),
        // Transport, where the automation belongs. Nothing is bound to quit.
        button(41, "p"),
        button(43, "7"),
        button(44, "8"),
        button(58, "9"),
        button(59, "0"),
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
pub struct Stream {
    /// The status byte in force, or 0 for none.
    status: u8,
    data: [u8; 2],
    have: usize,
}

impl Stream {
    /// Feed one byte in, and push out any control change it completed.
    pub fn push(&mut self, byte: u8, out: &mut Vec<Cc>) {
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
                    out.push(Cc {
                        channel: self.status & 0x0F,
                        control: self.data[0],
                        value: self.data[1],
                    });
                }
            }
        }
    }
}

/// Where a fader is, 0 at the bottom and 1 at the top, as the value its knob
/// would take.
fn value_at(knob: Knob, position: f32) -> f32 {
    match knob.limit() {
        Limit::Clamp(low, high) => low + position * (high - low),
        // A phase has no ends, so the fader's are the same point: one full
        // turn from bottom to top, arriving back where it set out.
        Limit::Wrap => (position - 0.5) * std::f32::consts::TAU,
    }
}

/// The inverse: where a fader would have to be for the knob to read `value`.
/// Only the pickup below needs this, and only to compare against.
fn position_of(knob: Knob, value: f32) -> f32 {
    match knob.limit() {
        Limit::Clamp(low, high) => (value - low) / (high - low),
        Limit::Wrap => value / std::f32::consts::TAU + 0.5,
    }
}

/// One fader's grip on its knob.
///
/// A fader sends where it is, so the first one touched after the surface is
/// plugged in would otherwise throw its knob to wherever the fader happens
/// to be standing — nineteen knobs' worth of that on a hot-plug, mid-piece,
/// with the headroom fader slamming a monitor to white. So a fader does not
/// take its knob over until it has passed through where the knob already is,
/// and then keeps it until the surface is unplugged or the whole panel is
/// replaced under it by a recall.
#[derive(Clone, Copy, Debug, Default)]
struct Pickup {
    caught: bool,
    was: Option<f32>,
}

impl Pickup {
    /// Whether this move of the fader reaches the knob. `knob` is where the
    /// knob is now, in the fader's own units.
    fn catches(&mut self, position: f32, knob: f32) -> bool {
        let was = self.was.replace(position).unwrap_or(position);
        // One step of a 7-bit control, so a fader that cannot land exactly
        // on the knob's value still catches it rather than sweeping past.
        const STEP: f32 = 1.0 / 127.0;
        self.caught |=
            (was - knob).min(position - knob) <= STEP && (was - knob).max(position - knob) >= -STEP;
        self.caught
    }
}

/// The surface, connected or not.
pub struct Midi {
    map: Map,
    /// Read at every rescan rather than kept: the whole point is that a
    /// device may not be there yet.
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
    /// So a device that is there but will not open is said once, not sixty
    /// times a second.
    complained: bool,
}

struct Port {
    path: PathBuf,
    rx: Receiver<Cc>,
}

impl Midi {
    pub fn new(map: Map) -> Midi {
        Midi {
            pickup: vec![Pickup::default(); map.fader.len()],
            held: vec![false; map.button.len()],
            map,
            snd: PathBuf::from(DEV_SND),
            cards: PathBuf::from(CARDS),
            port: None,
            next_scan: Instant::now(),
            complained: false,
        }
    }

    /// Look somewhere other than the real ALSA for the surface. Tests only —
    /// it is the seam that lets the whole path, discovery through action, run
    /// against a directory and a file this process wrote.
    #[cfg(test)]
    fn looking_in(mut self, snd: PathBuf, cards: PathBuf) -> Midi {
        self.snd = snd;
        self.cards = cards;
        self
    }

    /// Every action the surface has produced since the last call. Called once
    /// a frame; never blocks, and never waits on a device that is not there.
    pub fn poll(&mut self, params: &Params, focus: Focus) -> Vec<Action> {
        self.connect();
        let mut messages = Vec::new();
        let mut gone = false;
        if let Some(port) = &self.port {
            loop {
                match port.rx.try_recv() {
                    Ok(cc) => messages.push(cc),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        log::info!("surface: {} went away", port.path.display());
                        gone = true;
                        break;
                    }
                }
            }
        }
        // Acted on before the unplug is: the last messages off a device are
        // the ones already in hand, and dropping the port lets go of every
        // fader's grip on its knob.
        let out = messages
            .into_iter()
            .filter_map(|cc| self.act(cc, params, focus))
            .collect();
        if gone {
            self.drop_port();
        }
        out
    }

    /// The panel has been replaced under the faders — a recall, a reset — so
    /// every fader has to find its knob again. Without this the first fader
    /// brushed after a recall throws its knob back to where the fader was
    /// standing, which is the recalled preset undone one knob at a time.
    pub fn recatch(&mut self) {
        for pickup in &mut self.pickup {
            *pickup = Pickup::default();
        }
    }

    fn drop_port(&mut self) {
        self.port = None;
        self.recatch();
        self.held.iter_mut().for_each(|held| *held = false);
        self.next_scan = Instant::now() + RESCAN;
    }

    fn connect(&mut self) {
        if self.port.is_some() || Instant::now() < self.next_scan {
            return;
        }
        self.next_scan = Instant::now() + RESCAN;
        let cards = std::fs::read_to_string(&self.cards).unwrap_or_default();
        let Some(path) = find(&self.snd, &cards, &self.map.device) else {
            return;
        };
        match open(&path) {
            Ok(port) => {
                log::info!("surface: {} on {}", self.map.device, port.path.display());
                self.complained = false;
                self.port = Some(port);
            }
            Err(why) if !self.complained => {
                log::error!("surface: {why}");
                self.complained = true;
            }
            Err(_) => {}
        }
    }

    /// What one control change does, if anything.
    fn act(&mut self, cc: Cc, params: &Params, focus: Focus) -> Option<Action> {
        if let Some(i) = self.map.fader.iter().position(|f| f.cc == cc.control) {
            let knob = self.map.fader[i].knob;
            let position = f32::from(cc.value) / 127.0;
            let at = position_of(knob, params.knob(knob, focus));
            return self.pickup[i]
                .catches(position, at)
                .then(|| Action::Set(knob, value_at(knob, position)));
        }
        let i = self.map.button.iter().position(|b| b.cc == cc.control)?;
        let down = cc.value >= PUSHED;
        let pressed = down && !self.held[i];
        self.held[i] = down;
        // Bound at load, so this cannot be a miss at play time.
        pressed
            .then(|| action_for_label(&self.map.button[i].key))
            .flatten()
    }
}

/// The card lines of [`CARDS`]: `" 2 [nanoKONTROL2  ]: USB-Audio - nanoKONTROL2"`.
/// Each card has a second, indented line as well, which has no number in
/// front and so is not one of these.
fn cards(text: &str) -> impl Iterator<Item = (u32, &str)> {
    text.lines().filter_map(|line| {
        let index = line.split_whitespace().next()?.parse().ok()?;
        Some((index, line))
    })
}

/// The raw MIDI device of the first card whose line names `device`.
///
/// `D0` and upwards: a controller has one MIDI endpoint, and the lowest is
/// it. A card with several would need a name that says which, and no surface
/// this instrument is played from has one.
fn find(snd: &Path, cards_text: &str, device: &str) -> Option<PathBuf> {
    let wanted = device.to_lowercase();
    let card = cards(cards_text).find(|(_, line)| line.to_lowercase().contains(&wanted))?;
    (0..8)
        .map(|d| snd.join(format!("midiC{}D{d}", card.0)))
        .find(|path| path.exists())
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

    fn cc(control: u8, value: u8) -> Vec<u8> {
        vec![0xB0, control, value]
    }

    fn decode(bytes: &[u8]) -> Vec<Cc> {
        let mut stream = Stream::default();
        let mut out = Vec::new();
        for byte in bytes {
            stream.push(*byte, &mut out);
        }
        out
    }

    #[test]
    fn a_control_change_arrives_however_the_reads_fall() {
        let bytes = cc(7, 100);
        let whole = decode(&bytes);
        assert_eq!(
            whole,
            [Cc {
                channel: 0,
                control: 7,
                value: 100
            }]
        );
        // The same three bytes fed one at a time, which is what a `read` off
        // a device that a hand is moving actually delivers.
        let mut stream = Stream::default();
        let mut out = Vec::new();
        for byte in &bytes {
            stream.push(*byte, &mut out);
        }
        assert_eq!(out, whole);
    }

    #[test]
    fn running_status_is_how_a_sweep_arrives() {
        // One status byte and then pairs: a surface drops the status on
        // every message after the first of a sweep.
        let mut bytes = cc(0, 1);
        bytes.extend([0, 2, 0, 3]);
        let values: Vec<u8> = decode(&bytes).iter().map(|m| m.value).collect();
        assert_eq!(values, [1, 2, 3]);
    }

    #[test]
    fn a_clock_byte_in_the_middle_of_a_message_is_not_part_of_it() {
        // Real time is interleaved anywhere, including between a control
        // number and its value, and must not disturb what it lands in.
        let bytes = [0xB0, 0x07, 0xF8, 0x40, 0xFE, 0x07, 0x41];
        let values: Vec<u8> = decode(&bytes).iter().map(|m| m.value).collect();
        assert_eq!(values, [0x40, 0x41]);
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
        assert_eq!(
            decode(&bytes),
            [Cc {
                channel: 0,
                control: 1,
                value: 9
            }]
        );
    }

    #[test]
    fn only_a_control_change_comes_out() {
        // A surface sends notes and a pitch bend too, and none of them is a
        // knob. Their data bytes look exactly like a control change's, so
        // the status they arrived under is the only thing keeping them out —
        // including for the two that carry one data byte rather than two,
        // which this decoder deliberately pairs up wrongly.
        let mut bytes = vec![0x90, 0x40, 0x7F]; // note on
        bytes.extend([0xC0, 0x05]); // program change
        bytes.extend([0xE0, 0x00, 0x40]); // pitch bend
        bytes.extend([0xD0, 0x20]); // channel pressure
        bytes.extend(cc(3, 11));
        assert_eq!(
            decode(&bytes),
            [Cc {
                channel: 0,
                control: 3,
                value: 11
            }]
        );
    }

    #[test]
    fn a_surface_on_another_channel_is_still_the_surface() {
        // There is one device plugged in and it is the instrument's; which
        // channel it was set to is not a reason to ignore it.
        let bytes = [0xB9, 0x07, 0x64];
        assert_eq!(decode(&bytes)[0].channel, 9);
    }

    const SAMPLE_CARDS: &str = "\
 0 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0xf7080000 irq 100
 1 [Generic        ]: HDA-Intel - HD-Audio Generic
                      HD-Audio Generic at 0xf7b00000 irq 102
 2 [nanoKONTROL2   ]: USB-Audio - nanoKONTROL2
                      KORG INC. nanoKONTROL2 at usb-0000:00:14.0-2, full speed
";

    /// A directory of this test's own, with the device nodes a card would
    /// have. The suite runs in one process, so the pid alone would have
    /// every test here sharing one.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lightherder-midi-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_surface_is_found_by_name_among_the_cards_that_are_not_it() {
        let dir = scratch("find");
        for card in [0, 1, 2] {
            std::fs::write(dir.join(format!("midiC{card}D0")), "").unwrap();
        }
        // Card 2 is the one named, and two others have raw MIDI devices too
        // — an onboard codec with a MIDI port is not far-fetched, and a
        // search that took the first device would take one of them.
        assert_eq!(
            find(&dir, SAMPLE_CARDS, "nanoKONTROL"),
            Some(dir.join("midiC2D0"))
        );
        // Case, because a card's line spells it however the vendor did.
        assert_eq!(
            find(&dir, SAMPLE_CARDS, "nanokontrol2"),
            Some(dir.join("midiC2D0"))
        );
        assert_eq!(find(&dir, SAMPLE_CARDS, "Launchpad"), None);
        // Named, but not plugged in: the card is gone from the file too.
        assert_eq!(find(&dir, " 0 [NVidia ]: x\n", "nanoKONTROL"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_card_with_no_raw_midi_device_is_not_a_surface() {
        // Every card is in the file whether or not it has a MIDI endpoint.
        let dir = scratch("no-device");
        assert_eq!(find(&dir, SAMPLE_CARDS, "nanoKONTROL"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_second_line_of_a_card_is_not_a_card() {
        let seen: Vec<u32> = cards(SAMPLE_CARDS).map(|(index, _)| index).collect();
        assert_eq!(seen, [0, 1, 2]);
    }

    #[test]
    fn the_factory_map_is_a_map_the_instrument_would_accept() {
        Map::nano_kontrol2().validate().unwrap();
    }

    #[test]
    fn the_factory_map_covers_the_surface_it_names() {
        let map = Map::nano_kontrol2();
        assert_eq!(map.fader.len(), 16, "eight faders and eight rotaries");
        // Every knob a hand sweeps is on it. The three per-channel gain
        // trims are the documented exception, and naming them here is what
        // makes a knob added later show up as a failure rather than as a
        // knob nobody can reach from the panel.
        let missing: Vec<&str> = Knob::ALL
            .into_iter()
            .filter(|knob| !map.fader.iter().any(|f| f.knob == *knob))
            .map(Knob::name)
            .collect();
        assert_eq!(
            missing,
            ["loop gain, red", "loop gain, green", "loop gain, blue"]
        );
        // And every slot is reachable, both ways round.
        for slot in 1..=crate::slots::SLOTS {
            for key in [format!("f{slot}"), format!("shift f{slot}")] {
                assert!(map.button.iter().any(|b| b.key == key), "{key}");
            }
        }
    }

    #[test]
    fn a_map_that_would_play_the_wrong_thing_is_refused() {
        let mut map = Map::nano_kontrol2();
        map.button.push(button(48, "n"));
        assert!(map.validate().unwrap_err().contains("bound twice"));

        let mut map = Map::nano_kontrol2();
        map.button.push(button(90, "wiggle"));
        let why = map.validate().unwrap_err();
        assert!(why.contains("wiggle"), "{why}");
        // The error lists what a key may be, because a config file is
        // written by hand and there are forty of them.
        assert!(why.contains("space") && why.contains("f1"), "{why}");

        let mut map = Map::nano_kontrol2();
        map.device = String::new();
        assert!(map.validate().is_err());
    }

    #[test]
    fn a_map_file_is_read_the_way_it_is_written() {
        // A literal file, not a round trip: the README documents these key
        // names, and a round trip agrees with itself whatever serde calls
        // them.
        let dir = scratch("map-file");
        std::fs::write(
            Map::path(&dir),
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
            Map::path(&dir),
            "device = \"x\"\n[[fader]]\ncc = 0\nknob = \"wobble\"\n",
        )
        .unwrap();
        let why = Map::load(&dir).unwrap_err();
        assert!(why.contains("wobble"), "{why}");
        // A file that parses but would misplay is caught by the same door.
        std::fs::write(
            Map::path(&dir),
            "device = \"x\"\n[[button]]\ncc = 1\nkey = \"nope\"\n",
        )
        .unwrap();
        assert!(Map::load(&dir).unwrap_err().contains("nope"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_fader_spans_exactly_its_knob_s_travel() {
        for knob in Knob::ALL {
            let (bottom, top) = (value_at(knob, 0.0), value_at(knob, 1.0));
            match knob.limit() {
                Limit::Clamp(low, high) => {
                    assert!((bottom - low).abs() < 1e-6, "{}", knob.name());
                    assert!((top - high).abs() < 1e-6, "{}", knob.name());
                }
                // A phase's ends are the same point, half a turn either side
                // of the middle — so a fader makes one revolution.
                Limit::Wrap => {
                    assert!((top - bottom - std::f32::consts::TAU).abs() < 1e-5);
                    assert!(value_at(knob, 0.5).abs() < 1e-6);
                }
            }
            // The pickup compares in fader units, so the two must agree at
            // more than the ends.
            for step in 0..=127 {
                let position = step as f32 / 127.0;
                let round = position_of(knob, value_at(knob, position));
                assert!((round - position).abs() < 1e-5, "{}", knob.name());
            }
        }
    }

    /// The map, the pickup and the buttons, driven by bytes rather than by
    /// calling into the middle of it.
    fn surface() -> (Midi, Params) {
        (Midi::new(Map::nano_kontrol2()), Params::default())
    }

    fn feed(midi: &mut Midi, params: &Params, bytes: &[u8]) -> Vec<Action> {
        let mut stream = Stream::default();
        let mut out = Vec::new();
        for byte in bytes {
            stream.push(*byte, &mut out);
        }
        out.into_iter()
            .filter_map(|m| midi.act(m, params, Focus::default()))
            .collect()
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
        // The other way a fader is caught, and the only way one at an end of
        // its travel can be: a knob at its rail is reached by a fader that
        // was already there, not by one sweeping through.
        let (mut midi, mut params) = surface();
        params.monitors[0].seed_brightness = 1.0;
        // Fader 1 is the seed, whose range is 0 to 1, so 127 is exactly it.
        assert!(matches!(
            feed(&mut midi, &params, &cc(0, 127))[..],
            [Action::Set(Knob::Seed, _)]
        ));
    }

    #[test]
    fn unplugging_the_surface_makes_every_fader_find_its_knob_again() {
        let (mut midi, params) = surface();
        // Sweep the seed fader down through where the seed is standing, so
        // it catches, and then take it back up: the knob follows.
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
    fn a_button_acts_when_it_goes_down_and_not_when_it_comes_up() {
        let (mut midi, params) = surface();
        // CC 48 is the mute button bound to "n".
        assert_eq!(feed(&mut midi, &params, &cc(48, 127)), [Action::NextCamera]);
        // Held: a surface that repeats while a finger is on it must not
        // walk the focus round the graph.
        assert_eq!(feed(&mut midi, &params, &cc(48, 127)), []);
        assert_eq!(feed(&mut midi, &params, &cc(48, 0)), []);
        assert_eq!(feed(&mut midi, &params, &cc(48, 127)), [Action::NextCamera]);
    }

    #[test]
    fn the_buttons_reach_the_slots_and_the_automation() {
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(32, 127)), [Action::Recall(0)]);
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
        // Nothing is bound to quit: a slipped finger on a control surface
        // must not be able to stop the instrument.
        assert!(!Map::nano_kontrol2()
            .button
            .iter()
            .any(|b| action_for_label(&b.key) == Some(Action::Quit)));
    }

    #[test]
    fn a_control_nothing_is_bound_to_does_nothing() {
        let (mut midi, params) = surface();
        assert_eq!(feed(&mut midi, &params, &cc(100, 127)), []);
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
        let mut midi = Midi::new(Map::nano_kontrol2());
        let mut stream = Stream::default();
        let mut out = Vec::new();
        // Fader 3 is saturation. At the top it is monitor 2's value, so it
        // catches there — and would not have on monitor 1.
        for byte in cc(2, 127) {
            stream.push(byte, &mut out);
        }
        let acted: Vec<Action> = out
            .iter()
            .filter_map(|m| midi.act(*m, &params, far))
            .collect();
        assert!(matches!(acted[..], [Action::Set(Knob::Saturation, _)]));
    }

    #[test]
    fn the_whole_path_runs_off_a_device_that_appears_and_goes_away() {
        // Discovery, the open, the thread, the decode and the map, driven by
        // bytes down a pipe that is not there when the instrument starts —
        // which is what hot-plug is.
        let dir = scratch("hotplug");
        let cards = dir.join("cards");
        std::fs::write(&cards, SAMPLE_CARDS).unwrap();
        let mut midi = Midi::new(Map::nano_kontrol2()).looking_in(dir.clone(), cards);
        let params = Params::default();

        // Nothing plugged in: no device node, and no waiting for one.
        assert_eq!(midi.poll(&params, Focus::default()), []);

        let node = dir.join("midiC2D0");
        let status = std::process::Command::new("mkfifo")
            .arg(&node)
            .status()
            .expect("mkfifo");
        assert!(status.success());
        // A writer has to be there before the reader's open returns, and the
        // reader is the thread the poll below spawns.
        let writing = std::thread::spawn({
            let node = node.clone();
            move || {
                let mut pipe = std::fs::OpenOptions::new().write(true).open(&node).unwrap();
                // A sweep, in running status, ending where the seed already
                // is so the pickup catches it at the last message.
                pipe.write_all(&[0xB0, 0x00, 0x7F, 0x00, 0x40, 0x00, 0x00])
                    .unwrap();
                pipe.flush().unwrap();
            }
        });

        let acted = wait_for_actions(&mut midi, &params);
        assert!(
            matches!(acted[..], [Action::Set(Knob::Seed, v)] if v.abs() < 1e-6),
            "{acted:?}"
        );
        writing.join().unwrap();

        // The writer is gone, so the read hits end of file: the surface is
        // unplugged, and the frame loop is told once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while midi.port.is_some() && Instant::now() < deadline {
            midi.poll(&params, Focus::default());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(midi.port.is_none(), "the surface never went away");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Poll until the surface has said something, or give up. The device is
    /// read on a thread, so how many frames it takes is not ours to say.
    fn wait_for_actions(midi: &mut Midi, params: &Params) -> Vec<Action> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let acted = midi.poll(params, Focus::default());
            if !acted.is_empty() {
                return acted;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Vec::new()
    }
}
