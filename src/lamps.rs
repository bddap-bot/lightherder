//! The lit half of the control surface: the Solo button of the focused
//! camera, so the panel says where the knobs are without anyone reading the
//! log line.
//!
//! A nanoKONTROL2 leaves the factory with **LED Mode: Internal** — a button
//! lights itself when it is pressed and ignores the host entirely — and the
//! only supported way to change that is Korg's KONTROL Editor, which is
//! Windows and macOS. So the mode is flipped here instead, over the same
//! device the surface is read from: ask the surface for its current scene,
//! set the one byte that is the LED mode, and hand the scene back. Nothing
//! else in it is touched and nothing is written to the surface's flash, so
//! the change lasts until the cable comes out and a performer's own
//! assignments survive it.
//!
//! All of that runs on a thread of its own, which is what keeps the promise
//! the rest of the instrument is owed: the handshake waits on replies that
//! may never come and a MIDI write blocks when the wire is full, and neither
//! may ever happen inside a frame. The frame loop's whole part in this is to
//! say which camera is focused, down a channel, once a frame.

use std::fs::File;
use std::io::Write;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use web_time::Instant;

/// How long the surface has to answer each step of the handshake. Generous
/// for a device that replies in milliseconds over USB, and it only ever
/// costs a thread nobody is waiting on: the instrument plays through the
/// whole of this, lit or not.
const PATIENCE: Duration = Duration::from_secs(1);

/// Korg's system-exclusive manufacturer id, and the two bytes that name a
/// nanoKONTROL2 inside a universal device-inquiry reply.
const KORG: u8 = 0x42;
const NANO_KONTROL2: [u8; 2] = [0x13, 0x01];

/// A universal device inquiry, addressed to every channel — the one message
/// that can be sent before the surface's global MIDI channel is known, and
/// the only place that channel comes from. Every other message here is
/// addressed to it, and so is the light itself.
const INQUIRY: [u8; 6] = [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7];

/// The fixed head of every scene message, the low nibble of byte 2 standing
/// in for the global channel. The tail `40` is the function: "here is a
/// scene", in both directions.
const SCENE: [u8; 13] = [
    0xF0, KORG, 0x40, 0x00, 0x01, 0x13, 0x00, 0x7F, 0x7F, 0x02, 0x03, 0x05, 0x40,
];

/// A scene, decoded: 339 bytes, which ride the wire as 388.
const SCENE_BYTES: usize = 339;

/// Where the LED mode sits in a decoded scene, and what it may say. Byte 0
/// is the global MIDI channel and byte 1 the control mode; this is the third
/// and last of the scene's globals.
const LED_MODE: usize = 2;
const EXTERNAL: u8 = 1;

/// A control change on the surface's global channel, which is what an LED in
/// external mode answers to: the very control number the button transmits,
/// at the button's own On and Off values. Those are 127 and 0 as the factory
/// set them — the same factory layout [`crate::midi::Map::nano_kontrol2`]
/// and the silkscreen are written against, so a surface reassigned past that
/// point has more wrong with it than its lights.
const CONTROL: u8 = 0xB0;
const LIT: u8 = 127;
const DARK: u8 = 0;

/// What the thread is told, by the two that talk to it: the frame loop says
/// which lamp it wants, the reader thread hands over every system-exclusive
/// frame the surface sends, and [`Lamps::drop`] says to put the lights out.
pub(crate) enum Msg {
    Show(Option<u8>),
    Sysex(Vec<u8>),
    Quit,
}

/// The lights of one connected surface. Dropping it darkens them and waits
/// for that to reach the device — a surface left holding a lamp for an
/// instrument that has exited is a lie about where the knobs are.
pub(crate) struct Lamps {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Lamps {
    /// Take over the lights on `file`, the surface's raw MIDI device opened
    /// for writing. Returns `None` only when the thread will not start.
    pub(crate) fn spawn(file: File) -> Option<Lamps> {
        let (tx, rx) = channel();
        let wire = Wire {
            file,
            rx,
            channel: 0,
            want: None,
            lit: None,
            quit: false,
        };
        match std::thread::Builder::new()
            .name("midi-lamps".into())
            .spawn(move || run(wire))
        {
            Ok(thread) => Some(Lamps {
                tx,
                thread: Some(thread),
            }),
            Err(e) => {
                log::warn!("surface: no thread for its lights ({e})");
                None
            }
        }
    }

