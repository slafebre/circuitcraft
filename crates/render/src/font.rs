//! A single-stroke font for text on the board: silkscreen, fabrication drawings, notes.
//!
//! The glyphs are "CADCraft Stroke", an original font designed for CADCraft, one of the ArtCraft
//! Crafting Apps, on a small grid and dedicated to the public domain (CC0-1.0); see
//! ATTRIBUTION.md. Each glyph is an advance and strokes: a stroke is a run of points, each two
//! characters, an x digit (`0` to `6`) and a y letter (`a` is -2, `c` the baseline at 0, `g` the
//! x-height at 4, `i` the cap height at 6, and up to `l` at 9 for accents). The advance is in
//! tenths of a grid unit.
//!
//! [`layout`] lays a line out in nanometres for a given character height (the cap height).

use circuitcraft_model::Justify;
use circuitcraft_units::{Nm, Point};

/// The cap height, in grid units: a text's height is this many units.
const CAP: i64 = 6;

/// The gap between two characters, in tenths of a grid unit.
const GAP: i64 = 16;

/// The distance from one baseline to the next, in grid units.
const LINE: i64 = 10;

/// The glyph drawn for a character the font doesn't have.
const MISSING: char = '?';

/// The most lines a text is laid out with; the rest is not drawn. A board text has at most
/// [`circuitcraft_model::MAX_NAME_CHARS`] characters, so this only bounds a text made of line breaks.
const MAX_LINES: usize = 256;

