//! `cargo xtask bundle`: the desktop app as a macOS app bundle in `dist/` (gitignored), for the
//! machine that built it. The bundle is signed ad hoc, not notarised and built for one
//! architecture, so it is not a release artefact. Signed installers are a 1.0 deliverable
//! (ROADMAP.md).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// The cargo package of the desktop app, and the name of its binary.
const PACKAGE: &str = "circuitcraft";

/// The bundle's folder name; Finder shows it without the extension.
const BUNDLE: &str = "CircuitCraft.app";

/// The bundle identifier, our own: named after the repository's owner and name on GitHub.
const BUNDLE_ID: &str = "io.github.slafebre.circuitcraft";

pub fn run(root: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("`bundle` makes a macOS app bundle and runs only on macOS; other platforms are not available yet".into());
    }
    let version = app_version(&super::metadata(true)?)?;
    let binary = build()?;
    let app = assemble(&root.join("dist"), &binary, &version)?;
    // Signing the bundle seals Info.plist into the signature and names the signature after the
    // bundle identifier. Without it the binary carries at most the linker's own signature.
    let mut sign = Command::new("codesign");
    sign.args(["--force", "--sign", "-"]).arg(&app);
    super::run(sign, "codesign --force --sign - (ad hoc)")?;
    println!("bundle: {}", app.display());
    Ok(())
}

/// The desktop app's version, from `cargo metadata`.
fn app_version(meta: &Value) -> Result<String, String> {
    let packages = meta.get("packages").and_then(Value::as_array).ok_or("cargo metadata: no `packages`")?;
    let package = packages.iter().find(|p| p.get("name").and_then(Value::as_str) == Some(PACKAGE));
    let version = package.and_then(|p| p.get("version")).and_then(Value::as_str);
    version.map(String::from).ok_or_else(|| format!("cargo metadata: no version for package `{PACKAGE}`"))
}

/// Builds the release binary and returns the path cargo reports for it. Asking cargo, rather than
/// assuming `target/release`, stays right when the target directory or the target is configured.
fn build() -> Result<PathBuf, String> {
    let what = format!("cargo build --release -p {PACKAGE}");
    eprintln!("$ {what}");
    let mut c = super::cargo();
    c.args(["build", "--release", "-p", PACKAGE, "--message-format", "json-render-diagnostics"]);
    // `output()` captures stdout, the JSON messages; cargo's progress and diagnostics pass through.
    c.stderr(Stdio::inherit());
    let out = c.output().map_err(|e| format!("could not start `{what}`: {e}"))?;
    if !out.status.success() {
        return Err(format!("`{what}` failed ({})", out.status));
    }
    executable(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| format!("`{what}` did not report an executable for `{PACKAGE}`"))
}

/// The desktop app's executable in cargo's JSON messages (one JSON object per line).
fn executable(messages: &str) -> Option<PathBuf> {
    let messages = messages.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok());
    let mut ours = messages.filter(|m| m.pointer("/target/name").and_then(Value::as_str) == Some(PACKAGE));
    ours.find_map(|m| m.get("executable").and_then(Value::as_str).map(PathBuf::from))
}

/// Writes the bundle into `dist` and returns its path.
fn assemble(dist: &Path, binary: &Path, version: &str) -> Result<PathBuf, String> {
    let app = dist.join(BUNDLE);
    // Start from nothing, so no file of an earlier bundle ends up inside this one's signature.
    match std::fs::remove_dir_all(&app) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", app.display())),
    }
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    std::fs::create_dir_all(&macos).map_err(|e| format!("{}: {e}", macos.display()))?;
    let target = macos.join(PACKAGE);
    std::fs::copy(binary, &target).map_err(|e| format!("copying {} to {}: {e}", binary.display(), target.display()))?;
    let plist = contents.join("Info.plist");
    std::fs::write(&plist, info_plist(version)).map_err(|e| format!("{}: {e}", plist.display()))?;
    Ok(app)
}

/// `Info.plist` for `version`, a cargo (semver) version. Bundle versions are numbers and dots
/// only, so a pre-release or build suffix is left out: `0.1.0-rc.1` is bundled as `0.1.0`.
fn info_plist(version: &str) -> String {
    let version = version.split(['-', '+']).next().unwrap_or(version);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>{PACKAGE}</string>
  <key>CFBundleIdentifier</key>
  <string>{BUNDLE_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>CircuitCraft</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>{version}</string>
  <key>CFBundleVersion</key>
  <string>{version}</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder, removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("circuitcraft-xtask-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_plist_names_the_binary_and_a_numeric_version() {
        let plist = info_plist("0.1.0-rc.1+build.5");
        assert!(plist.contains("<key>CFBundleExecutable</key>\n  <string>circuitcraft</string>"));
        assert!(plist.contains("<key>CFBundleShortVersionString</key>\n  <string>0.1.0</string>"));
        assert!(plist.contains("<key>CFBundleVersion</key>\n  <string>0.1.0</string>"));
        assert!(info_plist("2.3.4").contains("<string>2.3.4</string>"));
    }

    #[test]
    fn the_version_comes_from_the_desktop_app_package() {
        let meta = serde_json::json!({"packages": [
            {"name": "circuitcraft-cli", "version": "9.9.9"},
            {"name": "circuitcraft", "version": "0.0.1"},
        ]});
        assert_eq!(app_version(&meta).unwrap(), "0.0.1");
        assert!(app_version(&serde_json::json!({"packages": []})).is_err());
        assert!(app_version(&serde_json::json!({})).is_err());
    }

    #[test]
    fn the_executable_is_read_from_cargo_messages() {
        let messages = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"circuitcraft_engine"},"executable":null}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"circuitcraft-cli"},"executable":"/t/release/circuitcraft-cli"}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"circuitcraft"},"executable":"/t/release/circuitcraft"}"#,
            "\n",
            "not json\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        assert_eq!(executable(messages), Some(PathBuf::from("/t/release/circuitcraft")));
        assert_eq!(executable(r#"{"reason":"build-finished","success":true}"#), None);
    }

    #[test]
    fn assembling_lays_out_the_bundle_and_replaces_an_earlier_one() {
        let tmp = TempDir::new("bundle");
        let binary = tmp.0.join("built");
        std::fs::write(&binary, b"binary").unwrap();
        let dist = tmp.0.join("dist");
        let stale = dist.join(BUNDLE).join("Contents").join("Resources").join("stale");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, b"left over").unwrap();

        let app = assemble(&dist, &binary, "0.0.1").unwrap();

        assert_eq!(app, dist.join(BUNDLE));
        assert_eq!(std::fs::read(app.join("Contents").join("MacOS").join("circuitcraft")).unwrap(), b"binary");
        assert_eq!(std::fs::read_to_string(app.join("Contents").join("Info.plist")).unwrap(), info_plist("0.0.1"));
        assert!(!stale.exists());
    }

    /// The only check that the template is a property list macOS accepts.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_accepts_the_plist() {
        let tmp = TempDir::new("plist");
        let plist = tmp.0.join("Info.plist");
        std::fs::write(&plist, info_plist("0.0.1")).unwrap();
        let lint = Command::new("plutil").arg("-lint").arg(&plist).output().unwrap();
        assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
    }
}
