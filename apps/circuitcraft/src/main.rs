//! CircuitCraft on the desktop: the shared egui front end in a native window (wgpu).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_engine::Session;
use circuitcraft_ui_egui::CircuitApp;
use eframe::egui;

struct App(CircuitApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("CircuitCraft").with_inner_size([1400.0, 900.0]).with_min_inner_size([800.0, 500.0]),
        // The board is drawn as triangles, which egui doesn't smooth: the GPU smooths their edges.
        multisampling: 4,
        ..Default::default()
    };
    eframe::run_native("CircuitCraft", options, Box::new(|_cc| Ok(Box::new(App(CircuitApp::new(Session::new()))))))
}
