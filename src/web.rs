//! The instrument in a browser tab.
//!
//! Everything below the window is already portable — the graph, the shader
//! and the knobs are the same code the deployed instrument runs — so this is
//! only the three things a page supplies that a terminal does not: an entry
//! point, somewhere for the log to go, and the canvas winit draws on.
//! Which graph to play arrives the way a web page takes an argument, in the
//! query string: `?preset=insanity`.

use wasm_bindgen::prelude::wasm_bindgen;

/// The element the page keeps for the instrument. The stylesheet has already
/// stretched it over the viewport, and winit follows its size from there — so
/// the canvas is the whole window and there is nothing to lay out here.
const CANVAS_ID: &str = "lightherder";

/// The canvas winit is handed. The page and this module ship together, so a
/// missing one is a broken build rather than anything a visitor can do — but
/// it is a refusal rather than a panic all the same, because this is looked
/// up from inside the run loop, where [`complain`] is the only thing a
/// visitor would see.
///
/// Nothing here sizes it. How many pixels the canvas holds is winit's answer
/// and then wgpu's — winit measures the element with a `ResizeObserver` and
/// reports it as a resize, and wgpu writes the backing store from the surface
/// config on every `configure`. So the first frame is one pixel across and
/// the second is the viewport; anything set here is overwritten before it can
/// be read.
pub(crate) fn canvas() -> Result<web_sys::HtmlCanvasElement, String> {
    use wasm_bindgen::JsCast;
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(CANVAS_ID))
        .ok_or_else(|| format!("the page has no #{CANVAS_ID} canvas"))?
        .dyn_into()
        .map_err(|_| format!("#{CANVAS_ID} is not a canvas"))
}

/// `?preset=…` if the page was asked for one, which `config::load` then
/// resolves against the same preset names the command line takes. A graph
/// file is not reachable from here: there is no disk to read one off.
fn requested_preset() -> String {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|search| web_sys::UrlSearchParams::new_with_str(&search).ok())
        .and_then(|params| params.get("preset"))
        .unwrap_or_else(|| crate::config::PRESETS[0].0.to_string())
}

/// Write into an element of the page, if it is there.
fn fill(id: &str, text: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(id))
    {
        el.set_text_content(Some(text));
    }
}

/// Say why nothing is going to happen, on the page rather than only in the
/// console — a visitor whose browser has no WebGPU sees a black rectangle
/// otherwise, and has no way to tell that from a bug. Reached from inside the
/// run loop as well, where the alternative is the console and nothing else.
pub(crate) fn complain(why: &str) {
    log::error!("{why}");
    fill("why", why);
}

/// The corner legend: the presets as the query string spells them, which is
/// the whole of what a browser can be told here. No control surface — there
/// is no ALSA in a page — and no keys, so the instrument plays itself.
fn legend() -> String {
    let names: Vec<&str> = crate::config::PRESETS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    format!("?preset={}\n", names.join(" | "))
}

/// Called by the page as soon as the module is instantiated.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    fill("legend", &legend());
    wasm_bindgen_futures::spawn_local(async {
        let preset = requested_preset();
        let params = match crate::config::load(&preset) {
            Ok(params) => params,
            Err(why) => return complain(&why),
        };
        // Windowed: the canvas already covers the viewport, and asking the
        // browser for real fullscreen without a click is refused anyway.
        let cli = crate::cli::Cli {
            graph: preset,
            fullscreen: false,
            ..crate::cli::Cli::default()
        };
        if let Err(why) = crate::app::run(params, &cli).await {
            complain(&format!("{why}"));
        }
    });
}

/// The document, or why not.
fn document() -> Result<web_sys::Document, String> {
    web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "no document to put a video in".to_string())
}

/// A browser's word for what went wrong, as the one line every refusal is.
fn js(e: wasm_bindgen::JsValue) -> String {
    format!("{e:?}")
}

/// An input's feed in a page: a `<video>` where a terminal has an ffmpeg. A
/// capture is the page's own camera — `getUserMedia`, whatever device the
/// graph named, since a graph is written for the rig and a browser has one
/// camera to offer — and a file is a URL the page can reach.
///
/// The pixels come back through a 2D canvas the size of a bank layer, the
/// video drawn onto it letterboxed as ffmpeg's `pad` would, and read out as
/// the same tightly packed RGBA8 the pipe delivers — so past [`Feed::swap`]
/// there is one upload path, not a browser one. A frame a pass, like a
/// generator: nothing here says whether the video has a new one.
pub(crate) struct Feed {
    video: web_sys::HtmlVideoElement,
    canvas: web_sys::CanvasRenderingContext2d,
    /// Where the picture lands on the canvas: `x, y, width, height`.
    place: (f64, f64, f64, f64),
    size: (u32, u32),
}

