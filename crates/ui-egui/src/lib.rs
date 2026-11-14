//! The egui front end, shared by the desktop and web apps.
//!
//! The UI is thin: panels read engine state and act only through [`Session::execute`]. Menus are
//! built from the command table, so a new command with a menu path appears without UI changes.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod board_view;
mod files;

use board_view::BoardView;
use circuitcraft_engine::{COMMANDS, CommandSpec, Session};
use circuitcraft_mcp::control::{MAX_QUEUED, Server};
use circuitcraft_render::layer_color;
use circuitcraft_units::Point;
use egui::{Color32, RichText, vec2};
use serde_json::Value;

/// Colours of the application chrome (CircuitCraft's own dark theme).
mod theme {
    use egui::Color32;
    pub const CHROME: Color32 = Color32::from_rgb(38, 42, 48);
    pub const CHROME_DARK: Color32 = Color32::from_rgb(30, 33, 38);
    /// The canvas behind the board: the renderer's background, which holes show too.
    pub const CANVAS: Color32 = {
        let [r, g, b, _] = circuitcraft_render::BACKGROUND.0;
        Color32::from_rgb(r, g, b)
    };
    pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 146, 156);
    /// The control channel's indicator: it should catch the eye.
    pub const AGENTS: Color32 = Color32::from_rgb(240, 180, 60);
}

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
    /// The board drawn on the canvas.
    view: BoardView,
    /// The board point under the pointer, while it is over the canvas.
    pointer: Option<Point>,
    /// Files the browser's file picker handed over, waiting to be opened. The desktop's dialogs
    /// answer at once, so there it stays empty.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    inbox: files::Inbox,
    /// The control channel, while agents may drive this window.
    control: Option<Server>,
}

