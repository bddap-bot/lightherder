//! Capturing what the display shows: a still on a press, a recording while a
//! button is held.
//!
//! The picture is drawn a second time into a texture of this module's own
//! rather than read back off the window's: a swapchain texture is not a copy
//! source on every backend, and the present pass takes whatever target it is
//! handed — so a capture is the same pass the glass gets, at a size this
//! module picks. The file itself is written by an `ffmpeg` reading raw frames
//! on its stdin, which is [`crate::input`] in the other direction.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use web_time::Instant;

use crate::clock::Clock;
use crate::feedback::Feedback;
use crate::overlay::Overlay;
use crate::present::{Present, View};

/// The most a capture is. A 4K display read back frame by frame is a
/// gigabyte a second over the bus and more than a real-time encoder takes;
/// bigger windows are fitted inside this, keeping their shape, so what is
/// written is framed the way the display is.
const MAX: (u32, u32) = (1920, 1080);

/// Frames a second a recording is written at, whatever rate the display is
/// handing out — see [`Capture::frame`].
const RATE: f32 = 30.0;

/// What a texture-to-buffer copy pads its rows to.
const ROW_ALIGN: u32 = 256;

pub fn dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("lightherder")
}

/// A capture in progress: somewhere to draw the display, somewhere to read
/// it back to, and the ffmpeg turning those frames into a file.
///
/// A still and a recording are the same thing asked for once or asked for
/// until a hand lets go — so they differ in what ffmpeg is told to write and
/// in whether there is a clock, and in nothing else.
pub struct Capture {
    target: wgpu::Texture,
    readback: wgpu::Buffer,
    size: (u32, u32),
    /// Bytes per row of `readback`, which is `4 * width` rounded up to
    /// [`ROW_ALIGN`] and so is not the row ffmpeg is given.
    row: u32,
    /// The last frame read back, packed as ffmpeg takes it.
    frame: Vec<u8>,
    frames: u64,
    /// A recording's clock. `None` is a still, which is one frame the moment
    /// it is asked for.
    clock: Option<Clock>,
    child: Child,
    stdin: Option<ChildStdin>,
    path: PathBuf,
}

impl Capture {
    /// One frame of the display, as a PNG.
    pub fn still(
        device: &wgpu::Device,
        dir: &Path,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<Capture, String> {
        Capture::new(device, dir, size, format, "png", &[], None)
    }

    /// The display for as long as the capture is kept, as an H.264 file.
    pub fn video(
        device: &wgpu::Device,
        dir: &Path,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<Capture, String> {
        Capture::new(
            device,
            dir,
            size,
            format,
            "mp4",
            // Ultrafast because this encodes beside a running instrument and
            // the piece is what the machine is for; yuv420p because nothing
            // else plays a rawvideo-fed H.264 file back everywhere.
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ],
            Some(Clock::new(RATE)),
        )
    }

    fn new(
        device: &wgpu::Device,
        dir: &Path,
        size: (u32, u32),
        format: wgpu::TextureFormat,
        ext: &str,
        encode: &[&str],
        clock: Option<Clock>,
    ) -> Result<Capture, String> {
        let pix = pix_fmt(format)?;
        let size = fitted(size);
        let row = (size.0 * 4).div_ceil(ROW_ALIGN) * ROW_ALIGN;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!("{}.{ext}", stamp()));
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", pix])
            .args(["-s", &format!("{}x{}", size.0, size.1)])
            .args(["-framerate", &RATE.to_string()])
            .args(["-i", "-"])
            .args(encode)
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // ffmpeg's own account of a codec or a path it will not take is
            // better than anything this could write about it.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: u64::from(row) * u64::from(size.1),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok(Capture {
            target,
            readback,
            size,
            row,
            frame: Vec::new(),
            frames: 0,
            clock,
            child,
            stdin: Some(stdin),
            path,
        })
    }

    /// Draw the display into this capture and hand ffmpeg what falls due: a
    /// still's one frame, or however many [`RATE`] owes since the last call.
    ///
    /// The rate a display hands frames out at is not the rate a video plays
    /// back at, so a slow display duplicates the frame it did draw and a
    /// fast one skips. Two costs ride on that, both the recording's and
    /// neither the piece's: the read back below waits for the GPU, and a
    /// write waits for ffmpeg — so a display that would rather not be held
    /// up should not be recorded. And a stall longer than [`Clock`]'s
    /// backlog is dropped rather than owed, exactly as the piece's own
    /// passes are, which leaves a recording made through one shorter than
    /// the hand was on the button.
    pub fn frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        present: &Present,
        monitors: &Feedback,
        view: View,
        overlay: Option<(&Overlay, &crate::params::Params)>,
    ) -> Result<(), String> {
        let due = match self.clock.as_mut() {
            Some(clock) => clock.take_due(Instant::now()),
            None => 1,
        };
        if due == 0 {
            return Ok(());
        }
        present.draw(device, queue, &self.target, monitors, view, overlay);
        self.read(device, queue)?;
        let stdin = self
            .stdin
            .as_mut()
            .expect("the pipe is open until the capture ends");
        for _ in 0..due {
            stdin
                .write_all(&self.frame)
                .map_err(|e| format!("{}: {e}", self.path.display()))?;
            self.frames += 1;
        }
        Ok(())
    }

    /// The capture texture into `self.frame`: the copy's rows are padded and
    /// a raw frame's are not.
    fn read(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(), String> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("capture readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.row),
                    rows_per_image: Some(self.size.1),
                },
            },
            wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| {
            if let Err(e) = r {
                log::error!("the capture could not be read back: {e}");
            }
        });
        // A refusal and not a panic, the way every other failure a capture
        // can meet is: the instrument goes on playing without one.
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("the GPU never handed the capture back: {e}"))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| format!("the capture's pixels: {e}"))?;
        let packed = (self.size.0 * 4) as usize;
        self.frame.clear();
        for row in mapped.chunks(self.row as usize) {
            self.frame.extend_from_slice(&row[..packed]);
        }
        drop(mapped);
        self.readback.unmap();
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf, String> {
        if self.frames == 0 {
            return Err("no frames were captured".into());
        }
        self.stdin.take();
        match self.child.wait() {
            Ok(status) if status.success() => Ok(self.path.clone()),
            Ok(status) => Err(format!("ffmpeg {status} writing {}", self.path.display())),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }
}

