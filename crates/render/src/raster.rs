//! PNG output: a display list drawn into pixels on the CPU, with no GPU and no window.
//!
//! The picture shows the same area as the SVG ([`crate::svg`]), its longer side a given number of
//! pixels and the other following from the board's proportions. Each group is drawn on its own,
//! over just the area it covers, and then laid over the picture with its opacity, so a group's
//! shapes don't show through each other and a small group costs little however large the picture.

use crate::RenderError;
use crate::display::{BACKGROUND, DisplayList, Shape};
use crate::svg::frame;
use circuitcraft_geom::Rect;
use circuitcraft_units::Point;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

/// The most pixels a picture may have on a side.
pub const MAX_SIDE: u32 = 16_384;

/// The most pixels a picture may have in all: 64 MB for the picture and at most as much again
/// for the group being drawn.
pub const MAX_PIXELS: u64 = 16 * 1_024 * 1_024;

/// The display list as a PNG picture whose longer side is `size` pixels.
pub fn png(list: &DisplayList, size: u32) -> Result<Vec<u8>, RenderError> {
    pixels(list, size)?.encode_png().map_err(|e| RenderError::Encode(e.to_string()))
}

/// The width and height of a picture of `frame` whose longer side is `size` pixels: whole
/// numbers worked out from whole nanometres, the shorter side rounded up and at least a pixel.
pub(crate) fn picture_size(frame: Rect, size: u32) -> Result<(u32, u32), RenderError> {
    if size == 0 {
        return Err(RenderError::Empty);
    }
    let (fw, fh) = (i128::from(frame.width().0.max(1)), i128::from(frame.height().0.max(1)));
    let long = fw.max(fh);
    let side = |short: i128| -> u32 {
        let pixels = (short * i128::from(size) + long - 1) / long;
        u32::try_from(pixels.max(1)).unwrap_or(size)
    };
    let (width, height) = if fw >= fh { (size, side(fh)) } else { (side(fw), size) };
    if size > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(RenderError::TooLarge(u64::from(width), u64::from(height)));
    }
    Ok((width, height))
}

/// The display list drawn into a picture whose longer side is `size` pixels.
pub(crate) fn pixels(list: &DisplayList, size: u32) -> Result<Pixmap, RenderError> {
    let frame = frame(list.bounds);
    let (width, height) = picture_size(frame, size)?;
    let too_large = || RenderError::TooLarge(u64::from(width), u64::from(height));
    let mut picture = Pixmap::new(width, height).ok_or_else(too_large)?;
    let [r, g, b, a] = BACKGROUND.0;
    picture.fill(Color::from_rgba8(r, g, b, a));
    // Board coordinates to pixels: x from the left of the frame, y down from its top.
    let scale = f64::from(width.max(height)) / frame.width().0.max(frame.height().0).max(1) as f64;
    let (left, top) = (frame.min.x.0 as f64, frame.max.y.0 as f64);
    let at = |p: Point| ((p.x.0 as f64 - left) * scale, (top - p.y.0 as f64) * scale);
    for group in &list.groups {
        // The pixels the group covers, with a pixel to spare on each side for smoothing.
        let Some(bounds) = group.shapes.iter().filter_map(Shape::bounds).reduce(Rect::union) else { continue };
        let ((x0, y1), (x1, y0)) = (at(bounds.min), at(bounds.max));
        let clamp = |v: f64, max: u32| (v.max(0.0).min(f64::from(max))) as u32;
        let (px0, py0) = (clamp(x0.floor() - 1.0, width), clamp(y0.floor() - 1.0, height));
        let (px1, py1) = (clamp(x1.ceil() + 1.0, width), clamp(y1.ceil() + 1.0, height));
        let Some(mut layer) = Pixmap::new(px1.saturating_sub(px0), py1.saturating_sub(py0)) else { continue };
        let local = |p: Point| {
            let (x, y) = at(p);
            ((x - f64::from(px0)) as f32, (y - f64::from(py0)) as f32)
        };
        let [r, g, b, alpha] = group.color.0;
        let mut paint = Paint::default();
        paint.set_color_rgba8(r, g, b, 255);
        paint.anti_alias = true;
        for shape in &group.shapes {
            draw(&mut layer, &paint, shape, &local, scale);
        }
        let opacity = PixmapPaint { opacity: f32::from(alpha) / 255.0, ..PixmapPaint::default() };
        let (x, y) = (i32::try_from(px0).unwrap_or(0), i32::try_from(py0).unwrap_or(0));
        picture.draw_pixmap(x, y, layer.as_ref(), &opacity, Transform::identity(), None);
    }
    Ok(picture)
}

