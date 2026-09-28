//! The lit half of the control surface: the Solo buttons of the focused
//! camera and the focused monitor, and the button holding any latched mode,
//! so the panel says where each hand's knobs are and what is on without
//! anyone reading the log line.
//!
//! A nanoKONTROL2 leaves the factory with **LED Mode: Internal** — a button
//! lights itself while it is held and ignores the host entirely — and the
//! only supported way to change that is Korg's KONTROL Editor, which is
//! Windows and macOS. So the mode is set here instead, over the same device
//! the surface is read from: ask the surface for the scene it is playing,
//! set the one byte that is the LED mode, hand the scene back. Nothing else
//! in it is touched and nothing is written to the surface's flash, so a
//! performer's own assignments survive it.
//!
//! That switch is one switch for the whole panel, which is why this drives
//! *every* button rather than the one it came for: external mode takes every
//! button's light at once. So a bound button is lit here while it is held —
//! exactly what internal mode did for it — and a dead button stays dark,
//! which is now what it means.
//!
//! And the mode goes back to Internal on the way out, because a surface left
//! in a mode only this program drives is a surface whose buttons have gone
//! dark for everything else on the machine.
//!
//! The conversation is a [`Panel`] the frame loop plays: told what the surface
//! said and what time it is, it writes what that calls for and returns, so a
//! reply that never comes costs a frame nothing.

use std::io::Write;
use std::time::Duration;

#[cfg(test)]
use std::fs::File;
#[cfg(test)]
use std::io::Read;
#[cfg(test)]
use std::os::fd::OwnedFd;
#[cfg(test)]
use std::os::unix::net::UnixStream;

use web_time::Instant;

/// How long the surface has to answer each step of the handshake. Generous
/// for a device that replies in milliseconds over USB, and nothing waits on
/// it: the instrument plays through the whole of this, lit or not.
const PATIENCE: Duration = Duration::from_secs(1);

/// Korg's system-exclusive manufacturer id, and the two bytes that name a
/// nanoKONTROL2 inside a universal device-inquiry reply.
const KORG: u8 = 0x42;
const NANO_KONTROL2: [u8; 2] = [0x13, 0x01];

/// A universal device inquiry, addressed to every channel — the one message
/// that can be sent before the surface's global MIDI channel is known, and
/// the only place that channel comes from. Every other message here is
/// addressed to it, and so are the lamps.
const INQUIRY: [u8; 6] = [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7];

/// A scene, decoded: 339 bytes, which ride the wire as 388.
const SCENE_BYTES: usize = 339;

/// The bytes of a scene that are the surface's own rather than one strip's,
/// and what the second of them may say.
const GLOBAL_CHANNEL: usize = 0;
const LED_MODE: usize = 2;
const INTERNAL: u8 = 0;
const EXTERNAL: u8 = 1;

/// A control change, which is what a lamp in external mode answers to: the
/// very control number the button transmits, at the button's own On and Off
/// values. Those are 127 and 0 as the factory set them — the same factory
/// layout [`crate::midi::BUTTONS`] is written against.
const CONTROL: u8 = 0xB0;
const LIT: u8 = 127;
const DARK: u8 = 0;

/// A set of lamps, one bit per control number. A `u128` because that is
/// exactly the 128 numbers a control change can name, so a mask cannot
/// address a lamp that could not exist.
pub(crate) type Lamplight = u128;

/// The one lamp of control number `cc`. Safe for every bound control: each
/// is on the panel, which stops at 71.
pub(crate) fn lamp(cc: u8) -> Lamplight {
    1 << cc
}

type Scene = [u8; SCENE_BYTES];

enum Stage {
    Asking {
        by: Instant,
    },
    Reading {
        channel: u8,
        by: Instant,
    },
    Taking {
        channel: u8,
        by: Instant,
        restore: Scene,
    },
    Lighting {
        channel: u8,
        restore: Option<Scene>,
    },
    Done,
}

pub(crate) struct Panel<W> {
    out: W,
    buttons: Lamplight,
    /// The lamps the frame loop last asked for, which it may have asked for
    /// while the handshake was still running.
    want: Lamplight,
    /// What the panel was last *asked* for, which is the only account of it
    /// there is — a surface cannot be asked what is lit. Set before the write
    /// rather than after, because a `write_all` that failed part way through
    /// may have lit some of them, and the pessimistic account is the one that
    /// gets them turned off.
    lit: Lamplight,
    stage: Stage,
}

impl<W: Write> Panel<W> {
    /// Ask the surface on `out` what it is. `buttons` is every control number
    /// a button answers to — the only numbers that will ever be written, so
    /// a lamp mask cannot reach a control that is not one.
    pub(crate) fn new(mut out: W, buttons: Lamplight, now: Instant) -> Panel<W> {
        let stage = match write(&mut out, &INQUIRY) {
            Ok(()) => Stage::Asking { by: now + PATIENCE },
            Err(why) => {
                log::warn!("surface: {why}; its buttons light themselves, as before");
                Stage::Done
            }
        };
        Panel {
            out,
            buttons,
            want: 0,
            lit: 0,
            stage,
        }
    }

    pub(crate) fn done(&self) -> bool {
        matches!(self.stage, Stage::Done)
    }

