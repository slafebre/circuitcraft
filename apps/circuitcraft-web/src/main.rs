//! CircuitCraft in the browser: the same egui front end as the desktop app, through eframe's web
//! runner. Build with `trunk build --release` from this directory (output in `dist/web`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
fn main() {
    use circuitcraft_engine::Session;
    use circuitcraft_ui_egui::CircuitApp;
    use eframe::egui;
    use wasm_bindgen::JsCast as _;

    struct App(CircuitApp);

    impl eframe::App for App {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.0.ui(ui);
        }
    }

    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        let Some(canvas) = document.get_element_by_id("circuitcraft_canvas").and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            return;
        };
        let started = eframe::WebRunner::new()
            .start(canvas, eframe::WebOptions::default(), Box::new(|_cc| Ok(Box::new(App(CircuitApp::new(Session::new()))))))
            .await;
        if let Some(loading) = document.get_element_by_id("circuitcraft_loading") {
            match started {
                Ok(()) => loading.remove(),
                Err(_) => loading.set_text_content(Some("CircuitCraft could not start: this browser lacks WebGPU and WebGL2.")),
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("circuitcraft-web runs in the browser: build it with `trunk build --release` in apps/circuitcraft-web");
}