impl CircuitApp {
    /// An app showing `session`. If an earlier session left unsaved work behind, the status bar
    /// says how to get it back.
    pub fn new(session: Session) -> CircuitApp {
        let mut app = CircuitApp {
            session,
            status: "Ready".to_owned(),
            result: None,
            unsaved: None,
            discarding: None,
            autosaved_at: 0.0,
            view: BoardView::default(),
            pointer: None,
            inbox: files::Inbox::default(),
            control: None,
        };
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

    /// Lets agents drive this window through `server`, until the app closes. The status bar shows
    /// it for as long as it listens.
    pub fn listen(&mut self, server: Server) {
        self.control = Some(server);
    }

    /// Stops letting agents drive this window: the control channel closes and its control file is
    /// removed.
    pub fn stop_listening(&mut self) {
        self.control = None;
    }

    /// Says something in the status bar, for the host: why the control channel isn't on, say.
    pub fn report(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Answers what agents have sent over the control channel, on this thread, which owns the
    /// session. Their commands go through [`Session::execute`] like every other, so they are
    /// undoable and the board view shows what they changed; they never open a window. The host
    /// calls this whenever the server wakes it, also while the window is hidden and draws nothing
    /// (eframe's `App::logic`), so an agent never waits for the window to be shown.
    pub fn serve_agents(&mut self) {
        // Held aside while its requests run, which may change anything else in the app.
        let Some(server) = self.control.take() else { return };
        // At most a queue's worth at a time, so the window stays responsive; the server woke the
        // app for every request, so the rest are answered next time.
        for _ in 0..MAX_QUEUED {
            let Some(request) = server.next() else { break };
            let reply = circuitcraft_mcp::handle_line(&mut self.session, request.line());
            if let Some((id, status)) = agent_status(request.line(), reply.as_ref()) {
                self.status = status;
                self.after_command(id);
            }
            request.answer(reply);
        }
        self.control = Some(server);
    }

    /// Keeps what the app shows besides the board up to date after command `id`, whoever ran it.
    /// Unsaved work is offered in a window with buttons, which is listed again after any recovery
    /// command: also when the command failed, which may be because another session took what was
    /// listed.
    fn after_command(&mut self, id: &str) {
        if id.starts_with("recovery.") {
            self.list_unsaved();
            self.discarding = None;
        }
    }

    /// The session, for hosts that drive the app (control channel, tests).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Runs a command the way every UI element does, and reports the outcome in the status bar.
    /// Returns whether it succeeded.
    pub fn run(&mut self, id: &str, params: &Value) -> bool {
        let outcome = self.session.execute(id, params);
        self.after_command(id);
        let recovery = id.starts_with("recovery.");
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
            Err(e) => {
                self.status = format!("{id}: {e}");
                return false;
            }
        }
        true
    }

    /// Draws one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.take_in_picked();
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
            Some(id) if self.discarding.as_deref() == Some(id.as_str()) => {
                self.run("recovery.discard", &serde_json::json!({ "id": id }));
            }
            asked => self.discarding = asked,
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut clicked: Option<&'static str> = None;
        let mut file_action = None;
        egui::Panel::top("cc_menubar").frame(egui::Frame::NONE.fill(theme::CHROME_DARK).inner_margin(4)).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for menu in menu_names() {
                    ui.menu_button(menu, |ui| {
                        // Opening and saving ask where, in a dialog, before they run a command.
                        if menu == "File" {
                            for (label, action) in
                                [("Open…", files::FileAction::Open), ("Save", files::FileAction::Save), ("Save As…", files::FileAction::SaveAs)]
                            {
                                if ui.button(label).clicked() {
                                    file_action = Some(action);
                                }
                            }
                            ui.separator();
                        }
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
        if let Some(action) = file_action {
            self.file_action(action, ui.ctx());
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("cc_status").frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(p) = self.pointer {
                        ui.label(RichText::new(format!("x {:.3}  y {:.3} mm", p.x.to_mm(), p.y.to_mm())).color(theme::TEXT_DIM).monospace());
                    }
                    // Whoever can drive this window should never be a secret.
                    if let Some(server) = &self.control {
                        if self.pointer.is_some() {
                            ui.separator();
                        }
                        let agents = match server.agents() {
                            0 => String::new(),
                            1 => " · 1 connected".to_owned(),
                            n => format!(" · {n} connected"),
                        };
                        let label = ui.label(RichText::new(format!("Agents on {}{agents}", server.address())).color(theme::AGENTS));
                        // A dot drawn, not a character: the default fonts have no round bullet.
                        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, theme::AGENTS);
                        label.on_hover_text(
                            "Programs you run can drive this window over the control channel: they need the token in CircuitCraft's control file, which only you can read. Start the app without --control to turn it off.",
                        );
                    }
                });
            });
        });
    }

    /// A tab for each layer: its colour, a box that shows or hides it, and its name, which makes
    /// it the active layer; and a switch for single-layer mode.
    fn layer_tabs(&mut self, ui: &mut egui::Ui) {
        let session = &self.session;
        let stack = &session.board().stack;
        let active = session.active_layer();
        let mut single = session.single_layer();
        let mut copper_index = 0;
        let mut chosen = None;
        let mut shown = None;
        let mut single_changed = false;
        egui::Panel::bottom("cc_layer_tabs").frame(egui::Frame::NONE.fill(theme::CHROME_DARK).inner_margin(4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                single_changed = ui.toggle_value(&mut single, "Single layer").on_hover_text("Draw only the active layer").changed();
                ui.separator();
                for layer in &stack.layers {
                    let [r, g, b, _] = layer_color(layer, copper_index).0;
                    if layer.kind == circuitcraft_model::LayerKind::Copper {
                        copper_index += 1;
                    }
                    let (swatch, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(swatch, 2.0, Color32::from_rgb(r, g, b));
                    let mut visible = !session.layer_hidden(layer.id);
                    if ui.checkbox(&mut visible, "").on_hover_text(format!("Show or hide {}", layer.name)).changed() {
                        shown = Some((layer.name.clone(), visible));
                    }
                    if ui.selectable_label(active == Some(layer.id), &layer.name).clicked() {
                        chosen = Some(layer.name.clone());
                    }
                }
            });
        });
        if single_changed {
            self.run("view.single", &serde_json::json!({ "on": single }));
        }
        if let Some((layer, visible)) = shown {
            self.run("view.layer", &serde_json::json!({ "layer": layer, "visible": visible }));
        }
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

    fn properties_panel(&mut self, ui: &mut egui::Ui) {
        let board = self.session.board();
        let stack = &board.stack;
        let highlighted = self.session.highlight().and_then(|id| board.nets.get(id)).map(|n| n.name.clone());
        let mut pick: Option<Option<String>> = None;
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
            if !board.nets.is_empty() {
                ui.separator();
                let mut names: Vec<&str> = board.nets.iter().map(|n| n.name.as_str()).collect();
                names.sort_unstable();
                egui::ComboBox::from_label("Highlight").selected_text(highlighted.as_deref().unwrap_or("No net")).show_ui(ui, |ui| {
                    if ui.selectable_label(highlighted.is_none(), "No net").clicked() {
                        pick = Some(None);
                    }
                    for name in names {
                        if ui.selectable_label(highlighted.as_deref() == Some(name), name).clicked() {
                            pick = Some(Some(name.to_owned()));
                        }
                    }
                });
            }
        });
        if let Some(net) = pick {
            self.run("view.highlight", &serde_json::json!({ "net": net }));
        }
    }

    /// The board view: the board drawn as the session's view shows it, panned by dragging,
    /// zoomed by scrolling or pinching, fitted again by a double click.
    fn canvas(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::CANVAS)).show(ui, |ui| {
            let rect = ui.max_rect();
            let response = ui.interact(rect, ui.scope_id().with("board_view"), egui::Sense::click_and_drag());
            self.view.update(&self.session);
            let camera = self.view.camera(&self.session, rect.size());
            // Outside `ui.input`: the gesture reads the response, which takes that lock itself.
            let gesture = BoardView::gesture(&response, camera);
            // Moving the view is quiet: it says something only when it fails.
            if let Some((id, params)) = board_view::command(gesture)
                && let Err(e) = self.session.execute(id, &params)
            {
                self.status = format!("{id}: {e}");
            }
            let camera = self.view.camera(&self.session, rect.size());
            let painter = ui.painter_at(rect);
            self.view.paint(&painter, rect, camera);
            self.pointer = response.hover_pos().map(|p| BoardView::board_point(rect, camera, p));
            if self.view.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "File > Open… opens a board",
                    egui::FontId::proportional(15.0),
                    theme::TEXT_DIM,
                );
            }
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

