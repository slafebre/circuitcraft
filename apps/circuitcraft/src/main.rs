//! CircuitCraft on the desktop: the shared egui front end in a native window (wgpu).
//!
//! `circuitcraft --control [ADDRESS]` lets agents and scripts drive the window over the control
//! channel (`circuitcraft_mcp::control`), on 127.0.0.1:7979 unless an address is given;
//! `circuitcraft-cli mcp --connect` bridges an MCP client to it. It is off unless asked for.
//! `circuitcraft board.kicad_pcb` opens a board or project at the start.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_engine::Session;
use circuitcraft_mcp::control::{self, Server};
use circuitcraft_ui_egui::CircuitApp;
use eframe::egui;
use std::net::SocketAddr;
use std::sync::Arc;

struct App(CircuitApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }

    /// Runs whenever the app is woken, also while the window is hidden and nothing is drawn, so
    /// agents are answered then too.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.serve_agents();
    }

    /// Stops the control channel, which removes the control file. Quitting on macOS may end the
    /// process without dropping the app, so this doesn't wait for the drop.
    fn on_exit(&mut self) {
        self.0.stop_listening();
    }
}

const USAGE: &str = "usage: circuitcraft [--control [ADDRESS]] [FILE]\n  --control [ADDRESS]  let agents drive this window over the control channel, on a loopback address (127.0.0.1:7979 unless given)\n  FILE                 a board or project to open";

/// What the command line asks for.
#[derive(Debug, Default, PartialEq)]
struct Launch {
    /// Where agents may connect: `None` without `--control`.
    control: Option<SocketAddr>,
    /// The file to open at the start: `circuitcraft board.kicad_pcb`, or on macOS `open
    /// CircuitCraft.app --args board.kicad_pcb`.
    file: Option<String>,
}

/// Reads the arguments after the program's name. A name that isn't UTF-8 (Linux allows one) is
/// taken with its bad bytes replaced, so it fails to open and says so like any file that can't be
/// opened, rather than stopping the app.
fn launch(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Launch, String> {
    let mut launch = Launch::default();
    let mut args = args.into_iter().map(|a| a.to_string_lossy().into_owned()).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // The address, if given, comes straight after.
            "--control" => {
                let given = args.peek().and_then(|a| a.parse::<SocketAddr>().ok());
                if given.is_some() {
                    args.next();
                }
                launch.control = Some(given.unwrap_or(control::DEFAULT_ADDRESS));
            }
            "--help" | "-h" => return Err(USAGE.to_owned()),
            // Older macOS versions add a process serial number when the app is opened from the Finder.
            psn if psn.starts_with("-psn_") => {}
            option if option.starts_with('-') => return Err(format!("unknown argument `{arg}`\n{USAGE}")),
            _ if launch.file.is_some() => return Err(format!("one file at a time: `{arg}` is a second one\n{USAGE}")),
            _ => launch.file = Some(arg),
        }
    }
    Ok(launch)
}

/// Starts the control channel on `address`, waking `ctx` for each request, with its token in the
/// user's control file.
fn start_control(address: SocketAddr, ctx: egui::Context) -> Result<Server, String> {
    let file = control::control_file().ok_or("there is no folder for the control file (HOME or LOCALAPPDATA is not set)")?;
    let server = Server::start(address, Some(&file), Arc::new(move || ctx.request_repaint())).map_err(|e| e.to_string())?;
    eprintln!("circuitcraft: agents can connect on {} with the token in {}", server.address(), file.display());
    Ok(server)
}

