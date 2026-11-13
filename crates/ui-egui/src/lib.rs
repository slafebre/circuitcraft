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

/// How often unsaved work is written to the recovery folder, in seconds. A crash (and in the
/// browser any panic, which aborts) loses at most this much work.
const AUTOSAVE_SECONDS: f64 = 30.0;

/// A board with unsaved work in the recovery folder, as `recovery.list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unsaved {
    id: String,
    /// The board's name; `None` for a file that can't be read.
    name: Option<String>,
    /// The board file it belongs to, if it was ever saved.
    from: Option<String>,
}

/// The application state behind every window.
pub struct CircuitApp {
    session: Session,
    status: String,
    result: Option<(String, String)>,
    /// The unsaved work that can be recovered, while the window that offers it is open.
    unsaved: Option<Vec<Unsaved>>,
    /// The id of the unsaved work whose Discard button was pressed once. Discarding can't be
    /// undone, so it takes a second press.
    discarding: Option<String>,
    /// When unsaved work was last looked after, in seconds since the app started.
    autosaved_at: f64,
}

impl CircuitApp {
    /// An app showing `session`. If an earlier session left unsaved work behind, the status bar
    /// says how to get it back.
    pub fn new(session: Session) -> CircuitApp {
        let mut app = CircuitApp { session, status: "Ready".to_owned(), result: None, unsaved: None, discarding: None, autosaved_at: 0.0 };
        app.list_unsaved();
        if app.unsaved.is_some() {
            app.status = "Unsaved work was found: recover it or discard it (File > Unsaved Work)".to_owned();
        } else if app.session.storage().recovery_folder().is_none() {
            // The browser, today: a crash there takes unsaved work with it.
            app.status = "Unsaved work is not kept here: save often".to_owned();
        }
        app
    }

    /// Looks up the unsaved work that can be recovered. The window that offers it is open while
    /// there is some.
    fn list_unsaved(&mut self) {
        let listed = self.session.execute("recovery.list", &Value::Null).unwrap_or_default();
        let text = |v: &Value| v.as_str().map(str::to_owned);
        let entries = listed["recoverable"].as_array().into_iter().flatten();
        let unsaved: Vec<Unsaved> =
            entries.filter_map(|e| Some(Unsaved { id: text(&e["id"])?, name: text(&e["name"]), from: text(&e["from"]) })).collect();
        self.unsaved = (!unsaved.is_empty()).then_some(unsaved);
    }

    /// Looks after unsaved work when it is time to: `now` is in seconds since the app started.
    /// Nothing is written for a board that hasn't changed.
    fn keep_unsaved_work(&mut self, now: f64) {
        if now - self.autosaved_at < AUTOSAVE_SECONDS {
            return;
        }
        self.autosaved_at = now;
        if let Err(e) = self.session.execute("file.autosave", &Value::Null) {
            self.status = format!("Unsaved work could not be kept: {e}");
        }
    }

    /// The session, for hosts that drive the app (control channel, tests).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Runs a command the way every UI element does, and reports the outcome in the status bar.
    pub fn run(&mut self, id: &str, params: &Value) {
        let outcome = self.session.execute(id, params);
        // Unsaved work is offered in a window with buttons, which is kept up to date: also when
        // the command failed, which may be because another session took what was listed.
        let recovery = id.starts_with("recovery.");
        if recovery {
            self.list_unsaved();
            self.discarding = None;
        }
        match outcome {
            Ok(v) => {
                let spec = circuitcraft_engine::command(id);
                let label = spec.map_or(id, |c| c.label);
                self.status = format!("{label}: done");
                if id == "recovery.list" && self.unsaved.is_none() {
                    self.status = "There is no unsaved work to recover".to_owned();
                }
                // A command that only reports produces something to read; the others change the
                // board or the view, which shows.
                if !recovery && spec.is_some_and(|c| c.read_only) {
                    let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                    self.result = Some((label.to_owned(), text));
                }
            }
            Err(e) => self.status = format!("{id}: {e}"),
        }
    }

