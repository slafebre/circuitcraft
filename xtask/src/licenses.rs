//! Every dependency's licence must be on the allowlist (AGENTS.md#licence-allowlist), or be an
//! accepted exception for that named crate. Exceptions are per crate, so a new dependency under
//! the same licence still needs a decision.

use serde_json::Value;

/// SPDX identifiers allowed for any dependency.
pub const ALLOWLIST: &[&str] = &[
    "ISC",
    "MIT",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Apache-2.0",
    "OFL-1.1",
    "Ubuntu-font-1.0",
    "Bitstream-Vera",
    "CC0-1.0",
    "CC-BY-4.0",
    "CC-BY-SA-4.0",
    "LicenseRef-Public-Domain",
];

/// Accepted exceptions: a licence outside the allowlist, the crates it is accepted for, and why.
/// Decided 2026-11-13 (ROADMAP.md, open decisions): all three come in through egui and eframe.
pub const EXCEPTIONS: &[(&str, &[&str], &str)] = &[
    (
        "Unicode-3.0",
        &[
            "icu_collections",
            "icu_locale_core",
            "icu_normalizer",
            "icu_normalizer_data",
            "icu_properties",
            "icu_properties_data",
            "icu_provider",
            "litemap",
            "potential_utf",
            "tinystr",
            "unicode-ident",
            "unicode-properties",
            "writeable",
            "yoke",
            "yoke-derive",
            "zerofrom",
            "zerofrom-derive",
            "zerotrie",
            "zerovec",
            "zerovec-derive",
        ],
        "Unicode data tables and the ICU4X crates behind URL handling and text properties",
    ),
    ("Zlib", &["foldhash", "slotmap", "zlib-rs"], "hashing, slot maps and compression used by egui and wgpu"),
    ("BSL-1.0", &["clipboard-win", "error-code"], "the Windows clipboard, used by eframe's clipboard feature"),
];

/// Expressions nest at most this deep; real ones are one or two levels.
const MAX_DEPTH: usize = 16;

/// Whether one licence identifier is allowed for `krate`.
fn id_allowed(id: &str, krate: &str) -> bool {
    ALLOWLIST.contains(&id) || EXCEPTIONS.iter().any(|(lic, crates, _)| *lic == id && crates.contains(&krate))
}

/// Whether an SPDX expression (with `OR`, `AND`, `WITH` and parentheses, or the legacy `/` for OR)
/// is satisfied for `krate`. A malformed expression is not allowed.
pub fn allowed(expr: &str, krate: &str) -> bool {
    let spaced = expr.replace('/', " OR ").replace('(', " ( ").replace(')', " ) ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut pos = 0;
    match parse_or(&tokens, &mut pos, krate, 0) {
        Some(ok) => ok && pos == tokens.len(),
        None => false,
    }
}

fn parse_or(t: &[&str], pos: &mut usize, krate: &str, depth: usize) -> Option<bool> {
    let mut ok = parse_and(t, pos, krate, depth)?;
    while t.get(*pos) == Some(&"OR") {
        *pos += 1;
        ok |= parse_and(t, pos, krate, depth)?;
    }
    Some(ok)
}

fn parse_and(t: &[&str], pos: &mut usize, krate: &str, depth: usize) -> Option<bool> {
    let mut ok = parse_factor(t, pos, krate, depth)?;
    while t.get(*pos) == Some(&"AND") {
        *pos += 1;
        ok &= parse_factor(t, pos, krate, depth)?;
    }
    Some(ok)
}

fn parse_factor(t: &[&str], pos: &mut usize, krate: &str, depth: usize) -> Option<bool> {
    if depth > MAX_DEPTH {
        return None;
    }
    let tok = *t.get(*pos)?;
    *pos += 1;
    if tok == "(" {
        let ok = parse_or(t, pos, krate, depth + 1)?;
        if t.get(*pos) != Some(&")") {
            return None;
        }
        *pos += 1;
        return Some(ok);
    }
    if matches!(tok, ")" | "OR" | "AND" | "WITH") {
        return None;
    }
    // `X WITH exception`: an exception only adds permissions, so X decides.
    if t.get(*pos) == Some(&"WITH") {
        t.get(*pos + 1)?;
        *pos += 2;
    }
    Some(id_allowed(tok, krate))
}

/// Checks every package in the full dependency graph (`cargo metadata` without `--no-deps`, which
/// covers every target platform).
pub fn check(meta: &Value) -> Result<(), String> {
    let packages = meta.get("packages").and_then(Value::as_array).ok_or("cargo metadata has no packages")?;
    let mut bad = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for p in packages {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let version = p.get("version").and_then(Value::as_str).unwrap_or("?");
        seen.push(name);
        match p.get("license").and_then(Value::as_str) {
            Some(expr) if allowed(expr, name) => {}
            Some(expr) => bad.push(format!("{name} {version}: {expr}")),
            None => bad.push(format!("{name} {version}: no SPDX licence expression")),
        }
    }
    for (lic, crates, _) in EXCEPTIONS {
        for c in crates.iter().filter(|c| !seen.contains(c)) {
            println!("licenses: note: the {lic} exception for {c} is no longer needed; remove it");
        }
    }
    if bad.is_empty() {
        println!("licenses: {} packages, all allowed", packages.len());
        Ok(())
    } else {
        for b in &bad {
            eprintln!("  not allowed: {b}");
        }
        Err(format!(
            "{} package(s) with a licence outside the allowlist; decide in ROADMAP.md and add an exception in xtask/src/licenses.rs",
            bad.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spdx_expressions() {
        assert!(allowed("MIT OR Apache-2.0", "x"));
        assert!(allowed("MIT/Apache-2.0", "x"));
        assert!(allowed("Unlicense/MIT", "x"));
        assert!(allowed("Apache-2.0 WITH LLVM-exception", "x"));
        assert!(!allowed("GPL-3.0-or-later", "x"));
        assert!(!allowed("MIT AND GPL-2.0", "x"));
        assert!(allowed("(MIT OR Apache-2.0) AND Unicode-3.0", "unicode-ident"));
        assert!(!allowed("(MIT OR Apache-2.0) AND Unicode-3.0", "some-new-crate"));
    }

    #[test]
    fn exceptions_are_per_crate() {
        assert!(allowed("Zlib", "foldhash"));
        assert!(!allowed("Zlib", "another-crate"));
        assert!(allowed("BSL-1.0", "clipboard-win"));
        assert!(!allowed("BSL-1.0", "boost-port"));
    }

    #[test]
    fn malformed_expressions_are_rejected() {
        for e in ["", "(", "MIT OR", "(MIT", "MIT)", "OR MIT", "MIT WITH", &"(".repeat(100)] {
            assert!(!allowed(e, "x"), "{e:?}");
        }
    }
}
