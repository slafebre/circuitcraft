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

/// The file to open at the start, from the arguments after the program's name, if one was
/// given: `circuitcraft board.kicad_pcb`, or on macOS `open CircuitCraft.app --args
/// board.kicad_pcb`. Older macOS adds a `-psn_…` process number when an app is started from the
/// Finder; that is not a file. Anything else is opened as a file, and a wrong one is reported in
/// the status bar like any file that can't be opened.
fn file_to_open(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<String> {
    args.into_iter().map(|a| a.to_string_lossy().into_owned()).find(|a| !a.starts_with("-psn_"))
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("CircuitCraft").with_inner_size([1400.0, 900.0]).with_min_inner_size([800.0, 500.0]),
        // The board is drawn as triangles, which egui doesn't smooth: the GPU smooths their edges.
        multisampling: 4,
        ..Default::default()
    };
    let mut app = CircuitApp::new(Session::new());
    // Opened like any other file: the status bar says how it went.
    if let Some(path) = file_to_open(std::env::args_os().skip(1)) {
        app.run("file.open", &serde_json::json!({ "path": path }));
    }
    eframe::run_native("CircuitCraft", options, Box::new(|_cc| Ok(Box::new(App(app)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn the_first_argument_that_is_a_file_is_opened() {
        assert_eq!(file_to_open(args(&[])), None);
        assert_eq!(file_to_open(args(&["board.kicad_pcb"])), Some("board.kicad_pcb".to_owned()));
        assert_eq!(file_to_open(args(&["/work/Blinky", "other.brd"])), Some("/work/Blinky".to_owned()), "the first one");
        // Older macOS's process number is not a file.
        assert_eq!(file_to_open(args(&["-psn_0_12345"])), None);
        assert_eq!(file_to_open(args(&["-psn_0_12345", "board.brd"])), Some("board.brd".to_owned()));
    }

    /// A name that isn't UTF-8 (Linux allows one) is passed on with its bad bytes replaced; the
    /// open then fails and says so, as for any file that can't be opened.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_passed_on_readably() {
        use std::os::unix::ffi::OsStringExt as _;
        assert_eq!(file_to_open([std::ffi::OsString::from_vec(vec![b'f', 0xff])]), Some("f\u{fffd}".to_owned()));
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_reported_not_a_crash() {
        let mut app = CircuitApp::new(Session::in_memory());
        assert!(!app.run("file.open", &serde_json::json!({ "path": "/definitely/not/here.kicad_pcb" })));
        assert_eq!(app.session().board().name, "Untitled", "the board is as it was");
    }
}
