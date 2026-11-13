//! File formats: CircuitCraft's own project and board files ([`native`]), detection, EAGLE and
//! KiCad board import in 0.1, fabrication outputs in 0.5.
//!
//! Every reader treats its input as hostile: no panics, bounded allocations, bounded recursion.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod native;

/// How many bytes [`detect`] looks at.
pub const SNIFF_LEN: usize = 4096;

/// A file format CircuitCraft recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// A CircuitCraft board (`.ccpcb`).
    CircuitCraftBoard,
    /// A CircuitCraft project (`.ccproj`).
    CircuitCraftProject,
    /// EAGLE board, schematic or library (XML with an `<eagle>` root).
    Eagle,
    /// KiCad board (`(kicad_pcb …)`).
    KicadPcb,
    /// KiCad schematic (`(kicad_sch …)`).
    KicadSch,
    /// KiCad symbol library (`(kicad_symbol_lib …)`).
    KicadSymbolLib,
    /// KiCad footprint (`(footprint …)` or the older `(module …)`).
    KicadFootprint,
}

impl Format {
    /// A human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Format::CircuitCraftBoard => "CircuitCraft board",
            Format::CircuitCraftProject => "CircuitCraft project",
            Format::Eagle => "EAGLE",
            Format::KicadPcb => "KiCad board",
            Format::KicadSch => "KiCad schematic",
            Format::KicadSymbolLib => "KiCad symbol library",
            Format::KicadFootprint => "KiCad footprint",
        }
    }
}

/// Recognises a format from the first bytes of a file (only the first [`SNIFF_LEN`] are read).
/// Content decides, not the file name, so a renamed file is still recognised.
pub fn detect(bytes: &[u8]) -> Option<Format> {
    let head = bytes.get(..bytes.len().min(SNIFF_LEN)).unwrap_or_default();
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if let Some(rest) = text.strip_prefix('(') {
        let word: String = rest.trim_start().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        return match word.as_str() {
            "kicad_pcb" => Some(Format::KicadPcb),
            "kicad_sch" => Some(Format::KicadSch),
            "kicad_symbol_lib" => Some(Format::KicadSymbolLib),
            "footprint" | "module" => Some(Format::KicadFootprint),
            _ => None,
        };
    }
    if text.starts_with('<') && has_eagle_root(text) {
        return Some(Format::Eagle);
    }
    if text.starts_with('{') {
        return match native_format(text) {
            Some(native::BOARD_FORMAT) => Some(Format::CircuitCraftBoard),
            Some(native::PROJECT_FORMAT) => Some(Format::CircuitCraftProject),
            _ => None,
        };
    }
    None
}

/// The value of `"format"` when it is the first key of a JSON object, as native files write it;
/// a `"format"` key anywhere else (nested, or after other keys) doesn't count.
fn native_format(text: &str) -> Option<&str> {
    let after = text.strip_prefix('{')?.trim_start().strip_prefix("\"format\"")?;
    let value = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    value.split_once('"').map(|(v, _)| v)
}

/// Whether an XML prefix opens an `<eagle` element (after the declaration, comments and doctype).
fn has_eagle_root(text: &str) -> bool {
    text.match_indices('<').any(|(i, _)| {
        text.get(i + 1..)
            .and_then(|after| after.strip_prefix("eagle"))
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kicad_by_content() {
        assert_eq!(detect(b"(kicad_pcb (version 20240108) (generator x))"), Some(Format::KicadPcb));
        assert_eq!(detect(b"\xef\xbb\xbf  (kicad_sch (version 1))"), Some(Format::KicadSch));
        assert_eq!(detect(b"(kicad_symbol_lib)"), Some(Format::KicadSymbolLib));
        assert_eq!(detect(b"(footprint \"R_0603\")"), Some(Format::KicadFootprint));
        assert_eq!(detect(b"(something_else)"), None);
    }

    #[test]
    fn detects_eagle_xml() {
        let xml = b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE eagle SYSTEM \"eagle.dtd\">\n<eagle version=\"9.6.2\">";
        assert_eq!(detect(xml), Some(Format::Eagle));
        assert_eq!(detect(b"<eagles>"), None);
        assert_eq!(detect(b"<svg></svg>"), None);
    }

    #[test]
    fn detects_native_files() {
        assert_eq!(detect(b"{\n  \"format\": \"circuitcraft.board\",\n  \"version\": 1"), Some(Format::CircuitCraftBoard));
        assert_eq!(detect(b"{\"format\":\"circuitcraft.project\"}"), Some(Format::CircuitCraftProject));
        assert_eq!(detect(b"{\"format\": \"something.else\"}"), None);
        assert_eq!(detect(b"{\"name\": 1}"), None);
        assert_eq!(detect(b"{\"name\":\"x\",\"config\":{\"format\":\"circuitcraft.board\"}}"), None, "not the first key");
        assert_eq!(detect(b"{\"version\": 1, \"format\": \"circuitcraft.board\"}"), None, "not the first key");
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for bytes in [&b""[..], b"(", b"<", b"<eagle", b"\xff\xfe\x00", &[b'('; 10_000], b"{\"format\"", b"{\"format\":\"x", &[b'{'; 10_000]] {
            let _ = detect(bytes);
        }
    }
}