/// The advance and the strokes of a character, if the font has it.
fn glyph(c: char) -> Option<(u8, &'static str)> {
    let g: (u8, &str) = match c {
        ' ' => (24, ""),
        'A' => (40, "0c2i4c 1f3f"),
        'B' => (40, "0c0i3i4h4g3f0f 3f4e4d3c0c"),
        'C' => (40, "4h3i1i0h0d1c3c4d"),
        'D' => (40, "0c0i3i4h4d3c0c"),
        'E' => (40, "4i0i0c4c 0f3f"),
        'F' => (40, "4i0i0c 0f3f"),
        'G' => (40, "4h3i1i0h0d1c3c4d4f2f"),
        'H' => (40, "0c0i 4c4i 0f4f"),
        'I' => (20, "0i2i 1i1c 0c2c"),
        'J' => (40, "4i4d3c1c0d0e"),
        'K' => (40, "0c0i 4i0e 1f4c"),
        'L' => (40, "0i0c4c"),
        'M' => (40, "0c0i2f4i4c"),
        'N' => (40, "0c0i4c4i"),
        'O' => (40, "1c0d0h1i3i4h4d3c1c"),
        'P' => (40, "0c0i3i4h4g3f0f"),
        'Q' => (40, "1c0d0h1i3i4h4d3c1c 2e4b"),
        'R' => (40, "0c0i3i4h4g3f0f 2f4c"),
        'S' => (40, "4h3i1i0h0g1f3f4e4d3c1c0d"),
        'T' => (40, "0i4i 2i2c"),
        'U' => (40, "0i0d1c3c4d4i"),
        'V' => (40, "0i2c4i"),
        'W' => (40, "0i1c2f3c4i"),
        'X' => (40, "0i4c 0c4i"),
        'Y' => (40, "0i2f4i 2f2c"),
        'Z' => (40, "0i4i0c4c"),
        '0' => (40, "1c0d0h1i3i4h4d3c1c"),
        '1' => (40, "1h2i2c 1c3c"),
        '2' => (40, "0h1i3i4h4g0c4c"),
        '3' => (40, "0h1i3i4h4g3f2f 3f4e4d3c1c0d"),
        '4' => (40, "3c3i0e4e"),
        '5' => (40, "4i0i0f3f4e4d3c1c0d"),
        '6' => (40, "4h3i1i0h0d1c3c4d4e3f1f0e"),
        '7' => (40, "0i4i1c"),
        '8' => (40, "1f0g0h1i3i4h4g3f1f0e0d1c3c4d4e3f"),
        '9' => (40, "4f1f0g0h1i3i4h4d3c1c0d"),
        'a' => (40, "4g4c 4f3g1g0f0d1c3c4d"),
        'b' => (40, "0i0c 0d1c3c4d4f3g1g0f"),
        'c' => (40, "4f3g1g0f0d1c3c4d"),
        'd' => (40, "4i4c 4d3c1c0d0f1g3g4f"),
        'e' => (40, "0e4e4f3g1g0f0d1c3c4d"),
        'f' => (30, "3i2i1h1c 0g3g"),
        'g' => (40, "4g4b3a1a0b 4f3g1g0f0d1c3c4d"),
        'h' => (40, "0i0c 0f1g3g4f4c"),
        'i' => (10, "0c0g 0h0i"),
        'j' => (20, "2g2b1a0a 2h2i"),
        'k' => (40, "0i0c 3g0d 1e3c"),
        'l' => (20, "1i1d2c"),
        'm' => (40, "0c0g 0f1g2f2c 2f3g4f4c"),
        'n' => (40, "0c0g 0f1g3g4f4c"),
        'o' => (40, "1c0d0f1g3g4f4d3c1c"),
        'p' => (40, "0g0a 0f1g3g4f4d3c1c0d"),
        'q' => (40, "4g4a 4f3g1g0f0d1c3c4d"),
        'r' => (30, "0c0g 0e2g3g"),
        's' => (40, "4f3g1g0f1e3e4d3c1c0d"),
        't' => (30, "1i1d2c3c 0g3g"),
        'u' => (40, "0g0d1c3c4d 4g4c"),
        'v' => (40, "0g2c4g"),
        'w' => (40, "0g1c2f3c4g"),
        'x' => (40, "0g4c 0c4g"),
        'y' => (40, "0g2c 4g1a0a"),
        'z' => (40, "0g4g0c4c"),
        '!' => (10, "0i0e 0d0c"),
        '"' => (20, "0i0g 2i2g"),
        '#' => (40, "1c1i 3c3i 0e4e 0g4g"),
        '$' => (40, "4h3i1i0h0g1f3f4e4d3c1c0d 2i2c"),
        '%' => (40, "0c4i 0i0h1h1i0i 3c3d4d4c3c"),
        '&' => (40, "4c1h1i2i3h0e0d1c2c4e"),
        '\'' => (10, "0i0g"),
        '(' => (20, "2i1h0f0e1d2c"),
        ')' => (20, "0i1h2f2e1d0c"),
        '*' => (40, "2h2d 0g4e 0e4g"),
        '+' => (40, "2d2h 0f4f"),
        ',' => (10, "1d1c0b"),
        '-' => (40, "0f4f"),
        '.' => (10, "0c0d"),
        '/' => (40, "0c4i"),
        ':' => (10, "0c0d 0g0h"),
        ';' => (10, "1d1c0b 1g1h"),
        '<' => (40, "4h0f4d"),
        '=' => (40, "0e4e 0g4g"),
        '>' => (40, "0h4f0d"),
        '?' => (40, "0h1i3i4h4g2f2e 2d2c"),
        '@' => (40, "3e3g1g1e3e4f4h3i1i0h0d1c4c"),
        '[' => (20, "2i0i0c2c"),
        '\\' => (40, "0i4c"),
        ']' => (20, "0i2i2c0c"),
        '^' => (40, "0g2i4g"),
        '_' => (40, "0b4b"),
        '`' => (10, "0i1h"),
        '{' => (20, "2i1h1g0f1e1d2c"),
        '|' => (10, "0i0a"),
        '}' => (20, "0i1h1g2f1e1d0c"),
        '~' => (40, "0g1h3f4g"),
        '°' => (20, "1i0h1g2h1i"),
        '±' => (40, "2e2i 0g4g 0d4d"),
        'Ø' | 'ø' | '⌀' => (40, "1c0d0h1i3i4h4d3c1c 0c4i"),
        // List bullet: a small crossed diamond at mid x-height.
        '•' => (20, "1d0e1f2e1d 0e2e 1d1f"),
        // Original Latin diacritics, composed from our own letter bodies and marks.
        '\u{00C0}' => (40, concat!("0c2i4c 1f3f", " 1k3j")),
        '\u{00C1}' => (40, concat!("0c2i4c 1f3f", " 1j3k")),
        '\u{00C2}' => (40, concat!("0c2i4c 1f3f", " 0j2k4j")),
        '\u{00C3}' => (40, concat!("0c2i4c 1f3f", " 0j1k3j4k")),
        '\u{00C4}' => (40, concat!("0c2i4c 1f3f", " 1j1k 3j3k")),
        '\u{00C5}' => (40, concat!("0c2i4c 1f3f", " 2j1k2l3k2j")),
        '\u{00C7}' => (40, concat!("4h3i1i0h0d1c3c4d", " 2c3b2a1a")),
        '\u{00C8}' => (40, concat!("4i0i0c4c 0f3f", " 1k3j")),
        '\u{00C9}' => (40, concat!("4i0i0c4c 0f3f", " 1j3k")),
        '\u{00CA}' => (40, concat!("4i0i0c4c 0f3f", " 0j2k4j")),
        '\u{00CB}' => (40, concat!("4i0i0c4c 0f3f", " 1j1k 3j3k")),
        '\u{00CC}' => (20, concat!("0i2i 1i1c 0c2c", " 0k2j")),
        '\u{00CD}' => (20, concat!("0i2i 1i1c 0c2c", " 0j2k")),
        '\u{00CE}' => (20, concat!("0i2i 1i1c 0c2c", " 0j1k2j")),
        '\u{00CF}' => (20, concat!("0i2i 1i1c 0c2c", " 0j0k 2j2k")),
        '\u{00D1}' => (40, concat!("0c0i4c4i", " 0j1k3j4k")),
        '\u{00D2}' => (40, concat!("1c0d0h1i3i4h4d3c1c", " 1k3j")),
        '\u{00D3}' => (40, concat!("1c0d0h1i3i4h4d3c1c", " 1j3k")),
        '\u{00D4}' => (40, concat!("1c0d0h1i3i4h4d3c1c", " 0j2k4j")),
        '\u{00D5}' => (40, concat!("1c0d0h1i3i4h4d3c1c", " 0j1k3j4k")),
        '\u{00D6}' => (40, concat!("1c0d0h1i3i4h4d3c1c", " 1j1k 3j3k")),
        '\u{00D9}' => (40, concat!("0i0d1c3c4d4i", " 1k3j")),
        '\u{00DA}' => (40, concat!("0i0d1c3c4d4i", " 1j3k")),
        '\u{00DB}' => (40, concat!("0i0d1c3c4d4i", " 0j2k4j")),
        '\u{00DC}' => (40, concat!("0i0d1c3c4d4i", " 1j1k 3j3k")),
        '\u{00DD}' => (40, concat!("0i2f4i 2f2c", " 1j3k")),
        '\u{00E0}' => (40, concat!("4g4c 4f3g1g0f0d1c3c4d", " 1i3h")),
        '\u{00E1}' => (40, concat!("4g4c 4f3g1g0f0d1c3c4d", " 1h3i")),
        '\u{00E2}' => (40, concat!("4g4c 4f3g1g0f0d1c3c4d", " 0h2i4h")),
        '\u{00E3}' => (40, concat!("4g4c 4f3g1g0f0d1c3c4d", " 0h1i3h4i")),
        '\u{00E4}' => (40, concat!("4g4c 4f3g1g0f0d1c3c4d", " 1h1i 3h3i")),
        '\u{00E5}' => (40, concat!("4g4c 4f3g1g0f0d1c3c4d", " 2h1i2j3i2h")),
        '\u{00E7}' => (40, concat!("4f3g1g0f0d1c3c4d", " 2c3b2a1a")),
        '\u{00E8}' => (40, concat!("0e4e4f3g1g0f0d1c3c4d", " 1i3h")),
        '\u{00E9}' => (40, concat!("0e4e4f3g1g0f0d1c3c4d", " 1h3i")),
        '\u{00EA}' => (40, concat!("0e4e4f3g1g0f0d1c3c4d", " 0h2i4h")),
        '\u{00EB}' => (40, concat!("0e4e4f3g1g0f0d1c3c4d", " 1h1i 3h3i")),
        '\u{00EC}' => (10, concat!("0c0g", " 0i1h")),
        '\u{00ED}' => (10, concat!("0c0g", " 0h1i")),
        '\u{00EE}' => (10, concat!("0c0g", " 0h0i1h")),
        '\u{00EF}' => (10, concat!("0c0g", " 0h0i 1h1i")),
        '\u{00F1}' => (40, concat!("0c0g 0f1g3g4f4c", " 0h1i3h4i")),
        '\u{00F2}' => (40, concat!("1c0d0f1g3g4f4d3c1c", " 1i3h")),
        '\u{00F3}' => (40, concat!("1c0d0f1g3g4f4d3c1c", " 1h3i")),
        '\u{00F4}' => (40, concat!("1c0d0f1g3g4f4d3c1c", " 0h2i4h")),
        '\u{00F5}' => (40, concat!("1c0d0f1g3g4f4d3c1c", " 0h1i3h4i")),
        '\u{00F6}' => (40, concat!("1c0d0f1g3g4f4d3c1c", " 1h1i 3h3i")),
        '\u{00F9}' => (40, concat!("0g0d1c3c4d 4g4c", " 1i3h")),
        '\u{00FA}' => (40, concat!("0g0d1c3c4d 4g4c", " 1h3i")),
        '\u{00FB}' => (40, concat!("0g0d1c3c4d 4g4c", " 0h2i4h")),
        '\u{00FC}' => (40, concat!("0g0d1c3c4d 4g4c", " 1h1i 3h3i")),
        '\u{00FD}' => (40, concat!("0g2c 4g1a0a", " 1h3i")),
        '\u{00FF}' => (40, concat!("0g2c 4g1a0a", " 1h1i 3h3i")),
        '\u{0178}' => (40, concat!("0i2f4i 2f2c", " 1j1k 3j3k")),
        _ => return None,
    };
    Some(g)
}

