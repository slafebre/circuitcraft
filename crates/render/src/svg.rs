//! SVG output: a display list as a picture any browser shows.
//!
//! Coordinates are written in micrometres from whole nanometres, with integer arithmetic only, so
//! the same display list gives the same bytes on every machine and a reviewed picture can be
//! pinned by its checksum. The display list itself rounds a little floating point to whole
//! nanometres (curves, and rotations other than quarter turns); machines whose maths libraries
//! differ in the last bit could in principle round a point differently. The picture is at its
//! real size (its width and height are in millimetres) with y up, as on a fabrication drawing.

use crate::display::{BACKGROUND, DisplayList, Group, Shape};
use circuitcraft_geom::Rect;
use circuitcraft_units::{Nm, Point};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The room left around the board, at least: a millimetre.
const MIN_MARGIN: i64 = 1_000_000;

/// The area shown when there is nothing to draw: ten millimetres square around the origin.
const EMPTY: i64 = 5_000_000;

/// The display list as an SVG document.
pub fn svg(list: &DisplayList) -> String {
    let frame = frame(list.bounds);
    let mut out = String::with_capacity(4_096);
    let (x, y) = (frame.min.x.0, frame.max.y.0.saturating_neg());
    let (w, h) = (frame.width().0, frame.height().0);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="{}mm" height="{}mm">"#,
        um(x),
        um(y),
        um(w),
        um(h),
        mm(w),
        mm(h)
    );
    let _ = writeln!(out, r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#, um(x), um(y), um(w), um(h), BACKGROUND.hex());
    for group in &list.groups {
        write_group(&mut out, group);
    }
    out.push_str("</svg>\n");
    out
}

/// The area the picture shows: the board's bounds and a margin around them.
pub(crate) fn frame(bounds: Option<Rect>) -> Rect {
    let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
    let Some(b) = bounds else { return Rect::from_corners(p(-EMPTY, -EMPTY), p(EMPTY, EMPTY)) };
    let margin = (b.width().0.max(b.height().0) / 50).max(MIN_MARGIN);
    Rect::from_corners(
        p(b.min.x.0.saturating_sub(margin), b.min.y.0.saturating_sub(margin)),
        p(b.max.x.0.saturating_add(margin), b.max.y.0.saturating_add(margin)),
    )
}

