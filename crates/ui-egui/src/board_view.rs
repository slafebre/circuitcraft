//! The board view: the open board drawn on the GPU, panned and zoomed with the mouse or the
//! trackpad.
//!
//! The view keeps the triangles of the board ([`circuitcraft_render::meshes`]) and builds them
//! again only when the board or what the view shows changes. Where the view looks is the
//! session's camera, changed through `view.camera` and `view.fit` like any other command, so a
//! person, the CLI and an agent move the same view. Without a camera the whole board is shown.

use circuitcraft_engine::{Camera, MAX_NM_PER_PIXEL, MIN_NM_PER_PIXEL, Session};
use circuitcraft_geom::Rect;
use circuitcraft_render::{GroupMesh, Options};
use circuitcraft_units::{Nm, Point};
use egui::{Color32, Pos2, Vec2, pos2};
use serde_json::json;

/// How much of the view the board fills when it is fitted.
const FIT: f64 = 0.92;

/// How far one point of mouse-wheel or trackpad scrolling zooms: the zoom changes by this
/// factor's power of the distance scrolled.
const ZOOM_PER_POINT: f64 = 1.0015;

/// How many points one line of mouse-wheel scrolling is.
const LINE_POINTS: f32 = 40.0;

/// What the triangles were built from: the board (by generation and revision) and what the view
/// shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    generation: u64,
    revision: u64,
    options: Options,
}

/// The triangles of the board, and where they are.
#[derive(Debug, Default)]
pub(crate) struct BoardView {
    key: Option<Key>,
    meshes: Vec<(GroupMesh, Option<Rect>)>,
    bounds: Option<Rect>,
}

/// What the person did in the view this frame, for the session to know.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Gesture {
    /// Nothing that moves the view.
    None,
    /// The view looks somewhere else now.
    Moved(Camera),
    /// Show the whole board again.
    Fit,
}

impl BoardView {
    /// Builds the triangles again if the board or what the view shows changed since last time.
    pub(crate) fn update(&mut self, session: &Session) {
        let key = Key { generation: session.doc().generation(), revision: session.doc().revision(), options: session.render_options() };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let list = circuitcraft_render::display_list(session.board(), &key.options);
        self.bounds = list.bounds;
        self.meshes = circuitcraft_render::meshes(&list, key.options.tolerance)
            .into_iter()
            .map(|mesh| {
                let bounds = Rect::bounding(mesh.points.iter().copied());
                (mesh, bounds)
            })
            .collect();
        self.key = Some(key);
    }

    /// Whether there is nothing to draw.
    pub(crate) fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }

    /// Where the view looks: the session's camera, or the whole board fitted into `size`.
    pub(crate) fn camera(&self, session: &Session, size: Vec2) -> Camera {
        session.camera().unwrap_or_else(|| fit(self.bounds, size))
    }

    /// Draws the board into `rect` as `camera` sees it.
    pub(crate) fn paint(&self, painter: &egui::Painter, rect: egui::Rect, camera: Camera) {
        let screen = Screen { rect, camera };
        let visible = screen.board_rect();
        for (mesh, bounds) in &self.meshes {
            // A group entirely off screen costs nothing to leave out.
            if bounds.is_some_and(|b| !overlaps(b, visible)) {
                continue;
            }
            let [r, g, b, a] = mesh.color.0;
            let color = Color32::from_rgba_unmultiplied(r, g, b, a);
            let vertices = mesh.points.iter().map(|&p| egui::epaint::Vertex { pos: screen.pos(p), uv: egui::epaint::WHITE_UV, color }).collect();
            painter.add(egui::Shape::mesh(egui::Mesh { vertices, indices: mesh.indices.clone(), ..egui::Mesh::default() }));
        }
    }

    /// Reads the mouse and the trackpad over the view, the way drawing programs do: dragging pans;
    /// a mouse wheel zooms about the pointer, and so do pinching and scrolling with Ctrl or Cmd
    /// held; two fingers on a trackpad pan; a double click shows the whole board. Where the
    /// system doesn't say whether a wheel or a trackpad scrolled, scrolling zooms.
    ///
    /// Must not be called inside `ctx.input(…)`: `double_clicked` and `drag_delta` lock the
    /// context's input themselves, and egui's lock is not re-entrant, so a click there hangs the
    /// app. The input is read here in one short lock that calls nothing else.
    pub(crate) fn gesture(response: &egui::Response, camera: Camera) -> Gesture {
        if response.double_clicked() {
            return Gesture::Fit;
        }
        let mut moved = camera;
        let screen = Screen { rect: response.rect, camera };
        if response.dragged() {
            moved = pan(moved, response.drag_delta());
        }
        if response.hovered() {
            let page = response.rect.height();
            let (pointer, zoom_delta, wheel) = response.ctx.input(|input| (input.pointer.hover_pos(), input.zoom_delta(), scrolls(input, page)));
            let at = pointer.map_or(camera.center, |p| screen.board(p));
            let mut factor = f64::from(zoom_delta);
            for (points, source) in wheel {
                match source {
                    egui::MouseWheelSource::Trackpad | egui::MouseWheelSource::Momentum => moved = pan(moved, points),
                    egui::MouseWheelSource::Wheel | egui::MouseWheelSource::Unknown => factor *= ZOOM_PER_POINT.powf(f64::from(points.y)),
                }
            }
            if factor.is_finite() && factor > 0.0 && (factor - 1.0).abs() > 1e-6 {
                moved = zoom(moved, factor, at);
            }
        }
        if moved == camera { Gesture::None } else { Gesture::Moved(moved) }
    }

    /// The board point under a screen position.
    pub(crate) fn board_point(rect: egui::Rect, camera: Camera, pos: Pos2) -> Point {
        Screen { rect, camera }.board(pos)
    }
}

