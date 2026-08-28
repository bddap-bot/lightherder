//! The light the switcher has that the graph did not make.
//!
//! A monitor and an external input are the same kind of thing to the pass
//! that samples one: a layer of the source bank. Which layer a monitor is
//! shown comes off the switcher — the cameras by
//! [`crate::params::Params::routing`], the inputs by
//! [`crate::params::Params::routing_inputs`].
//! That is the whole of this stage's model — an input is a source plugged
//! into the switcher, so everything the switcher already does to a camera it
//! does to this unchanged, and nothing new appears in the shader. No camera
//! watches one: a camera watches monitors, which is what makes every loop in
//! the graph a loop.
//!
//! Where the pixels come from is three cases and two implementations.
//! [`Input::File`] and [`Input::Capture`] are both an `ffmpeg` reading
//! something and writing raw RGBA down a pipe, so anything ffmpeg can open is
//! an input — including its own generators, `capture = { format = "lavfi",
//! device = "testsrc2" }`, and a screen, `x11grab` + `:0.0`.
//! [`Input::Pattern`] is drawn here instead: no process, no thread, no
//! decode, and exact levels a test can assert without pinning an ffmpeg
//! version, which is what makes the bars worth having when lavfi could draw
//! them too.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::time::Duration;

use serde::Deserialize;

/// How long an input has to hand over its first frame before it counts as
/// broken. A capture device negotiates a format and a file may be on a slow
/// disk, so this is generous; it only has to be shorter than a performer's
/// patience, since an ffmpeg that dies says so at once and does not wait it
/// out.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// One external source in the graph.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    /// Drawn here, once. Still on purpose: motion in this instrument is the
    /// camera's job, and a still layer is uploaded once instead of twice a
    /// frame forever.
    Pattern(Pattern),
    /// A video file, played on a loop at its own frame rate.
    File(PathBuf),
    /// A live device: `format` is ffmpeg's `-f` and `device` its `-i`, so
    /// `v4l2` + `/dev/video0` is a webcam, `x11grab` + `:0.0` a screen, and
    /// `lavfi` + `testsrc2` ffmpeg's own pattern generators.
    Capture { format: String, device: String },
}

/// The patterns that are drawn rather than decoded. One of them, because one
/// is what drawing earns over `lavfi`: exact levels a test can assert without
/// pinning an ffmpeg version. Geometry has no such claim — a grid or a
/// timecode is `{ format = "lavfi", device = "testsrc2" }` and always was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// Eight vertical bars at 75%: white, yellow, cyan, green, magenta, red,
    /// blue, black. Every primary and both ends of the scale, which is what
    /// the hue and saturation knobs need to have anything to turn.
    Bars,
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Input::Pattern(p) => write!(f, "pattern {p:?}"),
            Input::File(path) => write!(f, "file {}", path.display()),
            Input::Capture { format, device } => write!(f, "capture {format}:{device}"),
        }
    }
}

/// A running input: the frame its layer is showing, and — for the two kinds
/// that decode — the ffmpeg still sending them.
///
/// A pattern is drawn once and never refilled, which is the state a pipe
/// reaches the moment its ffmpeg ends: a frame in hand that nothing will
/// replace. So there is one buffer and one flag here, not a variant per kind.
pub struct Source {
    /// The frame in hand, and on the layer once it has been handed over.
    /// Kept rather than given away, since a borrow of it is what
    /// [`Source::frame`] lends out — and while there is a pipe it is also one
    /// of the two buffers going round between the reader and here, neither of
    /// them allocated or freed for as long as frames keep arriving.
    showing: Vec<u8>,
    /// Whether `showing` has yet to be handed over. A frame the layer already
    /// holds is not worth a second upload, so this is what makes a still
    /// pattern upload once and a second [`Source::frame`] between arrivals
    /// answer `None`.
    pending: bool,
    /// The ffmpeg behind the frames, dropped — and so reaped — as soon as it
    /// ends. `None` for a pattern, and for a source that has ended.
    pipe: Option<Pipe>,
}

