//! The instrument in a browser tab.
//!
//! Everything below the window is already portable — the graph, the shader
//! and the knobs are the same code the deployed instrument runs — so this is
//! only the three things a page supplies that a terminal does not: an entry
//! point, somewhere for the log to go, and the canvas winit draws on.
//! Nothing arrives in the query string: there is one instrument.

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

/// Say why nothing is going to happen, on the page rather than only in the
/// console — a visitor whose browser has no WebGPU sees a black rectangle
/// otherwise, and has no way to tell that from a bug. Reached from inside the
/// run loop as well, where the alternative is the console and nothing else.
pub(crate) fn complain(why: &str) {
    log::error!("{why}");
    if let Some(el) = document().ok().and_then(|d| d.get_element_by_id("why")) {
        el.set_text_content(Some(why));
    }
}

/// Called by the page as soon as the module is instantiated.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    wasm_bindgen_futures::spawn_local(async {
        let params = crate::config::instrument();
        // Windowed: the canvas already covers the viewport, and asking the
        // browser for real fullscreen without a click is refused anyway.
        let cli = crate::cli::Cli {
            fullscreen: false,
            ..crate::cli::Cli::default()
        };
        if let Err(why) = crate::app::run(params, &cli).await {
            complain(&format!("{why}"));
        }
    });
}