/// This frame's scrolling, in points, and what drove each: a page of scrolling is `page` points.
/// With Ctrl or Cmd held, egui already counts the scroll as zoom, so it is left out.
fn scrolls(input: &egui::InputState, page: f32) -> Vec<(Vec2, egui::MouseWheelSource)> {
    let mut out = Vec::new();
    for event in &input.raw.events {
        let egui::Event::MouseWheel { unit, delta, source, modifiers, .. } = event else { continue };
        if modifiers.ctrl || modifiers.command {
            continue;
        }
        let points = match unit {
            egui::MouseWheelUnit::Point => *delta,
            egui::MouseWheelUnit::Line => *delta * LINE_POINTS,
            egui::MouseWheelUnit::Page => *delta * page,
        };
        out.push((points, *source));
    }
    out
}

/// The session command that puts the view where a gesture leaves it.
pub(crate) fn command(gesture: Gesture) -> Option<(&'static str, serde_json::Value)> {
    match gesture {
        Gesture::None => None,
        Gesture::Fit => Some(("view.fit", serde_json::Value::Null)),
        Gesture::Moved(c) => {
            Some(("view.camera", json!({ "center": [c.center.x.to_mm(), c.center.y.to_mm()], "pixelsPerMm": 1e6 / c.nm_per_pixel as f64 })))
        }
    }
}

/// The camera that shows all of `bounds` in a view of `size` points, centred.
pub(crate) fn fit(bounds: Option<Rect>, size: Vec2) -> Camera {
    let Some(b) = bounds else {
        // Nothing to show: ten millimetres around the origin.
        let across = f64::from(size.x.min(size.y)).max(1.0);
        return Camera { center: Point::ORIGIN, nm_per_pixel: clamp_scale(10_000_000.0 / across) };
    };
    let center = Point::new(Nm(b.min.x.0 / 2 + b.max.x.0 / 2), Nm(b.min.y.0 / 2 + b.max.y.0 / 2));
    let per_x = b.width().0 as f64 / (f64::from(size.x).max(1.0) * FIT);
    let per_y = b.height().0 as f64 / (f64::from(size.y).max(1.0) * FIT);
    Camera { center, nm_per_pixel: clamp_scale(per_x.max(per_y)) }
}

/// A scale in nanometres per pixel, whole and within what a view allows.
fn clamp_scale(nm_per_pixel: f64) -> i64 {
    let (min, max) = (MIN_NM_PER_PIXEL as f64, MAX_NM_PER_PIXEL as f64);
    if nm_per_pixel.is_finite() { nm_per_pixel.round().clamp(min, max) as i64 } else { MAX_NM_PER_PIXEL }
}

