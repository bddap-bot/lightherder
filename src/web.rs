//! The instrument in a browser tab.
//!
//! Everything below the window is already portable — the graph, the shader
//! and the knobs are the same code the deployed instrument runs — so this is
//! only the three things a page supplies that a terminal does not: an entry
//! point, somewhere for the log to go, and the canvas winit draws on.
//! How fast to play arrives the way a web page takes an argument, in the
//! query string: `?rate=30`. Which instrument does not: there is one.

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
    document()?
        .get_element_by_id(CANVAS_ID)
        .ok_or_else(|| format!("the page has no #{CANVAS_ID} canvas"))?
        .dyn_into()
        .map_err(|_| format!("#{CANVAS_ID} is not a canvas"))
}

/// The page, or why not — the one thing every element below hangs off.
pub(crate) fn document() -> Result<web_sys::Document, String> {
    web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "no document to draw in".to_string())
}

/// `?rate=…` if the page was asked for one, judged by the same rule as
/// `--rate`: outside the range the instrument plays at is a refusal, not a
/// clamp.
fn requested_rate() -> Result<Option<f32>, String> {
    match query().and_then(|params| params.get("rate")) {
        Some(value) => crate::cli::rate(&value).map(Some),
        None => Ok(None),
    }
}

fn query() -> Option<web_sys::UrlSearchParams> {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|search| web_sys::UrlSearchParams::new_with_str(&search).ok())
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

/// The corner legend: the tempo as the query string spells it, which is the
/// whole of what a browser can be told here. No control surface — there is
/// no ALSA in a page — and no keys, so the instrument plays itself.
fn legend() -> String {
    format!(
        "?rate={}  ({} to {})\n",
        crate::tempo::DEFAULT_RATE,
        crate::tempo::MIN_RATE,
        crate::tempo::MAX_RATE
    )
}

/// Called by the page as soon as the module is instantiated.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    fill("legend", &legend());
    wasm_bindgen_futures::spawn_local(async {
        let params = crate::config::instrument();
        let rate = match requested_rate() {
            Ok(rate) => rate,
            Err(why) => return complain(&why),
        };
        // Windowed: the canvas already covers the viewport, and asking the
        // browser for real fullscreen without a click is refused anyway.
        let mut cli = crate::cli::Cli {
            fullscreen: false,
            ..crate::cli::Cli::default()
        };
        if let Some(rate) = rate {
            cli.rate = rate;
        }
        if let Err(why) = crate::app::run(params, &cli).await {
            complain(&format!("{why}"));
        }
    });
}