    /// Draws one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.keep_unsaved_work(ui.input(|i| i.time));
        // Nothing else may wake the app up before unsaved work is due again.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(AUTOSAVE_SECONDS));
        self.menu_bar(ui);
        self.status_bar(ui);
        self.layer_tabs(ui);
        self.projects_panel(ui);
        self.properties_panel(ui);
        self.canvas(ui);
        self.result_window(ui);
        self.unsaved_window(ui);
    }

    /// The window that offers unsaved work: each board with where it belongs, to recover or to
    /// discard.
    fn unsaved_window(&mut self, ui: &mut egui::Ui) {
        let Some(unsaved) = &self.unsaved else { return };
        let mut open = true;
        let mut recover = None;
        // The Discard button that was pressed, or the Keep button that takes it back.
        let mut discard: Option<Option<String>> = None;
        egui::Window::new("Unsaved Work").open(&mut open).collapsible(false).resizable(true).show(ui.ctx(), |ui| {
            ui.label("These boards were not saved. A recovered board opens as an unsaved board; save it to keep it.");
            ui.separator();
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for board in unsaved {
                    ui.horizontal(|ui| {
                        // A file that can't be read can only be discarded.
                        if ui.add_enabled(board.name.is_some(), egui::Button::new("Recover")).clicked() {
                            recover = Some(board.id.clone());
                        }
                        // Discarding can't be undone, so it takes two presses in two places: once
                        // asked, Keep is where Discard was, so a double click takes it back.
                        if self.discarding.as_deref() == Some(board.id.as_str()) {
                            if ui.button("Keep").clicked() {
                                discard = Some(None);
                            }
                            if ui.button("Discard for good").clicked() {
                                discard = Some(Some(board.id.clone()));
                            }
                        } else if ui.button("Discard").clicked() {
                            discard = Some(Some(board.id.clone()));
                        }
                        ui.label(board.name.as_deref().unwrap_or("A file that can't be read"));
                        ui.label(RichText::new(board.from.as_deref().unwrap_or("never saved")).color(theme::TEXT_DIM));
                    });
                }
            });
        });
        if let Some(id) = recover {
            self.run("recovery.open", &serde_json::json!({ "id": id }));
        } else if let Some(pressed) = discard {
            self.press_discard(pressed);
        } else if !open {
            (self.unsaved, self.discarding) = (None, None);
        }
    }

    /// A press on a Discard button (`Some`: the first press on a board's button asks, the second
    /// discards) or on Keep (`None`: nothing is discarded).
    fn press_discard(&mut self, id: Option<String>) {
        match id {
            Some(id) if self.discarding.as_deref() == Some(id.as_str()) => self.run("recovery.discard", &serde_json::json!({ "id": id })),
            asked => self.discarding = asked,
        }
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
        let stack = &self.session.board().stack;
        let active = self.session.active_layer();
        let mut copper_index = 0;
        let mut chosen = None;
        egui::Panel::bottom("cc_layer_tabs").frame(egui::Frame::NONE.fill(theme::CHROME_DARK).inner_margin(4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for layer in &stack.layers {
                    let [r, g, b, _] = layer_color(layer, copper_index).0;
                    if layer.kind == circuitcraft_model::LayerKind::Copper {
                        copper_index += 1;
                    }
                    let (swatch, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(swatch, 2.0, Color32::from_rgb(r, g, b));
                    if ui.selectable_label(active == Some(layer.id), &layer.name).clicked() {
                        chosen = Some(layer.name.clone());
                    }
                }
            });
        });
        if let Some(layer) = chosen {
            self.run("view.active", &serde_json::json!({ "layer": layer }));
        }
    }

    fn projects_panel(&self, ui: &mut egui::Ui) {
        egui::Panel::left("cc_projects").default_size(200.0).frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(8)).show(ui, |ui| {
            ui.strong("Projects");
            ui.separator();
            // A dot marks a board with unsaved changes.
            let unsaved = if self.session.modified() { " •" } else { "" };
            ui.label(format!("▸ {}{unsaved}", self.session.board().name));
            ui.label(RichText::new("Schematics arrive in 0.3").color(theme::TEXT_DIM));
        });
    }

    fn properties_panel(&self, ui: &mut egui::Ui) {
        let stack = &self.session.board().stack;
        egui::Panel::right("cc_properties").default_size(240.0).frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(8)).show(ui, |ui| {
            ui.strong("Properties");
            ui.separator();
            ui.label(format!("Board: {}", self.session.board().name));
            ui.label(format!("Copper layers: {}", stack.copper_count()));
            ui.label(format!("Thickness: {:.3} mm", stack.thickness().to_mm()));
            if let Some(layer) = self.session.active_layer().and_then(|id| stack.get(id)) {
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
    fn the_active_layer_lives_in_the_session() {
        let mut app = CircuitApp::new(Session::in_memory());
        assert_eq!(app.session().active_layer(), Some(circuitcraft_model::layer_ids::TOP_COPPER));
        app.run("view.active", &serde_json::json!({ "layer": "Bottom Copper" }));
        assert_eq!(app.session().active_layer(), Some(circuitcraft_model::layer_ids::BOTTOM_COPPER));
        // Changing the view shows no result window; a report does.
        assert!(app.result.is_none());
        app.run("board.info", &Value::Null);
        assert_eq!(app.result.as_ref().map(|(title, _)| title.as_str()), Some("Board Information"));
    }

    #[test]
    fn unsaved_work_is_kept_every_half_minute_and_offered_at_the_next_start() {
        let files = std::sync::Arc::new(circuitcraft_io::storage::MemoryStorage::with_recovery());
        let mut app = CircuitApp::new(Session::with_storage(Box::new(files.clone())));
        assert_eq!((app.status.as_str(), &app.unsaved), ("Ready", &None));
        app.run("net.add", &serde_json::json!({ "name": "GND" }));
        app.keep_unsaved_work(AUTOSAVE_SECONDS - 1.0);
        assert!(files.snapshot().is_empty(), "not yet");
        app.keep_unsaved_work(AUTOSAVE_SECONDS);
        assert_eq!(files.snapshot().len(), 1);
        // Half a minute is counted from the last time, not from the start.
        app.run("net.add", &serde_json::json!({ "name": "VCC" }));
        let kept = files.snapshot();
        app.keep_unsaved_work(2.0 * AUTOSAVE_SECONDS - 1.0);
        assert_eq!(files.snapshot(), kept);
        app.keep_unsaved_work(2.0 * AUTOSAVE_SECONDS);
        assert_ne!(files.snapshot(), kept);
        assert_eq!(app.status, "Add Net: done", "keeping unsaved work says nothing when it goes well");
        // The next start finds it and offers it.
        let mut next = CircuitApp::new(Session::with_storage(Box::new(files.clone())));
        assert!(next.status.contains("Unsaved work was found"), "{}", next.status);
        let offered = next.unsaved.clone().expect("the window is open");
        assert_eq!(offered.iter().map(|u| (u.name.as_deref(), u.from.as_deref())).collect::<Vec<_>>(), [(Some("Untitled"), None)]);
        // Recovering it opens the board; it is the session's own copy now, so nothing is left to
        // offer and the window closes.
        next.run("recovery.open", &serde_json::json!({ "id": offered[0].id }));
        assert!(next.session().board().net_by_name("VCC").is_some() && next.unsaved.is_none());
        // Asked for from the menu with nothing to recover, it says so instead of showing a list.
        next.run("recovery.list", &Value::Null);
        assert_eq!((next.status.as_str(), &next.unsaved, &next.result), ("There is no unsaved work to recover", &None, &None));
    }

    #[test]
    fn unsaved_work_can_be_discarded_one_by_one() {
        let files = std::sync::Arc::new(circuitcraft_io::storage::MemoryStorage::with_recovery());
        for name in ["One", "Two"] {
            let mut ended = Session::with_storage(Box::new(files.clone()));
            ended.execute("board.new", &serde_json::json!({ "name": name })).expect("new");
            ended.execute("net.add", &serde_json::json!({ "name": "GND" })).expect("net");
            ended.execute("file.autosave", &Value::Null).expect("autosave");
        }
        let mut app = CircuitApp::new(Session::with_storage(Box::new(files.clone())));
        let offered = app.unsaved.clone().expect("the window is open");
        assert_eq!(offered.len(), 2);
        // The first press on Discard only asks; Keep, or a press on another board's button, takes
        // it back; the second press on the same button discards.
        let (first, second) = (offered[0].id.clone(), offered[1].id.clone());
        app.press_discard(Some(first.clone()));
        assert_eq!((app.discarding.as_ref(), files.snapshot().len()), (Some(&first), 2));
        app.press_discard(None);
        assert_eq!((app.discarding.as_ref(), files.snapshot().len()), (None, 2));
        app.press_discard(Some(first.clone()));
        app.press_discard(Some(second.clone()));
        assert_eq!((app.discarding.as_ref(), files.snapshot().len()), (Some(&second), 2), "another board's button asks about that board");
        app.press_discard(Some(first.clone()));
        app.press_discard(Some(first));
        assert_eq!((app.discarding.as_ref(), files.snapshot().len()), (None, 1));
        assert_eq!(app.unsaved.as_ref().map(Vec::len), Some(1), "the list follows");
        // A command that fails still brings the list up to date, and takes back a first press:
        // here another session discarded the last one first.
        let mut other = Session::with_storage(Box::new(files.clone()));
        other.execute("recovery.discard", &serde_json::json!({ "id": second })).expect("discard");
        app.press_discard(Some(second.clone()));
        app.run("recovery.open", &serde_json::json!({ "id": second }));
        assert!(app.status.starts_with("recovery.open: "), "{}", app.status);
        assert!(app.unsaved.is_none() && app.discarding.is_none() && files.snapshot().is_empty());
    }

    #[test]
    fn where_unsaved_work_is_not_kept_the_app_says_so_once() {
        let mut app = CircuitApp::new(Session::in_memory());
        assert_eq!(app.status, "Unsaved work is not kept here: save often");
        app.run("net.add", &serde_json::json!({ "name": "GND" }));
        app.keep_unsaved_work(AUTOSAVE_SECONDS);
        assert_eq!(app.status, "Add Net: done", "and does not say it again every half minute");
    }

    #[test]
    fn running_a_command_updates_status() {
        let mut app = CircuitApp::new(Session::in_memory());
        app.run("board.new", &serde_json::json!({"name": "Blinky"}));
        assert_eq!(app.session().board().name, "Blinky");
        app.run("nope", &Value::Null);
        assert!(app.status.contains("unknown command"));
    }
}