/// The camera moved so the board follows a drag of `delta` points (y down on screen, up on the
/// board).
fn pan(camera: Camera, delta: Vec2) -> Camera {
    let nm = camera.nm_per_pixel as f64;
    let center = offset(camera.center, -f64::from(delta.x) * nm, f64::from(delta.y) * nm).unwrap_or(camera.center);
    Camera { center, ..camera }
}

/// The camera zoomed in by `factor` (out when below 1), keeping the board point `at` where it is
/// on screen.
fn zoom(camera: Camera, factor: f64, at: Point) -> Camera {
    let nm_per_pixel = clamp_scale(camera.nm_per_pixel as f64 / factor);
    // The real factor, after the scale was rounded and kept within its range.
    let kept = nm_per_pixel as f64 / camera.nm_per_pixel as f64;
    let dx = (camera.center.x.0 as f64 - at.x.0 as f64) * kept;
    let dy = (camera.center.y.0 as f64 - at.y.0 as f64) * kept;
    let center = offset(at, dx, dy).unwrap_or(camera.center);
    Camera { center, nm_per_pixel }
}

/// `p` moved by `(dx, dy)` nanometres, within the design range; `None` beyond it.
fn offset(p: Point, dx: f64, dy: f64) -> Option<Point> {
    let max = circuitcraft_units::MAX_ABS as f64;
    let (x, y) = ((p.x.0 as f64 + dx).round(), (p.y.0 as f64 + dy).round());
    (x.abs() <= max && y.abs() <= max).then(|| Point::new(Nm(x as i64), Nm(y as i64)))
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.y <= b.max.y && b.min.y <= a.max.y
}

/// Board coordinates and screen positions, for a view of `rect` looking through `camera`.
struct Screen {
    rect: egui::Rect,
    camera: Camera,
}

impl Screen {
    fn pos(&self, p: Point) -> Pos2 {
        let nm = self.camera.nm_per_pixel as f64;
        let c = self.rect.center();
        let x = (p.x.0 as f64 - self.camera.center.x.0 as f64) / nm;
        let y = (p.y.0 as f64 - self.camera.center.y.0 as f64) / nm;
        pos2(c.x + x as f32, c.y - y as f32)
    }

    /// The board point at a screen position, each axis kept within the design range: zoomed far
    /// out, a corner of the view can lie beyond it.
    fn board(&self, pos: Pos2) -> Point {
        let nm = self.camera.nm_per_pixel as f64;
        let c = self.rect.center();
        let max = circuitcraft_units::MAX_ABS as f64;
        let axis = |from: i64, by: f64| Nm((from as f64 + by).round().clamp(-max, max) as i64);
        Point::new(axis(self.camera.center.x.0, f64::from(pos.x - c.x) * nm), axis(self.camera.center.y.0, -f64::from(pos.y - c.y) * nm))
    }