/// A point of a glyph, in grid units.
fn grid_point(p: &[u8]) -> Option<(i64, i64)> {
    let x = i64::from(p.first()?.checked_sub(b'0')?);
    let y = i64::from(p.get(1)?.checked_sub(b'a')?) - 2;
    Some((x, y))
}

/// The strokes of `text` laid out for characters `height` high (the cap height), each a run of
/// points in nanometres around the text's anchor at the origin: the first line's baseline is at
/// y = 0 and each further line one line spacing below it; `justify` puts the anchor at the left
/// end, the middle or the right end of each line. Characters the font doesn't have are drawn as
/// a question mark, and a space or a tab leaves room.
pub fn layout(text: &str, height: Nm, justify: Justify) -> Vec<Vec<Point>> {
    let mut out = Vec::new();
    for (row, line) in text.split('\n').take(MAX_LINES).enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // Positions in tenths of a grid unit, then scaled: whole numbers until the last step.
        let mut glyphs = Vec::new();
        let mut x: i64 = 0;
        for c in line.chars() {
            let c = if c == '\t' { ' ' } else { c };
            let Some((advance, strokes)) = glyph(c).or_else(|| glyph(MISSING)) else { continue };
            if x > 0 {
                x += GAP;
            }
            glyphs.push((x, strokes));
            x += i64::from(advance);
        }
        let shift = match justify {
            Justify::Left => 0,
            Justify::Center => -x / 2,
            Justify::Right => -x,
        };
        let baseline = -i64::try_from(row).unwrap_or(0) * LINE * 10;
        for (at, strokes) in glyphs {
            for stroke in strokes.split(' ').filter(|s| !s.is_empty()) {
                let points: Vec<Point> = stroke
                    .as_bytes()
                    .chunks(2)
                    .filter_map(grid_point)
                    .filter_map(|(gx, gy)| scaled(at + shift + gx * 10, baseline + gy * 10, height))
                    .collect();
                if !points.is_empty() {
                    out.push(points);
                }
            }
        }
    }
    out
}