/// What the status bar says about a message from an agent: for a command, its id and label and
/// how it went; nothing for the rest (the handshake, the command list).
fn agent_status(line: &str, reply: Option<&Value>) -> Option<(&'static str, String)> {
    let msg: Value = serde_json::from_str(line).ok()?;
    let args =
        msg.pointer("/params/arguments").filter(|_| msg["method"] == "tools/call" && msg.pointer("/params/name") == Some(&Value::from("execute")))?;
    let id = args.get("command")?.as_str()?;
    let spec = circuitcraft_engine::command(id)?;
    let result = reply?.get("result")?;
    let label = spec.label;
    Some((
        spec.id,
        if result["isError"] == true {
            let why = result.pointer("/content/0/text").and_then(Value::as_str).unwrap_or("failed");
            format!("Agent: {label}: {why}")
        } else {
            format!("Agent: {label}: done")
        },
    ))
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

    /// Runs `frames` frames of the app in a 1200 by 800 window, each with `events`, and returns
    /// the last frame's output.
    fn frames(app: &mut CircuitApp, ctx: &egui::Context, frames: usize, events: &dyn Fn(usize) -> Vec<egui::Event>) -> egui::FullOutput {
        let mut out = egui::FullOutput::default();
        // Time goes on from the last frame of an earlier call.
        let start = ctx.input(|i| i.time);
        for frame in 0..frames {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1_200.0, 800.0))),
                time: Some(start + (frame + 1) as f64 / 60.0),
                events: events(frame),
                ..Default::default()
            };
            out = ctx.run_ui(raw, |ui| app.ui(ui));
            // No GPU takes the font textures here.
            out.textures_delta.clear();
        }
        out
    }

    #[test]
    fn the_board_is_drawn_on_the_canvas_and_the_wheel_zooms_it() {
        let mut session = Session::in_memory();
        session.execute("track.add", &serde_json::json!({ "layer": "Top Copper", "width": 0.5, "from": [0, 0], "to": [10, 0] })).expect("track");
        let mut app = CircuitApp::new(session);
        let ctx = egui::Context::default();
        let out = frames(&mut app, &ctx, 2, &|_| Vec::new());
        let meshes = out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Mesh(m) if !m.vertices.is_empty())).count();
        assert_eq!(meshes, 1, "the track's layer drawn as triangles");
        assert!(app.session().camera().is_none(), "fitted until someone moves the view");
        // The wheel over the board zooms about the pointer: one way in, the other way out.
        let over = egui::pos2(600.0, 400.0);
        let wheel = |dy: f32| {
            move |frame: usize| {
                let mut events = vec![egui::Event::PointerMoved(over)];
                if frame == 1 {
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, dy),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                events
            }
        };
        frames(&mut app, &ctx, 3, &wheel(200.0));
        let closer = app.session().camera().expect("a camera once the view moved");
        let under = app.pointer.expect("the pointer's place on the board is known");
        frames(&mut app, &ctx, 3, &wheel(-400.0));
        let farther = app.session().camera().expect("a camera");
        assert!(farther.nm_per_pixel > closer.nm_per_pixel, "{} then {}", closer.nm_per_pixel, farther.nm_per_pixel);
        let still = app.pointer.expect("the pointer's place");
        let near = |a: i64, b: i64| (a - b).abs() <= 2 * farther.nm_per_pixel;
        assert!(near(still.x.0, under.x.0) && near(still.y.0, under.y.0), "the point under the pointer stays: {under:?} {still:?}");
        // Two fingers on a trackpad pan instead: the scale stays, the centre moves.
        let fingers = |frame: usize| {
            let mut events = vec![egui::Event::PointerMoved(over)];
            if frame == 1 {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(50.0, 0.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Trackpad,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            events
        };
        frames(&mut app, &ctx, 3, &fingers);
        let panned = app.session().camera().expect("a camera");
        assert_eq!(panned.nm_per_pixel, farther.nm_per_pixel);
        assert_eq!(panned.center.x.0, farther.center.x.0 - 50 * farther.nm_per_pixel, "the board follows the fingers to the right");
        // Zoom to Fit from the View menu shows the whole board again.
        app.run("view.fit", &Value::Null);
        assert!(app.session().camera().is_none());
    }

    /// A press or a release of the primary button at `pos`.
    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    /// Clicking, double-clicking and dragging on the board used to hang the app: reading the
    /// click inside egui's input lock took that lock again. Run in a thread with a deadline, so a
    /// hang fails the test instead of hanging it.
    #[test]
    fn clicking_double_clicking_and_dragging_the_board_do_not_hang() {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut session = Session::in_memory();
            session.execute("track.add", &serde_json::json!({ "layer": "Top Copper", "width": 0.5, "from": [0, 0], "to": [10, 0] })).expect("track");
            let mut app = CircuitApp::new(session);
            let ctx = egui::Context::default();
            let at = egui::pos2(600.0, 400.0);
            frames(&mut app, &ctx, 2, &|_| vec![egui::Event::PointerMoved(at)]);
            // A click.
            frames(&mut app, &ctx, 3, &|frame| match frame {
                0 => vec![button(at, true)],
                1 => vec![button(at, false)],
                _ => Vec::new(),
            });
            // A drag of 100 points to the right pans the view.
            let to = egui::pos2(700.0, 400.0);
            frames(&mut app, &ctx, 12, &|frame| match frame {
                0 => vec![button(at, true)],
                1..=10 => vec![egui::Event::PointerMoved(egui::pos2(at.x + 10.0 * frame as f32, at.y))],
                _ => vec![button(to, false)],
            });
            let panned = app.session().camera();
            // Half a second later, so it is not counted with the first click: a double click
            // fits the board again.
            frames(&mut app, &ctx, 30, &|_| Vec::new());
            frames(&mut app, &ctx, 6, &|frame| match frame {
                0 | 2 => vec![egui::Event::PointerMoved(to), button(to, true)],
                1 | 3 => vec![button(to, false)],
                _ => Vec::new(),
            });
            let _ = done.send((panned, app.session().camera()));
        });
        let (panned, fitted) = match finished.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("the app hung on a click or a drag"),
            // A debug build's lock gives up on a deadlock after ten seconds and panics.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the app's thread panicked (see above): a click or a drag deadlocked or failed")
            }
        };
        assert!(panned.is_some(), "the drag moved the view");
        assert!(fitted.is_none(), "the double click fitted it again");
    }

    #[test]
    fn running_a_command_updates_status() {
        let mut app = CircuitApp::new(Session::in_memory());
        app.run("board.new", &serde_json::json!({"name": "Blinky"}));
        assert_eq!(app.session().board().name, "Blinky");
        app.run("nope", &Value::Null);
        assert!(app.status.contains("unknown command"));
    }

    /// Plays an agent: sends each of `calls` to `app` over the control channel (after the
    /// authentication) and returns the replies, while this thread plays the UI thread.
    fn agent(app: &mut CircuitApp, calls: Vec<Value>) -> Vec<Value> {
        use std::io::{BufRead as _, Write as _};
        let server = Server::start(std::net::SocketAddr::from(([127, 0, 0, 1], 0)), None, std::sync::Arc::new(|| {})).expect("start");
        let (address, token) = (server.address(), server.secrets().token.clone());
        app.listen(server);
        let client = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(address).expect("connect");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
            let hello = serde_json::json!({ "jsonrpc": "2.0", "id": 0, "method": "control/authenticate", "params": { "token": token } });
            let mut replies = Vec::new();
            for (id, line) in std::iter::once(hello).chain(calls).enumerate() {
                writeln!(stream, "{line}").expect("write");
                let mut reply = String::new();
                reader.read_line(&mut reply).expect("read");
                if id > 0 {
                    replies.push(serde_json::from_str::<Value>(&reply).expect("json"));
                }
            }
            replies
        });
        while !client.is_finished() {
            app.serve_agents();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        client.join().expect("client")
    }

    fn execute(command: &str, params: Value) -> Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "execute", "arguments": { "command": command, "params": params } } })
    }

    #[test]
    fn agents_drive_the_open_board_on_the_ui_thread() {
        let mut app = CircuitApp::new(Session::in_memory());
        let replies = agent(
            &mut app,
            vec![
                execute("track.add", serde_json::json!({ "layer": "Top Copper", "from": [0, 0], "to": [3, 4] })),
                execute("board.info", Value::Null),
            ],
        );
        assert_eq!(replies[0]["result"]["isError"], false, "{replies:?}");
        assert_eq!(app.session().board().tracks.len(), 1, "the track is on the board the window shows");
        assert!(app.result.is_none(), "a report asked for by an agent opens no window");
        assert_eq!(app.status, "Agent: Board Information: done");
        app.run("edit.undo", &Value::Null);
        assert_eq!(app.session().board().tracks.len(), 0, "and the person can undo it");
    }

    #[test]
    fn the_unsaved_work_window_follows_what_an_agent_recovers() {
        let files = std::sync::Arc::new(circuitcraft_io::storage::MemoryStorage::with_recovery());
        let mut ended = Session::with_storage(Box::new(files.clone()));
        ended.execute("net.add", &serde_json::json!({ "name": "GND" })).expect("net");
        ended.execute("file.autosave", &Value::Null).expect("autosave");
        let mut app = CircuitApp::new(Session::with_storage(Box::new(files.clone())));
        let offered = app.unsaved.clone().expect("the window is open");
        app.press_discard(Some(offered[0].id.clone()));
        let replies = agent(&mut app, vec![execute("recovery.discard", serde_json::json!({ "id": offered[0].id }))]);
        assert_eq!(replies[0]["result"]["isError"], false, "{replies:?}");
        assert!(app.unsaved.is_none() && app.discarding.is_none(), "nothing left to offer, and no Discard half pressed");
    }

    #[test]
    fn the_status_bar_names_what_an_agent_ran() {
        let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"execute","arguments":{"command":"via.add"}}}"#;
        let failed = serde_json::json!({ "result": { "isError": true, "content": [{ "type": "text", "text": "bad parameters" }] } });
        assert_eq!(agent_status(call, Some(&failed)), Some(("via.add", "Agent: Add Via: bad parameters".to_owned())));
        assert_eq!(agent_status(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, None), None);
        assert_eq!(agent_status("not json", None), None);
        let unknown = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"execute","arguments":{"command":"nope"}}}"#;
        assert_eq!(agent_status(unknown, Some(&failed)), None);
    }
}