    /// A second door on the same channel, for the reader thread to post
    /// system-exclusive frames through. The surface answers questions about
    /// itself on the wire it sends knob moves on, and only the reader is
    /// looking at that wire.
    pub(crate) fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    /// Light control number `cc` and no other, or darken the panel with
    /// `None`. Called once a frame with whatever the focus is now: the
    /// thread holds what is actually lit, so saying the same thing sixty
    /// times a second puts nothing on the wire.
    pub(crate) fn show(&self, cc: Option<u8>) {
        // A thread that has ended is a surface whose lights are not ours;
        // the unplug it came from is reported where unplugs are.
        let _ = self.tx.send(Msg::Show(cc));
    }
}

impl Drop for Lamps {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread's own state: the device, what it is being told, and what it
/// has put on the surface.
struct Wire {
    file: File,
    rx: Receiver<Msg>,
    /// The surface's global MIDI channel, learnt from the inquiry reply.
    channel: u8,
    /// The lamp the frame loop last asked for, which may have been asked for
    /// while the handshake was still running.
    want: Option<u8>,
    /// The lamp on the surface now, so the one that is lit can be put out.
    lit: Option<u8>,
    /// Whether the surface — or the instrument — has gone. Separates a
    /// handshake that failed from one that was simply cut short, which is
    /// not worth a line.
    quit: bool,
}

/// The whole life of one surface's lights.
fn run(mut wire: Wire) {
    match wire.identify() {
        Ok(channel) => wire.channel = channel,
        // Not a nanoKONTROL2, or not answering as one: nothing is written to
        // it after this. Control changes are how *this* surface is told to
        // light a lamp, and what they are to some other device is not
        // knowable from here.
        Err(why) => {
            if !wire.quit {
                log::warn!("surface: {why}; its buttons light themselves, as before");
            }
            return;
        }
    }
    // A surface that will not take the mode is still played and still
    // lit — the writes do nothing in internal mode, which is exactly the
    // behaviour of every version before this one.
    if let Err(why) = wire.external() {
        if wire.quit {
            return;
        }
        log::warn!(
            "surface: {why}; its LED Mode is still Internal, so its buttons light themselves \
             rather than showing the focus. The instrument plays exactly as before."
        );
    }
    while !wire.quit {
        if wire.light().is_err() {
            return;
        }
        match wire.rx.recv() {
            Ok(Msg::Show(cc)) => wire.want = cc,
            Ok(Msg::Sysex(_)) => {}
            Ok(Msg::Quit) | Err(_) => break,
        }
    }
    // Dark on the way out. The surface outlives the instrument, and a lamp
    // left burning is a claim about a focus that no longer exists.
    wire.want = None;
    let _ = wire.light();
}

impl Wire {
    /// Ask what the surface is, and get its global MIDI channel back. Both
    /// halves matter: everything below is addressed to that channel, and a
    /// device that does not answer as a nanoKONTROL2 is one this knows
    /// nothing about.
    fn identify(&mut self) -> Result<u8, String> {
        self.write(&INQUIRY)?;
        self.reply(identify, "no answer to a device inquiry")
    }

    /// Put the surface's lights under the host: read the scene it is
    /// playing, set the one byte, hand it back. Read-modify-write rather
    /// than a scene of our own, because the rest of that scene is the
    /// performer's — every control number, every curve — and a blind
    /// overwrite would take it.
    fn external(&mut self) -> Result<(), String> {
        let channel = self.channel;
        self.write(&request(channel))?;
        let mut scene = self.reply(
            |frame| scene(channel, frame),
            "no answer to a scene dump request",
        )?;
        // Already ours, from an earlier run or a KONTROL Editor: nothing to
        // write, and nothing to say.
        if scene[LED_MODE] == EXTERNAL {
            return Ok(());
        }
        scene[LED_MODE] = EXTERNAL;
        self.write(&dump(self.channel, &scene))?;
        // Deliberately not followed by a write request: that is the message
        // that commits a scene to the surface's flash, and an instrument
        // that rewrites the hardware every time it starts is one nobody can
        // plug into anything else afterwards. This lasts until the cable
        // does, which is exactly as long as it is needed.
        match self.reply(|frame| ack(channel, frame), "no answer to the scene")? {
            true => Ok(()),
            false => Err("the surface refused the scene".into()),
        }
    }

