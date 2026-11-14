//! `circuitcraft-cli`: CircuitCraft from the command line. Every subcommand goes through the
//! engine's commands, so the CLI can do exactly what the app can.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_engine::Session;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "circuitcraft-cli", version, about = "CircuitCraft from the command line")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List every command with its parameters, as JSON.
    Commands,
    /// Run one command with JSON parameters and print its JSON result.
    Exec {
        /// Command id, for example `board.info`.
        id: String,
        /// JSON parameters, for example '{"name":"Blinky"}'. Defaults to {}.
        params: Option<String>,
    },
    /// Tell what kind of design file FILE is.
    Info {
        /// The file to look at.
        file: String,
    },
    /// Draw a board into an SVG or PNG picture, without a window.
    Render {
        /// The board: a project folder, a .ccproj or .ccpcb file, a KiCad .kicad_pcb or an EAGLE .brd.
        file: String,
        /// The picture to write; its extension (.svg or .png) says which kind.
        out: String,
        /// The longer side of a PNG in pixels.
        #[arg(long)]
        size: Option<u32>,
        /// A layer to draw, by name; give it once for each layer. Every layer when left out.
        #[arg(long = "layer")]
        layers: Vec<String>,
    },
    /// Serve the Model Context Protocol on standard input and output.
    Mcp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut session = Session::new();
    let outcome = match cli.command {
        Cmd::Commands => run(&mut session, "commands", &Value::Null),
        Cmd::Exec { id, params } => match params.as_deref().map(serde_json::from_str::<Value>).transpose() {
            Ok(p) => run(&mut session, &id, &p.unwrap_or(Value::Null)),
            Err(e) => Err(format!("parameters are not valid JSON: {e}")),
        },
        Cmd::Info { file } => run(&mut session, "file.detect", &json!({ "path": file })),
        Cmd::Render { file, out, size, layers } => session.execute("file.open", &json!({ "path": file })).map_err(|e| e.to_string()).and_then(|_| {
            let layers = (!layers.is_empty()).then_some(layers);
            run(&mut session, "file.render", &json!({ "path": out, "size": size, "layers": layers }))
        }),
        Cmd::Mcp => circuitcraft_mcp::serve(&mut session, std::io::stdin().lock(), std::io::stdout().lock()).map_err(|e| e.to_string()),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(session: &mut Session, id: &str, params: &Value) -> Result<(), String> {
    let v = session.execute(id, params).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
    Ok(())
}