    pub(crate) fn show(&mut self, want: Lamplight) {
        self.want = want;
        if let Stage::Lighting { channel, .. } = self.stage {
            if let Err(why) = self.light(channel, want) {
                self.lost(why);
            }
        }
    }

    /// A system-exclusive frame from the surface. A device that does not
    /// answer the inquiry as a nanoKONTROL2 is never written to again: what a
    /// control change does to it is not knowable from here.
    pub(crate) fn sysex(&mut self, frame: &[u8], now: Instant) {
        match self.stage {
            Stage::Asking { .. } => {
                if let Some(channel) = channel_of(frame) {
                    match write(&mut self.out, &request(channel)) {
                        Ok(()) => {
                            self.stage = Stage::Reading {
                                channel,
                                by: now + PATIENCE,
                            }
                        }
                        Err(why) => self.light_up(channel, None, Err(why)),
                    }
                }
            }
            Stage::Reading { channel, .. } => {
                if let Some(scene) = scene_in(channel, frame) {
                    self.take(channel, scene, now);
                }
            }
            Stage::Taking {
                channel, restore, ..
            } => match ack(channel, frame) {
                Some(true) => self.light_up(channel, Some(restore), Ok(())),
                // A refusal is the one answer that says the surface did *not*
                // take the scene, so there is nothing to undo; a timeout is
                // not this, and keeps the undo.
                Some(false) => {
                    self.light_up(channel, None, Err("the surface refused the scene".into()))
                }
                None => {}
            },
            Stage::Lighting { .. } | Stage::Done => {}
        }
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        match self.stage {
            Stage::Asking { by } if now >= by => {
                log::warn!(
                    "surface: no answer to a device inquiry; its buttons light themselves, as before"
                );
                self.stage = Stage::Done;
            }
            Stage::Reading { channel, by } if now >= by => self.light_up(
                channel,
                None,
                Err("no answer to a scene dump request".into()),
            ),
            Stage::Taking {
                channel,
                by,
                restore,
            } if now >= by => {
                self.light_up(channel, Some(restore), Err("no answer to the scene".into()))
            }
            _ => {}
        }
    }

    /// Dark, and the mode put back: the surface outlives the instrument.
    pub(crate) fn quit(&mut self) {
        match self.stage {
            Stage::Reading { channel, .. } => self.way_out(channel, None),
            Stage::Taking {
                channel, restore, ..
            } => self.way_out(channel, Some(restore)),
            Stage::Lighting { channel, restore } => self.way_out(channel, restore),
            Stage::Asking { .. } | Stage::Done => self.stage = Stage::Done,
        }
    }

    /// Put the surface's lights under the host: its own scene handed back
    /// with the one byte that is the LED mode set. Read-modify-write rather
    /// than a scene of this program's own, because the rest of that scene is
    /// the performer's: every control number, every curve.
    fn take(&mut self, channel: u8, mut scene: Scene, now: Instant) {
        // The one offset the surface has also said another way. A scene read
        // where this does not think it is fails here rather than going back
        // with a performer's assignments shifted along it.
        if scene[GLOBAL_CHANNEL] != channel {
            let says = scene[GLOBAL_CHANNEL];
            return self.light_up(
                channel,
                None,
                Err(format!(
                    "its scene reads channel {says} where it answered on {channel}"
                )),
            );
        }
        // Already external is a KONTROL Editor's doing, or this program's
        // own, killed before it put the mode back: no scene to write.
        let taking = scene[LED_MODE] != EXTERNAL;
        if taking {
            scene[LED_MODE] = EXTERNAL;
            if let Err(why) = write(&mut self.out, &dump(channel, &scene)) {
                return self.light_up(channel, None, Err(why));
            }
        }
        // Internal, not whatever was found: found-external is the state a
        // killed run leaves behind. The undo is kept from the moment the
        // scene is on the wire, since the surface applies it as it arrives.
        // Never followed by a write request, which would commit the scene to
        // the surface's flash.
        scene[LED_MODE] = INTERNAL;
        match taking {
            true => {
                self.stage = Stage::Taking {
                    channel,
                    by: now + PATIENCE,
                    restore: scene,
                }
            }
            false => self.light_up(channel, Some(scene), Ok(())),
        }
    }

    /// Light, however the handshake went: a surface that will not take the
    /// mode is still written to, where the lamps do nothing.
    fn light_up(&mut self, channel: u8, restore: Option<Scene>, took: Result<(), String>) {
        match took {
            Ok(()) => log::info!(
                "surface: its lights are the instrument's — the Solo buttons of the \
                 focused camera and monitor, any latched mode, and every other \
                 button while it is held"
            ),
            Err(why) => log::warn!(
                "surface: {why}; its LED Mode is still Internal, so its buttons light \
                 themselves and only their own presses. The instrument plays as before."
            ),
        }
        self.stage = Stage::Lighting { channel, restore };
        // Blanked first, to nothing: whatever drove these lamps last left
        // them somewhere unknown, and a lamp believed lit is never written.
        self.lit = self.buttons;
        if let Err(why) = self
            .light(channel, 0)
            .and_then(|()| self.light(channel, self.want))
        {
            self.lost(why);
        }
    }

    fn lost(&mut self, why: String) {
        log::warn!("surface: {why}");
        self.quit();
    }

