//! File formats: CircuitCraft's own project and board files ([`native`]), the storage they go
//! through ([`storage`]), zipped project folders ([`archive`]) and detection of every file the
//! app knows. Reading other programs' boards is the `circuitcraft-import` crate, published on its
//! own so other programs can use it; fabrication outputs arrive in 0.5.
//!
//! Every reader treats its input as hostile: no panics, bounded allocations, bounded recursion.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod archive;
#[cfg(test)]
mod fuzz;
pub mod native;
pub mod storage;

/// How many bytes [`detect`] looks at.
pub const SNIFF_LEN: usize = circuitcraft_import::SNIFF_LEN;

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
    /// KiCad schematic (`(kicad_sch …)`), as KiCad 6 and later write it.
    KicadSch,
    /// KiCad 5 schematic (`.sch`), recognised but not read.
    KicadLegacySch,
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
            Format::Eagle => circuitcraft_import::Format::Eagle.name(),
            Format::KicadPcb => circuitcraft_import::Format::KicadPcb.name(),
            Format::KicadSch => circuitcraft_import::Format::KicadSch.name(),
            Format::KicadLegacySch => circuitcraft_import::Format::KicadLegacySch.name(),
            Format::KicadSymbolLib => circuitcraft_import::Format::KicadSymbolLib.name(),
            Format::KicadFootprint => circuitcraft_import::Format::KicadFootprint.name(),
        }
    }
}

impl From<circuitcraft_import::Format> for Format {
    fn from(format: circuitcraft_import::Format) -> Format {
        match format {
            circuitcraft_import::Format::Eagle => Format::Eagle,
            circuitcraft_import::Format::KicadPcb => Format::KicadPcb,
            circuitcraft_import::Format::KicadSch => Format::KicadSch,
            circuitcraft_import::Format::KicadLegacySch => Format::KicadLegacySch,
            circuitcraft_import::Format::KicadSymbolLib => Format::KicadSymbolLib,
            circuitcraft_import::Format::KicadFootprint => Format::KicadFootprint,
        }
    }
}

/// Recognises a format from the first bytes of a file (only the first [`SNIFF_LEN`] are read).
/// Content decides, not the file name, so a renamed file is still recognised.
pub fn detect(bytes: &[u8]) -> Option<Format> {
    if let Some(format) = circuitcraft_import::detect(bytes) {
        return Some(format.into());
    }
    let head = bytes.get(..bytes.len().min(SNIFF_LEN)).unwrap_or_default();
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_programs_files_are_told_apart_by_the_import_crate() {
        assert_eq!(detect(b"(kicad_pcb (version 20240108))"), Some(Format::KicadPcb));
        assert_eq!(detect(b"<?xml version=\"1.0\"?>\n<eagle version=\"9.6.2\">"), Some(Format::Eagle));
        assert_eq!(detect(b"(something_else)"), None);
        for (theirs, ours) in [
            (circuitcraft_import::Format::Eagle, Format::Eagle),
            (circuitcraft_import::Format::KicadPcb, Format::KicadPcb),
            (circuitcraft_import::Format::KicadSch, Format::KicadSch),
            (circuitcraft_import::Format::KicadLegacySch, Format::KicadLegacySch),
            (circuitcraft_import::Format::KicadSymbolLib, Format::KicadSymbolLib),
            (circuitcraft_import::Format::KicadFootprint, Format::KicadFootprint),
        ] {
            assert_eq!(Format::from(theirs), ours);
            assert_eq!(ours.name(), theirs.name());
        }
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