    /// Wait for a system-exclusive frame that `want` recognises, taking
    /// everything the frame loop says in the meantime so a focus moved
    /// during the handshake is not lost. `why` is what to say if the
    /// surface never answers.
    fn reply<T>(&mut self, want: impl Fn(&[u8]) -> Option<T>, why: &str) -> Result<T, String> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default();
            match self.rx.recv_timeout(left) {
                Ok(Msg::Sysex(frame)) => {
                    if let Some(got) = want(&frame) {
                        return Ok(got);
                    }
                }
                Ok(Msg::Show(cc)) => self.want = cc,
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => {
                    self.quit = true;
                    return Err("gone".into());
                }
                Err(RecvTimeoutError::Timeout) => return Err(why.into()),
            }
        }
    }

    /// Make the surface agree with `want`, if it does not already.
    fn light(&mut self) -> Result<(), String> {
        if self.want == self.lit {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(6);
        // Out first, then on: two lamps lit at once, even for as long as one
        // MIDI message, is a panel claiming the knobs are in two places.
        bytes.extend(
            self.lit
                .iter()
                .flat_map(|cc| [CONTROL | self.channel, *cc, DARK]),
        );
        bytes.extend(
            self.want
                .iter()
                .flat_map(|cc| [CONTROL | self.channel, *cc, LIT]),
        );
        self.lit = self.want;
        self.write(&bytes)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file.write_all(bytes).map_err(|e| {
            // The surface has gone, or the wire has. Either way the frame
            // loop finds the unplug out for itself, off the read that is
            // still the only thing watching for one.
            self.quit = true;
            e.to_string()
        })
    }
}

/// The global MIDI channel out of a device-inquiry reply, if it came from a
/// nanoKONTROL2: `F0 7E 0g 06 02 42 13 01 …`.
fn identify(frame: &[u8]) -> Option<u8> {
    (frame.len() >= 8
        && frame[..2] == [0xF0, 0x7E]
        && frame[3..6] == [0x06, 0x02, KORG]
        && frame[6..8] == NANO_KONTROL2)
        .then(|| frame[2] & 0x0F)
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

/// The scene inside a dump from the surface on `channel`, decoded.
fn scene(channel: u8, frame: &[u8]) -> Option<[u8; SCENE_BYTES]> {
    let head = with_channel(channel, SCENE);
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
    let mut out = with_channel(channel, SCENE).to_vec();
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

fn with_channel(channel: u8, mut head: [u8; 13]) -> [u8; 13] {
    head[2] = 0x40 | channel;
    head
}

/// How many wire bytes `len` bytes of scene take. Seven bytes ride as eight,
/// and a part group takes one byte more than it has: 339 becomes 388.
fn packed_len(len: usize) -> usize {
    len + len.div_ceil(7)
}

/// Korg's seven-bit packing: each group of seven data bytes goes out as a
/// byte of their top bits, least significant first, and then the seven
/// bodies with those bits stripped.
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
        let Some((top, bodies)) = group.split_first() else {
            continue;
        };
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
        assert_eq!(identify(&reply(KORG, NANO_KONTROL2)), Some(5));
        // Some other Korg, and some other maker's device that happens to
        // have answered: neither is a surface whose scene may be rewritten.
        assert_eq!(identify(&reply(KORG, [0x14, 0x01])), None);
        assert_eq!(identify(&reply(0x41, NANO_KONTROL2)), None);
        // The request is not the reply: byte 4 is 01 rather than 02, and a
        // loopback that fed our own inquiry back must not look like an
        // answer to it.
        assert_eq!(identify(&INQUIRY), None);
        assert_eq!(identify(&[]), None);
    }

    /// A scene dump as the surface sends it, for a scene of `led` mode.
    fn dumped(channel: u8, led: u8) -> Vec<u8> {
        let mut scene = [0u8; SCENE_BYTES];
        scene[LED_MODE] = led;
        // Something in the tail with its top bit set, so a dump that lost
        // the packing on the way through cannot pass.
        scene[SCENE_BYTES - 1] = 0xFF;
        dump(channel, &scene)
    }

    #[test]
    fn a_scene_is_read_back_out_of_the_dump_that_carried_it() {
        let got = scene(0, &dumped(0, 1)).expect("a well-formed dump");
        assert_eq!(got[LED_MODE], 1);
        assert_eq!(got[SCENE_BYTES - 1], 0xFF);
        // Addressed to another channel, so it is another surface's scene.
        assert_eq!(scene(1, &dumped(0, 1)), None);
        assert_eq!(scene(0, &dumped(1, 1)), None);
        // Truncated, and lengthened: a dump is one length exactly, and a
        // short one decoded anyway would put the LED mode wherever the
        // shortfall left it.
        let mut short = dumped(0, 1);
        short.pop();
        assert_eq!(scene(0, &short), None);
        let mut long = dumped(0, 1);
        long.push(0xF7);
        assert_eq!(scene(0, &long), None);
        // And the surface's other messages are not scenes.
        assert_eq!(scene(0, &request(0)), None);
    }

    #[test]
    fn the_acknowledgement_and_the_refusal_are_told_apart() {
        let reply = |channel: u8, func| {
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
        };
        assert_eq!(ack(0, &reply(0, 0x23)), Some(true));
        assert_eq!(ack(0, &reply(0, 0x24)), Some(false));
        // The write-completed replies, which answer a message this never
        // sends — the one that commits a scene to the surface's flash.
        assert_eq!(ack(0, &reply(0, 0x21)), None);
        assert_eq!(ack(3, &reply(3, 0x23)), Some(true));
        assert_eq!(ack(3, &reply(0, 0x23)), None);
    }

    /// The surface's side of a socket pair, standing in for the device node:
    /// what the instrument writes really goes out of a file descriptor and
    /// is really read back here, which is the boundary these tests are
    /// about. Replies go in the way the reader thread puts them in — down
    /// the channel — because on a real surface they arrive on the other
    /// direction of a wire this end never reads.
    struct Surface {
        wire: UnixStream,
        /// `None` once the instrument has let go of the surface, which is
        /// the exit and the unplug both.
        lamps: Option<Lamps>,
    }

    impl Surface {
        fn new() -> Surface {
            let (ours, theirs) = UnixStream::pair().unwrap();
            ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            Surface {
                wire: ours,
                lamps: Some(Lamps::spawn(File::from(OwnedFd::from(theirs))).unwrap()),
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

        fn say(&self, frame: Vec<u8>) {
            self.plugged().sender().send(Msg::Sysex(frame)).unwrap();
        }

        fn show(&self, cc: Option<u8>) {
            self.plugged().show(cc);
        }

        /// Letting go: the instrument exiting, or the surface unplugged.
        fn unplug(&mut self) {
            self.lamps = None;
        }

        fn plugged(&self) -> &Lamps {
            self.lamps.as_ref().expect("still plugged in")
        }

        /// Answer the inquiry and the scene request, leaving the surface in
        /// external mode with its lights under the host.
        fn handshake(&mut self, channel: u8, led: u8) {
            assert_eq!(self.read(INQUIRY.len()), INQUIRY);
            let mut reply = vec![0xF0, 0x7E, channel, 0x06, 0x02, KORG];
            reply.extend(NANO_KONTROL2);
            reply.extend([0x00, 0x00, 0, 0, 0, 0, 0xF7]);
            self.say(reply);
            assert_eq!(self.read(11), request(channel));
            self.say(dumped(channel, led));
        }
    }

    #[test]
    fn an_internal_surface_is_handed_back_its_own_scene_with_the_mode_set() {
        let mut surface = Surface::new();
        surface.handshake(0, 0);
        // The whole scene comes back, unchanged but for the one byte — the
        // rest of it is the performer's assignments, and a blind overwrite
        // would take them.
        assert_eq!(
            surface.read(402),
            dumped(0, EXTERNAL),
            "the scene was not handed back as it came"
        );
        // The acknowledgement, and then a lamp — nothing in between. What is
        // *not* on the wire is the point: the message that commits a scene
        // to the surface's flash is never sent, so a surface plugged into
        // something else afterwards is the one its owner set up.
        surface.say(vec![
            0xF0, KORG, 0x40, 0x00, 0x01, 0x13, 0x00, 0x5F, 0x23, 0x00, 0xF7,
        ]);
        surface.show(Some(33));
        assert_eq!(surface.read(3), [CONTROL, 33, LIT]);
    }

    #[test]
    fn a_surface_already_in_external_mode_is_left_alone() {
        let mut surface = Surface::new();
        surface.handshake(0, EXTERNAL);
        // Straight to the lamp: no scene goes back, because there is nothing
        // in it to change and writing one anyway is a scene the surface can
        // refuse.
        surface.show(Some(32));
        assert_eq!(surface.read(3), [CONTROL, 32, LIT]);
    }

    #[test]
    fn one_lamp_is_lit_at_a_time_and_the_last_one_is_put_out_first() {
        let mut surface = Surface::new();
        surface.handshake(0, EXTERNAL);
        surface.show(Some(32));
        assert_eq!(surface.read(3), [CONTROL, 32, LIT]);
        // Moving the focus: out, then on, in one write — a panel with two
        // lamps lit says the knobs are in two places.
        surface.show(Some(35));
        assert_eq!(surface.read(6), [CONTROL, 32, DARK, CONTROL, 35, LIT]);
        // The same focus again, sixty times a second, is nothing on the
        // wire.
        for _ in 0..60 {
            surface.show(Some(35));
        }
        surface.show(None);
        assert_eq!(surface.read(3), [CONTROL, 35, DARK]);
    }

    #[test]
    fn the_lights_go_out_when_the_instrument_does() {
        let mut surface = Surface::new();
        surface.handshake(0, EXTERNAL);
        surface.show(Some(39));
        assert_eq!(surface.read(3), [CONTROL, 39, LIT]);
        // Dropping is the exit and the unplug both. A lamp left burning on a
        // surface that outlives the instrument claims a focus that is gone.
        surface.unplug();
        assert_eq!(surface.read(3), [CONTROL, 39, DARK]);
    }

    #[test]
    fn the_lamps_are_addressed_to_the_channel_the_surface_answered_on() {
        // A surface set to MIDI channel 4 sends and receives on it, and its
        // scene messages carry the same nibble.
        let mut surface = Surface::new();
        surface.handshake(3, EXTERNAL);
        surface.show(Some(32));
        assert_eq!(surface.read(3), [CONTROL | 3, 32, LIT]);
    }

    #[test]
    fn a_device_that_is_not_a_nano_kontrol2_is_never_written_to() {
        let mut surface = Surface::new();
        assert_eq!(surface.read(INQUIRY.len()), INQUIRY);
        // Some other maker's device, answering the one message every MIDI
        // device answers. Its buttons are not this instrument's to drive:
        // what a control change does to it is not knowable from here.
        let mut reply = vec![0xF0, 0x7E, 0x00, 0x06, 0x02, 0x41];
        reply.extend([0x13, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0xF7]);
        surface.say(reply);
        surface.show(Some(32));
        surface.unplug();
        // Nothing at all after the inquiry — not the scene request, not a
        // lamp, and not the darkening on the way out.
        let mut rest = Vec::new();
        surface.wire.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "wrote {rest:?} to a device it cannot know");
    }

    #[test]
    fn a_focus_moved_during_the_handshake_is_the_one_that_lights() {
        let mut surface = Surface::new();
        assert_eq!(surface.read(INQUIRY.len()), INQUIRY);
        // The frame loop does not wait for any of this: it says where the
        // focus is every frame, from the first one.
        surface.show(Some(32));
        surface.show(Some(37));
        let mut reply = vec![0xF0, 0x7E, 0x00, 0x06, 0x02, KORG];
        reply.extend(NANO_KONTROL2);
        reply.extend([0x00, 0x00, 0, 0, 0, 0, 0xF7]);
        surface.say(reply);
        assert_eq!(surface.read(11), request(0));
        surface.say(dumped(0, EXTERNAL));
        assert_eq!(surface.read(3), [CONTROL, 37, LIT]);
    }

    #[test]
    fn a_surface_that_will_not_answer_is_still_played() {
        // No reply to anything. The thread gives up on its own — the
        // instrument was never waiting on it — and writes no lamp, because a
        // device that did not identify itself is not one to write to.
        let mut surface = Surface::new();
        assert_eq!(surface.read(INQUIRY.len()), INQUIRY);
        surface.show(Some(32));
        let gave_up = Instant::now();
        surface.unplug();
        assert!(gave_up.elapsed() < PATIENCE * 3, "{:?}", gave_up.elapsed());
        let mut rest = Vec::new();
        surface.wire.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "wrote {rest:?}");
    }

    #[test]
    fn a_refused_scene_still_leaves_the_instrument_lighting_what_it_can() {
        // The surface took the dump request but would not take the scene.
        // Its LEDs are its own — but writing them is what a nanoKONTROL2 in
        // external mode wants, and this cannot tell the two apart from here,
        // so the lamps still go out on the wire and do nothing.
        let mut surface = Surface::new();
        surface.handshake(0, 0);
        assert_eq!(surface.read(402).len(), 402);
        surface.say(vec![
            0xF0, KORG, 0x40, 0x00, 0x01, 0x13, 0x00, 0x5F, 0x24, 0x00, 0xF7,
        ]);
        surface.show(Some(32));
        assert_eq!(surface.read(3), [CONTROL, 32, LIT]);
    }
}
