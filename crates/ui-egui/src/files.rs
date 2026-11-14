//! Opening and saving from the File menu: the system's file dialogs on the desktop, and in the
//! browser the file picker and a download.
//!
//! A dialog only finds a place; what happens there is a command (`file.open`, `file.save`), as
//! for every other front end. The browser can't hand over a place on disk, only the bytes of a
//! file the person picked, so there the file is taken in with `file.unpack` (a zipped project is
//! unpacked) and opened from memory; saving zips the project with `file.zip` and offers it as a
//! download. The browser's dialogs answer later, on a later frame: what they hand over waits in
//! an inbox until then.

use crate::CircuitApp;
use serde_json::{Value, json};

/// What the File menu asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileAction {
    /// Open a board or project, or import a KiCad or EAGLE board.
    Open,
    /// Save the board where it was opened from or last saved, or ask where when it has no place.
    Save,
    /// Save the board as a new project.
    SaveAs,
}

/// The file kinds the Open dialog offers.
const OPENABLE: &[&str] = &["ccproj", "ccpcb", "kicad_pcb", "brd"];

/// Where the browser keeps what it was handed, and where it saves projects, in its memory.
const RECEIVED: &str = "/received";
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const PROJECTS: &str = "/projects";

/// Files the browser's picker handed over (name, and bytes or why they weren't read), shared
/// with the future that waits for the picker.
#[cfg(target_arch = "wasm32")]
type Picked = std::rc::Rc<std::cell::RefCell<Vec<(String, Result<Vec<u8>, String>)>>>;

/// Files the browser's picker handed over, waiting for the next frame, and how many files were
/// taken in and projects saved, so each goes into a folder of its own.
#[derive(Debug, Default)]
pub(crate) struct Inbox {
    #[cfg(target_arch = "wasm32")]
    picked: Picked,
    taken: u64,
    #[cfg(target_arch = "wasm32")]
    saved: u64,
}

/// What a picked file holds, read only if the browser says it is no larger than a file may be:
/// reading holds the whole file, and one far too large would take the page's memory before it
/// is refused. `size` is what the browser reports; `read` reads the file.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) async fn picked_contents<F, R>(size: f64, read: F) -> Result<Vec<u8>, String>
where
    F: FnOnce() -> R,
    R: std::future::Future<Output = Vec<u8>>,
{
    let max = circuitcraft_io::native::MAX_FILE_BYTES;
    if size.is_nan() {
        return Err("the browser gave no size for it".to_owned());
    }
    if size > max as f64 {
        return Err(format!("larger than {max} bytes"));
    }
    Ok(read().await)
}

impl CircuitApp {
    /// Does what the File menu asked for.
    pub(crate) fn file_action(&mut self, action: FileAction, ctx: &egui::Context) {
        match action {
            FileAction::Open => self.open_dialog(ctx),
            FileAction::Save => {
                if self.session.folder().is_none() {
                    self.save_as_dialog(ctx);
                } else if self.run("file.save", &Value::Null) {
                    self.offer_download(ctx);
                }
            }
            FileAction::SaveAs => self.save_as_dialog(ctx),
        }
    }

    /// Opens what a file dialog picked: a project or board file, or a KiCad or EAGLE board.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_dialog(&mut self, _ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Open a board").add_filter("Boards and projects", OPENABLE);
        if let Some(folder) = self.session.folder() {
            dialog = dialog.set_directory(folder);
        }
        if let Some(path) = dialog.pick_file() {
            self.run("file.open", &json!({ "path": path.to_string_lossy() }));
        }
    }

    /// Saves the board as a new project folder, named and placed in a save dialog.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_as_dialog(&mut self, _ctx: &egui::Context) {
        let name = self.session.board().name.clone();
        let mut dialog = rfd::FileDialog::new().set_title("Save the project as a folder").set_file_name(&name);
        if let Some(folder) = self.session.folder().and_then(|f| f.parent()) {
            dialog = dialog.set_directory(folder);
        }
        // The system's dialog asked whether to replace what is at the place it gave, so a project
        // there is saved over; a folder made from a typed name with `.ccproj` was not asked about.
        if let Some(path) = dialog.save_file() {
            let folder = project_folder(&path);
            self.run("file.save", &json!({ "path": folder.to_string_lossy(), "overwrite": folder == path }));
        }
    }

    /// The desktop saves to disk; there is nothing to download.
    #[cfg(not(target_arch = "wasm32"))]
    fn offer_download(&mut self, _ctx: &egui::Context) {}

    /// The desktop's dialogs answer at once; nothing waits.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn take_in_picked(&mut self) {}

    /// Asks the browser for a file; what the person picks is taken in on a later frame.
    #[cfg(target_arch = "wasm32")]
    fn open_dialog(&mut self, ctx: &egui::Context) {
        let inbox = self.inbox.picked.clone();
        let ctx = ctx.clone();
        let kinds: Vec<&str> = std::iter::once("zip").chain(OPENABLE.iter().copied()).collect();
        let dialog = rfd::AsyncFileDialog::new().set_title("Open a board or a zipped project").add_filter("Boards and projects", &kinds);
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(file) = dialog.pick_file().await {
                let picked = picked_contents(file.inner().size(), || file.read()).await;
                inbox.borrow_mut().push((file.file_name(), picked));
                ctx.request_repaint();
            }
        });
    }

    /// Takes in what the browser's picker handed over, and opens it.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn take_in_picked(&mut self) {
        let picked: Vec<(String, Result<Vec<u8>, String>)> = std::mem::take(&mut *self.inbox.picked.borrow_mut());
        self.take_in(picked);
    }

    /// Takes in picked files, each into a folder of its own, and opens them; a file that wasn't
    /// read says why in the status bar.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn take_in(&mut self, picked: Vec<(String, Result<Vec<u8>, String>)>) {
        for (name, bytes) in picked {
            let bytes = match bytes {
                Ok(bytes) => bytes,
                Err(why) => {
                    self.status = format!("{name}: {why}");
                    continue;
                }
            };
            // Refused before it is copied: base64 and JSON would hold it several times over.
            if bytes.len() as u64 > circuitcraft_io::native::MAX_FILE_BYTES {
                self.status = format!("{name}: larger than {} bytes", circuitcraft_io::native::MAX_FILE_BYTES);
                continue;
            }
            self.inbox.taken += 1;
            let folder = format!("{RECEIVED}/{}", self.inbox.taken);
            let params = json!({ "name": name, "data": circuitcraft_engine::base64::encode(&bytes), "folder": folder });
            match self.session.execute("file.unpack", &params) {
                Ok(taken) => {
                    self.run("file.open", &json!({ "path": taken["path"] }));
                }
                Err(e) => self.status = format!("{name}: {e}"),
            }
        }
    }

    /// Saves the board in the browser's memory as a new project, in a folder of its name inside
    /// one of its own (two boards of one name never share a project), and offers it as a zip to
    /// download.
    #[cfg(target_arch = "wasm32")]
    fn save_as_dialog(&mut self, ctx: &egui::Context) {
        self.inbox.saved += 1;
        let folder = format!("{PROJECTS}/{}/{}", self.inbox.saved, safe_name(&self.session.board().name));
        if self.run("file.save", &json!({ "path": folder })) {
            self.offer_download(ctx);
        }
    }

    /// Offers the open board's project folder as a zip to download.
    #[cfg(target_arch = "wasm32")]
    fn offer_download(&mut self, _ctx: &egui::Context) {
        let zipped = match self.session.execute("file.zip", &Value::Null) {
            Ok(zipped) => zipped,
            Err(e) => return self.status = format!("file.zip: {e}"),
        };
        let (Some(name), Some(bytes)) = (zipped["name"].as_str(), zipped["data"].as_str().and_then(circuitcraft_engine::base64::decode)) else {
            return self.status = "file.zip gave nothing to download".to_owned();
        };
        let dialog = rfd::AsyncFileDialog::new().set_file_name(name);
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(file) = dialog.save_file().await {
                let _ = file.write(&bytes).await;
            }
        });
    }
}