fn draw(pixmap: &mut Pixmap, paint: &Paint, shape: &Shape, at: &dyn Fn(Point) -> (f32, f32), scale: f64) {
    match shape {
        Shape::Stroke { points, width } => {
            let mut path = PathBuilder::new();
            for (i, p) in points.iter().enumerate() {
                let (x, y) = at(*p);
                if i == 0 { path.move_to(x, y) } else { path.line_to(x, y) }
            }
            // A line that stays in one place is a dot: tiny-skia draws nothing for a zero-length
            // line, so it is drawn as the disc its round ends make.
            let still = points.windows(2).all(|w| w.first() == w.get(1));
            let width = (width.0 as f64 * scale) as f32;
            match (still, points.first()) {
                (true, Some(&p)) => {
                    let (x, y) = at(p);
                    if let Some(dot) = PathBuilder::from_circle(x, y, width / 2.0) {
                        pixmap.fill_path(&dot, paint, FillRule::Winding, Transform::identity(), None);
                    }
                }
                _ => {
                    if let Some(path) = path.finish() {
                        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
                        pixmap.stroke_path(&path, paint, &stroke, Transform::identity(), None);
                    }
                }
            }
        }
        Shape::Disc { center, radius } => {
            let (x, y) = at(*center);
            if let Some(disc) = PathBuilder::from_circle(x, y, (radius.0 as f64 * scale) as f32) {
                pixmap.fill_path(&disc, paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        Shape::Area(polygon) => {
            let mut path = PathBuilder::new();
            for ring in std::iter::once(&polygon.outer).chain(&polygon.holes) {
                for (i, p) in ring.iter().enumerate() {
                    let (x, y) = at(*p);
                    if i == 0 { path.move_to(x, y) } else { path.line_to(x, y) }
                }
                path.close();
            }
            if let Some(path) = path.finish() {
                pixmap.fill_path(&path, paint, FillRule::EvenOdd, Transform::identity(), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{Group, GroupKind};
    use circuitcraft_geom::{Polygon, Rect};
    use circuitcraft_units::{Nm, Point};

    const MM: i64 = 1_000_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    /// The colour of a pixel, without premultiplied alpha.
    fn at(pixmap: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let c = pixmap.pixel(x, y).map(|c| c.demultiply()).expect("a pixel");
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    /// A 98 mm square of red with a 10 mm hole in the middle, which the frame grows to 100 mm.
    fn square_with_hole() -> DisplayList {
        let square = Polygon::new(vec![p(-49 * MM, -49 * MM), p(49 * MM, -49 * MM), p(49 * MM, 49 * MM), p(-49 * MM, 49 * MM)]);
        let red = Group {
            layer: None,
            name: "red".to_owned(),
            kind: GroupKind::Layer,
            color: crate::Rgba([255, 0, 0, 255]),
            shapes: vec![Shape::Area(square)],
        };
        let hole = Group {
            layer: None,
            name: "Holes".to_owned(),
            kind: GroupKind::Holes,
            color: BACKGROUND,
            shapes: vec![Shape::Disc { center: p(0, 0), radius: Nm(5 * MM) }],
        };
        DisplayList { groups: vec![red, hole], bounds: Some(Rect::from_corners(p(-49 * MM, -49 * MM), p(49 * MM, 49 * MM))) }
    }

    #[test]
    fn shapes_are_drawn_where_they_are_and_holes_show_the_background() {
        let picture = pixels(&square_with_hole(), 100).expect("pixels");
        assert_eq!((picture.width(), picture.height()), (100, 100));
        let [r, g, b, _] = BACKGROUND.0;
        assert_eq!(at(&picture, 0, 0), [r, g, b, 255], "the margin");
        assert_eq!(at(&picture, 10, 90), [255, 0, 0, 255], "the square");
        assert_eq!(at(&picture, 50, 50), [r, g, b, 255], "the hole");
        let png = png(&square_with_hole(), 100).expect("png");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn a_group_is_laid_over_with_its_opacity_as_a_whole() {
        // Two overlapping discs at half opacity: where they overlap is no stronger than elsewhere.
        let disc = |x| Shape::Disc { center: p(x, 0), radius: Nm(30 * MM) };
        let group = Group {
            layer: None,
            name: "g".to_owned(),
            kind: GroupKind::Layer,
            color: crate::Rgba([255, 255, 255, 128]),
            shapes: vec![disc(-10 * MM), disc(10 * MM)],
        };
        let list = DisplayList { groups: vec![group], bounds: Some(Rect::from_corners(p(-49 * MM, -49 * MM), p(49 * MM, 49 * MM))) };
        let picture = pixels(&list, 100).expect("pixels");
        assert_eq!(at(&picture, 50, 50), at(&picture, 25, 50));
    }

    #[test]
    fn the_longer_side_is_the_size_asked_for_and_sizes_are_bounded() {
        // 48 by 18 mm and a millimetre around it: 50 by 20.
        let wide = DisplayList { groups: Vec::new(), bounds: Some(Rect::from_corners(p(0, 0), p(48 * MM, 18 * MM))) };
        let picture = pixels(&wide, 400).expect("pixels");
        assert_eq!((picture.width(), picture.height()), (400, 160));
        // A long, narrow board standing up: the size is its height, and the default fits.
        let tall = DisplayList { groups: Vec::new(), bounds: Some(Rect::from_corners(p(0, 0), p(MM / 4, 100 * MM))) };
        let picture = pixels(&tall, 1_600).expect("pixels");
        assert_eq!((picture.width(), picture.height()), (66, 1_600), "4.25 by 104 mm, with 2 mm around a 100 mm board");
        assert_eq!(picture_size(Rect::from_corners(p(0, 0), p(1, 10_000 * MM)), 100), Ok((1, 100)), "at least a pixel wide");
        assert_eq!(picture_size(Rect::from_corners(p(0, 0), p(3, 3)), 2), Ok((2, 2)));
        assert_eq!(png(&wide, 0), Err(RenderError::Empty));
        assert!(matches!(png(&wide, MAX_SIDE + 1), Err(RenderError::TooLarge(..))));
        let square = DisplayList { groups: Vec::new(), bounds: Some(Rect::from_corners(p(0, 0), p(MM, MM))) };
        assert_eq!(png(&square, 8_192), Err(RenderError::TooLarge(8_192, 8_192)), "more pixels than allowed");
        assert!(png(&square, 4_096).is_ok());
    }

    #[test]
    fn a_small_group_is_drawn_where_it_is_on_a_large_picture() {
        // A 2 mm red disc near a corner: only its own pixels change. The frame is 98 mm and 1.96
        // around, so the disc's centre is at 892.5 pixels across and down, its radius 9.8 pixels.
        let disc = Group {
            layer: None,
            name: "red".to_owned(),
            kind: GroupKind::Layer,
            color: crate::Rgba([255, 0, 0, 255]),
            shapes: vec![Shape::Disc { center: p(40 * MM, -40 * MM), radius: Nm(MM) }],
        };
        let list = DisplayList { groups: vec![disc], bounds: Some(Rect::from_corners(p(-49 * MM, -49 * MM), p(49 * MM, 49 * MM))) };
        let picture = pixels(&list, 1_000).expect("pixels");
        assert_eq!(at(&picture, 892, 892), [255, 0, 0, 255], "the disc, at 40 mm right and 40 mm down of the middle");
        let [r, g, b, _] = BACKGROUND.0;
        assert_eq!(at(&picture, 870, 892), [r, g, b, 255]);
        assert_eq!(at(&picture, 500, 500), [r, g, b, 255]);
    }
}
