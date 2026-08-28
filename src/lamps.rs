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
//! button's light at once. So a button the map binds is lit here while it is
//! held — exactly what internal mode did for it — and a button the map binds
//! nothing to stays dark, which is now what it means.
//!
//! And the mode goes back to Internal on the way out, because a surface left
//! in a mode only this program drives is a surface whose buttons have gone
//! dark for everything else on the machine.
//!
//! All of it runs on a thread of its own. The handshake waits on replies that
//! may never come and a MIDI write blocks when the wire is full, and neither
//! may happen inside a frame; the frame loop's whole part is to say which
//! lamps it wants, down a channel. The one exception is the unplug, which
//! joins the thread it is ending from inside the frame loop — bounded,
//! because a wire that has gone fails a write rather than waiting on one.

use std::fs::File;
use std::io::Write;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use web_time::Instant;

/// How long the surface has to answer each step of the handshake. Generous
/// for a device that replies in milliseconds over USB, and it only ever costs
/// a thread nobody is waiting on: the instrument plays through the whole of
/// this, lit or not.
#[cfg(not(test))]
const PATIENCE: Duration = Duration::from_secs(1);

/// Short enough that the tests can let it run out rather than reason about
/// it: a surface that simply never answers is one of the two ways the
/// handshake ends.
#[cfg(test)]
const PATIENCE: Duration = Duration::from_millis(50);

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
/// layout [`crate::midi::Map::nano_kontrol2`] and the silkscreen are written
/// against.
const CONTROL: u8 = 0xB0;
const LIT: u8 = 127;
const DARK: u8 = 0;

/// A set of lamps, one bit per control number. A `u128` because that is
/// exactly the 128 numbers a control change can name, so a mask cannot
/// address a lamp that could not exist.
pub(crate) type Lamplight = u128;

/// The one lamp of control number `cc`. Safe for every control a map can
/// name: [`crate::midi::Map::validate`] refuses one past 127.
pub(crate) fn lamp(cc: u8) -> Lamplight {
    1 << cc
}

/// What the thread is told: the frame loop says which lamps it wants, the
/// reader thread hands over the surface's system-exclusive frames, and
/// [`Lamps::drop`] says to put the panel back.
enum Msg {
    Show(Lamplight),
    Sysex(Vec<u8>),
    Quit,
}

/// Why a step of the handshake did not finish.
enum Lost {
    /// The surface never answered, or a write to it failed. Always worth a
    /// line: the lights are not going to work this session.
    Surface(String),
    /// The instrument is going. The `Quit` that says so is taken off the
    /// channel where it is noticed and never put back, so a caller that
    /// carried on would park in `recv` waiting for one that has been and
    /// gone.
    Quit,
}

/// The lights of one connected surface. Dropping it puts the panel back and
/// waits for that to reach the device: a surface left holding a lamp for an
/// instrument that has exited is a lie about where the knobs are.
pub(crate) struct Lamps {
    tx: Sender<Msg>,
    /// `None` only inside [`Lamps::drop`], which takes the handle to join it.
    thread: Option<JoinHandle<()>>,
}

/// The reader thread's door onto the lights. Frames and nothing else: what is
/// lit is not the read side's to say, and neither is when to stop.
pub(crate) struct Frames(Sender<Msg>);

impl Frames {
    /// A frame that reaches nobody was asked for by a handshake that has
    /// already given up, which is its own business.
    pub(crate) fn say(&self, frame: Vec<u8>) {
        let _ = self.0.send(Msg::Sysex(frame));
    }
}

impl Lamps {
    /// Take over the lights on `file`, the surface's raw MIDI device opened
    /// for writing. `buttons` is every control number the map binds to a
    /// button — the only numbers that will ever be written, so a lamp mask
    /// cannot reach a control that is not one.
    pub(crate) fn spawn(file: File, buttons: Lamplight) -> Result<Lamps, String> {
        let (tx, rx) = channel();
        let panel = Panel {
            file,
            rx,
            buttons,
            want: 0,
            lit: 0,
        };
        let thread = std::thread::Builder::new()
            .name("midi-lamps".into())
            .spawn(move || panel.run())
            .map_err(|e| e.to_string())?;
        Ok(Lamps {
            tx,
            thread: Some(thread),
        })
    }