/// The project folder for a place a save dialog gave: the place itself, without an extension a
/// person may have typed (`Blinky.ccproj` makes the folder `Blinky`).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn project_folder(path: &std::path::Path) -> std::path::PathBuf {
    let typed = path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ccproj", "ccpcb"].contains(&e));
    if typed { path.with_extension("") } else { path.to_path_buf() }
}

/// A board name as a folder name: what a file system or a zip may refuse is replaced.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn safe_name(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
    let safe = safe.trim().trim_start_matches('.').to_owned();
    if safe.is_empty() { "Untitled".to_owned() } else { safe }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Runs a future that never waits to the end.
    fn finished<T>(future: impl std::future::Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("the future waited"),
        }
    }

    #[test]
    fn a_picked_file_too_large_is_refused_without_being_read() {
        let max = circuitcraft_io::native::MAX_FILE_BYTES as f64;
        let never = || async { panic!("a file too large was read") };
        assert_eq!(finished(picked_contents(max + 1.0, never)), Err(format!("larger than {max} bytes")));
        assert_eq!(finished(picked_contents(f64::NAN, never)), Err("the browser gave no size for it".to_owned()));
        assert_eq!(finished(picked_contents(max, || async { b"board".to_vec() })), Ok(b"board".to_vec()));
        // Taken in, the refusal reaches the status bar and nothing is written or opened.
        let mut app = CircuitApp::new(circuitcraft_engine::Session::in_memory());
        let refused = finished(picked_contents(max + 1.0, never));
        app.take_in(vec![("huge.zip".to_owned(), refused)]);
        assert_eq!(app.status, format!("huge.zip: larger than {max} bytes"));
        assert!(app.session.storage().kind(Path::new(RECEIVED)).is_err(), "nothing taken in");
        // A file that was read is taken into a folder of its own and opened.
        let board = r#"<eagle version="9"><drawing><board><signals><signal name="GND"/></signals></board></drawing></eagle>"#;
        app.take_in(vec![("Blinky.brd".to_owned(), Ok(board.as_bytes().to_vec()))]);
        assert!(app.session.board().net_by_name("GND").is_some(), "{}", app.status);
        assert!(app.session.storage().kind(Path::new("/received/1/Blinky.brd")).is_ok());
    }

    #[test]
    fn a_typed_extension_does_not_end_up_in_the_folder_name() {
        assert_eq!(project_folder(Path::new("/work/Blinky")), Path::new("/work/Blinky"));
        assert_eq!(project_folder(Path::new("/work/Blinky.ccproj")), Path::new("/work/Blinky"));
        assert_eq!(project_folder(Path::new("/work/Blinky.ccpcb")), Path::new("/work/Blinky"));
        assert_eq!(project_folder(Path::new("/work/Rev.2")), Path::new("/work/Rev.2"), "a dot of the name's own stays");
    }

    #[test]
    fn a_board_name_becomes_a_safe_folder_name() {
        assert_eq!(safe_name("Blinky v2"), "Blinky v2");
        assert_eq!(safe_name("a/b\\c:d"), "a_b_c_d");
        assert_eq!(safe_name("../.."), "_..");
        assert_eq!(safe_name("  "), "Untitled");
        assert_eq!(safe_name(".hidden"), "hidden");
    }
}
