//! What the graph looks at besides its own monitors.
//!
//! A monitor and an external input are the same kind of thing to a camera: a
//! layer of the source bank, addressed by [`crate::params::Camera::look`].
//! That is the whole of this stage's model — an input is a source the
//! cameras can be aimed at, so everything the switcher and the splitters
//! already do to monitors works on it unchanged, and nothing new appears in
//! the shader.
//!
//! Where the pixels come from is three cases, and two of them are one
//! implementation: [`Input::Pattern`] is drawn here, while [`Input::File`]
//! and [`Input::Capture`] are both an `ffmpeg` reading something and writing
//! raw RGBA down a pipe. Anything ffmpeg can open is therefore an input,
//! including its own generators — `capture = { format = "lavfi", device =
//! "testsrc2" }` — which is why the drawn patterns are only the two that
//! must work with no ffmpeg on the box at all.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long an input has to hand over its first frame before it counts as
/// broken. A capture device negotiates a format and a file may be on a slow
/// disk, so this is generous; the point is only that a source that will
/// never produce anything says so at startup rather than showing black.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// One external source in the graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

/// The patterns that need no ffmpeg. Two, because two is what the knobs
/// divide into: one that is all colour and one that is all geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// Eight vertical bars at 75%: white, yellow, cyan, green, magenta, red,
    /// blue, black. Every primary and both ends of the scale, which is what
    /// the hue and saturation knobs need to have anything to turn.
    Bars,
    /// White lines on black, eight cells across the height. Straight edges at
    /// known spacing, so a camera's zoom, turn and pan are legible in the
    /// image instead of having to be read off the log line.
    Grid,
}

impl Input {
    /// What to call this input in a message, short enough for a log line.
    pub fn describe(&self) -> String {
        match self {
            Input::Pattern(p) => format!("pattern {p:?}"),
            Input::File(path) => format!("file {}", path.display()),
            Input::Capture { format, device } => format!("capture {format}:{device}"),
        }
    }
}

/// A running input: whatever is producing its frames, and the size they
/// arrive at.
pub struct Source {
    frames: Frames,
    size: (u32, u32),
}

enum Frames {
    /// Drawn once and handed over once. `None` after that: a still layer that
    /// is already on the GPU needs no upload.
    Still(Option<Vec<u8>>),
    Pipe(Pipe),
}

impl Source {
    /// Starts `input`, blocking until it has produced a first frame — so a
    /// missing file, an absent ffmpeg or a device that will not open is an
    /// error here, at startup, rather than a black layer nobody can explain.
    pub fn open(input: &Input, size: (u32, u32)) -> Result<Source, String> {
        let frames = match input {
            Input::Pattern(pattern) => Frames::Still(Some(draw(*pattern, size))),
            _ => Frames::Pipe(Pipe::spawn(input, size)?),
        };
        Ok(Source { frames, size })
    }