fn main() -> eframe::Result {
    let Launch { control, file } = match launch(std::env::args_os().skip(1)) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("CircuitCraft").with_inner_size([1400.0, 900.0]).with_min_inner_size([800.0, 500.0]),
        // The board is drawn as triangles, which egui doesn't smooth: the GPU smooths their edges.
        multisampling: 4,
        ..Default::default()
    };
    eframe::run_native(
        "CircuitCraft",
        options,
        Box::new(move |cc| {
            let mut app = CircuitApp::new(Session::new());
            // Opened like any other file: the status bar says how it went.
            if let Some(path) = file {
                app.run("file.open", &serde_json::json!({ "path": path }));
            }
            if let Some(address) = control {
                match start_control(address, cc.egui_ctx.clone()) {
                    Ok(server) => app.listen(server),
                    // Also on stderr: the status bar holds one message, and a file that failed to
                    // open may want it too.
                    Err(e) => {
                        eprintln!("circuitcraft: agents cannot connect: {e}");
                        app.report(format!("Agents cannot connect: {e}"));
                    }
                }
            }
            Ok(Box::new(App(app)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(list: &[&str]) -> Result<Launch, String> {
        launch(list.iter().map(std::ffi::OsString::from))
    }

    fn control(address: Option<SocketAddr>) -> Result<Launch, String> {
        Ok(Launch { control: address, file: None })
    }

    fn file(name: &str) -> Result<Launch, String> {
        Ok(Launch { control: None, file: Some(name.to_owned()) })
    }

    #[test]
    fn the_control_channel_is_asked_for() {
        assert_eq!(parse(&[]), control(None), "off unless asked for");
        assert_eq!(parse(&["--control"]), control(Some(control::DEFAULT_ADDRESS)));
        assert_eq!(parse(&["--control", "127.0.0.1:8000"]), control(Some(SocketAddr::from(([127, 0, 0, 1], 8000)))));
        assert_eq!(parse(&["--control", "[::1]:8000"]).map(|l| l.control.map(|a| a.port())), Ok(Some(8000)));
        assert!(parse(&["--frobnicate"]).is_err());
        assert_eq!(parse(&["-psn_0_12345"]), control(None));
    }

    #[test]
    fn a_plain_argument_is_the_file_to_open() {
        assert_eq!(parse(&["board.kicad_pcb"]), file("board.kicad_pcb"));
        assert_eq!(parse(&["-psn_0_12345", "board.brd"]), file("board.brd"), "older macOS's process number is not a file");
        // Without --control an address is just a name, which then fails to open and says so.
        assert_eq!(parse(&["127.0.0.1:8000"]), file("127.0.0.1:8000"));
        assert_eq!(
            parse(&["--control", "nowhere"]),
            Ok(Launch { control: Some(control::DEFAULT_ADDRESS), file: Some("nowhere".to_owned()) }),
            "after --control, what isn't an address is the file"
        );
        assert_eq!(
            parse(&["/work/Blinky", "--control", "127.0.0.1:8000"]),
            Ok(Launch { control: Some(SocketAddr::from(([127, 0, 0, 1], 8000))), file: Some("/work/Blinky".to_owned()) })
        );
        assert!(parse(&["/work/Blinky", "other.brd"]).is_err(), "one file at a time");
        assert!(parse(&["--control", "a.brd", "127.0.0.1:9000"]).is_err(), "an address only straight after --control");
        assert_eq!(
            parse(&["--control", "127.0.0.1:8000", "127.0.0.1:9000"]).map(|l| l.control.map(|a| a.port())),
            Ok(Some(8000)),
            "a second address is a name, which then fails to open"
        );
    }

    /// A name that isn't UTF-8 (Linux allows one) is passed on with its bad bytes replaced; the
    /// open then fails and says so, as for any file that can't be opened.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_passed_on_readably() {
        use std::os::unix::ffi::OsStringExt as _;
        assert_eq!(launch([std::ffi::OsString::from_vec(vec![b'f', 0xff])]).map(|l| l.file), Ok(Some("f\u{fffd}".to_owned())));
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_reported_not_a_crash() {
        let mut app = CircuitApp::new(Session::in_memory());
        assert!(!app.run("file.open", &serde_json::json!({ "path": "/definitely/not/here.kicad_pcb" })));
        assert_eq!(app.session().board().name, "Untitled", "the board is as it was");
    }
}