    /// Both are attempted whatever the other did, and each failure says what
    /// the performer is left holding: nothing downstream of the last write
    /// is left to notice it.
    fn way_out(&mut self, channel: u8, restore: Option<Scene>) {
        self.stage = Stage::Done;
        if let Err(why) = self.light(channel, 0) {
            log::warn!("surface: {why}; the lamps it is holding are left lit");
        }
        if let Some(scene) = restore {
            if let Err(why) = write(&mut self.out, &dump(channel, &scene)) {
                log::warn!(
                    "surface: {why}; its LED Mode is left External, so every button on it \
                     is dark for everything else on the machine until it is replugged"
                );
            }
        }
    }

    /// Make the panel show exactly `want` and nothing else.
    fn light(&mut self, channel: u8, want: Lamplight) -> Result<(), String> {
        let want = want & self.buttons;
        let change = want ^ self.lit;
        if change == 0 {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(3 * change.count_ones() as usize);
        let message = |cc, value| [CONTROL | channel, cc, value];
        // Out before on, and both in one `write_all`: a lamp moving from one
        // button to the next must not spend even one message with both
        // alight, which is a panel claiming the knobs are in two places.
        bytes.extend(controls(change & !want).flat_map(|cc| message(cc, DARK)));
        bytes.extend(controls(change & want).flat_map(|cc| message(cc, LIT)));
        self.lit = want;
        write(&mut self.out, &bytes)
    }
}

fn write(out: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    out.write_all(bytes)
        .map_err(|e| format!("its lights stopped taking writes ({e})"))
}

/// The control numbers a mask names, low to high.
fn controls(mask: Lamplight) -> impl Iterator<Item = u8> {
    (0..128u8).filter(move |cc| mask >> cc & 1 == 1)
}

/// The global MIDI channel out of a device-inquiry reply, if it came from a
/// nanoKONTROL2: `F0 7E 0g 06 02 42 13 01 …`.
fn channel_of(frame: &[u8]) -> Option<u8> {
    (frame.len() >= 8
        && frame[..2] == [0xF0, 0x7E]
        && frame[3..6] == [0x06, 0x02, KORG]
        && frame[6..8] == NANO_KONTROL2)
        // Masked here so it stays a nibble: every message below is addressed
        // by or-ing it into a status byte, which a wider value would turn
        // into a different message entirely.
        .then(|| frame[2] & 0x0F)
}

/// The head of a scene message addressed to the surface on `channel`, in
/// either direction. Byte 2's low nibble is where a Korg message carries the
/// channel; the tail `40` is the function, "here is a scene".
fn head(channel: u8) -> [u8; 13] {
    [
        0xF0,
        KORG,
        0x40 | channel,
        0x00,
        0x01,
        0x13,
        0x00,
        0x7F,
        0x7F,
        0x02,
        0x03,
        0x05,
        0x40,
    ]
}

/// "Send me the scene you are playing", to the surface on `channel`.
fn request(channel: u8) -> [u8; 11] {
    [
        0xF0,
        KORG,
        0x40 | channel,
        0x00,
        0x01,
        0x13,
        0x00,
        0x1F,
        0x10,
        0x00,
        0xF7,
    ]
}

/// The scene inside a dump from the surface on `channel`, decoded. The length
/// is settled before anything is indexed, so a dump that is not one is
/// refused rather than read at whatever offsets it happens to have.
fn scene_in(channel: u8, frame: &[u8]) -> Option<[u8; SCENE_BYTES]> {
    let head = head(channel);
    if frame.len() != head.len() + packed_len(SCENE_BYTES) + 1
        || frame[..head.len()] != head
        || frame[frame.len() - 1] != 0xF7
    {
        return None;
    }
    unpack(&frame[head.len()..frame.len() - 1]).try_into().ok()
}

/// The same message the other way: a scene, on its way back to the surface.
fn dump(channel: u8, scene: &[u8; SCENE_BYTES]) -> Vec<u8> {
    let mut out = head(channel).to_vec();
    out.extend(pack(scene));
    out.push(0xF7);
    out
}

/// Whether a frame is the surface's answer to a scene it was handed: `true`
/// for the acknowledgement, `false` for the refusal, `None` for anything
/// else. The two differ in one byte, which is why they are read together.
fn ack(channel: u8, frame: &[u8]) -> Option<bool> {
    let head = [0xF0, KORG, 0x40 | channel, 0x00, 0x01, 0x13, 0x00, 0x5F];
    (frame.len() == 11 && frame[..8] == head && frame[9..] == [0x00, 0xF7]).then_some(())?;
    match frame[8] {
        0x23 => Some(true),
        0x24 => Some(false),
        _ => None,
    }
}

/// How many wire bytes `len` bytes of scene take. Seven bytes ride as eight,
/// and a part group takes one byte more than it has: 339 becomes 388.
fn packed_len(len: usize) -> usize {
    len + len.div_ceil(7)
}

/// Korg's seven-bit packing: each group of seven data bytes goes out as a
/// byte of their top bits, least significant first, and then the seven bodies
/// with those bits stripped.
fn pack(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packed_len(data.len()));
    for group in data.chunks(7) {
        out.push(
            group
                .iter()
                .enumerate()
                .fold(0, |top, (i, byte)| top | ((byte >> 7) << i)),
        );
        out.extend(group.iter().map(|byte| byte & 0x7F));
    }
    out
}