impl Feed {
    /// Resolves once the video is playing and a first frame has been read
    /// off it. A camera the visitor has yet to allow waits here for them —
    /// that is a prompt, not an error; a URL the page cannot play is one.
    pub(crate) async fn open(
        input: &crate::input::Input,
        size: (u32, u32),
    ) -> Result<(Feed, Vec<u8>), String> {
        use crate::input::Input;
        use wasm_bindgen::JsCast;
        let document = document()?;
        let video: web_sys::HtmlVideoElement = document
            .create_element("video")
            .map_err(js)?
            .dyn_into()
            .map_err(|_| "the page will not make a video".to_string())?;
        // Muted is what lets it play without a click.
        video.set_muted(true);
        match input {
            Input::File(path) => {
                video.set_src(&path.display().to_string());
                video.set_loop(true);
            }
            Input::Capture { .. } => {
                log::info!("{input}: the page's camera stands in");
                let constraints = web_sys::MediaStreamConstraints::new();
                constraints.set_video_bool(true);
                constraints.set_audio_bool(false);
                let asked = web_sys::window()
                    .ok_or("no window")?
                    .navigator()
                    .media_devices()
                    .map_err(js)?
                    .get_user_media_with_constraints(&constraints)
                    .map_err(js)?;
                let stream: web_sys::MediaStream = wasm_bindgen_futures::JsFuture::from(asked)
                    .await
                    .map_err(|e| format!("{input}: no camera: {}", js(e)))?
                    .dyn_into()
                    .map_err(|_| format!("{input}: getUserMedia gave no stream"))?;
                video.set_src_object(Some(&stream));
            }
            Input::Pattern(_) => unreachable!("a pattern is drawn, never played"),
        }
        wasm_bindgen_futures::JsFuture::from(video.play().map_err(js)?)
            .await
            .map_err(|e| format!("{input}: will not play: {}", js(e)))?;
        let (vw, vh) = (video.video_width() as f64, video.video_height() as f64);
        if vw == 0.0 || vh == 0.0 {
            return Err(format!("{input}: playing, but no picture"));
        }
        // Letterboxed, not stretched, for the same reason the pipe's
        // filter letterboxes: an input's own shape is not the monitor's.
        let (w, h) = (size.0 as f64, size.1 as f64);
        let scale = (w / vw).min(h / vh);
        let (dw, dh) = (vw * scale, vh * scale);
        let place = ((w - dw) / 2.0, (h - dh) / 2.0, dw, dh);

        let backing: web_sys::HtmlCanvasElement = document
            .create_element("canvas")
            .map_err(js)?
            .dyn_into()
            .map_err(|_| "the page will not make a canvas".to_string())?;
        backing.set_width(size.0);
        backing.set_height(size.1);
        let options = web_sys::ContextAttributes2d::new();
        // Opaque, so the bars are black rather than clear; and read every
        // frame, so the browser keeps it where a read is a copy and not a
        // trip back from the GPU.
        options.set_alpha(false);
        options.set_will_read_frequently(true);
        let canvas: web_sys::CanvasRenderingContext2d = backing
            .get_context_with_context_options("2d", &options)
            .map_err(js)?
            .ok_or("no 2d context")?
            .dyn_into()
            .map_err(|_| "not a 2d context".to_string())?;
        let mut feed = Feed {
            video,
            canvas,
            place,
            size,
        };
        let mut first = Vec::new();
        feed.read(&mut first)?;
        Ok((feed, first))
    }

    /// The video's current frame, letterboxed, as one bank layer's bytes.
    fn read(&mut self, into: &mut Vec<u8>) -> Result<(), String> {
        let (x, y, w, h) = self.place;
        self.canvas
            .draw_image_with_html_video_element_and_dw_and_dh(&self.video, x, y, w, h)
            .map_err(js)?;
        let data = self
            .canvas
            .get_image_data(0.0, 0.0, self.size.0 as f64, self.size.1 as f64)
            .map_err(js)?
            .data();
        *into = data.0;
        Ok(())
    }

    /// A frame every time the video has one to draw, for as long as it
    /// plays; a stream whose camera went away, or a video the browser gave
    /// up on, ends it.
    pub(crate) fn swap(&mut self, showing: &mut Vec<u8>) -> crate::input::Next {
        use crate::input::Next;
        if self.video.ended() || self.video.error().is_some() {
            return Next::Ended;
        }
        // Below HAVE_CURRENT_DATA a draw of the video paints nothing, and
        // the read after it would hand back the frame before.
        if self.video.ready_state() < web_sys::HtmlMediaElement::HAVE_CURRENT_DATA {
            return Next::Waiting;
        }
        match self.read(showing) {
            Ok(()) => Next::Frame,
            Err(why) => {
                log::warn!("input ended: {why}");
                Next::Ended
            }
        }
    }
}

impl Drop for Feed {
    /// The camera's light goes out with the feed, rather than staying on
    /// for a stream nothing reads.
    fn drop(&mut self) {
        use wasm_bindgen::JsCast;
        let _ = self.video.pause();
        if let Some(stream) = self.video.src_object() {
            for track in stream.get_tracks().iter() {
                if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
                    track.stop();
                }
            }
        }
    }
}
