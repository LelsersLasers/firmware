#[cfg(target_arch = "wasm32")]
mod wasm_entry {
    use daqweb::{build_web_viewer_app, platform::BrowserEnv};
    use wasm_bindgen::JsCast as _;

    pub struct WasmBrowserEnv;

    impl BrowserEnv for WasmBrowserEnv {
        fn origin_url(&self) -> String {
            web_sys::window()
                .and_then(|w| w.location().origin().ok())
                .unwrap_or_default()
        }

        fn pathname(&self) -> String {
            web_sys::window()
                .and_then(|w| w.location().pathname().ok())
                .unwrap_or_else(|| "/".to_owned())
        }

        fn replace_url_path(&self, new_path: &str) {
            if let Some(window) = web_sys::window()
                && let Ok(history) = window.history()
            {
                let _ = history.replace_state_with_url(
                    &wasm_bindgen::JsValue::NULL,
                    "",
                    Some(new_path),
                );
            }
        }

        fn load_storage(&self, key: &str) -> Option<String> {
            web_sys::window()?
                .local_storage()
                .ok()??
                .get_item(key)
                .ok()?
        }

        fn save_storage(&self, key: &str, value: &str) {
            if let Some(window) = web_sys::window()
                && let Ok(Some(storage)) = window.local_storage()
            {
                let _ = storage.set_item(key, value);
            }
        }

        fn trigger_file_download(&self, filename: &str, mime_type: &str, bytes: &[u8]) {
            let Some(window) = web_sys::window() else {
                return;
            };
            let Some(document) = window.document() else {
                return;
            };
            let uint8_arr = js_sys::Uint8Array::from(bytes);
            let parts = js_sys::Array::new();
            parts.push(&uint8_arr.buffer());
            let bag = web_sys::BlobPropertyBag::new();
            bag.set_type(mime_type);
            let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag)
            else {
                return;
            };
            let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
                return;
            };
            if let Ok(elem) = document.create_element("a")
                && let Ok(anchor) = elem.dyn_into::<web_sys::HtmlAnchorElement>()
            {
                anchor.set_href(&url);
                anchor.set_download(filename);
                anchor.click();
            }
            let _ = web_sys::Url::revoke_object_url(&url);
        }
    }

    pub fn run() {
        eframe::WebLogger::init(log::LevelFilter::Info).ok();
        let web_options = eframe::WebOptions::default();

        wasm_bindgen_futures::spawn_local(async {
            let Some(window) = web_sys::window() else {
                return;
            };
            let Some(document) = window.document() else {
                return;
            };
            let Some(canvas_elem) = document.get_element_by_id("the_canvas_id") else {
                log::error!("Failed to find #the_canvas_id in index.html");
                return;
            };
            let Ok(canvas) = canvas_elem.dyn_into::<web_sys::HtmlCanvasElement>() else {
                log::error!("#the_canvas_id is not an HtmlCanvasElement");
                return;
            };

            let start_result = eframe::WebRunner::new()
                .start(
                    canvas,
                    web_options,
                    Box::new(|cc| {
                        Ok(Box::new(build_web_viewer_app(
                            WasmBrowserEnv,
                            Some(&cc.egui_ctx),
                        )))
                    }),
                )
                .await;

            if let Some(loading) = document.get_element_by_id("loading-screen") {
                loading.remove();
            }

            if let Err(err) = start_result {
                log::error!("Failed to start DaqWeb eframe WebRunner: {err:?}");
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    wasm_entry::run();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!(
        "daqweb is the WebAssembly browser entrypoint (`cargo build -p daqweb --target wasm32-unknown-unknown`). Use `daqcloud` to serve it at `/`."
    );
}