/// A point given in tenths of a grid unit, in nanometres for characters `height` high, or `None`
/// when it is beyond what a coordinate can hold.
fn scaled(x: i64, y: i64, height: Nm) -> Option<Point> {
    let scale = |v: i64| -> Option<Nm> {
        let nm = i128::from(v).checked_mul(i128::from(height.0))? / i128::from(CAP * 10);
        i64::try_from(nm).ok().map(Nm)
    };
    Some(Point::new(scale(x)?, scale(y)?))
}

/// How wide the longest line of `text` is for characters `height` high.
pub fn width(text: &str, height: Nm) -> Nm {
    let points = layout(text, height, Justify::Left);
    Nm(points.iter().flatten().map(|p| p.x.0).max().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: i64 = 1_000_000;

    #[test]
    fn every_printable_ascii_character_has_a_glyph_on_the_grid() {
        for c in ' '..='~' {
            let (advance, strokes) = glyph(c).unwrap_or_else(|| panic!("no glyph for {c:?}"));
            assert!(advance > 0, "{c:?}");
            for stroke in strokes.split(' ').filter(|s| !s.is_empty()) {
                assert_eq!(stroke.len() % 2, 0, "{c:?}: {stroke}");
                for p in stroke.as_bytes().chunks(2) {
                    let (x, y) = grid_point(p).unwrap_or_else(|| panic!("{c:?}: {stroke}"));
                    assert!((0..=6).contains(&x) && (-2..=9).contains(&y), "{c:?}: {stroke}");
                    assert!(x * 10 <= i64::from(advance), "{c:?} draws past its advance");
                }
            }
        }
    }

    #[test]
    fn a_line_is_laid_out_at_its_height_and_anchored_as_justified() {
        // "L" is a vertical stroke and a foot four units wide: 6 units is 1.2 mm here.
        let l = layout("L", Nm(1_200_000), Justify::Left);
        assert_eq!(l, vec![vec![Point::new(Nm(0), Nm(1_200_000)), Point::new(Nm(0), Nm(0)), Point::new(Nm(800_000), Nm(0))]]);
        // Two characters: 4 units, a gap of 1.6 and 4 more, so 9.6 units, 1.92 mm.
        assert_eq!(width("LL", Nm(1_200_000)), Nm(1_920_000));
        let right = layout("LL", Nm(1_200_000), Justify::Right);
        assert_eq!(right.iter().flatten().map(|p| p.x.0).max(), Some(0));
        let centre = layout("LL", Nm(1_200_000), Justify::Center);
        assert_eq!(centre.iter().flatten().map(|p| p.x.0).max(), Some(960_000));
        assert_eq!(centre.iter().flatten().map(|p| p.x.0).min(), Some(-960_000));
    }

    #[test]
    fn lines_go_down_and_unknown_characters_show_as_question_marks() {
        let two = layout("L\nL", Nm(MM * 6), Justify::Left);
        let lowest = two.iter().flatten().map(|p| p.y.0).min();
        assert_eq!(lowest, Some(-10 * MM), "the second baseline is a line spacing down");
        assert_eq!(layout("\u{4e2d}", Nm(MM), Justify::Left), layout("?", Nm(MM), Justify::Left));
        assert!(layout("", Nm(MM), Justify::Left).is_empty());
        assert!(layout("   ", Nm(MM), Justify::Left).is_empty());
        assert_eq!(width(" L", Nm(6 * MM)), Nm(4_000_000 + 1_600_000 + 2_400_000), "a space leaves room");
    }

    #[test]
    fn hostile_text_is_bounded() {
        let many = "\n".repeat(100_000) + "L";
        assert!(layout(&many, Nm(MM), Justify::Left).is_empty(), "lines past the limit are not drawn");
        // A height far beyond the design range doesn't overflow: points that don't fit are left out.
        let huge = layout(&"W".repeat(1_000), Nm(i64::MAX), Justify::Center);
        assert!(huge.iter().flatten().count() < layout(&"W".repeat(1_000), Nm(MM), Justify::Center).iter().flatten().count());
    }
}
