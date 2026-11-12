//! The egui front end, shared by the desktop and web apps.
//!
//! The UI is thin: panels read engine state and act only through [`Session::execute`]. Menus are
//! built from the command table, so a new command with a menu path appears without UI changes.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_engine::{COMMANDS, CommandSpec, Session};
use circuitcraft_render::layer_color;
use egui::{Color32, RichText, Stroke, pos2, vec2};
use serde_json::Value;

/// Colours of the application chrome (CircuitCraft's own dark theme).
mod theme {
    use egui::Color32;
    pub const CHROME: Color32 = Color32::from_rgb(38, 42, 48);
    pub const CHROME_DARK: Color32 = Color32::from_rgb(30, 33, 38);
    pub const CANVAS: Color32 = Color32::from_rgb(16, 18, 22);
    pub const GRID: Color32 = Color32::from_rgb(34, 38, 46);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 146, 156);
}

/// Grid spacing on screen, in points.
const GRID_STEP: f32 = 24.0;

/// The application state behind every window.
pub struct CircuitApp {
    session: Session,
    status: String,
    result: Option<(String, String)>,
    active_layer: usize,
}

impl Default for CircuitApp {
    fn default() -> Self {
        CircuitApp::new(Session::new())
    }
}

impl CircuitApp {
    /// An app showing `session`.
    pub fn new(session: Session) -> CircuitApp {
        CircuitApp { session, status: "Ready".to_owned(), result: None, active_layer: 0 }
    }

    /// The session, for hosts that drive the app (control channel, tests).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Runs a command the way every UI element does, and reports the outcome in the status bar.
    pub fn run(&mut self, id: &str, params: &Value) {
        match self.session.execute(id, params) {
            Ok(v) => {
                self.status = format!("{id}: done");
                if !matches!(id, "board.new") {
                    let label = circuitcraft_engine::command(id).map_or(id, |c| c.label);
                    let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                    self.result = Some((label.to_owned(), text));
                }
            }
            Err(e) => self.status = format!("{id}: {e}"),
        }
    }

    /// Draws one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.menu_bar(ui);
        self.status_bar(ui);
        self.layer_tabs(ui);
        self.projects_panel(ui);
        self.properties_panel(ui);
        self.canvas(ui);
        self.result_window(ui);
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut clicked: Option<&'static str> = None;
        egui::Panel::top("cc_menubar").frame(egui::Frame::NONE.fill(theme::CHROME_DARK).inner_margin(4)).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for menu in menu_names() {
                    ui.menu_button(menu, |ui| {
                        for (label, spec) in menu_items(menu) {
                            if ui.button(label).clicked() {
                                clicked = Some(spec.id);
                            }
                        }
                    });
                }
            });
        });
        if let Some(id) = clicked {
            self.run(id, &Value::Null);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("cc_status").frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).color(theme::TEXT_DIM));
            });
        });
    }

    fn layer_tabs(&mut self, ui: &mut egui::Ui) {
        let stack = &self.session.board.stack;
        let mut copper_index = 0;
        let mut chosen = None;
        egui::Panel::bottom("cc_layer_tabs").frame(egui::Frame::NONE.fill(theme::CHROME_DARK).inner_margin(4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, layer) in stack.layers.iter().enumerate() {
                    let [r, g, b, _] = layer_color(layer, copper_index).0;
                    if layer.kind == circuitcraft_model::LayerKind::Copper {
                        copper_index += 1;
                    }
                    let (swatch, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(swatch, 2.0, Color32::from_rgb(r, g, b));
                    if ui.selectable_label(self.active_layer == i, &layer.name).clicked() {
                        chosen = Some(i);
                    }
                }
            });
        });
        if let Some(i) = chosen {
            self.active_layer = i;
        }
    }

    fn projects_panel(&self, ui: &mut egui::Ui) {
        egui::Panel::left("cc_projects").default_size(200.0).frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(8)).show(ui, |ui| {
            ui.strong("Projects");
            ui.separator();
            ui.label(format!("▸ {}", self.session.board.name));
            ui.label(RichText::new("Schematics arrive in 0.3").color(theme::TEXT_DIM));
        });
    }

    fn properties_panel(&self, ui: &mut egui::Ui) {
        let stack = &self.session.board.stack;
        egui::Panel::right("cc_properties").default_size(240.0).frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(8)).show(ui, |ui| {
            ui.strong("Properties");
            ui.separator();
            ui.label(format!("Board: {}", self.session.board.name));
            ui.label(format!("Copper layers: {}", stack.copper_count()));
            ui.label(format!("Thickness: {:.3} mm", stack.thickness().to_mm()));
            if let Some(layer) = stack.layers.get(self.active_layer) {
                ui.separator();
                ui.label(format!("Active layer: {}", layer.name));
                if let Some(t) = layer.thickness {
                    ui.label(format!("Layer thickness: {:.3} mm", t.to_mm()));
                }
            }
        });
    }

    fn canvas(&self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::CANVAS)).show(ui, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter_at(rect);
            let stroke = Stroke::new(1.0, theme::GRID);
            let mut x = rect.left() + GRID_STEP;
            while x < rect.right() {
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], stroke);
                x += GRID_STEP;
            }
            let mut y = rect.top() + GRID_STEP;
            while y < rect.bottom() {
                painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
                y += GRID_STEP;
            }
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "CircuitCraft 0.0 — opening EAGLE and KiCad boards arrives in 0.1",
                egui::FontId::proportional(15.0),
                theme::TEXT_DIM,
            );
        });
    }

    fn result_window(&mut self, ui: &mut egui::Ui) {
        let Some((title, text)) = &self.result else { return };
        let mut open = true;
        egui::Window::new(title.as_str()).open(&mut open).collapsible(false).resizable(true).show(ui.ctx(), |ui| {
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                ui.monospace(text.as_str());
            });
        });
        if !open {
            self.result = None;
        }
    }
}

/// Top-level menus in order of first appearance, with Help last.
fn menu_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for top in COMMANDS.iter().filter_map(|c| c.menu).filter_map(|m| m.split('/').next()) {
        if !names.contains(&top) {
            names.push(top);
        }
    }
    if let Some(i) = names.iter().position(|n| *n == "Help") {
        let help = names.remove(i);
        names.push(help);
    }
    names
}

/// The items of one top-level menu: (label, command).
fn menu_items(menu: &str) -> impl Iterator<Item = (&'static str, &'static CommandSpec)> + '_ {
    COMMANDS.iter().filter_map(move |c| {
        let (top, label) = c.menu?.split_once('/')?;
        (top == menu).then_some((label, c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_menu_is_last_and_every_menu_has_items() {
        let names = menu_names();
        assert_eq!(names.last(), Some(&"Help"));
        for n in names {
            assert!(menu_items(n).next().is_some(), "{n} is empty");
        }
    }

    #[test]
    fn running_a_command_updates_status() {
        let mut app = CircuitApp::default();
        app.run("board.new", &serde_json::json!({"name": "Blinky"}));
        assert_eq!(app.session().board.name, "Blinky");
        app.run("nope", &Value::Null);
        assert!(app.status.contains("unknown command"));
    }
}