impl Source {
    /// Starts `input`, blocking until it has produced a first frame — so a
    /// missing file, an absent ffmpeg or a device that will not open is an
    /// error here, at startup, rather than a black layer nobody can explain.
    pub fn open(input: &Input, size: (u32, u32)) -> Result<Source, String> {
        // What ffmpeg is told to open is the whole of what the kinds differ
        // by.
        let ffmpeg = match input {
            Input::Pattern(pattern) => {
                return Ok(Source {
                    showing: draw(*pattern, size),
                    pending: true,
                    pipe: None,
                })
            }
            Input::File(path) => file_args(path),
            Input::Capture { format, device } => capture_args(format, device),
        };
        let (pipe, first) = Pipe::spawn(input, ffmpeg, size)?;
        Ok(Source {
            showing: first,
            pending: true,
            pipe: Some(pipe),
        })
    }

    /// The frame this source has ready, tightly packed RGBA8, or `None` when
    /// the layer already holds it — in which case it wants no upload. A
    /// source that has ended returns `None` for good, and its layer holds
    /// still on its last frame.
    ///
    /// The next frame in order, not the newest ffmpeg has produced: nothing
    /// is dropped on the way here, because a bound of one frame in flight
    /// blocks ffmpeg on its own pipe instead.
    ///
    /// Borrowed rather than handed over, because the source keeps the buffer
    /// behind it either way.
    pub fn frame(&mut self) -> Option<&[u8]> {
        let arrived = self.pipe.as_ref().map(|pipe| pipe.swap(&mut self.showing));
        match arrived {
            Some(Ok(())) => self.pending = true,
            // This is the one moment the pipe can be let go: dropping it
            // joins the reader and reaps the ffmpeg, rather than leaving a
            // defunct child for the rest of the run.
            Some(Err(TryRecvError::Disconnected)) => self.pipe = None,
            // Between frames, or no pipe at all: either way the layer keeps
            // what it has.
            Some(Err(TryRecvError::Empty)) | None => {}
        }
        std::mem::take(&mut self.pending).then_some(self.showing.as_slice())
    }

    /// Hands the frame the layer is showing over once more, for a caller that
    /// has replaced the bank that layer lived in. A new bank is blank, and a
    /// still pattern uploads exactly once, so without this its layer would
    /// stay black for the rest of the run — as would an ffmpeg's that has
    /// already ended and will send nothing further.
    ///
    /// Cheaper than reopening, and it cannot fail: an exclusive device that
    /// is already open would refuse a second ffmpeg, which would turn a
    /// recall into an error over a bank the graph did not even change.
    pub fn replay(&mut self) {
        self.pending = true;
    }
}

/// The circuit two frame buffers travel round: full ones coming up from the
/// reader, spent ones going back down to it. No third buffer is ever
/// allocated — one is being read into while the other is on the layer — which
/// at 1920x1080 is eight megabytes a frame per input neither allocated nor
/// freed.
///
/// Both bounded at one frame, and that bound is the throttle: ffmpeg blocks
/// on its own pipe once the reader is waiting to hand a frame over, so a
/// source with no pacing of its own — `lavfi` generates as fast as the
/// machine allows — runs at the rate frames are collected instead of flat
/// out. A device paces itself well under that rate, so it never blocks and
/// nothing queues ahead of what is on screen.
struct Channels {
    full: Receiver<Vec<u8>>,
    spent: SyncSender<Vec<u8>>,
}

/// An ffmpeg writing raw frames down a pipe, and the thread draining it.
struct Pipe {
    child: Child,
    /// Both ends in one field, because releasing the reader means dropping
    /// both of them: it may be parked on either, and only dropping the pair
    /// wakes it whichever it is. `None` only in [`Pipe::drop`].
    channels: Option<Channels>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // The channels go first: a reader blocked handing over a frame, or
        // waiting for one back, is not reading, so killing the child would
        // not wake it. Dropping both ends fails whichever it is parked on;
        // killing the child then ends the read the reader would otherwise be
        // blocked in. Only once it is joined is there nothing left to reap.
        self.channels.take();
        let _ = self.child.kill();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = self.child.wait();
    }
}