    /// The newest frame since the last call, tightly packed RGBA8, or `None`
    /// when nothing has arrived — in which case the layer already holds the
    /// most recent one and wants no upload.
    pub fn frame(&mut self) -> Option<Vec<u8>> {
        match &mut self.frames {
            Frames::Still(once) => once.take(),
            Frames::Pipe(pipe) => pipe.take(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
}

/// An ffmpeg writing raw frames down a pipe, and the thread draining it.
struct Pipe {
    child: std::process::Child,
    /// The newest whole frame the reader has seen, and nothing older: a live
    /// instrument wants the current image, not a queue of stale ones. The
    /// condvar is only for the startup wait.
    latest: Arc<(Mutex<Option<Vec<u8>>>, Condvar)>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Pipe {
    fn spawn(input: &Input, size: (u32, u32)) -> Result<Pipe, String> {
        let what = input.describe();
        let mut child = std::process::Command::new("ffmpeg")
            .args(argv(input, size))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{what}: cannot run ffmpeg: {e}"))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");

        let bytes = frame_bytes(size);
        let latest = Arc::new((Mutex::new(None), Condvar::new()));
        let into = Arc::clone(&latest);
        let reader = std::thread::spawn(move || {
            let mut frame = vec![0u8; bytes];
            // A short read is the stream ending — EOF, a killed child, or a
            // device pulled out. Whatever it was, the last whole frame stays
            // where it is and the layer holds still.
            while stdout.read_exact(&mut frame).is_ok() {
                let (slot, arrived) = &*into;
                *slot.lock().expect("frame slot") = Some(frame.clone());
                arrived.notify_all();
            }
        });

        let pipe = Pipe {
            child,
            latest,
            reader: Some(reader),
        };
        // Wait for the first frame with the pipe already built, so failing
        // here still runs Pipe's Drop and reaps the child.
        let (slot, arrived) = &*pipe.latest;
        let mut held = slot.lock().expect("frame slot");
        while held.is_none() {
            let (next, timeout) = arrived
                .wait_timeout(held, FIRST_FRAME_TIMEOUT)
                .expect("frame slot");
            held = next;
            if held.is_none() && timeout.timed_out() {
                drop(held);
                return Err(format!(
                    "{what}: no frame in {}s (ffmpeg's own error is above)",
                    FIRST_FRAME_TIMEOUT.as_secs()
                ));
            }
        }
        drop(held);
        Ok(pipe)
    }

    fn take(&mut self) -> Option<Vec<u8>> {
        self.latest.0.lock().expect("frame slot").take()
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // Killing the child closes the pipe, which is what ends the reader's
        // blocking read; joining then guarantees the thread is gone before
        // the process it was reading is reaped.
        let _ = self.child.kill();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = self.child.wait();
    }
}

/// The ffmpeg command line for an input. Every piece is chosen here rather
/// than taken from the config: a config supplies a path, a format name and a
/// device name, never an argument, so nothing in a file can turn into a flag.
/// `config::validate` is what holds that line, by refusing a leading `-`.
fn argv(input: &Input, size: (u32, u32)) -> Vec<String> {
    let (width, height) = size;
    let mut argv: Vec<String> = ["-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match input {
        Input::Pattern(_) => unreachable!("patterns are drawn, not decoded"),
        Input::File(path) => {
            // At its own frame rate and forever: a file that raced through as
            // fast as the pipe drained would play at the render rate, and one
            // that stopped would freeze the layer a few seconds in.
            argv.extend(["-re", "-stream_loop", "-1", "-i"].map(String::from));
            argv.push(path.display().to_string());
        }
        Input::Capture { format, device } => {
            // No -re: a device paces itself, and asking for both stutters.
            argv.extend(["-f", format, "-i", device].map(String::from));
        }
    }
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
                Pattern::Grid => grid(x, y, size),
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

fn grid(x: u32, y: u32, size: (u32, u32)) -> [u8; 3] {
    // Square cells an eighth of the height, and lines thin enough that the
    // camera's linear sampling has something to soften.
    let cell = (size.1 / 8).max(1);
    let line = (size.1 / 128).max(1);
    let on_line = |v: u32| v % cell < line;
    if on_line(x) || on_line(y) {
        [255; 3]
    } else {
        [0; 3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (64, 64);

    fn rgb(pixels: &[u8], size: (u32, u32), x: u32, y: u32) -> [u8; 3] {
        let i = ((y * size.0 + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    }

    #[test]
    fn a_pattern_fills_the_frame_it_was_asked_for() {
        for pattern in [Pattern::Bars, Pattern::Grid] {
            for size in [(64, 64), (16, 9), (1, 1)] {
                let pixels = draw(pattern, size);
                assert_eq!(pixels.len(), frame_bytes(size), "{pattern:?} at {size:?}");
                assert!(
                    pixels.chunks_exact(4).all(|p| p[3] == 255),
                    "{pattern:?} left a transparent texel"
                );
            }
        }
    }

    #[test]
    fn the_bars_run_white_to_black_through_every_primary() {
        let pixels = draw(Pattern::Bars, SIZE);
        let l = BAR_LEVEL;
        let expected = [
            [l, l, l],
            [l, l, 0],
            [0, l, l],
            [0, l, 0],
            [l, 0, l],
            [l, 0, 0],
            [0, 0, l],
            [0, 0, 0],
        ];
        for (i, want) in expected.iter().enumerate() {
            // The middle of bar i, so a rounding edge cannot be what is read.
            let x = (i as u32 * SIZE.0 / 8) + SIZE.0 / 16;
            assert_eq!(&rgb(&pixels, SIZE, x, 0), want, "bar {i} at x={x}");
        }
    }

    #[test]
    fn the_grid_is_lines_on_black_and_mostly_black() {
        let pixels = draw(Pattern::Grid, SIZE);
        // The corner is where two lines cross, and the middle of a cell is
        // the furthest point from any of them.
        assert_eq!(rgb(&pixels, SIZE, 0, 0), [255; 3]);
        assert_eq!(rgb(&pixels, SIZE, SIZE.0 / 16, SIZE.1 / 16), [0; 3]);
        let lit = pixels.chunks_exact(4).filter(|p| p[0] > 0).count();
        let texels = (SIZE.0 * SIZE.1) as usize;
        assert!(lit * 4 < texels, "{lit} of {texels} texels lit: not lines");
        assert!(lit > 0, "no lines at all");
    }

    #[test]
    fn a_still_pattern_is_handed_over_once() {
        let mut source = Source::open(&Input::Pattern(Pattern::Bars), SIZE).unwrap();
        assert_eq!(source.frame().map(|f| f.len()), Some(frame_bytes(SIZE)));
        assert!(source.frame().is_none(), "a still frame uploaded twice");
    }

    #[test]
    fn the_file_command_loops_at_the_file_s_own_rate() {
        let argv = argv(&Input::File("clip.mp4".into()), (320, 240));
        assert!(argv.windows(2).any(|w| w == ["-i", "clip.mp4"]));
        assert!(argv.contains(&"-re".to_string()));
        assert!(argv.windows(2).any(|w| w == ["-stream_loop", "-1"]));
        assert!(argv.windows(2).any(|w| w == ["-pix_fmt", "rgba"]));
        assert!(argv.iter().any(|a| a.contains("scale=320:240")));
    }

    #[test]
    fn the_capture_command_names_the_format_and_paces_itself() {
        let argv = argv(
            &Input::Capture {
                format: "v4l2".into(),
                device: "/dev/video0".into(),
            },
            (320, 240),
        );
        assert!(argv.windows(2).any(|w| w == ["-f", "v4l2"]));
        assert!(argv.windows(2).any(|w| w == ["-i", "/dev/video0"]));
        assert!(!argv.contains(&"-re".to_string()), "a device is not paced");
    }

    /// `false` when there is no ffmpeg to test with, which is not a failure —
    /// the drawn patterns are the half of this module that must work without
    /// one. Printed to stderr, since libtest eats a passing test's output and
    /// a skip nobody sees is a silent pass.
    fn have_ffmpeg() -> bool {
        let ok = std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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
        assert!(mean(&frame, 0) > 200.0, "red {}", mean(&frame, 0));
        assert!(mean(&frame, 1) < 40.0, "green {}", mean(&frame, 1));
        assert!(mean(&frame, 2) < 40.0, "blue {}", mean(&frame, 2));
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
        let made = std::process::Command::new("ffmpeg")
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
        let at = |x: u32, y: u32| rgb(&frame, size, x, y);
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