fn write_group(out: &mut String, group: &Group) {
    let [.., alpha] = group.color.0;
    let opacity = if alpha == 255 { String::new() } else { format!(r#" opacity="{}""#, fraction(alpha)) };
    let _ = writeln!(out, r#"<g data-name="{}" color="{}" fill="currentColor"{opacity}>"#, escape(&group.name), group.color.hex());
    // Lines of one width go in one path: same colour, same group, so nothing shows through.
    let mut lines: BTreeMap<i64, String> = BTreeMap::new();
    for shape in &group.shapes {
        match shape {
            Shape::Stroke { points, width } => {
                let d = lines.entry(width.0).or_default();
                for (i, p) in points.iter().enumerate() {
                    let _ = write!(d, "{}{} {}", if i == 0 { "M" } else { "L" }, um(p.x.0), um(p.y.0.saturating_neg()));
                }
            }
            Shape::Disc { center, radius } => {
                let _ = writeln!(out, r#"<circle cx="{}" cy="{}" r="{}"/>"#, um(center.x.0), um(center.y.0.saturating_neg()), um(radius.0));
            }
            Shape::Area(polygon) => {
                let mut d = String::new();
                for ring in std::iter::once(&polygon.outer).chain(&polygon.holes).filter(|r| !r.is_empty()) {
                    for (i, p) in ring.iter().enumerate() {
                        let _ = write!(d, "{}{} {}", if i == 0 { "M" } else { "L" }, um(p.x.0), um(p.y.0.saturating_neg()));
                    }
                    d.push('Z');
                }
                if !d.is_empty() {
                    let _ = writeln!(out, r#"<path fill-rule="evenodd" d="{d}"/>"#);
                }
            }
        }
    }
    for (width, d) in lines {
        let _ = writeln!(
            out,
            r#"<path fill="none" stroke="currentColor" stroke-width="{}" stroke-linecap="round" stroke-linejoin="round" d="{d}"/>"#,
            um(width)
        );
    }
    out.push_str("</g>\n");
}

/// Nanometres as micrometres, exactly: `1234567` is `1234.567`, `-500` is `-0.5`.
fn um(nm: i64) -> String {
    decimal(nm, 1_000)
}

/// Nanometres as millimetres, exactly.
fn mm(nm: i64) -> String {
    decimal(nm, 1_000_000)
}

/// `value / unit` written exactly, without trailing zeros. `unit` is a power of ten.
fn decimal(value: i64, unit: i128) -> String {
    let v = i128::from(value);
    let (whole, part) = (v.abs() / unit, v.abs() % unit);
    let sign = if v < 0 { "-" } else { "" };
    if part == 0 {
        return format!("{sign}{whole}");
    }
    let digits = unit.to_string().len() - 1;
    let part = format!("{part:0digits$}");
    format!("{sign}{whole}.{}", part.trim_end_matches('0'))
}

/// An opacity from 0 to 255 as a fraction with up to three decimals, rounded.
fn fraction(alpha: u8) -> String {
    let thousandths = (u32::from(alpha) * 1_000 + 127) / 255;
    decimal(i64::from(thousandths), 1_000)
}

/// Text that is safe inside an attribute value.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no way to write most control characters, even escaped.
            c if c.is_control() || matches!(c, '\u{fffe}' | '\u{ffff}') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_exactly_and_without_trailing_zeros() {
        assert_eq!([um(0), um(1_000), um(1_234_567), um(-500), um(1), um(-1_000_010)], ["0", "1", "1234.567", "-0.5", "0.001", "-1000.01"]);
        assert_eq!([mm(1_600_000), mm(25_400_000), mm(1)], ["1.6", "25.4", "0.000001"]);
        assert_eq!(um(i64::MIN), "-9223372036854775.808");
        assert_eq!([fraction(255), fraction(160), fraction(0), fraction(1)], ["1", "0.627", "0", "0.004"]);
    }

    #[test]
    fn names_are_escaped() {
        assert_eq!(escape(r#"A & "B" <c> 'd'"#), "A &amp; &quot;B&quot; &lt;c&gt; &apos;d&apos;");
        assert_eq!(escape("a\u{0}b\nc\u{ffff}"), "a b c ");
    }

    #[test]
    fn a_board_is_written_the_same_way_every_time() {
        use circuitcraft_model::layer_ids::{TOP_COPPER, TOP_SILK};
        use circuitcraft_model::{Board, Justify, Rotation, Text, TrackPath};
        let mut b = Board::new("x");
        b.add_track(TOP_COPPER, Nm(250_000), TrackPath::Line { from: Point::new(Nm(0), Nm(0)), to: Point::new(Nm(5_000_000), Nm(1_000_000)) }, None)
            .expect("track");
        b.add_via(Point::new(Nm(5_000_000), Nm(1_000_000)), Nm(600_000), Nm(300_000), None).expect("via");
        let text = Text {
            id: circuitcraft_model::ItemId::from_u128(7),
            layer: TOP_SILK,
            text: "R&D".to_owned(),
            position: Point::new(Nm(0), Nm(2_000_000)),
            height: Nm(1_000_000),
            stroke: Nm(150_000),
            rotation: Rotation(0),
            mirrored: false,
            justify: Justify::Left,
        };
        b.texts.insert(text).expect("room");
        let list = crate::display_list(&b, &crate::Options::all(&b));
        let first = svg(&list);
        assert_eq!(svg(&crate::display_list(&b, &crate::Options::all(&b))), first);
        // The track is a line in the copper's colour, y turned over; the via a disc; the hole a
        // disc in the background colour, last.
        assert!(first.contains(r##"<g data-name="Top Copper" color="#e8483c" fill="currentColor">"##), "{first}");
        assert!(first.contains(r#"stroke-width="250" stroke-linecap="round" stroke-linejoin="round" d="M0 0L5000 -1000"/>"#), "{first}");
        assert!(first.contains(r#"<circle cx="5000" cy="-1000" r="300"/>"#));
        let holes = first.rfind(r##"<g data-name="Holes" color="#101216""##).expect("holes");
        assert!(first[holes..].contains(r#"<circle cx="5000" cy="-1000" r="150"/>"#) && first.ends_with("</g>\n</svg>\n"));
        // Every element that is opened is closed.
        assert_eq!(first.matches("<g ").count(), first.matches("</g>").count());
        assert_eq!(first.matches("<svg ").count(), 1);
    }

    #[test]
    fn nothing_to_draw_is_ten_millimetres_of_background() {
        let text = svg(&DisplayList { groups: Vec::new(), bounds: None });
        assert_eq!(
            text,
            concat!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-5000 -5000 10000 10000" width="10mm" height="10mm">"#,
                "\n",
                r##"<rect x="-5000" y="-5000" width="10000" height="10000" fill="#101216"/>"##,
                "\n</svg>\n"
            )
        );
    }
}