    pub(crate) fn frames(&self) -> Frames {
        Frames(self.tx.clone())
    }

    /// Light exactly the lamps in `want`. Called once a redraw with whatever
    /// the panel should look like now: the thread holds what is actually on
    /// the surface, so saying the same thing sixty times a second puts
    /// nothing on the wire.
    pub(crate) fn show(&self, want: Lamplight) {
        // A thread that has ended is a surface whose lights are not ours;
        // the unplug it came from is reported where unplugs are.
        let _ = self.tx.send(Msg::Show(want));
    }
}

impl Drop for Lamps {
    /// The word before the wait: the thread is parked in `recv` and only this
    /// wakes it. The join is bounded by what it wakes to do — blank the panel
    /// and put the mode back — and a wire that will not take those has
    /// already failed the write and returned.
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread's own state: the device, what it is being told, and what it has
/// put on the panel.
struct Panel {
    file: File,
    rx: Receiver<Msg>,
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
}

impl Panel {
    /// The whole life of one surface's lights.
    fn run(mut self) {
        let channel = match self.identify() {
            Ok(channel) => channel,
            Err(Lost::Quit) => return,
            // Not a nanoKONTROL2, or not answering as one: nothing is written
            // to it after this. Control changes are how *this* surface is
            // told to light a lamp, and what they are to some other device is
            // not knowable from here.
            Err(Lost::Surface(why)) => {
                return log::warn!("surface: {why}; its buttons light themselves, as before")
            }
        };
        // Nothing above has written to the surface's scene, so nothing above
        // needs undoing. Everything below might, and `restore` is filled the
        // instant a flip reaches the wire rather than once it is confirmed —
        // so however this ends, the way out runs, and the way out is the only
        // thing that puts the panel back.
        let mut restore = None;
        if let Err(Lost::Surface(why)) = self.play(channel, &mut restore) {
            log::warn!("surface: {why}");
        }
        // Dark, and then lighting itself again. The surface outlives the
        // instrument: a lamp left burning claims a focus that is gone, and a
        // panel left in external mode has no lights at all for whatever is
        // played next. Both are attempted whatever the other did.
        let _ = self.light(channel, 0);
        if let Some(scene) = restore {
            let _ = self.write(&dump(channel, &scene));
        }
    }

    /// Take the lights, blank the panel, and keep it agreeing with the frame
    /// loop until the instrument goes. Every way out of here leaves [`run`]
    /// to put the surface back.
    fn play(&mut self, channel: u8, restore: &mut Option<[u8; SCENE_BYTES]>) -> Result<(), Lost> {
        match self.external(channel, restore) {
            Ok(()) => log::info!(
                "surface: its lights are the instrument's — the Solo buttons of the \
                 focused camera and monitor, any latched mode, and every other \
                 button while it is held"
            ),
            Err(Lost::Quit) => return Ok(()),
            // A surface that will not take the mode is still played and still
            // written to: the lamps do nothing in internal mode, which is the
            // behaviour the instrument had before it lit anything.
            Err(Lost::Surface(why)) => log::warn!(
                "surface: {why}; its LED Mode is still Internal, so its buttons light \
                 themselves and only their own presses. The instrument plays as before."
            ),
        }
        // Blank the panel, so `lit` stops being a guess. Whatever drove these
        // lamps last — another program, or this one killed without putting
        // them out — left them somewhere unknown.
        //
        // To nothing, not to what is wanted: `lit` says everything is on, so
        // a lamp that is wanted is a lamp already believed lit, and folding
        // this into the first ordinary pass would leave it unwritten until
        // the focus first moved.
        self.lit = self.buttons;
        self.light(channel, 0)?;
        loop {
            self.light(channel, self.want)?;
            match self.rx.recv() {
                Ok(Msg::Show(want)) => self.want = want,
                // The surface only talks about itself when asked, and nothing
                // is asked after the handshake.
                Ok(Msg::Sysex(_)) => {}
                Ok(Msg::Quit) | Err(_) => return Ok(()),
            }
        }
    }