/// Every way out of a capture, `finish` included: nothing may be left behind
/// unwaited, and a file with no frames in it is not a capture.
impl Drop for Capture {
    fn drop(&mut self) {
        // The pipe first — an ffmpeg still reading is an ffmpeg that has not
        // ended, and closing it is what asks it to finish the file.
        self.stdin.take();
        if self.frames == 0 {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if self.frames == 0 {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What ffmpeg calls the bytes a target of `format` reads back as. The
/// capture is drawn by the present pipeline, which was built for the
/// display's own format, so this is the display's — and a display in a
/// format no raw frame names is a refusal rather than a file of noise.
fn pix_fmt(format: wgpu::TextureFormat) -> Result<&'static str, String> {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba8Unorm | F::Rgba8UnormSrgb => Ok("rgba"),
        F::Bgra8Unorm | F::Bgra8UnormSrgb => Ok("bgra"),
        other => Err(format!(
            "a {other:?} display is not one a capture can write"
        )),
    }
}

/// `size` inside [`MAX`], keeping its shape, and even on both sides — an
/// odd one has no yuv420p to encode to.
fn fitted(size: (u32, u32)) -> (u32, u32) {
    let (width, height) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let scale = (MAX.0 as f32 / width).min(MAX.1 as f32 / height).min(1.0);
    (even(width * scale), even(height * scale))
}

fn even(side: f32) -> u32 {
    ((side as u32) & !1).max(2)
}

/// A capture's name: the wall clock, UTC, down to the millisecond — which is
/// finer than a hand presses, so two captures cannot land on one name.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let (day, second) = (now.as_secs() / 86_400, now.as_secs() % 86_400);
    let (year, month, date) = civil(day as i64);
    format!(
        "{year:04}{month:02}{date:02}-{:02}{:02}{:02}-{:03}",
        second / 3600,
        second / 60 % 60,
        second % 60,
        now.subsec_millis(),
    )
}

/// The civil date `day` days after the epoch, which is the one thing a
/// timestamp needs and `std` does not give. Hinnant's `civil_from_days`,
/// whose arithmetic runs off a March-based year — hence the shifts.
fn civil(day: i64) -> (i64, u32, u32) {
    let day = day + 719_468;
    let era = day.div_euclid(146_097);
    let doe = day.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_is_named_by_the_moment_it_was_taken() {
        assert_eq!(civil(0), (1970, 1, 1));
        // A leap day, the day before it and the day after: the three dates
        // the arithmetic gets wrong when it is wrong at all.
        assert_eq!(civil(19_781), (2024, 2, 28));
        assert_eq!(civil(19_782), (2024, 2, 29));
        assert_eq!(civil(19_783), (2024, 3, 1));
        assert_eq!(civil(20_693), (2026, 8, 28));
        // And the name is the whole moment, sortable, with nothing in it a
        // filesystem argues with.
        let name = stamp();
        assert_eq!(name.len(), "YYYYMMDD-HHMMSS-mmm".len(), "{name}");
        assert!(
            name.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "{name}"
        );
    }

    #[test]
    fn a_display_bigger_than_the_capture_keeps_its_shape() {
        assert_eq!(fitted((1920, 1080)), (1920, 1080));
        assert_eq!(fitted((1280, 720)), (1280, 720));
        assert_eq!(fitted((3840, 2160)), (1920, 1080));
        // A tall window is fitted by its height, and both sides stay even.
        let (width, height) = fitted((1200, 1600));
        assert!(height <= MAX.1 && width <= MAX.0, "{width}x{height}");
        assert!(
            (width * 1600).abs_diff(height * 1200) <= 1600,
            "{width}x{height}"
        );
        for size in [(1, 1), (1919, 1081), (7, 3), (0, 0)] {
            let (width, height) = fitted(size);
            assert!(width % 2 == 0 && height % 2 == 0, "{size:?}");
            assert!(width >= 2 && height >= 2, "{size:?}");
        }
    }

    #[test]
    fn a_display_no_raw_frame_names_is_refused() {
        assert_eq!(pix_fmt(wgpu::TextureFormat::Bgra8UnormSrgb), Ok("bgra"));
        assert_eq!(pix_fmt(wgpu::TextureFormat::Rgba8Unorm), Ok("rgba"));
        assert!(pix_fmt(wgpu::TextureFormat::Rgba16Float).is_err());
    }
}
