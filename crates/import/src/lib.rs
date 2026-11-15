//! Reads boards that KiCad and EAGLE wrote into CircuitCraft's board model
//! ([`circuitcraft_model::Board`]): lengths in integer nanometres, arcs exact, every footprint's
//! pads, texts and drawings placed on the board, and a board that has passed
//! [`Board::validate`](circuitcraft_model::Board::validate).
//!
//! An import is read-only and lossy by design. A reader keeps what the model can hold, says in
//! [`Notes`] what it left out or approximated, and never guesses silently.
//!
//! Every reader treats its input as hostile: no panics, bounded allocations, bounded recursion.
//! CircuitCraft's fuzz tests (in its `circuitcraft-io` crate) feed each reader thousands of
//! damaged files from a fixed seed and check that every board that comes out is valid.
//!
//! ```
//! let text = "(kicad_pcb (version 20221018) (generator pcbnew))";
//! assert_eq!(circuitcraft_import::detect(text.as_bytes()), Some(circuitcraft_import::Format::KicadPcb));
//! let imported = circuitcraft_import::import_board(text, "Empty").map_err(|e| e.to_string())?;
//! assert_eq!(imported.board.name, "Empty");
//! assert!(imported.board.footprints.is_empty());
//! for line in imported.notes.lines() {
//!     println!("left out: {line}");
//! }
//! # Ok::<(), String>(())
//! ```
//!
//! The readers are written from the formats' public documentation and from files we made
//! ourselves, never from either program's source; see the crate's README.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod eagle;
mod import;
pub mod kicad;
mod sexp;
mod xml;

pub use import::{ImportError, Imported, MAX_NOTES, Notes};

/// Reads a board another program wrote, telling which by its content. `name` becomes the
/// board's name. The file it came from is never written.
pub fn import_board(text: &str, name: &str) -> Result<Imported, ImportError> {
    match detect(text.as_bytes()) {
        Some(Format::KicadPcb) => kicad::read_board(text, name),
        Some(Format::Eagle) => eagle::read_board(text, name),
        _ => Err(ImportError::WrongFormat("a KiCad or EAGLE board")),
    }
}

/// How many bytes [`detect`] looks at.
pub const SNIFF_LEN: usize = 4096;

/// A file format of another program that [`detect`] recognises. Only boards are read so far;
/// the others are recognised so that a caller can say what a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
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
    None
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
    fn other_files_are_not_recognised() {
        assert_eq!(detect(b"{\"format\": \"circuitcraft.board\"}"), None);
        assert_eq!(detect(b"G04 a Gerber file*"), None);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for bytes in [&b""[..], b"(", b"<", b"<eagle", b"\xff\xfe\x00", &[b'('; 10_000], &[b'<'; 10_000]] {
            let _ = detect(bytes);
        }
    }

    #[test]
    fn only_boards_are_imported() {
        assert_eq!(import_board("(kicad_sch (version 1))", "x"), Err(ImportError::WrongFormat("a KiCad or EAGLE board")));
        assert_eq!(import_board("", "x"), Err(ImportError::WrongFormat("a KiCad or EAGLE board")));
    }
}