    /// The part of the board the view shows.
    fn board_rect(&self) -> Rect {
        Rect::from_corners(self.board(self.rect.left_top()), self.board(self.rect.right_bottom()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: i64 = 1_000_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn fitting_shows_the_whole_board_in_the_middle() {
        // A 100 by 50 mm board in an 800 by 800 point view: the width decides.
        let camera = fit(Some(Rect::from_corners(p(0, 0), p(100 * MM, 50 * MM))), Vec2::new(800.0, 800.0));
        assert_eq!(camera.center, p(50 * MM, 25 * MM));
        assert_eq!(camera.nm_per_pixel, (100.0 * MM as f64 / (800.0 * FIT)).round() as i64);
        let nothing = fit(None, Vec2::new(500.0, 1_000.0));
        assert_eq!((nothing.center, nothing.nm_per_pixel), (Point::ORIGIN, 20_000));
        // A view of no size, or a board of none, still gives a camera within range.
        let tiny = fit(Some(Rect::from_corners(p(0, 0), p(0, 0))), Vec2::ZERO);
        assert_eq!(tiny.nm_per_pixel, MIN_NM_PER_PIXEL);
        let huge = fit(Some(Rect::from_corners(p(-i64::MAX, 0), p(i64::MAX, 0))), Vec2::new(1.0, 1.0));
        assert_eq!(huge.nm_per_pixel, MAX_NM_PER_PIXEL);
    }

    #[test]
    fn panning_moves_the_board_with_the_pointer() {
        let camera = Camera { center: p(0, 0), nm_per_pixel: 10_000 };
        // Dragging right and down by 100 points moves the board right and down: the view looks
        // 1 mm further left and up.
        assert_eq!(pan(camera, Vec2::new(100.0, 100.0)).center, p(-MM, MM));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_in_place() {
        let rect = egui::Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(1_000.0, 1_000.0));
        let camera = Camera { center: p(0, 0), nm_per_pixel: 10_000 };
        let pointer = pos2(800.0, 300.0);
        let under = BoardView::board_point(rect, camera, pointer);
        assert_eq!(under, p(3 * MM, 2 * MM));
        let closer = zoom(camera, 2.0, under);
        assert_eq!(closer.nm_per_pixel, 5_000);
        assert_eq!(BoardView::board_point(rect, closer, pointer), under);
        // Zoom stays within its range.
        assert_eq!(zoom(camera, 1e12, under).nm_per_pixel, MIN_NM_PER_PIXEL);
        assert_eq!(zoom(camera, 1e-12, under).nm_per_pixel, MAX_NM_PER_PIXEL);
        assert_eq!(clamp_scale(f64::NAN), MAX_NM_PER_PIXEL);
    }

    #[test]
    fn a_wide_view_zoomed_far_out_still_sees_the_board() {
        // 2400 points across at ten millimetres a pixel reaches 12 m to each side, past the 10 m
        // design range: the edge is kept within it, and the board in the middle is seen.
        let rect = egui::Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(2_400.0, 800.0));
        let screen = Screen { rect, camera: Camera { center: p(50 * MM, 50 * MM), nm_per_pixel: MAX_NM_PER_PIXEL } };
        let seen = screen.board_rect();
        let max = circuitcraft_units::MAX_ABS;
        assert_eq!((seen.min.x.0, seen.max.x.0), (-max, max));
        assert!(overlaps(Rect::from_corners(p(0, 0), p(100 * MM, 100 * MM)), seen));
        // Zoomed in, a group off to the side is not seen, and one in view is.
        let close = Screen { rect, camera: Camera { center: p(0, 0), nm_per_pixel: 1_000 } };
        let seen = close.board_rect();
        assert!(overlaps(Rect::from_corners(p(0, 0), p(MM, MM)), seen));
        assert!(!overlaps(Rect::from_corners(p(10 * MM, 0), p(11 * MM, MM)), seen));
    }

    #[test]
    fn a_gesture_becomes_a_command() {
        assert_eq!(command(Gesture::None), None);
        assert_eq!(command(Gesture::Fit).map(|(id, _)| id), Some("view.fit"));
        let (id, params) = command(Gesture::Moved(Camera { center: p(MM, -2 * MM), nm_per_pixel: 10_000 })).expect("a command");
        assert_eq!((id, params), ("view.camera", json!({ "center": [1.0, -2.0], "pixelsPerMm": 100.0 })));
    }

    #[test]
    fn the_triangles_are_built_again_only_when_something_changed() {
        let mut session = Session::in_memory();
        session.execute("track.add", &json!({ "layer": "Top Copper", "width": 0.5, "from": [0, 0], "to": [10, 0] })).expect("track");
        let mut view = BoardView::default();
        view.update(&session);
        let first = view.meshes.len();
        assert!(first > 0);
        let key = view.key.clone();
        view.update(&session);
        assert_eq!(view.key, key, "nothing changed");
        session.execute("view.layer", &json!({ "layer": "Top Copper", "visible": false })).expect("hide");
        view.update(&session);
        assert_ne!(view.key, key);
        assert!(view.meshes.len() < first, "the hidden layer's triangles are gone");
        // An edit is.
        let key = view.key.clone();
        session.execute("view.layer", &json!({ "layer": "Top Copper", "visible": true })).expect("show");
        view.update(&session);
        let shown = view.meshes.len();
        session.execute("track.add", &json!({ "layer": "Bottom Copper", "width": 0.5, "from": [0, 0], "to": [10, 0] })).expect("track");
        view.update(&session);
        assert!(view.meshes.len() > shown, "the new track's layer is drawn");
        assert_ne!(view.key, key);
        // Moving the camera is not a reason to build them again.
        let key = view.key.clone();
        session.execute("view.camera", &json!({ "center": [1, 1], "pixelsPerMm": 10 })).expect("camera");
        view.update(&session);
        assert_eq!(view.key, key);
    }
}