impl Pipe {
    /// The first frame comes back beside the pipe rather than inside it,
    /// since a [`Source`] holds one frame whether or not there is a pipe
    /// behind it.
    fn spawn(
        input: &Input,
        source: Vec<String>,
        size: (u32, u32),
    ) -> Result<(Pipe, Vec<u8>), String> {
        let mut child = Command::new("ffmpeg")
            .args(argv(source, size))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // ffmpeg's own diagnosis of a file or device that will not open
            // is better than anything this could write about it.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{input}: cannot run ffmpeg: {e}"))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");

        let bytes = frame_bytes(size);
        let (full_out, full) = sync_channel(1);
        let (spent, spent_in) = sync_channel(1);
        // The second of the two buffers, put where the reader will find it
        // once it has handed the first one over. Without it the reader would
        // wait for a buffer the layer cannot return until a second frame has
        // arrived, and no second frame ever would.
        spent
            .try_send(vec![0u8; bytes])
            .expect("a channel of one, still empty");
        let what = input.to_string();
        let reader = std::thread::spawn(move || {
            let mut frame = vec![0u8; bytes];
            loop {
                // A short read is the stream ending — EOF, a killed child, or
                // a device pulled out — and a send that fails is this Pipe
                // being dropped. Either way the last whole frame stays on the
                // layer and this thread is done.
                if stdout.read_exact(&mut frame).is_err() {
                    log::warn!("{what} ended; its layer holds its last frame");
                    return;
                }
                if full_out.send(frame).is_err() {
                    return;
                }
                // Blocks until the layer gives one back: the reader owns no
                // buffer to read into until then, which is the same throttle
                // the full channel's bound is.
                match spent_in.recv() {
                    Ok(next) => frame = next,
                    Err(_) => return,
                }
            }
        });

        // Built before the first frame is waited for, so every way out of
        // here drops it and reaps the child — including the timeout, and the
        // ffmpeg that exited instead of sending anything.
        let pipe = Pipe {
            child,
            channels: Some(Channels { full, spent }),
            reader: Some(reader),
        };
        // A dead ffmpeg drops its end of the channel, so this returns as soon
        // as it exits rather than waiting out the timeout.
        let first = pipe
            .channels
            .as_ref()
            .expect("just built")
            .full
            .recv_timeout(FIRST_FRAME_TIMEOUT)
            .map_err(|e| format!("{input}: no frame ({e}); ffmpeg's own error is above"))?;
        Ok((pipe, first))
    }

    /// Swaps whatever frame the reader has ready into `showing`, handing the
    /// buffer it replaces back down for the next read — so no third buffer is
    /// ever allocated.
    ///
    /// The error is the channel's own: `Empty` while ffmpeg is between
    /// frames, and `Disconnected` once the reader has ended, which it only
    /// ever does for good.
    fn swap(&self, showing: &mut Vec<u8>) -> Result<(), TryRecvError> {
        let channels = self.channels.as_ref().ok_or(TryRecvError::Disconnected)?;
        let next = channels.full.try_recv()?;
        let spent = std::mem::replace(showing, next);
        // The reader is waiting for this one rather than allocating a
        // replacement. It fails only once the reader is gone, and a buffer
        // nothing will ever read into is nothing to keep.
        let _ = channels.spent.try_send(spent);
        Ok(())
    }
}

/// The input-side options for a file: at its own frame rate and forever,
/// through the file protocol and no other.
///
/// A file that raced through as fast as the pipe drained would play at the
/// render rate, and one that stopped would freeze the layer a few seconds in.
/// The whitelist is there because a graph is someone else's file to write and
/// ffmpeg opens a URL as readily as a path: without it, `file =
/// "http://..."` fetches from the network for as long as the instrument runs.
/// All three are the input's options, so all three come before `-i`.
fn file_args(path: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-protocol_whitelist",
        "file",
        "-re",
        "-stream_loop",
        "-1",
        "-i",
    ]
    .map(String::from)
    .into();
    args.push(path.display().to_string());
    args
}

/// The input-side options for a live device. No `-re`: a device paces itself,
/// and ffmpeg's own advice is not to gate one. A source that does not pace
/// itself is throttled by the pipe instead — see [`Pipe::channels`].
fn capture_args(format: &str, device: &str) -> Vec<String> {
    ["-f", format, "-i", device].map(String::from).into()
}

