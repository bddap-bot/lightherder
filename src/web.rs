//! The instrument in a browser tab.
//!
//! Everything below the window is already portable — the graph, the shader,
//! the knobs and the keys are the same code the deployed instrument runs — so
//! this is only the three things a page supplies that a terminal does not: an
//! entry point, somewhere for the log to go, and the canvas winit draws on.
//! Which graph to play arrives the way a web page takes an argument, in the
//! query string: `?preset=insanity`.

use wasm_bindgen::prelude::wasm_bindgen;

/// The element the page keeps for the instrument. The stylesheet has already
/// stretched it over the viewport, and winit follows its size from there — so
/// the canvas is the whole window and there is nothing to lay out here.
const CANVAS_ID: &str = "lightherder";

/// The canvas winit is handed. Panics if the page has none: the page and this
/// module ship together, so a missing canvas is a broken build rather than
/// anything a visitor can do.
pub(crate) fn canvas() -> web_sys::HtmlCanvasElement {
    use wasm_bindgen::JsCast;
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(CANVAS_ID))
        .expect("the page has no #lightherder canvas")
        .dyn_into()
        .expect("#lightherder is not a canvas")
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
/// otherwise, and has no way to tell that from a bug.
fn complain(why: &str) {
    log::error!("{why}");
    fill("why", why);
}

/// The corner legend: the keys off the same tables the instrument plays,
/// which is why there is no copy of them in the page, and the presets as the
/// query string spells them. No control surface: there is no ALSA here.
fn legend() -> String {
    let names: Vec<&str> = crate::config::PRESETS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    format!("{}?preset={}\n", crate::keys::help(), names.join(" | "))
}

/// Called by the page as soon as the module is instantiated.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    fill("keys", &legend());
    wasm_bindgen_futures::spawn_local(async {
        let preset = requested_preset();
        let params = match crate::config::load(&preset) {
            Ok(params) => params,
            Err(why) => return complain(&why),
        };
        // Windowed: the canvas already covers the viewport, and asking the
        // browser for real fullscreen without a click is refused anyway. The
        // `f` key still asks, and there it is a gesture the browser accepts.
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
