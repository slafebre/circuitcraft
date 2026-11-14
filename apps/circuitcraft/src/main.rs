//! CircuitCraft on the desktop: the shared egui front end in a native window (wgpu).
//!
//! `circuitcraft --control [ADDRESS]` lets agents and scripts drive the window over the control
//! channel (`circuitcraft_mcp::control`), on 127.0.0.1:7979 unless an address is given;
//! `circuitcraft-cli mcp --connect` bridges an MCP client to it. It is off unless asked for.
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

const USAGE: &str = "usage: circuitcraft [--control [ADDRESS]]\n  --control [ADDRESS]  let agents drive this window over the control channel, on a loopback address (127.0.0.1:7979 unless given)";

/// What the command line asks for: `None` without `--control`, else the address to listen on.
fn control_address(args: impl Iterator<Item = String>) -> Result<Option<SocketAddr>, String> {
    let mut address = None;
    for arg in args {
        match arg.as_str() {
            "--control" => address = Some(control::DEFAULT_ADDRESS),
            "--help" | "-h" => return Err(USAGE.to_owned()),
            // Older macOS versions add a process serial number when the app is opened from the Finder.
            psn if psn.starts_with("-psn_") => {}
            _ => match (address, arg.parse::<SocketAddr>()) {
                (Some(_), Ok(given)) => address = Some(given),
                _ => return Err(format!("unknown argument `{arg}`\n{USAGE}")),
            },
        }
    }
    Ok(address)
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
    let control = match control_address(std::env::args().skip(1)) {
        Ok(control) => control,
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
            if let Some(address) = control {
                match start_control(address, cc.egui_ctx.clone()) {
                    Ok(server) => app.listen(server),
                    Err(e) => app.report(format!("Agents cannot connect: {e}")),
                }
            }
            Ok(Box::new(App(app)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<SocketAddr>, String> {
        control_address(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn the_control_channel_is_asked_for() {
        assert_eq!(parse(&[]), Ok(None), "off unless asked for");
        assert_eq!(parse(&["--control"]), Ok(Some(control::DEFAULT_ADDRESS)));
        assert_eq!(parse(&["--control", "127.0.0.1:8000"]), Ok(Some(SocketAddr::from(([127, 0, 0, 1], 8000)))));
        assert_eq!(parse(&["--control", "[::1]:8000"]).map(|a| a.map(|a| a.port())), Ok(Some(8000)));
        assert!(parse(&["127.0.0.1:8000"]).is_err(), "an address alone asks for nothing");
        assert!(parse(&["--control", "nowhere"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
        assert_eq!(parse(&["-psn_0_12345"]), Ok(None));
    }
}