    /// Ask what the surface is, and get its global MIDI channel back. Both
    /// halves matter: everything below is addressed to that channel, and a
    /// device that does not answer as a nanoKONTROL2 is one this knows
    /// nothing about.
    fn identify(&mut self) -> Result<u8, Lost> {
        self.write(&INQUIRY)?;
        self.reply(channel_of, "no answer to a device inquiry")
    }

    /// Put the surface's lights under the host, filling `restore` with the
    /// scene that gives them back to it. Read-modify-write rather than a
    /// scene of this program's own, because the rest of that scene is the
    /// performer's: every control number, every curve.
    fn external(
        &mut self,
        channel: u8,
        restore: &mut Option<[u8; SCENE_BYTES]>,
    ) -> Result<(), Lost> {
        self.write(&request(channel))?;
        let mut scene = self.reply(
            |frame| scene_in(channel, frame),
            "no answer to a scene dump request",
        )?;
        // The scene's account of the surface has to agree with the one thing
        // already known about it. Every offset here is read on faith from
        // Korg's table, and this is the one of them the surface has also said
        // another way — so a scene being read where this does not think it is
        // fails now rather than going back with a performer's assignments
        // shifted along it.
        if scene[GLOBAL_CHANNEL] != channel {
            let says = scene[GLOBAL_CHANNEL];
            return Err(Lost::Surface(format!(
                "its scene reads channel {says} where it answered on {channel}"
            )));
        }
        // Already external is a scene not worth writing: a KONTROL Editor's
        // doing, or this program's own, killed before it could put the mode
        // back. The blanking pass is what clears the lamps such a killing
        // left burning.
        let taking = scene[LED_MODE] != EXTERNAL;
        if taking {
            scene[LED_MODE] = EXTERNAL;
            self.write(&dump(channel, &scene))?;
        }
        // Recorded here, before the acknowledgement. The surface applies a
        // scene as it arrives, so by this line it has the mode whatever it
        // says next — and an undo left unrecorded because the answer timed
        // out, or because the instrument exited inside the second it was
        // waited for, is a panel dark for everything else on the machine
        // until somebody pulls the cable.
        //
        // Internal, not whatever was found: found-external is exactly the
        // state a killed run leaves behind, so putting *that* back would keep
        // the panel dark just the same. A performer who chose external mode
        // chose it in the surface's flash, which a replug restores and this
        // never touches.
        scene[LED_MODE] = INTERNAL;
        *restore = Some(scene);
        if !taking {
            return Ok(());
        }
        // Deliberately not followed by a write request: that is the message
        // that commits a scene to the surface's flash, and an instrument that
        // rewrites the hardware every time it starts is one nobody can plug
        // into anything else afterwards.
        if !self.reply(|frame| ack(channel, frame), "no answer to the scene")? {
            // A refusal is the one answer that says the surface did *not*
            // take the scene — so there is nothing to undo, and a timeout is
            // not this: that one keeps the undo.
            *restore = None;
            return Err(Lost::Surface("the surface refused the scene".into()));
        }
        Ok(())
    }