/// The inverse. A trailing group short of its seven bodies is the tail of a
/// scene, which is three bytes long.
fn unpack(wire: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(wire.len());
    for group in wire.chunks(8) {
        let (top, bodies) = group.split_first().expect("a chunk is never empty");
        out.extend(
            bodies
                .iter()
                .enumerate()
                .map(|(i, byte)| (byte & 0x7F) | (((top >> i) & 1) << 7)),
        );
    }
    out
}

/// A socket pair standing in for the device node: what the instrument writes
/// really leaves a file descriptor and is read back at the other end, the
/// boundary a lamp is observable at. Here rather than in this module's own
/// tests because [`crate::midi`] and [`crate::app`] test through it too.
#[cfg(test)]
pub(crate) fn over_a_socket() -> (File, Wire) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let wire = Wire {
        wire: ours,
        pending: Vec::new(),
        panel: 0,
    };
    (File::from(OwnedFd::from(theirs)), wire)
}

/// The device's end of [`over_a_socket`].
#[cfg(test)]
pub(crate) struct Wire {
    wire: UnixStream,
    /// Bytes read but not yet a whole message, for [`Wire::panel_becomes`].
    pending: Vec<u8>,
    /// What the control changes read so far have made of the panel.
    panel: Lamplight,
}

#[cfg(test)]
impl Wire {
    /// The next `n` bytes the instrument wrote. Out of `pending` first: a
    /// read that went straight to the socket would skip whatever
    /// [`Wire::panel_becomes`] had read but not yet made a message of, and
    /// then compare every byte after it against the wrong one.
    pub(crate) fn read(&mut self, n: usize) -> Vec<u8> {
        let held = self.pending.len().min(n);
        let mut got: Vec<u8> = self.pending.drain(..held).collect();
        got.resize(n, 0);
        self.wire
            .read_exact(&mut got[held..])
            .unwrap_or_else(|e| panic!("the instrument wrote nothing: {e}"));
        got
    }

    /// Everything left, once the instrument has let go of the lights.
    pub(crate) fn rest(&mut self) -> Vec<u8> {
        let mut rest = std::mem::take(&mut self.pending);
        self.wire.read_to_end(&mut rest).unwrap();
        rest
    }

    /// Answer the handshake as a nanoKONTROL2 on `channel` whose LED Mode is
    /// already External — the one path that writes no scene, so what follows
    /// on the wire is the blanking pass and then lamps and nothing else.
    pub(crate) fn handshake(&mut self, channel: u8, mut say: impl FnMut(Vec<u8>)) {
        assert_eq!(self.read(INQUIRY.len()), INQUIRY);
        say(inquiry_reply(channel));
        assert_eq!(self.read(request(channel).len()), request(channel));
        say(dumped(channel, EXTERNAL));
    }

    /// Read until the panel the instrument has put on the wire is exactly
    /// `want`, and say whether it got there.
    ///
    /// Everything past the handshake is a control change and nothing else,
    /// which is what makes three bytes a message here — asserted rather than
    /// assumed, because a stream that had slipped by a byte would otherwise
    /// be read as lamps at control numbers no button has.
    pub(crate) fn panel_becomes(&mut self, want: Lamplight) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 64];
        while self.panel != want && Instant::now() < deadline {
            match self.wire.read(&mut buf) {
                // The one error that is not the wire going: a signal landed
                // in the wait, and what it interrupted is still coming.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Ok(0) | Err(_) => break,
                Ok(read) => self.pending.extend(&buf[..read]),
            }
            while self.pending.len() >= 3 {
                let message: Vec<u8> = self.pending.drain(..3).collect();
                assert_eq!(
                    message[0] & 0xF0,
                    CONTROL,
                    "not a control change: {message:02X?}"
                );
                match message[2] {
                    DARK => self.panel &= !lamp(message[1]),
                    _ => self.panel |= lamp(message[1]),
                }
            }
        }
        self.panel == want
    }
}

/// A scene as the surface would send it: `led` mode, on `channel`.
#[cfg(test)]
pub(crate) fn scene_of(channel: u8, led: u8) -> [u8; SCENE_BYTES] {
    let mut scene = [0u8; SCENE_BYTES];
    scene[GLOBAL_CHANNEL] = channel;
    scene[LED_MODE] = led;
    // Something in the tail with its top bit set, so a dump that lost the
    // packing on the way through cannot pass.
    scene[SCENE_BYTES - 1] = 0xFF;
    scene
}

#[cfg(test)]
pub(crate) fn dumped(channel: u8, led: u8) -> Vec<u8> {
    dump(channel, &scene_of(channel, led))
}

