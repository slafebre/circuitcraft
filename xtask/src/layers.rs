//! Crate layering. A crate may depend only on crates in lower layers (or earlier in an explicit
//! chain within its own layer), and nothing below the UI layer may depend on a UI toolkit. This is
//! what keeps the front end swappable and the engine testable headless.

use serde_json::Value;

/// Where a workspace crate sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A layered library crate.
    Layer(u8),
    /// Apps and tooling: exempt from the rules.
    Exempt,
}

/// The layering table, by package name without the `circuitcraft-` prefix. Names not created yet
/// are reserved here so their place is decided before the first line of code.
pub const TABLE: &[(&str, Class)] = &[
    // L0 foundation
    ("units", Class::Layer(0)),
    ("geom", Class::Layer(0)),
    // L1 the design database
    ("model", Class::Layer(1)),
    // L2 content, rules and drawing
    ("library", Class::Layer(2)),
    ("rules", Class::Layer(2)),
    ("render", Class::Layer(2)),
    // L3 algorithms and formats
    ("drc", Class::Layer(3)),
    ("pour", Class::Layer(3)),
    ("router", Class::Layer(3)),
    ("io", Class::Layer(3)),
    ("sim", Class::Layer(3)),
    // L4 commands and sessions
    ("engine", Class::Layer(4)),
    // L5 front ends
    ("ui-egui", Class::Layer(5)),
    ("mcp", Class::Layer(5)),
    // apps and tooling
    ("circuitcraft", Class::Exempt),
    ("cli", Class::Exempt),
    ("web", Class::Exempt),
    ("xtask", Class::Exempt),
];

/// Allowed orderings within a layer: a crate may use crates earlier in its chain.
pub const INTRA_LAYER_ORDER: &[&[&str]] = &[&["units", "geom"], &["rules", "library"], &["pour", "drc", "router"]];

/// External crates that make up a UI toolkit or windowing system.
pub const UI_CRATES: &[&str] = &["egui", "eframe", "egui-wgpu", "egui-winit", "egui_kittest", "wgpu", "winit", "rfd"];

/// The first layer that may use [`UI_CRATES`].
pub const UI_MIN_LAYER: u8 = 5;

/// A workspace crate and the dependencies that matter for layering.
#[derive(Debug, Clone)]
pub struct Crate {
    /// Package name.
    pub name: String,
    /// Normal and build dependencies (dev-dependencies are exempt).
    pub deps: Vec<String>,
}

/// The package name without the `circuitcraft-` prefix.
pub fn short_name(pkg: &str) -> &str {
    pkg.strip_prefix("circuitcraft-").unwrap_or(pkg)
}

fn class(pkg: &str) -> Option<Class> {
    let short = short_name(pkg);
    TABLE.iter().find(|(n, _)| *n == short).map(|(_, c)| *c)
}

fn intra_allowed(from: &str, to: &str) -> bool {
    let (from, to) = (short_name(from), short_name(to));
    INTRA_LAYER_ORDER.iter().any(|chain| match (chain.iter().position(|n| *n == from), chain.iter().position(|n| *n == to)) {
        (Some(f), Some(t)) => t < f,
        _ => false,
    })
}

/// Builds the crate list from `cargo metadata --no-deps` output.
pub fn from_metadata(meta: &Value) -> Result<Vec<Crate>, String> {
    let packages = meta.get("packages").and_then(Value::as_array).ok_or("cargo metadata has no packages")?;
    let mut crates = Vec::new();
    for p in packages {
        let name = p.get("name").and_then(Value::as_str).ok_or("package without a name")?;
        let deps = p
            .get("dependencies")
            .and_then(Value::as_array)
            .map(|ds| {
                ds.iter()
                    .filter(|d| d.get("kind").and_then(Value::as_str) != Some("dev"))
                    .filter_map(|d| d.get("name").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        crates.push(Crate { name: name.to_owned(), deps });
    }
    Ok(crates)
}

/// Every layering violation, as a readable line.
pub fn check(crates: &[Crate]) -> Vec<String> {
    let workspace: Vec<&str> = crates.iter().map(|c| c.name.as_str()).collect();
    let mut out = Vec::new();
    for c in crates {
        let Some(own) = class(&c.name) else {
            out.push(format!("{}: not in the layer table (xtask/src/layers.rs)", c.name));
            continue;
        };
        let Class::Layer(own_layer) = own else { continue };
        for dep in &c.deps {
            if own_layer < UI_MIN_LAYER && UI_CRATES.contains(&dep.as_str()) {
                out.push(format!("{} (L{own_layer}) depends on UI crate {dep}; only L{UI_MIN_LAYER}+ may", c.name));
            }
            if !workspace.contains(&dep.as_str()) {
                continue;
            }
            match class(dep) {
                Some(Class::Layer(dep_layer)) => {
                    let ok = dep_layer < own_layer || (dep_layer == own_layer && intra_allowed(&c.name, dep));
                    if !ok {
                        out.push(format!("{} (L{own_layer}) may not depend on {dep} (L{dep_layer})", c.name));
                    }
                }
                Some(Class::Exempt) => out.push(format!("{} may not depend on app or tool {dep}", c.name)),
                None => {} // reported on its own entry
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn krate(name: &str, deps: &[&str]) -> Crate {
        Crate { name: name.into(), deps: deps.iter().map(|d| (*d).into()).collect() }
    }

    #[test]
    fn downward_and_chained_deps_pass() {
        let cs = [
            krate("circuitcraft-units", &[]),
            krate("circuitcraft-geom", &["circuitcraft-units"]),
            krate("circuitcraft-model", &["circuitcraft-geom", "serde"]),
            krate("circuitcraft-ui-egui", &["circuitcraft-model", "egui"]),
        ];
        assert!(check(&cs).is_empty(), "{:?}", check(&cs));
    }

    #[test]
    fn upward_sideways_ui_and_unknown_fail() {
        let cs = [
            krate("circuitcraft-units", &["circuitcraft-geom"]),
            krate("circuitcraft-geom", &[]),
            krate("circuitcraft-io", &["circuitcraft-sim", "egui"]),
            krate("circuitcraft-sim", &[]),
            krate("circuitcraft-mystery", &[]),
        ];
        let v = check(&cs);
        assert_eq!(v.len(), 4, "{v:?}");
    }
}