    /// Wait for a system-exclusive frame that `answer` recognises, taking
    /// everything the frame loop says in the meantime so a focus moved during
    /// the handshake is not lost. `why` is what to say if the surface never
    /// answers.
    fn reply<T>(&mut self, answer: impl Fn(&[u8]) -> Option<T>, why: &str) -> Result<T, Lost> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default();
            match self.rx.recv_timeout(left) {
                Ok(Msg::Sysex(frame)) => {
                    if let Some(got) = answer(&frame) {
                        return Ok(got);
                    }
                }
                Ok(Msg::Show(want)) => self.want = want,
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => return Err(Lost::Quit),
                Err(RecvTimeoutError::Timeout) => return Err(Lost::Surface(why.into())),
            }
        }
    }

    /// Make the panel show exactly `want` and nothing else.
    fn light(&mut self, channel: u8, want: Lamplight) -> Result<(), Lost> {
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
        self.write(&bytes)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), Lost> {
        self.file
            .write_all(bytes)
            // The surface has gone, or the wire has. Either way the frame
            // loop finds the unplug out for itself, off the read that is
            // still the only thing watching for one.
            .map_err(|e| Lost::Surface(format!("its lights stopped taking writes ({e})")))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    /// The eight control numbers these tests let the instrument light. Which
    /// buttons those are is the map's business; nothing here knows.
    const BUTTONS: Lamplight = 0xFF << 32;

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

    /// A scene as the surface would send it: `led` mode, on `channel`.
    fn scene_of(channel: u8, led: u8) -> [u8; SCENE_BYTES] {
        let mut scene = [0u8; SCENE_BYTES];
        scene[GLOBAL_CHANNEL] = channel;
        scene[LED_MODE] = led;
        // Something in the tail with its top bit set, so a dump that lost the
        // packing on the way through cannot pass.
        scene[SCENE_BYTES - 1] = 0xFF;
        scene
    }

    fn dumped(channel: u8, led: u8) -> Vec<u8> {
        dump(channel, &scene_of(channel, led))
    }

    /// The surface naming itself, on `channel`.
    fn inquiry_reply(channel: u8) -> Vec<u8> {
        let mut reply = vec![0xF0, 0x7E, channel, 0x06, 0x02, KORG];
        reply.extend(NANO_KONTROL2);
        reply.extend([0x00, 0x00, 0, 0, 0, 0, 0xF7]);
        reply
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

    /// The surface's side of a socket pair, standing in for the device node:
    /// what the instrument writes really leaves a file descriptor and is
    /// really read back here, which is the boundary these tests are about.
    /// Replies go in the way the reader thread puts them in — down the
    /// channel — because on a real surface they arrive on the other direction
    /// of a wire this end never reads.
    struct Device {
        wire: UnixStream,
        /// `None` once the instrument has let go, which is the exit and the
        /// unplug both.
        lamps: Option<Lamps>,
    }

    impl Device {
        fn new() -> Device {
            let (ours, theirs) = UnixStream::pair().unwrap();
            ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            Device {
                wire: ours,
                lamps: Some(Lamps::spawn(File::from(OwnedFd::from(theirs)), BUTTONS).unwrap()),
            }
        }

        /// The next `n` bytes the instrument wrote.
        fn read(&mut self, n: usize) -> Vec<u8> {
            let mut got = vec![0u8; n];
            self.wire
                .read_exact(&mut got)
                .unwrap_or_else(|e| panic!("the instrument wrote nothing: {e}"));
            got
        }

        /// Let go, and assert that `tail` is the whole of what follows. Every
        /// test ends here rather than on a `read`, so a write the instrument
        /// had no business making cannot hide past the end of the last
        /// assertion — a flash commit, a second scene, a stray lamp.
        fn done(mut self, tail: &[u8]) {
            self.lamps = None;
            let mut rest = Vec::new();
            self.wire.read_to_end(&mut rest).unwrap();
            assert_eq!(rest, tail, "wrote more than it should have");
        }

        fn say(&self, frame: Vec<u8>) {
            self.plugged().frames().say(frame);
        }

        fn show(&self, want: Lamplight) {
            self.plugged().show(want);
        }

        fn plugged(&self) -> &Lamps {
            self.lamps.as_ref().expect("still plugged in")
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

        /// The blanking pass every connect opens with: every button the map
        /// binds, put out, before any lamp is lit.
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
        // One `show`, not two: two could be coalesced before the thread woke,
        // and this would pass without ever having masked anything.
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
        // Answered on channel 1, but the scene says channel 4.
        device.say(dump(0, &scene_of(3, INTERNAL)));
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
        // No reply to anything. The thread gives up on its own — the
        // instrument was never waiting on it — and writes no lamp, because a
        // device that did not identify itself is not one to write to.
        let mut device = Device::new();
        assert_eq!(device.read(INQUIRY.len()), INQUIRY);
        device.show(lamp(32));
        let gave_up = Instant::now();
        // Bounded, not merely finite: a wait that never ended would hang the
        // frame loop in `Lamps::drop` rather than fail this.
        device.done(&[]);
        assert!(
            gave_up.elapsed() < Duration::from_secs(5),
            "{:?}",
            gave_up.elapsed()
        );
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
}