/// The ffmpeg command line around `source`, which is the input-side options
/// naming what to open — everything up to and including `-i` and its
/// argument, built by the [`Source::open`] arm that knows which kind it is.
/// Taking those rather than an [`Input`] is what leaves no case here for a
/// pattern, which has no ffmpeg to give a command line to.
///
/// Every piece not in `source` is chosen here rather than taken from the
/// config: a config supplies a path, a format name and a device name, and
/// each of those lands as the argument of an option, which ffmpeg reads
/// positionally — so nothing a file can say becomes a flag.
fn argv(source: Vec<String>, size: (u32, u32)) -> Vec<String> {
    let (width, height) = size;
    let mut argv: Vec<String> = ["-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(source);
    argv.extend(
        [
            "-an",
            "-vf",
            // Letterboxed, not stretched, for the same reason the present
            // pass letterboxes: an input's own shape is not the monitor's.
            &format!(
                "scale={width}:{height}:force_original_aspect_ratio=decrease,\
                 pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
            ),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ]
        .map(String::from),
    );
    argv
}

/// Bytes in one tightly packed RGBA8 frame.
pub fn frame_bytes(size: (u32, u32)) -> usize {
    size.0 as usize * size.1 as usize * 4
}

/// A pattern, drawn into a tightly packed RGBA8 frame.
fn draw(pattern: Pattern, size: (u32, u32)) -> Vec<u8> {
    let (width, height) = size;
    let mut pixels = vec![0u8; frame_bytes(size)];
    for y in 0..height {
        for x in 0..width {
            let rgb = match pattern {
                Pattern::Bars => bar(x, width),
            };
            let i = ((y * width + x) * 4) as usize;
            pixels[i..i + 3].copy_from_slice(&rgb);
            pixels[i + 3] = 255;
        }
    }
    pixels
}

/// 75% of full scale, which is what a bar generator puts out: full-scale
/// primaries in composite exceed what the encoder can carry.
const BAR_LEVEL: u8 = 191;

/// Which channels are on in each bar. Not the binary count it looks like it
/// should be: the order is descending luma, which is what puts a staircase on
/// a waveform monitor and is the whole reason the pattern is read left to
/// right.
const BARS: [[bool; 3]; 8] = [
    [true, true, true],    // white
    [true, true, false],   // yellow
    [false, true, true],   // cyan
    [false, true, false],  // green
    [true, false, true],   // magenta
    [true, false, false],  // red
    [false, false, true],  // blue
    [false, false, false], // black
];

fn bar(x: u32, width: u32) -> [u8; 3] {
    BARS[(x * 8 / width).min(7) as usize].map(|on| if on { BAR_LEVEL } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (64, 64);

    fn rgb(pixels: &[u8], size: (u32, u32), x: u32, y: u32) -> [u8; 3] {
        let i = ((y * size.0 + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    }

    /// Where `arg` sits in a command line, for the assertions about order.
    fn at(argv: &[String], arg: &str) -> usize {
        argv.iter()
            .position(|a| a == arg)
            .unwrap_or_else(|| panic!("{arg} is not in {argv:?}"))
    }

    #[test]
    fn a_pattern_is_opaque_at_any_size_it_is_asked_for() {
        // Sizes a monitor is not, down to one texel: the arithmetic that
        // picks a bar divides by the width, so the degenerate frames are
        // where an off-by-one would land, and a layer with a transparent
        // texel is one the cameras would read as black.
        for size in [(64, 64), (16, 9), (1, 1)] {
            let pixels = draw(Pattern::Bars, size);
            assert!(
                pixels.chunks_exact(4).all(|p| p[3] == 255),
                "a transparent texel at {size:?}"
            );
        }
    }

    #[test]
    fn the_bars_run_white_to_black_through_every_primary() {
        let pixels = draw(Pattern::Bars, SIZE);
        // The levels written out rather than taken from BAR_LEVEL: 75% is a
        // claim about what a bar generator puts out, and a test that reads
        // the constant it is checking cannot hold it to anything.
        let expected = [
            [191, 191, 191],
            [191, 191, 0],
            [0, 191, 191],
            [0, 191, 0],
            [191, 0, 191],
            [191, 0, 0],
            [0, 0, 191],
            [0, 0, 0],
        ];
        for (i, want) in expected.iter().enumerate() {
            // The middle of bar i, so a rounding edge cannot be what is read.
            let x = (i as u32 * SIZE.0 / 8) + SIZE.0 / 16;
            assert_eq!(&rgb(&pixels, SIZE, x, 0), want, "bar {i} at x={x}");
        }
    }

    #[test]
    fn a_still_pattern_is_handed_over_once() {
        let mut source = Source::open(&Input::Pattern(Pattern::Bars), SIZE).unwrap();
        assert_eq!(source.frame().map(|f| f.len()), Some(frame_bytes(SIZE)));
        assert!(source.frame().is_none(), "a still frame uploaded twice");
    }

    #[test]
    fn the_file_command_loops_at_the_file_s_own_rate() {
        let argv = argv(file_args("clip.mp4".as_ref()), (320, 240));
        assert!(argv.windows(2).any(|w| w == ["-i", "clip.mp4"]));
        assert!(argv.windows(2).any(|w| w == ["-pix_fmt", "rgba"]));
        // All three are input options: after -i, ffmpeg reads them as the
        // output's and none of them does anything.
        assert!(at(&argv, "-re") < at(&argv, "-i"));
        assert!(at(&argv, "-stream_loop") < at(&argv, "-i"));
        assert!(at(&argv, "-protocol_whitelist") < at(&argv, "-i"));
        assert_eq!(argv[at(&argv, "-stream_loop") + 1], "-1");
        // Only the file protocol, or a graph that named an http URL would
        // have the instrument fetching from the network for as long as it
        // runs.
        assert_eq!(argv[at(&argv, "-protocol_whitelist") + 1], "file");
        let filter = &argv[at(&argv, "-vf") + 1];
        assert!(filter.contains("scale=320:240"), "{filter}");
        // Without both of these the clip is stretched to the monitor's shape
        // instead of letterboxed into it.
        assert!(
            filter.contains("force_original_aspect_ratio=decrease"),
            "{filter}"
        );
        assert!(filter.contains("pad=320:240"), "{filter}");
    }

    #[test]
    fn the_capture_command_names_the_format_and_paces_itself() {
        let argv = argv(capture_args("v4l2", "/dev/video0"), (320, 240));
        assert!(argv.windows(2).any(|w| w == ["-f", "v4l2"]));
        assert!(argv.windows(2).any(|w| w == ["-i", "/dev/video0"]));
        assert!(!argv.contains(&"-re".to_string()), "a device is not paced");
    }

    /// `false` when there is no ffmpeg to test with. `shell.nix` has one, so
    /// this only fires outside the pinned shell — printed to stderr, since
    /// libtest eats a passing test's output and a skip nobody sees is a
    /// silent pass.
    fn have_ffmpeg() -> bool {
        let ok = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !ok {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "SKIPPED: no ffmpeg on PATH");
        }
        ok
    }

    /// The mean of one channel over a frame, which is all these tests ask.
    fn mean(pixels: &[u8], channel: usize) -> f32 {
        let values = pixels.chunks_exact(4).map(|p| p[channel] as f32);
        values.clone().sum::<f32>() / values.count() as f32
    }

    #[test]
    fn a_capture_source_delivers_what_ffmpeg_generated() {
        if !have_ffmpeg() {
            return;
        }
        // lavfi is a capture device in every sense that matters here: it is
        // ffmpeg opening something named by `-f` and `-i` and handing over
        // frames until it is killed. Red, so a channel swap cannot pass.
        let mut source = Source::open(
            &Input::Capture {
                format: "lavfi".into(),
                device: "color=c=red:s=32x32".into(),
            },
            SIZE,
        )
        .unwrap();
        let frame = source.frame().expect("open() waits for the first frame");
        assert_eq!(frame.len(), frame_bytes(SIZE));
        assert!(mean(frame, 0) > 200.0, "red {}", mean(frame, 0));
        assert!(mean(frame, 1) < 40.0, "green {}", mean(frame, 1));
        assert!(mean(frame, 2) < 40.0, "blue {}", mean(frame, 2));
    }

    #[test]
    fn a_pipe_keeps_delivering_past_the_two_buffers_it_owns() {
        if !have_ffmpeg() {
            return;
        }
        // The whole of the two-buffer scheme: the reader owns no buffer to
        // read into until the layer hands one back, so a return trip that
        // never happened would show up as a source that delivered twice and
        // then stopped forever. Eight frames is several trips round.
        let mut source = Source::open(
            &Input::Capture {
                format: "lavfi".into(),
                device: "testsrc2=s=32x32".into(),
            },
            SIZE,
        )
        .unwrap();
        let mut delivered = 0;
        let began = std::time::Instant::now();
        while delivered < 8 && began.elapsed() < FIRST_FRAME_TIMEOUT {
            if source.frame().is_some() {
                delivered += 1;
            }
        }
        assert_eq!(delivered, 8, "the pipe stalled after {delivered} frames");
    }

    #[test]
    fn an_ended_source_lets_its_ffmpeg_go_and_keeps_its_last_frame() {
        if !have_ffmpeg() {
            return;
        }
        // A generator with an end to it, which nothing else here has: the
        // reader hits EOF, and what is left has to be a still holding the
        // last frame rather than an ffmpeg nobody waits for until the process
        // exits — a defunct child per ended input, for the whole run. Red, so
        // the frame it keeps can be told from an empty buffer.
        let mut source = Source::open(
            &Input::Capture {
                format: "lavfi".into(),
                device: "color=c=red:s=32x32:r=10:d=0.2".into(),
            },
            SIZE,
        )
        .unwrap();
        let pid = source.pipe.as_ref().expect("a capture pipes").child.id();
        let began = std::time::Instant::now();
        while source.pipe.is_some() && began.elapsed() < FIRST_FRAME_TIMEOUT {
            source.frame();
        }
        assert!(source.pipe.is_none(), "the pipe outlived its ffmpeg");
        // A child nobody has waited for stays in the table as a zombie; one
        // that has been waited for is gone from it entirely.
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "ffmpeg {pid} is still in the process table"
        );
        // And it still answers a bank rebuild with the frame it ended on.
        source.replay();
        let frame = source.frame().expect("the last frame, again");
        assert_eq!(frame.len(), frame_bytes(SIZE));
        assert!(mean(frame, 0) > 200.0, "not the red it ended on");
    }

    #[test]
    fn a_source_that_will_not_open_says_so_at_once() {
        if !have_ffmpeg() {
            return;
        }
        // The startup contract, and the "at once" is half of it: ffmpeg
        // cannot open this and exits, which closes the channel, so the wait
        // ends there instead of running out the ten-second timeout.
        let began = std::time::Instant::now();
        let Err(why) = Source::open(&Input::File("no-such-clip.mp4".into()), SIZE) else {
            panic!("a file that is not there opened")
        };
        assert!(why.contains("no-such-clip.mp4"), "{why}");
        assert!(
            began.elapsed() < FIRST_FRAME_TIMEOUT / 2,
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_video_file_is_decoded_scaled_and_letterboxed() {
        if !have_ffmpeg() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("lightherder-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        // A green square: not the frame's shape, so the letterboxing has
        // something to do, and not grey, so a decode that lost colour fails.
        let made = Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=green:s=64x64:r=25", "-t", "1"])
            .arg(&path)
            .status()
            .expect("run ffmpeg");
        assert!(made.success(), "could not write the fixture");

        // Wider than it is tall, so a square clip must gain side bars.
        let size = (128, 64);
        let mut source = Source::open(&Input::File(path), size).unwrap();
        let frame = source.frame().expect("open() waits for the first frame");
        assert_eq!(frame.len(), frame_bytes(size));
        let at = |x: u32, y: u32| rgb(frame, size, x, y);
        assert!(
            at(64, 32)[1] > 100,
            "the middle is not green: {:?}",
            at(64, 32)
        );
        assert_eq!(at(2, 32), [0; 3], "no bar on the left: {:?}", at(2, 32));
        assert_eq!(at(125, 32), [0; 3], "no bar on the right");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