/// The surface naming itself, on `channel`.
#[cfg(test)]
pub(crate) fn inquiry_reply(channel: u8) -> Vec<u8> {
    let mut reply = vec![0xF0, 0x7E, channel, 0x06, 0x02, KORG];
    reply.extend(NANO_KONTROL2);
    reply.extend([0x00, 0x00, 0, 0, 0, 0, 0xF7]);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eight control numbers these tests let the instrument light. Which
    /// buttons those are is [`crate::midi`]'s business; nothing here knows.
    const BUTTONS: Lamplight = 0xFF << 32;

    #[test]
    fn every_byte_taken_on_faith_is_written_out_again_from_korg_s_own_table() {
        // The one test here whose expectations come from outside this file.
        // Everything else builds its own out of these same names — a device
        // stand-in that packs a scene with `dump` and reads it back with
        // `scene_in` agrees with any offset at all, including a wrong one —
        // so this is what a transcription error fails against rather than
        // being carried through by. Every number below is off Korg's
        // nanoKONTROL2 MIDI implementation, and the ones a nanoKONTROL2 on
        // this machine also said out loud are noted where it said them.
        //
        // Global data, the head of a scene: byte 0 the global MIDI channel,
        // byte 1 the control mode, byte 2 the LED mode — 0 Internal, 1
        // External. The hardware agreed: a factory scene decoded to channel
        // 0, control mode 0, LED mode 0, and the inquiry named channel 0.
        assert_eq!(GLOBAL_CHANNEL, 0);
        assert_eq!(LED_MODE, 2);
        assert_eq!(INTERNAL, 0);
        assert_eq!(EXTERNAL, 1);
        // A scene is 339 bytes: 3 global, 8 groups of 31, and 88 of common.
        assert_eq!(SCENE_BYTES, 339);
        // Korg's manufacturer id, and the two bytes naming the nanoKONTROL2
        // family inside an inquiry reply — the hardware answered `42 13 01`.
        assert_eq!(KORG, 0x42);
        assert_eq!(NANO_KONTROL2, [0x13, 0x01]);
        // MIDI 1.0's universal non-realtime device inquiry, `7F` being every
        // channel: F0 7E <ch> 06 01 F7.
        assert_eq!(INQUIRY, [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7]);
        // A control change, and the Solo row's On and Off values as the
        // factory sets them — the hardware's own scene read `off=0 on=127`.
        assert_eq!(CONTROL, 0xB0);
        assert_eq!(LIT, 127);
        assert_eq!(DARK, 0);
        // The messages, by hand, on global channel 3 — so that the nibble
        // byte 2 carries is not the zero it would be on channel 0, and an
        // address written to the wrong byte fails here.
        assert_eq!(
            request(3),
            [0xF0, 0x42, 0x43, 0x00, 0x01, 0x13, 0x00, 0x1F, 0x10, 0x00, 0xF7],
            "the current scene data dump request"
        );
        assert_eq!(
            head(3),
            [0xF0, 0x42, 0x43, 0x00, 0x01, 0x13, 0x00, 0x7F, 0x7F, 0x02, 0x03, 0x05, 0x40],
            "the head of a current scene data dump"
        );
        // And the surface's two answers to a scene, which differ in byte 8.
        let mut answer = [
            0xF0, 0x42, 0x43, 0x00, 0x01, 0x13, 0x00, 0x5F, 0x23, 0x00, 0xF7,
        ];
        assert_eq!(ack(3, &answer), Some(true), "data load completed");
        answer[8] = 0x24;
        assert_eq!(ack(3, &answer), Some(false), "data load error");
    }

    #[test]
    fn the_flip_lands_on_the_third_byte_of_the_scene_and_moves_nothing_else() {
        // The scene that goes back, read at wire offsets worked out by hand
        // rather than compared against what `dump` would have made of it:
        // thirteen bytes of head, then Korg's packing, whose first group is a
        // byte of top bits and then seven bodies. So decoded byte `i` of that
        // first group is wire byte `13 + 1 + i`, and the LED mode is at 16.
        //
        // The scene coming *in* is laid out by literal index for the same
        // reason — built with `scene_of` it would put the mode wherever
        // `LED_MODE` said, and agree with itself.
        let mut scene = [0u8; SCENE_BYTES];
        scene[0] = 3; // global channel, which the inquiry names too
        scene[1] = 0; // control mode: CC
        scene[2] = 0; // LED mode: Internal, so there is a flip to make
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.say(inquiry_reply(3));
        assert_eq!(device.read(11), request(3));
        device.say(dump(3, &scene));

        let wire = device.read(402);
        assert_eq!(wire[13], 0x00, "no byte of the first group has bit 7 set");
        assert_eq!(wire[14], 3, "scene byte 0 is the global MIDI channel");
        assert_eq!(wire[15], 0, "scene byte 1 is the control mode, untouched");
        assert_eq!(wire[16], 1, "scene byte 2 is the LED mode, taken External");
        assert!(
            wire[17..21].iter().all(|byte| *byte == 0),
            "the bytes after it are the performer's and are not this file's to move"
        );
    }

    #[test]
    fn seven_bit_packing_is_korg_s_own() {
        // Worked out by hand rather than round-tripped, because a packer and
        // its own inverse agree on any bit order at all — including the
        // reversed one, which is the mistake this is here to catch.
        // 0x80 sets bit 0 of the top byte, 0xFF sets bit 2; the bodies keep
        // their low seven.
        assert_eq!(pack(&[0x80, 0x01, 0xFF]), [0b101, 0x00, 0x01, 0x7F]);
        assert_eq!(unpack(&[0b101, 0x00, 0x01, 0x7F]), [0x80, 0x01, 0xFF]);
        // Seven is the group, so the eighth byte starts another.
        let eight: Vec<u8> = (0..8).map(|i| 0x80 | i).collect();
        assert_eq!(
            pack(&eight),
            [0x7F, 0, 1, 2, 3, 4, 5, 6, 0x01, 7],
            "the eighth byte belongs to a group of its own"
        );
    }

    #[test]
    fn a_scene_is_three_hundred_and_thirty_nine_bytes_in_three_hundred_and_eighty_eight() {
        // Korg's own arithmetic: 339 = 7*48+3 -> 8*48+4 = 388. An off-by-one
        // here is a scene message the surface rejects whole.
        assert_eq!(packed_len(SCENE_BYTES), 388);
        let scene: Vec<u8> = (0..SCENE_BYTES).map(|i| i as u8).collect();
        assert_eq!(pack(&scene).len(), 388);
        assert!(pack(&scene).iter().all(|b| *b < 0x80), "a byte with bit 7");
        assert_eq!(unpack(&pack(&scene)), scene);
        // And a whole dump message is 402: thirteen of head, 388, and F7.
        assert_eq!(dump(0, &scene.try_into().unwrap()).len(), 402);
    }

    #[test]
    fn only_a_nano_kontrol2_answers_the_inquiry() {
        // The reply as Korg documents it, on global channel 5.
        let reply = |maker, family: [u8; 2]| {
            let mut frame = vec![0xF0, 0x7E, 0x05, 0x06, 0x02, maker];
            frame.extend(family);
            frame.extend([0x00, 0x00, 0, 0, 0, 0, 0xF7]);
            frame
        };
        assert_eq!(channel_of(&reply(KORG, NANO_KONTROL2)), Some(5));
        // Some other Korg, and some other maker's device that happens to have
        // answered: neither is a surface whose scene may be rewritten.
        assert_eq!(channel_of(&reply(KORG, [0x14, 0x01])), None);
        assert_eq!(channel_of(&reply(0x41, NANO_KONTROL2)), None);
        // A request is not a reply — they differ in one byte, byte 4 — so a
        // device that echoes what it is sent does not identify itself.
        assert_eq!(channel_of(&INQUIRY), None);
        assert_eq!(channel_of(&[]), None);
    }

    fn answered(channel: u8, func: u8) -> Vec<u8> {
        vec![
            0xF0,
            KORG,
            0x40 | channel,
            0x00,
            0x01,
            0x13,
            0x00,
            0x5F,
            func,
            0x00,
            0xF7,
        ]
    }

    #[test]
    fn a_scene_is_read_back_out_of_the_dump_that_carried_it() {
        let got = scene_in(0, &dumped(0, EXTERNAL)).expect("a well-formed dump");
        assert_eq!(got[LED_MODE], EXTERNAL);
        assert_eq!(got[SCENE_BYTES - 1], 0xFF);
        // Addressed to another channel, so it is another surface's scene.
        assert_eq!(scene_in(1, &dumped(0, EXTERNAL)), None);
        assert_eq!(scene_in(0, &dumped(1, EXTERNAL)), None);
        // Truncated, and lengthened: a dump is one length exactly, and a
        // short one decoded anyway would put the LED mode wherever the
        // shortfall left it.
        let mut short = dumped(0, EXTERNAL);
        short.pop();
        assert_eq!(scene_in(0, &short), None);
        let mut long = dumped(0, EXTERNAL);
        long.push(0xF7);
        assert_eq!(scene_in(0, &long), None);
        // And the surface's other messages are not scenes.
        assert_eq!(scene_in(0, &request(0)), None);
    }

    #[test]
    fn the_acknowledgement_and_the_refusal_are_told_apart() {
        assert_eq!(ack(0, &answered(0, 0x23)), Some(true));
        assert_eq!(ack(0, &answered(0, 0x24)), Some(false));
        // The write-completed replies, which answer a message this never
        // sends — the one that commits a scene to the surface's flash.
        assert_eq!(ack(0, &answered(0, 0x21)), None);
        assert_eq!(ack(3, &answered(3, 0x23)), Some(true));
        assert_eq!(ack(3, &answered(0, 0x23)), None);
    }

    /// One surface's lights, the device end of the wire they are on, and the
    /// clock the frame loop would hand them.
    struct Device {
        wire: Wire,
        /// `None` once the instrument has let go, which is the exit and the
        /// unplug both.
        panel: Option<Panel<File>>,
        now: Instant,
    }

    impl Device {
        fn new() -> Device {
            let (file, wire) = over_a_socket();
            let now = Instant::now();
            Device {
                wire,
                panel: Some(Panel::new(file, BUTTONS, now)),
                now,
            }
        }

        /// The next `n` bytes the instrument wrote.
        fn read(&mut self, n: usize) -> Vec<u8> {
            self.wire.read(n)
        }

        /// Let go, and assert that `tail` is the whole of what follows. Every
        /// test ends here rather than on a `read`, so a write the instrument
        /// had no business making cannot hide past the end of the last
        /// assertion — a flash commit, a second scene, a stray lamp.
        fn done(mut self, tail: &[u8]) {
            self.plugged().quit();
            self.panel = None;
            assert_eq!(self.wire.rest(), tail, "wrote more than it should have");
        }

        fn say(&mut self, frame: Vec<u8>) {
            let now = self.now;
            self.plugged().sysex(&frame, now);
        }

        fn show(&mut self, want: Lamplight) {
            self.plugged().show(want);
        }

        fn wait_out(&mut self) {
            self.now += PATIENCE;
            let now = self.now;
            self.plugged().tick(now);
        }

        fn plugged(&mut self) -> &mut Panel<File> {
            self.panel.as_mut().expect("still plugged in")
        }

        /// Answer the inquiry, and the dump request with a scene in `led`
        /// mode — taking the flipped scene back when there was one to flip.
        fn handshake(&mut self, channel: u8, led: u8) {
            assert_eq!(self.read(INQUIRY.len()), INQUIRY);
            self.say(inquiry_reply(channel));
            assert_eq!(self.read(11), request(channel));
            self.say(dumped(channel, led));
            if led != EXTERNAL {
                // The whole scene, unchanged but for the one byte: the rest
                // of it is the performer's assignments, and a blind overwrite
                // would take them.
                assert_eq!(
                    self.read(402),
                    dump(channel, &scene_of(channel, EXTERNAL)),
                    "the scene was not handed back as it came"
                );
                self.say(answered(channel, 0x23));
            }
        }

        /// The blanking pass every connect opens with: every bound button,
        /// put out, before any lamp is lit.
        fn blanked(&mut self, channel: u8) {
            let want: Vec<u8> = controls(BUTTONS)
                .flat_map(|cc| [CONTROL | channel, cc, DARK])
                .collect();
            assert_eq!(self.read(want.len()), want, "the panel was not blanked");
        }
    }

    #[test]
    fn an_internal_surface_is_handed_back_its_own_scene_with_the_mode_set() {
        let mut device = Device::new();
        device.handshake(0, INTERNAL);
        // What is *not* on the wire is half the point: the message that
        // commits a scene to the surface's flash is never sent, so what
        // follows the scene is the blanking pass and then a lamp.
        device.blanked(0);
        device.show(lamp(33));
        assert_eq!(device.read(3), [CONTROL, 33, LIT]);
    }

    #[test]
    fn the_mode_goes_back_to_internal_on_the_way_out() {
        // A surface left in external mode has no lights at all for whatever
        // is played next, so the scene that undoes the flip goes out on the
        // way through the door.
        let mut device = Device::new();
        device.handshake(0, INTERNAL);
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        device.done(&[vec![CONTROL, 32, DARK], dump(0, &scene_of(0, INTERNAL))].concat());
    }

    #[test]
    fn a_surface_already_in_external_mode_is_not_written_a_scene_to_get_there() {
        // Nothing to change, so nothing goes out to change it — the
        // handshake asserts that by reading the blanking pass next.
        let mut device = Device::new();
        device.handshake(0, EXTERNAL);
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        // But the mode still goes back to Internal on the way out. Found
        // external is exactly what a killed run leaves behind, and leaving it
        // there keeps the surface dark for everything else on the machine.
        device.done(&[vec![CONTROL, 32, DARK], dump(0, &scene_of(0, INTERNAL))].concat());
    }

    #[test]
    fn the_panel_is_blanked_before_the_first_lamp_rather_than_assumed_dark() {
        // The lamps this program left burning when it was killed are still
        // burning: external mode lives in the device's RAM, so a restart
        // finds the mode already set, writes no scene, and would otherwise
        // take a dark panel on faith and light a second lamp beside a stale
        // one — one more with every killing, up to the whole row.
        let mut device = Device::new();
        device.handshake(0, EXTERNAL);
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
    }

    #[test]
    fn one_lamp_moves_out_before_the_next_comes_on() {
        let mut device = Device::new();
        device.handshake(0, EXTERNAL);
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        // Moving the focus: out, then on, in one write — a panel with two
        // lamps lit says the knobs are in two places.
        device.show(lamp(35));
        assert_eq!(device.read(6), [CONTROL, 32, DARK, CONTROL, 35, LIT]);
        // The same panel again, sixty times a second, is nothing on the wire.
        for _ in 0..60 {
            device.show(lamp(35));
        }
        device.show(0);
        assert_eq!(device.read(3), [CONTROL, 35, DARK]);
    }

    #[test]
    fn every_button_the_map_binds_is_lit_while_it_is_held() {
        // The LED mode is one switch for the whole panel, so taking it costs
        // every other button its light unless the instrument gives it back.
        // Two at once, because a hand can hold one button and press another.
        let mut device = Device::new();
        device.handshake(0, EXTERNAL);
        device.blanked(0);
        device.show(lamp(32) | lamp(36));
        assert_eq!(device.read(6), [CONTROL, 32, LIT, CONTROL, 36, LIT]);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 36, DARK]);
    }

    #[test]
    fn a_lamp_no_button_of_the_map_answers_to_is_never_written() {
        // The mask the surface was opened with is the whole of what it may
        // write, so nothing can put a control change on a fader's number.
        let mut device = Device::new();
        device.handshake(0, EXTERNAL);
        device.blanked(0);
        device.show(lamp(7) | lamp(90) | lamp(33));
        assert_eq!(device.read(3), [CONTROL, 33, LIT]);
        device.done(&[vec![CONTROL, 33, DARK], dump(0, &scene_of(0, INTERNAL))].concat());
    }

    #[test]
    fn a_surface_that_stops_answering_is_left_in_the_mode_it_was_found_in() {
        // The acknowledgement never comes. The surface has the scene by then
        // — it applies one as it arrives — so the undo has to be recorded
        // before the answer, not after it, or the panel stays dark for
        // everything else on the machine until somebody pulls the cable.
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.say(inquiry_reply(0));
        assert_eq!(device.read(11), request(0));
        device.say(dumped(0, INTERNAL));
        assert_eq!(device.read(402), dump(0, &scene_of(0, EXTERNAL)));
        // Nothing said back. The handshake gives up, the lamps are written
        // anyway, and the mode still goes home.
        device.wait_out();
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        device.done(&[vec![CONTROL, 32, DARK], dump(0, &scene_of(0, INTERNAL))].concat());
    }

    #[test]
    fn the_lamps_are_addressed_to_the_channel_the_surface_answered_on() {
        // A surface set to MIDI channel 4 sends and receives on it, and its
        // scene messages carry the same nibble.
        let mut device = Device::new();
        device.handshake(3, EXTERNAL);
        device.blanked(3);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL | 3, 32, LIT]);
    }

    #[test]
    fn a_scene_that_does_not_agree_with_the_surface_is_not_written_back() {
        // The one offset that can be checked against something the surface
        // said another way. A scene whose channel byte is not where this
        // thinks it is, is a scene whose LED mode is not either — and writing
        // that back would move a performer's assignments along with it.
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.say(inquiry_reply(0));
        assert_eq!(device.read(11), request(0));
        // Answered on channel 1, but the scene says channel 4 — written at
        // the literal byte 0 rather than through `GLOBAL_CHANNEL`, so the
        // guard is read at the offset Korg names and not at its own.
        let mut disagrees = [0u8; SCENE_BYTES];
        disagrees[0] = 3;
        device.say(dump(0, &disagrees));
        // No scene back — straight to the panel, which is still blanked and
        // still lit, because the lamps are harmless on an internal surface.
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        device.done(&[CONTROL, 32, DARK]);
    }

    #[test]
    fn a_device_that_answers_as_something_else_is_written_to_no_further() {
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        // Some other maker's device, answering the one message every MIDI
        // device answers. Its buttons are not this instrument's to drive:
        // what a control change does to it is not knowable from here.
        let mut reply = vec![0xF0, 0x7E, 0x00, 0x06, 0x02, 0x41];
        reply.extend([0x13, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0xF7]);
        device.say(reply);
        device.show(lamp(32));
        // Nothing at all after the inquiry: not the scene request, not a
        // blanking pass, not a lamp.
        device.done(&[]);
    }

    #[test]
    fn a_focus_moved_during_the_handshake_is_the_one_that_lights() {
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        // The frame loop does not wait for any of this: it says what it wants
        // every redraw, from the first one.
        device.show(lamp(32));
        device.show(lamp(37));
        device.say(inquiry_reply(0));
        assert_eq!(device.read(11), request(0));
        device.say(dumped(0, EXTERNAL));
        // The panel is blanked even so, and the lamp that survives the
        // handshake is the last one asked for, not the first.
        device.blanked(0);
        assert_eq!(device.read(3), [CONTROL, 37, LIT]);
    }

    #[test]
    fn a_surface_that_will_not_answer_is_still_played() {
        // No reply to anything. The panel gives up at its deadline and writes
        // no lamp, because a device that did not identify itself is not one
        // to write to.
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.show(lamp(32));
        device.wait_out();
        assert!(device.plugged().done());
        device.show(lamp(33));
        device.done(&[]);
    }

    #[test]
    fn a_refused_scene_still_leaves_the_instrument_lighting_what_it_can() {
        // The surface took the dump request but would not take the scene. Its
        // LEDs are its own — but writing them is what a nanoKONTROL2 in
        // external mode wants, and this cannot tell the two apart from here,
        // so the lamps still go out on the wire and do nothing.
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.say(inquiry_reply(0));
        assert_eq!(device.read(11), request(0));
        device.say(dumped(0, INTERNAL));
        assert_eq!(device.read(402).len(), 402);
        device.say(answered(0, 0x24));
        device.blanked(0);
        device.show(lamp(32));
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        // And nothing is put back, because nothing was taken.
        device.done(&[CONTROL, 32, DARK]);
    }

    #[test]
    fn an_answer_is_taken_until_its_time_is_up_and_not_after() {
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.now += PATIENCE - Duration::from_millis(1);
        let now = device.now;
        device.plugged().tick(now);
        device.show(lamp(32));
        device.say(inquiry_reply(0));
        assert_eq!(device.read(11), request(0));
        device.wait_out();
        device.say(dumped(0, INTERNAL));
        device.blanked(0);
        assert_eq!(device.read(3), [CONTROL, 32, LIT]);
        device.done(&[CONTROL, 32, DARK]);
    }

    #[test]
    fn an_exit_while_the_scene_is_unanswered_still_puts_the_mode_back() {
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.say(inquiry_reply(0));
        assert_eq!(device.read(11), request(0));
        device.say(dumped(0, INTERNAL));
        assert_eq!(device.read(402), dump(0, &scene_of(0, EXTERNAL)));
        device.done(&dump(0, &scene_of(0, INTERNAL)));
    }
}
