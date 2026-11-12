//! Repository checks. `cargo xtask ci` runs them all, in the order a failure is cheapest to find.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod assets;
mod layers;

use std::path::PathBuf;
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: cargo xtask <command>

commands:
  ci       fmt, clippy, test, assets, layers, wasm (what CI runs)
  test     cargo test --workspace
  layers   check the crate layering (xtask/src/layers.rs)
  assets   check that every asset file has a row in ATTRIBUTION.md
  wasm     check that the browser build compiles";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => cmd_ci(),
        Some("test") => cmd_test(),
        Some("layers") => cmd_layers(),
        Some("assets") => assets::run(&root()),
        Some("wasm") => cmd_wasm(),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root (the parent of this crate's directory).
fn root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map(PathBuf::from).unwrap_or(manifest)
}

fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    c
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    eprintln!("$ {what}");
    let status = cmd.status().map_err(|e| format!("could not start `{what}`: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("`{what}` failed ({status})")) }
}

fn cmd_test() -> Result<(), String> {
    let mut c = cargo();
    c.args(["test", "--workspace", "--quiet"]);
    run(c, "cargo test --workspace")
}

fn cmd_layers() -> Result<(), String> {
    let out = cargo().args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))?;
    let crates = layers::from_metadata(&meta)?;
    let violations = layers::check(&crates);
    if violations.is_empty() {
        println!("layers: {} workspace crates, no violations", crates.len());
        Ok(())
    } else {
        for v in &violations {
            eprintln!("  {v}");
        }
        Err(format!("{} layering violation(s)", violations.len()))
    }
}

/// The crates that must build for the browser: every library up to the UI layer, and the web app.
const WASM_PACKAGES: &[&str] = &[
    "circuitcraft-units",
    "circuitcraft-geom",
    "circuitcraft-model",
    "circuitcraft-render",
    "circuitcraft-io",
    "circuitcraft-engine",
    "circuitcraft-ui-egui",
    "circuitcraft-web",
];

fn cmd_wasm() -> Result<(), String> {
    let mut c = cargo();
    c.args(["check", "--target", "wasm32-unknown-unknown", "--quiet"]);
    for p in WASM_PACKAGES {
        c.args(["-p", p]);
    }
    run(c, "cargo check --target wasm32-unknown-unknown (libraries and circuitcraft-web)")
}

type Step = (&'static str, fn() -> Result<(), String>);

fn cmd_ci() -> Result<(), String> {
    fn fmt() -> Result<(), String> {
        let mut c = cargo();
        c.args(["fmt", "--all", "--", "--check"]);
        run(c, "cargo fmt --all -- --check")
    }
    fn clippy() -> Result<(), String> {
        let mut c = cargo();
        c.args(["clippy", "--workspace", "--all-targets", "--quiet", "--", "-D", "warnings"]);
        run(c, "cargo clippy --workspace --all-targets -- -D warnings")
    }
    fn assets_step() -> Result<(), String> {
        assets::run(&root())
    }
    let steps: [Step; 6] =
        [("fmt", fmt), ("clippy", clippy), ("test", cmd_test), ("assets", assets_step), ("layers", cmd_layers), ("wasm", cmd_wasm)];
    for (i, (name, step)) in steps.iter().enumerate() {
        eprintln!("\n=== ci: {name} ===");
        if let Err(e) = step() {
            println!("\nCI summary:");
            for (done, _) in steps.iter().take(i) {
                println!("  ok    {done}");
            }
            println!("  FAIL  {name}: {e}");
            for (skipped, _) in steps.iter().skip(i + 1) {
                println!("  skip  {skipped}");
            }
            return Err(format!("ci failed at `{name}`"));
        }
    }
    println!("\nCI summary: all {} steps passed", steps.len());
    Ok(())
}
