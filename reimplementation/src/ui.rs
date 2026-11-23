//! Native adapter for component editing.
//!
//! Symbol drawing and picking live behind `symbol_render`; the canvas maps world units to the
//! screen through [`CanvasTransform`]. The shape style is transient UI state and never reaches
//! the engine or the document.
use crate::{
    canvas::CanvasTransform,
    components::KINDS,
    domain::{ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position, WireId},
    engine::{EditError, SaveError, Session, SimulationError},
    file_session::{DiscardIntent, FileIo, LocalFileIo, PublishOutcome, SaveAsIntent},
    runtime::{InputChange, Level, SettleOutcome},
    sim_text::{describe_simulation_error, level_badge, level_text},
    symbol_render::{PaintState, Palette, SymbolLayout, SymbolRenderer},
    symbols::{DEFAULT_STYLE, GRID_UNIT, STROKE_WIDTH, SymbolStyle},
};
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, Vec2};

/// Screen points per world unit. Pan and zoom will vary this; everything already goes through
/// the [`CanvasTransform`].
const CANVAS_ZOOM: f32 = 1.0;
/// Extra reach of a click around a symbol's bounds, in screen points.
const PICK_TOLERANCE: f32 = 2.0;
/// Gap between a symbol's bounds and its selection frame, in world units.
const SELECTION_MARGIN: f32 = 2.0;
/// Gap between a symbol's bounds and its label, in screen points.
const LABEL_GAP: f32 = 4.0;
/// Work units (gate evaluations plus microsteps) settled per frame. Small enough that a frame
/// stays short and Stop is handled between chunks, however large the circuit.
const SETTLE_BUDGET_PER_FRAME: usize = 2_048;
/// Shown wherever editing is disabled.
const EDIT_LOCK_NOTE: &str =
    "Editing is locked while the simulation is running. Stop it (Shift+F5) to edit.";

/// A simulation command from a button, shortcut or canvas click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimAction {
    Start,
    Stop,
    Step,
    Settle,
    Toggle(ComponentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimState {
    Idle,
    /// The last start failed and the document has not changed since.
    Error,
    Pending {
        settling: bool,
    },
    Settled,
}

pub struct DigitalApp {
    session: Session,
    selected: Option<ComponentId>,
    selected_wire: Option<WireId>,
    wiring: bool,
    wire_start: Option<PinRef>,
    focused_pin: Option<PinRef>,
    placing: Option<ComponentKind>,
    drag: Option<(ComponentId, Position, Pos2)>,
    error: Option<String>,
    /// Work units settled per frame while `settling`.
    settle_budget: usize,
    /// Whether frames keep settling in bounded chunks. Step pauses it; Settle, Start and an
    /// input toggle resume it.
    settling: bool,
    /// The document whose compilation last failed; the error state ends when it changes.
    failed_start: Option<Document>,
    style: SymbolStyle,
    renderer: SymbolRenderer,
    file_io: Option<Box<dyn FileIo>>,
    file_io_error: Option<String>,
    file_path_entry: String,
    pending_action: Option<PendingAction>,
    file_modal: Option<FileModal>,
    approved_close: bool,
    label_modal: Option<LabelDraft>,
    restore_canvas_focus: bool,
}
#[derive(Clone, Debug)]
struct LabelDraft {
    id: ComponentId,
    text: String,
    error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingAction {
    New,
    Open(std::path::PathBuf),
    Close,
}
#[derive(Clone, Debug)]
enum FileModal {
    OpenPath,
    SavePath {
        continuation: bool,
    },
    Unsaved,
    RunningClose,
    Overwrite {
        path: std::path::PathBuf,
        continuation: bool,
    },
    PublishedWarning {
        diagnostics: Vec<String>,
        continuation: bool,
    },
}
impl Default for DigitalApp {
    fn default() -> Self {
        let cwd = std::env::current_dir();
        let (file_io, file_io_error) = match cwd {
            Ok(path) if path.is_absolute() => (
                Some(Box::new(LocalFileIo::new(path)) as Box<dyn FileIo>),
                None,
            ),
            Ok(_) => (
                None,
                Some("The current directory is not absolute; file actions are unavailable".into()),
            ),
            Err(error) => (
                None,
                Some(format!("Cannot initialize local file access: {error}")),
            ),
        };
        Self::with_file_io(file_io, file_io_error)
    }
}
impl DigitalApp {
    pub fn with_file_io(file_io: Option<Box<dyn FileIo>>, file_io_error: Option<String>) -> Self {
        Self {
            session: Session::default(),
            selected: None,
            selected_wire: None,
            wiring: false,
            wire_start: None,
            focused_pin: None,
            placing: None,
            drag: None,
            error: None,
            settle_budget: SETTLE_BUDGET_PER_FRAME,
            settling: false,
            failed_start: None,
            style: DEFAULT_STYLE,
            renderer: SymbolRenderer::default(),
            file_io,
            file_io_error,
            file_path_entry: String::new(),
            pending_action: None,
            file_modal: None,
            approved_close: false,
            label_modal: None,
            restore_canvas_focus: false,
        }
    }
}
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
}
fn draw_grid(
    painter: &egui::Painter,
    rect: Rect,
    transform: CanvasTransform,
    color: egui::Color32,
) {
    let stroke = Stroke::new(0.5_f32, color);
    let [left, top] = transform.to_world(rect.min);
    let [right, bottom] = transform.to_world(rect.max);
    let mut column = (left / GRID_UNIT).floor();
    while column * GRID_UNIT < right {
        let x = transform.to_screen([column * GRID_UNIT, 0.]).x;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            stroke,
        );
        column += 1.;
    }
    let mut row = (top / GRID_UNIT).floor();
    while row * GRID_UNIT < bottom {
        let y = transform.to_screen([0., row * GRID_UNIT]).y;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            stroke,
        );
        row += 1.;
    }
}
impl DigitalApp {
    fn modality_active(&self) -> bool {
        self.file_modal.is_some() || self.label_modal.is_some()
    }
    fn begin_label_edit(&mut self) {
        if self.session.simulation_active() || self.file_modal.is_some() {
            self.error = Some(EDIT_LOCK_NOTE.into());
            return;
        }
        let Some(id) = self.selected else {
            return;
        };
        let Some(text) = self
            .session
            .document()
            .components
            .iter()
            .find(|component| component.id == id)
            .map(|component| component.label.clone())
        else {
            return;
        };
        self.cancel_gestures();
        self.label_modal = Some(LabelDraft {
            id,
            text,
            error: None,
        });
    }
    fn cancel_label_edit(&mut self) {
        if self.label_modal.take().is_some() {
            self.restore_canvas_focus = true;
        }
    }
    fn finish_label_edit(&mut self) {
        let Some((id, text)) = self
            .label_modal
            .as_ref()
            .map(|draft| (draft.id, draft.text.clone()))
        else {
            return;
        };
        match self.session.set_label(id, &text) {
            Ok(()) => {
                self.error = None;
                self.cancel_label_edit();
            }
            Err(error) => {
                if let Some(draft) = self.label_modal.as_mut() {
                    draft.error = Some(error.to_string());
                }
            }
        }
    }
    fn cancel_gestures(&mut self) {
        self.placing = None;
        self.wiring = false;
        self.wire_start = None;
        self.focused_pin = None;
        self.drag = None;
    }
    fn begin_pending(&mut self, action: PendingAction) {
        // A published warning is an acknowledgement barrier. In particular, a window-manager
        // close must not replace the New/Open continuation attached to the warning.
        if action == PendingAction::Close
            && matches!(self.file_modal, Some(FileModal::PublishedWarning { .. }))
        {
            return;
        }
        if self.session.simulation_active() && !matches!(action, PendingAction::Close) {
            self.error = Some("Stop the simulation before changing files".into());
            return;
        }
        self.cancel_gestures();
        self.pending_action = Some(action.clone());
        if action == PendingAction::Close && self.session.simulation_active() {
            self.file_modal = Some(FileModal::RunningClose);
            return;
        }
        if self.session.dirty() {
            self.file_modal = Some(FileModal::Unsaved);
        } else {
            self.complete_pending();
        }
    }
    fn complete_pending(&mut self) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        match action {
            PendingAction::New => match self.session.new_document(DiscardIntent::DiscardDirty) {
                Ok(()) => self.reset_document_ui(),
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.pending_action = Some(PendingAction::New);
                }
            },
            PendingAction::Open(path) => {
                let result = self
                    .file_io
                    .as_mut()
                    .ok_or_else(|| {
                        self.file_io_error
                            .clone()
                            .unwrap_or_else(|| "File access unavailable".into())
                    })
                    .and_then(|io| {
                        self.session
                            .open_path(io.as_mut(), &path, DiscardIntent::DiscardDirty)
                            .map_err(|e| e.to_string())
                    });
                match result {
                    Ok(()) => self.reset_document_ui(),
                    Err(error) => {
                        self.error = Some(error);
                        self.pending_action = Some(PendingAction::Open(path));
                    }
                }
            }
            PendingAction::Close => {
                if self.session.simulation_active() {
                    let _ = self.session.stop_simulation();
                }
                self.settling = false;
                self.approved_close = true;
            }
        }
    }
    fn reset_document_ui(&mut self) {
        self.cancel_label_edit();
        self.selected = None;
        self.selected_wire = None;
        self.cancel_gestures();
        self.failed_start = None;
        self.settling = false;
        self.error = None;
    }
    fn open_path_dialog(&mut self) {
        self.cancel_gestures();
        self.file_path_entry.clear();
        self.file_modal = Some(FileModal::OpenPath);
    }
    fn save_path_dialog(&mut self, continuation: bool) {
        self.cancel_gestures();
        self.file_path_entry.clear();
        self.file_modal = Some(FileModal::SavePath { continuation });
    }
    fn begin_save(&mut self, continuation: bool) {
        self.cancel_gestures();
        if self.session.simulation_active() {
            self.error = Some("Stop the simulation before saving".into());
            return;
        }
        if self.session.path().is_none() {
            self.save_path_dialog(continuation);
        } else {
            self.save_current(continuation);
        }
    }
    fn save_current(&mut self, continuation: bool) {
        let result = self
            .file_io
            .as_mut()
            .ok_or_else(|| {
                self.file_io_error
                    .clone()
                    .unwrap_or_else(|| "File access unavailable".into())
            })
            .and_then(|io| self.session.save(io.as_mut()).map_err(|e| e.to_string()));
        match result {
            Ok(outcome) => self.after_save(outcome, continuation),
            Err(error) => self.error = Some(error),
        }
    }
    fn save_as_path(&mut self, path: std::path::PathBuf, continuation: bool, replace: bool) {
        let Some(io) = self.file_io.as_mut() else {
            self.error = Some(
                self.file_io_error
                    .clone()
                    .unwrap_or_else(|| "File access unavailable".into()),
            );
            return;
        };
        let result = self.session.save_as(
            io.as_mut(),
            &path,
            if replace {
                SaveAsIntent::Replace
            } else {
                SaveAsIntent::CreateOnly
            },
        );
        match result {
            Ok(outcome) => self.after_save(outcome, continuation),
            Err(SaveError::NotPublished(failure))
                if !replace && failure.primary.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                self.file_modal = Some(FileModal::Overwrite { path, continuation });
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn after_save(&mut self, outcome: PublishOutcome, continuation: bool) {
        match outcome {
            PublishOutcome::Published => {
                if continuation {
                    self.complete_pending();
                }
            }
            PublishOutcome::PublishedDurabilityUnconfirmed(diagnostics) => {
                self.file_modal = Some(FileModal::PublishedWarning {
                    diagnostics: diagnostics.iter().map(ToString::to_string).collect(),
                    continuation,
                });
            }
        }
    }
    fn file_actions(&mut self, ctx: &egui::Context) {
        let locked = self.session.simulation_active() || self.modality_active();
        let shortcut = ctx.input_mut(|i| {
            let command = egui::Modifiers::COMMAND;
            let shift = egui::Modifiers {
                shift: true,
                ..command
            };
            if i.consume_key(command, egui::Key::N) {
                Some(0)
            } else if i.consume_key(command, egui::Key::O) {
                Some(1)
            } else if i.consume_key(shift, egui::Key::S) {
                Some(3)
            } else if i.consume_key(command, egui::Key::S) {
                Some(2)
            } else {
                None
            }
        });
        if !locked && let Some(action) = shortcut {
            match action {
                0 => self.begin_pending(PendingAction::New),
                1 => self.open_path_dialog(),
                2 => self.begin_save(false),
                _ => self.save_path_dialog(false),
            }
        }
    }
    fn file_modal_ui(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.file_modal.clone() else {
            return;
        };
        // Durability diagnostics are rendered in the palette panel so the complete list remains
        // visible and the acknowledgement control is operable even for several warnings.
        if matches!(
            modal,
            FileModal::PublishedWarning { .. }
                | FileModal::OpenPath
                | FileModal::SavePath { .. }
                | FileModal::Unsaved
                | FileModal::RunningClose
        ) {
            return;
        }
        enum Choice {
            Cancel,
            Open(std::path::PathBuf),
            Save(std::path::PathBuf),
            SavePending,
            Discard,
            ConfirmReplace(std::path::PathBuf, bool),
            Acknowledge(bool),
            Stop,
            ContinueClose,
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::new("document_file_modal")).show(ctx, |ui| {
            ui.set_min_width(360.);
            match &modal {
                FileModal::OpenPath | FileModal::SavePath { .. } => {
                    let saving = matches!(modal, FileModal::SavePath { .. });
                    ui.heading(if saving { "Save As" } else { "Open document" });
                    ui.label("Path (absolute or relative to the application's startup directory):");
                    ui.text_edit_singleline(&mut self.file_path_entry);
                    ui.horizontal(|ui| {
                        if ui.button(if saving { "Save" } else { "Open" }).clicked() {
                            let path = std::path::PathBuf::from(self.file_path_entry.as_str());
                            if saving { choice = Some(Choice::Save(path)); } else { choice = Some(Choice::Open(path)); }
                        }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                    if let Some(error) = &self.file_io_error { ui.colored_label(ui.visuals().error_fg_color, error); }
                }
                FileModal::Unsaved => {
                    ui.heading("Unsaved changes");
                    ui.label("Save changes before continuing?");
                    if self.session.simulation_active() { ui.colored_label(ui.visuals().warn_fg_color, "Stop simulation before saving. Cancel preserves the running simulation."); }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.session.simulation_active() && self.file_io.is_some(), egui::Button::new("Save")).clicked() { choice = Some(Choice::SavePending); }
                        if ui.button("Discard").clicked() { choice = Some(Choice::Discard); }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                    if self.session.simulation_active() && ui.button("Stop simulation").clicked() { choice = Some(Choice::Stop); }
                }
                FileModal::RunningClose => {
                    ui.heading("Simulation is running");
                    ui.label("Closing will stop the simulation. Cancel preserves its current runtime state.");
                    ui.horizontal(|ui| {
                        if ui.button("Stop and continue closing").clicked() { choice = Some(Choice::ContinueClose); }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                }
                FileModal::Overwrite { path, continuation } => {
                    ui.heading("Replace existing file?");
                    ui.label(format!("{} already exists. Replace it?", path.display()));
                    ui.horizontal(|ui| {
                        if ui.button("Replace").clicked() { choice = Some(Choice::ConfirmReplace(path.clone(), *continuation)); }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                }
                FileModal::PublishedWarning { diagnostics, .. } => {
                    ui.heading("File published; durability unconfirmed");
                    ui.label("The new file is published and the session is clean, but durability is unconfirmed. Review all diagnostics before continuing.");
                    for diagnostic in diagnostics { ui.label(diagnostic); }
                    if ui.button("Acknowledge and continue").clicked()
                        && let FileModal::PublishedWarning { continuation, .. } = modal
                    {
                        choice = Some(Choice::Acknowledge(continuation));
                    }
                }
            }
        });
        if let Some(choice) = choice {
            match choice {
                Choice::Cancel => {
                    self.file_modal = None;
                    self.pending_action = None;
                }
                Choice::Open(path) => {
                    self.file_modal = None;
                    self.begin_pending(PendingAction::Open(path));
                }
                Choice::Save(path) => {
                    let continuation = matches!(modal, FileModal::SavePath { continuation: true });
                    self.file_modal = None;
                    self.save_as_path(path, continuation, false);
                }
                Choice::SavePending => {
                    self.file_modal = None;
                    self.begin_save(true);
                }
                Choice::Discard => {
                    self.file_modal = None;
                    self.complete_pending();
                }
                Choice::ConfirmReplace(path, continuation) => {
                    self.file_modal = None;
                    self.save_as_path(path, continuation, true);
                }
                Choice::Acknowledge(continuation) => {
                    self.file_modal = None;
                    if continuation {
                        self.complete_pending();
                    }
                }
                Choice::Stop => {
                    let _ = self.session.stop_simulation();
                    self.settling = false;
                }
                Choice::ContinueClose => {
                    if self.session.simulation_active() {
                        let _ = self.session.stop_simulation();
                    }
                    self.settling = false;
                    self.file_modal = None;
                    if self.session.dirty() {
                        self.file_modal = Some(FileModal::Unsaved);
                    } else {
                        self.complete_pending();
                    }
                }
            }
        }
    }
    fn label_modal_ui(&mut self, ctx: &egui::Context) {
        if self.label_modal.is_none() {
            return;
        }
        let close =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let apply = ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
        });
        if close {
            self.cancel_label_edit();
            return;
        }
        if apply {
            self.finish_label_edit();
        }
        let mut action = None;
        egui::Modal::new(egui::Id::new("component_label_modal")).show(ctx, |ui| {
            ui.set_min_width(380.);
            if let Some(draft) = self.label_modal.as_mut() {
                ui.heading(format!("Edit label — component #{}", draft.id.0));
                ui.add(
                    egui::TextEdit::multiline(&mut draft.text)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("Component label"),
                );
                ui.label(format!("{} / 4096 UTF-8 bytes", draft.text.len()));
                if let Err(error) = crate::domain::validate_label(&draft.text) {
                    ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                }
                if let Some(error) = &draft.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                });
                ui.label("Ctrl/Cmd+Enter applies; Esc cancels.");
            }
        });
        match action {
            Some(true) => self.finish_label_edit(),
            Some(false) => self.cancel_label_edit(),
            None => {}
        }
    }
    fn choose_pin(&mut self, pin: PinRef) {
        if let Some(start) = self.wire_start {
            match self.session.connect([start, pin]) {
                Ok(_) => {
                    self.wire_start = None;
                    self.error = None;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        } else {
            self.wire_start = Some(pin);
        }
    }
    fn sim_state(&self) -> SimState {
        match self.session.inspect() {
            Some(runtime) if runtime.is_pending() => SimState::Pending {
                settling: self.settling,
            },
            Some(_) => SimState::Settled,
            None if self.failed_start.as_ref() == Some(self.session.document()) => SimState::Error,
            None => SimState::Idle,
        }
    }
    fn report_simulation_error(&mut self, error: &SimulationError) {
        self.error = Some(describe_simulation_error(self.session.document(), error));
    }
    /// Applies a simulation command through the engine. Starting cancels every transient
    /// editing gesture; a failed start leaves the editor exactly as it was.
    fn apply_simulation_action(&mut self, action: SimAction) {
        match action {
            SimAction::Start => match self.session.start_simulation() {
                Ok(()) => {
                    self.placing = None;
                    self.wiring = false;
                    self.wire_start = None;
                    self.focused_pin = None;
                    self.drag = None;
                    self.failed_start = None;
                    self.error = None;
                    self.settling = true;
                }
                Err(error) => {
                    if matches!(error, SimulationError::Compile(_)) {
                        self.failed_start = Some(self.session.document().clone());
                    }
                    self.report_simulation_error(&error);
                }
            },
            SimAction::Stop => match self.session.stop_simulation() {
                Ok(()) => {
                    self.settling = false;
                    self.error = None;
                }
                Err(error) => self.report_simulation_error(&error),
            },
            SimAction::Step => {
                self.settling = false;
                if let Err(error) = self.session.step_simulation() {
                    self.report_simulation_error(&error);
                }
            }
            SimAction::Settle => {
                if self.session.simulation_active() {
                    self.settling = true;
                }
            }
            SimAction::Toggle(id) => match self.session.toggle_input(id) {
                Ok(InputChange::Changed) => {
                    self.settling = true;
                    self.error = None;
                }
                Ok(InputChange::Unchanged) => {}
                Err(error) => self.report_simulation_error(&error),
            },
        }
    }
    /// Runs one bounded settle chunk. While work remains a repaint is requested, so the next
    /// frame continues and Stop is handled in between.
    fn advance_simulation(&mut self, ctx: &egui::Context) {
        if !self.settling {
            return;
        }
        match self.session.settle_simulation(self.settle_budget) {
            Ok(SettleOutcome::BudgetExhausted { .. }) => {}
            Ok(SettleOutcome::Settled { .. }) | Err(_) => self.settling = false,
        }
        // One more frame also after the last chunk, so the settled state is displayed.
        ctx.request_repaint();
    }
    fn simulation_shortcuts(&mut self, ctx: &egui::Context) {
        let action = ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::SHIFT, egui::Key::F5) {
                Some(SimAction::Stop)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::F5) {
                Some(SimAction::Start)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::F10) {
                Some(SimAction::Step)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::F6) {
                Some(SimAction::Settle)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.apply_simulation_action(action);
        }
    }
    fn simulation_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("simulation").show(ctx, |ui| {
            ui.heading("Simulation");
            let active = self.session.simulation_active();
            let modal_blocked = self.modality_active();
            let mut action = None;
            ui.horizontal_wrapped(|ui| {
                for (label, enabled, hint, command) in [
                    (
                        "Start",
                        !active && !modal_blocked,
                        "Start (F5)",
                        SimAction::Start,
                    ),
                    (
                        "Stop",
                        active && !modal_blocked,
                        "Stop (Shift+F5)",
                        SimAction::Stop,
                    ),
                    (
                        "Step",
                        active && !modal_blocked,
                        "One microstep (F10)",
                        SimAction::Step,
                    ),
                    (
                        "Settle",
                        active && !modal_blocked,
                        "Run until stable (F6)",
                        SimAction::Settle,
                    ),
                ] {
                    if ui
                        .add_enabled(enabled, egui::Button::new(label))
                        .on_hover_text(hint)
                        .clicked()
                    {
                        action = Some(command);
                    }
                }
            });
            ui.label(match self.sim_state() {
                SimState::Idle => "State: idle (not simulating)",
                SimState::Error => "State: error (cannot start, see message)",
                SimState::Pending { settling: true } => "State: pending (settling)",
                SimState::Pending { settling: false } => "State: pending (paused; Step or Settle)",
                SimState::Settled => "State: settled",
            });
            ui.label("F5 start, Shift+F5 stop, F10 step, F6 settle.");
            if let Some(runtime) = self.session.inspect() {
                let document = self.session.document();
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("simulation_values")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label("Inputs (toggle when settled)");
                        for component in &document.components {
                            if component.kind != ComponentKind::Input {
                                continue;
                            }
                            let level = runtime
                                .pin_value(PinRef {
                                    component: component.id,
                                    pin: PinId {
                                        direction: PinDirection::Output,
                                        index: 0,
                                    },
                                })
                                .unwrap_or(Level::Unavailable);
                            let text = format!(
                                "{} #{}: {}",
                                component.label,
                                component.id.0,
                                level_text(level)
                            );
                            if ui
                                .add_enabled(
                                    !runtime.is_pending() && !modal_blocked,
                                    egui::Button::new(text),
                                )
                                .clicked()
                            {
                                action = Some(SimAction::Toggle(component.id));
                            }
                        }
                        ui.separator();
                        ui.label("Outputs");
                        for component in &document.components {
                            if component.kind == ComponentKind::Output {
                                ui.label(format!(
                                    "{} #{}: {}",
                                    component.label,
                                    component.id.0,
                                    runtime
                                        .output_value(component.id)
                                        .map_or("? (unavailable)", level_text)
                                ));
                            }
                        }
                        ui.separator();
                        ui.label("Gates");
                        for component in &document.components {
                            if matches!(
                                component.kind,
                                ComponentKind::And
                                    | ComponentKind::NAnd
                                    | ComponentKind::Or
                                    | ComponentKind::NOr
                                    | ComponentKind::XOr
                                    | ComponentKind::XNOr
                                    | ComponentKind::Not
                            ) {
                                let level = runtime
                                    .pin_value(PinRef {
                                        component: component.id,
                                        pin: PinId {
                                            direction: PinDirection::Output,
                                            index: 0,
                                        },
                                    })
                                    .unwrap_or(Level::Unavailable);
                                ui.label(format!(
                                    "{} gate id {}: {}",
                                    component.kind.name(),
                                    component.id.0,
                                    level_text(level)
                                ));
                            }
                        }
                    });
            }
            if let Some(action) = action.filter(|_| !modal_blocked) {
                self.apply_simulation_action(action);
            }
        });
    }
    fn palette_panel(&mut self, ctx: &egui::Context) {
        let mut acknowledge_warning = None;
        let mut path_choice: Option<(bool, std::path::PathBuf, bool)> = None;
        let mut cancel_path = false;
        let mut unsaved_choice = None;
        let mut running_close_choice = None;
        egui::SidePanel::left("palette").show(ctx, |ui| {
            ui.heading("Components");
            let locked = self.session.simulation_active() || self.modality_active();
            ui.add_enabled_ui(!locked, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for kind in KINDS {
                        if ui
                            .selectable_label(self.placing == Some(kind), kind.name())
                            .clicked()
                        {
                            self.placing = Some(kind);
                            self.wiring = false;
                            self.wire_start = None;
                        }
                    }
                    if ui.selectable_label(self.wiring, "Wire (W)").clicked() {
                        self.wiring = !self.wiring;
                        self.wire_start = None;
                        self.placing = None;
                        self.drag = None;
                    }
                });
            });
            if locked {
                ui.label(if self.session.simulation_active() { EDIT_LOCK_NOTE } else { "Finish or cancel the file action to resume editing." });
            }
            ui.separator();
            ui.add_enabled_ui(!locked, |ui| {
                ui.label("Shape style");
                ui.radio_value(&mut self.style, SymbolStyle::Rect, "Rectangular");
                ui.radio_value(&mut self.style, SymbolStyle::Ieee, "IEEE");
            });
            ui.separator();
            ui.label("Click canvas to place. Drag to move. Escape cancels. Delete removes; Ctrl/Cmd-Z undoes; Shift-Ctrl/Cmd-Z redoes.");
            ui.label(if self.session.dirty() { "Modified" } else { "Unmodified" });
            if let Some(id) = self.selected {
                let label = self.session.document().components.iter().find(|component| component.id == id).map(|component| component.label.clone());
                if let Some(label) = label {
                    ui.label(format!("Selected label: {label}"));
                    if ui.add_enabled(!locked, egui::Button::new("Edit label… (F2)")).clicked() {
                        self.begin_label_edit();
                    }
                }
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(error) = &self.file_io_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(FileModal::PublishedWarning { diagnostics, continuation }) = self.file_modal.clone() {
                ui.colored_label(ui.visuals().warn_fg_color, "Published; durability unconfirmed. Session is clean; review all diagnostics.");
                egui::ScrollArea::vertical().max_height(180.).show(ui, |ui| {
                    for diagnostic in diagnostics { ui.label(diagnostic); }
                });
                if ui.button("Acknowledge and continue").clicked() { acknowledge_warning = Some(continuation); }
            }
            if matches!(self.file_modal, Some(FileModal::RunningClose)) {
                ui.separator();
                ui.heading("Simulation is running");
                ui.label("Closing will stop the simulation. Cancel preserves its current runtime state.");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Stop and continue closing").clicked() { running_close_choice = Some(true); }
                    if ui.button("Cancel").clicked() { running_close_choice = Some(false); }
                });
            }
            if matches!(self.file_modal, Some(FileModal::Unsaved)) {
                ui.separator();
                ui.heading("Unsaved changes");
                ui.label("Save changes before continuing?");
                if self.session.simulation_active() {
                    ui.colored_label(ui.visuals().warn_fg_color, "Stop simulation before saving. Cancel preserves the running simulation.");
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!self.session.simulation_active() && self.file_io.is_some(), egui::Button::new("Save")).clicked() { unsaved_choice = Some(0); }
                    if ui.button("Discard").clicked() { unsaved_choice = Some(1); }
                    if ui.button("Cancel").clicked() { unsaved_choice = Some(2); }
                    if self.session.simulation_active() && ui.button("Stop simulation").clicked() { unsaved_choice = Some(3); }
                });
            }
            match self.file_modal.clone() {
                Some(FileModal::OpenPath) | Some(FileModal::SavePath { .. }) => {
                    let saving = matches!(self.file_modal, Some(FileModal::SavePath { .. }));
                    let continuation = matches!(self.file_modal, Some(FileModal::SavePath { continuation: true }));
                    ui.separator();
                    ui.heading(if saving { "Save As" } else { "Open document" });
                    ui.label("Path relative to the application's startup directory, or an absolute path:");
                    ui.text_edit_singleline(&mut self.file_path_entry);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(if saving { "Save path" } else { "Open path" }).clicked() {
                            path_choice = Some((saving, std::path::PathBuf::from(self.file_path_entry.as_str()), continuation));
                        }
                        if ui.button("Cancel path").clicked() { cancel_path = true; }
                    });
                }
                _ => {}
            }
            ui.separator();
            let document_name = self
                .session
                .path()
                .and_then(std::path::Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into());
            ui.heading(format!(
                "{}{}",
                if self.session.dirty() { "• " } else { "" },
                document_name
            ));
            let mut file_action = None;
            if !self.modality_active() {
            ui.menu_button("File", |ui| {
                for (label, enabled, action) in [("New  Ctrl/Cmd+N", !locked, 0), ("Open…  Ctrl/Cmd+O", !locked, 1), ("Save  Ctrl/Cmd+S", !locked, 2), ("Save As…  Shift+Ctrl/Cmd+S", !locked, 3)] {
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() { file_action = Some(action); ui.close_menu(); }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (label, action) in [("New", 0), ("Open", 1), ("Save", 2), ("Save As", 3)] {
                    if ui.add_enabled(!locked, egui::Button::new(label)).clicked() { file_action = Some(action); }
                }
            });
            if let Some(action) = file_action {
                match action {
                    0 => self.begin_pending(PendingAction::New),
                    1 => self.open_path_dialog(),
                    2 => self.begin_save(false),
                    _ => self.save_path_dialog(false)
                }
            }
            }
        });
        if let Some(continuation) = acknowledge_warning {
            self.file_modal = None;
            if continuation {
                self.complete_pending();
            }
        }
        if cancel_path {
            self.file_modal = None;
            self.pending_action = None;
        }
        match unsaved_choice {
            Some(0) => {
                self.file_modal = None;
                self.begin_save(true);
            }
            Some(1) => {
                self.file_modal = None;
                self.complete_pending();
            }
            Some(2) => {
                self.file_modal = None;
                self.pending_action = None;
            }
            Some(3) => {
                let _ = self.session.stop_simulation();
                self.settling = false;
            }
            _ => {}
        }
        if let Some(continue_close) = running_close_choice {
            if continue_close {
                let _ = self.session.stop_simulation();
                self.settling = false;
                self.file_modal = None;
                if self.session.dirty() {
                    self.file_modal = Some(FileModal::Unsaved);
                } else {
                    self.complete_pending();
                }
            } else {
                self.file_modal = None;
                self.pending_action = None;
            }
        }
        if let Some((saving, path, continuation)) = path_choice {
            self.file_modal = None;
            if saving {
                self.save_as_path(path, continuation, false);
            } else {
                self.begin_pending(PendingAction::Open(path));
            }
        }
    }
    fn show(&mut self, ctx: &egui::Context) {
        self.file_actions(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.approved_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.cancel_label_edit();
            self.begin_pending(PendingAction::Close);
        }
        if self.approved_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.label_modal.is_some() && ctx.input(|input| !input.focused) {
            self.cancel_label_edit();
        }
        let modal_open = self.modality_active();
        if !modal_open {
            self.simulation_shortcuts(ctx);
        }
        self.palette_panel(ctx);
        self.simulation_panel(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            let locked = self.session.simulation_active();
            let (rect, response) =
                ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            // The world origin sits at the canvas corner until panning exists.
            let Some(transform) = CanvasTransform::new(rect.min, CANVAS_ZOOM) else {
                return;
            };
            let style = self.style;
            draw_grid(
                &painter,
                rect,
                transform,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            );
            let cancelled =
                modal_open || ctx.input(|i| !i.focused || i.key_pressed(egui::Key::Escape));
            let pointer = (!cancelled)
                .then(|| response.interact_pointer_pos())
                .flatten();
            if cancelled {
                self.drag = None;
                self.placing = None;
                self.wire_start = None;
            }
            let mut pins: Vec<_> = self
                .session
                .document()
                .components
                .iter()
                .flat_map(|c| {
                    let layout =
                        SymbolLayout::centered_on(style, c.kind, [c.position.x, c.position.y]);
                    [PinDirection::Input, PinDirection::Output]
                        .into_iter()
                        .flat_map(move |direction| {
                            (0..2).filter_map(move |index| {
                                let pin = PinId { direction, index };
                                layout.and_then(|l| l.pin_position(pin)).map(|p| {
                                    (
                                        PinRef {
                                            component: c.id,
                                            pin,
                                        },
                                        p,
                                    )
                                })
                            })
                        })
                })
                .collect();
            let press = ctx.input(|i| {
                i.events.iter().find_map(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    } => Some(*pos),
                    _ => None,
                })
            });
            if !cancelled
                && let Some(p) = press
                && rect.contains(p)
            {
                response.request_focus();
                if self.wiring && !locked {
                    let hit = pins
                        .iter()
                        .filter_map(|(pin, pos)| {
                            let d = transform.to_screen(*pos).distance(p);
                            (d <= 8.).then_some((*pin, d))
                        })
                        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
                        .map(|v| v.0);
                    if let Some(pin) = hit {
                        self.choose_pin(pin);
                    }
                } else if self.placing.is_none() {
                    let world = transform.to_world(p);
                    let tolerance = transform.length_to_world(PICK_TOLERANCE);
                    let hit = self
                        .session
                        .document()
                        .components
                        .iter()
                        .rev()
                        .find(|c| {
                            SymbolLayout::centered_on(style, c.kind, [c.position.x, c.position.y])
                                .is_some_and(|layout| layout.hit(world, tolerance))
                        })
                        .map(|c| (c.id, c.position));
                    self.selected_wire = if hit.is_none() {
                        self.session
                            .document()
                            .wires
                            .iter()
                            .rev()
                            .find(|wire| {
                                let a = pins.iter().find(|v| v.0 == wire.endpoints[0]);
                                let b = pins.iter().find(|v| v.0 == wire.endpoints[1]);
                                a.zip(b).is_some_and(|(a, b)| {
                                    crate::wire_geometry::hit(
                                        &crate::wire_geometry::path(*a, *b),
                                        world,
                                        transform.length_to_world(6.),
                                    )
                                })
                            })
                            .map(|w| w.id)
                    } else {
                        None
                    };
                    self.selected = hit.map(|v| v.0);
                    self.drag = hit.filter(|_| !locked).map(|(id, pos)| (id, pos, p));
                    if locked
                        && let Some((id, _)) = hit
                        && self
                            .session
                            .document()
                            .components
                            .iter()
                            .any(|c| c.id == id && c.kind == ComponentKind::Input)
                    {
                        self.apply_simulation_action(SimAction::Toggle(id));
                    }
                }
            }
            if !cancelled
                && !locked
                && response.clicked()
                && let Some(kind) = self.placing.take()
                && let Some(p) = pointer
            {
                let [x, y] = transform.to_world(p);
                match self.session.place(kind, Position { x, y }) {
                    Ok(id) => {
                        self.selected = Some(id);
                        self.selected_wire = None;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            if !cancelled && response.clicked() {
                self.drag = None;
            }
            if !cancelled
                && !locked
                && response.drag_stopped()
                && let (Some((id, start, origin)), Some(p)) = (self.drag.take(), pointer)
            {
                let [dx, dy] = transform.delta_to_world(p - origin);
                match self.session.move_component(
                    id,
                    Position {
                        x: start.x + dx,
                        y: start.y + dy,
                    },
                ) {
                    Ok(()) => {
                        // Keep committed wire anchors aligned with the symbol on the release frame.
                        for (pin, pos) in &mut pins {
                            if pin.component == id {
                                pos[0] += dx;
                                pos[1] += dy;
                            }
                        }
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            let palette = Palette::from_visuals(ui.visuals());
            // Use the same transient position for attached wires and symbols.
            let wire_pins: Vec<_> = pins
                .iter()
                .map(|(pin, pos)| {
                    let mut pos = *pos;
                    if let (Some((id, _, origin)), Some(p)) = (self.drag, pointer)
                        && id == pin.component
                    {
                        let [dx, dy] = transform.delta_to_world(p - origin);
                        pos[0] += dx;
                        pos[1] += dy;
                    }
                    (*pin, pos)
                })
                .collect();
            for wire in &self.session.document().wires {
                let a = wire_pins.iter().find(|v| v.0 == wire.endpoints[0]);
                let b = wire_pins.iter().find(|v| v.0 == wire.endpoints[1]);
                if let Some((a, b)) = a.zip(b) {
                    let path = crate::wire_geometry::path(*a, *b);
                    let color = if self.selected_wire == Some(wire.id) {
                        palette.selection
                    } else {
                        palette.ink
                    };
                    if path.len() == 1 {
                        painter.circle_filled(transform.to_screen(path[0]), 4., color);
                    }
                    for segment in path.windows(2) {
                        painter.line_segment(
                            [
                                transform.to_screen(segment[0]),
                                transform.to_screen(segment[1]),
                            ],
                            Stroke::new(2_f32, color),
                        );
                    }
                }
            }
            if self.wiring {
                for (pin, pos) in &wire_pins {
                    painter.circle_stroke(
                        transform.to_screen(*pos),
                        if self.focused_pin == Some(*pin) {
                            7.
                        } else {
                            4.
                        },
                        Stroke::new(2_f32, palette.selection),
                    );
                }
                if !cancelled
                    && let Some(start) = self.wire_start
                    && let Some(a) = wire_pins.iter().find(|v| v.0 == start)
                    && let Some(p) = response.hover_pos()
                {
                    let b = (start, transform.to_world(p));
                    for segment in crate::wire_geometry::path(*a, b).windows(2) {
                        painter.line_segment(
                            [
                                transform.to_screen(segment[0]),
                                transform.to_screen(segment[1]),
                            ],
                            Stroke::new(1_f32, palette.selection),
                        );
                    }
                }
            }
            let selection_width = ui.visuals().selection.stroke.width;
            for c in &self.session.document().components {
                let mut pos = c.position;
                if let (Some((id, start, origin)), Some(p)) = (self.drag, pointer)
                    && id == c.id
                {
                    let [dx, dy] = transform.delta_to_world(p - origin);
                    pos = Position {
                        x: start.x + dx,
                        y: start.y + dy,
                    };
                }
                let state = if self.selected == Some(c.id) {
                    PaintState::Selection
                } else {
                    PaintState::Ink
                };
                let Some(layout) = SymbolLayout::centered_on(style, c.kind, [pos.x, pos.y]) else {
                    self.error = Some(format!("No symbol is drawn for {}", c.kind.name()));
                    continue;
                };
                if matches!(
                    c.kind,
                    ComponentKind::NAnd
                        | ComponentKind::NOr
                        | ComponentKind::XOr
                        | ComponentKind::XNOr
                ) {
                    // No cleared IEEE artwork is available for these gates. Keep one shared
                    // bounds/anchor/picking layout and render an explicit named text box instead
                    // of misleadingly reusing a plain AND/OR symbol.
                    let screen = Rect::from_min_max(
                        transform.to_screen(layout.bounds().min),
                        transform.to_screen(layout.bounds().max),
                    );
                    painter.rect_stroke(
                        screen,
                        3.0,
                        Stroke::new(STROKE_WIDTH, palette.color(state)),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        screen.center(),
                        egui::Align2::CENTER_CENTER,
                        c.kind.name(),
                        egui::FontId::proportional(13.0),
                        palette.color(state),
                    );
                } else if let Err(e) = self
                    .renderer
                    .draw(&painter, &layout, &transform, &palette, state)
                {
                    self.error = Some(e.to_string());
                }
                let bounds = layout.bounds();
                if state == PaintState::Selection {
                    let frame = Rect::from_min_max(
                        transform.to_screen(bounds.min),
                        transform.to_screen(bounds.max),
                    )
                    .expand(SELECTION_MARGIN * transform.zoom());
                    painter.rect_stroke(
                        frame,
                        3.,
                        Stroke::new(selection_width.max(STROKE_WIDTH), palette.selection),
                        egui::StrokeKind::Inside,
                    );
                }
                painter.text(
                    transform.to_screen([(bounds.min[0] + bounds.max[0]) / 2., bounds.max[1]])
                        + Vec2::new(0., SELECTION_MARGIN * transform.zoom() + LABEL_GAP),
                    egui::Align2::CENTER_TOP,
                    &c.label,
                    egui::FontId::proportional(14.),
                    palette.color(state),
                );
                if let Some(runtime) = self.session.inspect() {
                    let pin = if c.kind == ComponentKind::Output {
                        PinId {
                            direction: PinDirection::Input,
                            index: 0,
                        }
                    } else {
                        PinId {
                            direction: PinDirection::Output,
                            index: 0,
                        }
                    };
                    let level = runtime
                        .pin_value(PinRef {
                            component: c.id,
                            pin,
                        })
                        .unwrap_or(Level::Unavailable);
                    painter.text(
                        transform.to_screen([(bounds.min[0] + bounds.max[0]) / 2., bounds.max[1]])
                            + Vec2::new(0., SELECTION_MARGIN * transform.zoom() + LABEL_GAP + 16.),
                        egui::Align2::CENTER_TOP,
                        level_badge(level),
                        egui::FontId::monospace(14.),
                        palette.color(state),
                    );
                }
            }
            if self.restore_canvas_focus
                && !self.modality_active()
                && ctx.input(|input| input.focused)
            {
                response.request_focus();
                self.restore_canvas_focus = false;
            }
            let owns_keyboard = response.has_focus() && !self.modality_active();
            if owns_keyboard {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        response.id,
                        egui::EventFilter {
                            tab: true,
                            ..Default::default()
                        },
                    )
                });
            }
            if owns_keyboard && locked {
                // Edit shortcuts are refused here, before the engine would refuse them.
                let command = egui::Modifiers::COMMAND;
                let shifted = egui::Modifiers {
                    shift: true,
                    ..command
                };
                let attempted = ctx.input_mut(|i| {
                    [
                        (egui::Modifiers::NONE, egui::Key::W),
                        (egui::Modifiers::NONE, egui::Key::F2),
                        (egui::Modifiers::NONE, egui::Key::Delete),
                        (command, egui::Key::Z),
                        (shifted, egui::Key::Z),
                        (command, egui::Key::Y),
                    ]
                    .into_iter()
                    .fold(false, |any, (modifiers, key)| {
                        i.consume_key(modifiers, key) | any
                    })
                });
                if attempted {
                    self.error = Some(EditError::SimulationActive.to_string());
                }
                if self.selected.is_some_and(|id| {
                    self.session
                        .document()
                        .components
                        .iter()
                        .any(|c| c.id == id && c.kind == ComponentKind::Input)
                }) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space))
                    && let Some(id) = self.selected
                {
                    self.apply_simulation_action(SimAction::Toggle(id));
                }
            }
            if owns_keyboard
                && !locked
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2))
            {
                self.begin_label_edit();
            }
            if owns_keyboard
                && !locked
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::W))
            {
                self.wiring = !self.wiring;
                self.wire_start = None;
                self.placing = None;
                self.drag = None;
            }
            if owns_keyboard {
                let backwards =
                    ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
                let forwards =
                    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
                if backwards || forwards {
                    let visible_pins: Vec<_> = pins
                        .iter()
                        .filter(|(_, p)| rect.contains(transform.to_screen(*p)))
                        .collect();
                    if self.wiring && !visible_pins.is_empty() {
                        let current = self
                            .focused_pin
                            .and_then(|p| visible_pins.iter().position(|v| v.0 == p));
                        let next = match current {
                            Some(i) if backwards => {
                                (i + visible_pins.len() - 1) % visible_pins.len()
                            }
                            Some(i) => (i + 1) % visible_pins.len(),
                            None if backwards => visible_pins.len() - 1,
                            None => 0,
                        };
                        self.focused_pin = Some(visible_pins[next].0);
                    } else if !self.wiring && !self.session.document().wires.is_empty() {
                        let wires = &self.session.document().wires;
                        let current = self
                            .selected_wire
                            .and_then(|id| wires.iter().position(|w| w.id == id));
                        let next = match current {
                            Some(i) if backwards => (i + wires.len() - 1) % wires.len(),
                            Some(i) => (i + 1) % wires.len(),
                            None if backwards => wires.len() - 1,
                            None => 0,
                        };
                        self.selected_wire = Some(wires[next].id);
                        self.selected = None;
                    }
                }
                if self.wiring
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                    && let Some(pin) = self.focused_pin
                    && pins.iter().any(|v| v.0 == pin)
                {
                    self.choose_pin(pin);
                }
                if self.selected_wire.is_some()
                    && !locked
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
                    && let Some(id) = self.selected_wire.take()
                    && let Err(e) = self.session.delete_wire(id)
                {
                    self.error = Some(e.to_string());
                }
            }
            if owns_keyboard
                && !locked
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
                && let Some(id) = self.selected
            {
                match self.session.delete(id) {
                    Ok(()) => {
                        self.selected = None;
                        self.drag = None;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            let (undo, redo) = if owns_keyboard && !locked {
                ctx.input_mut(|i| {
                    let command = egui::Modifiers::COMMAND;
                    let shifted = egui::Modifiers {
                        shift: true,
                        ..command
                    };
                    {
                        let redo = i.consume_key(shifted, egui::Key::Z)
                            || i.consume_key(command, egui::Key::Y);
                        let undo = i.consume_key(command, egui::Key::Z);
                        (undo, redo)
                    }
                })
            } else {
                (false, false)
            };
            if undo {
                self.focused_pin = None;
                self.wire_start = None;
                self.selected_wire = None;
                if self.session.undo().is_ok() {
                    self.selected = None;
                    self.drag = None;
                }
            }
            if redo {
                self.focused_pin = None;
                self.wire_start = None;
                self.selected_wire = None;
                if self.session.redo().is_ok() {
                    self.selected = None;
                    self.drag = None;
                }
            }
        });
        if !modal_open {
            self.advance_simulation(ctx);
        }
        self.file_modal_ui(ctx);
        self.label_modal_ui(ctx);
    }
}
impl eframe::App for DigitalApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.show(ctx);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_retry_and_cancel_are_non_destructive() {
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 10., y: 10. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 10. })
            .unwrap();
        let a = PinRef {
            component: a,
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        };
        let b = PinRef {
            component: b,
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        app.choose_pin(a);
        app.choose_pin(a);
        assert_eq!(app.wire_start, Some(a));
        assert!(app.error.is_some());
        app.choose_pin(b);
        assert_eq!(app.session.document().wires.len(), 1);
        assert!(app.error.is_none());
        app.choose_pin(a);
        let ctx = egui::Context::default();
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.wire_start, None);
        assert_eq!(app.session.document().wires.len(), 1);
    }
    #[test]
    fn keyboard_wire_creation_and_deletion() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        app.session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(400., 300.);
        frame(&ctx, &mut app, vec![button(p, true)], true);
        frame(&ctx, &mut app, vec![button(p, false)], true);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document().wires.len(), 1);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.session.document().wires.is_empty());
    }
    #[test]
    fn placement_replaces_wire_selection_before_delete() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        let pin = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        app.session
            .connect([pin(a, PinDirection::Output), pin(b, PinDirection::Input)])
            .unwrap();
        app.selected_wire = Some(app.session.document().wires[0].id);
        app.placing = Some(ComponentKind::And);
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(500., 300.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert!(app.selected_wire.is_none());
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document().components.len(), 2);
        assert_eq!(app.session.document().wires.len(), 1);
    }
    #[test]
    fn undo_clears_keyboard_pin_and_tab_skips_offscreen() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: -500., y: 100. })
            .unwrap();
        let visible = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(400., 300.);
        frame(&ctx, &mut app, vec![button(p, true)], true);
        frame(&ctx, &mut app, vec![button(p, false)], true);
        app.wiring = true;
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.focused_pin.unwrap().component, visible);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Z, egui::Modifiers::COMMAND)],
            true,
        );
        assert!(app.focused_pin.is_none());
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.wire_start.is_none());
    }
    fn frame(
        ctx: &egui::Context,
        app: &mut DigitalApp,
        events: Vec<egui::Event>,
        focused: bool,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused,
            events,
            ..Default::default()
        };
        ctx.run(input, |ctx| app.show(ctx))
    }
    fn has_line(output: &egui::FullOutput, from: Pos2, to: Pos2, width: f32) -> bool {
        output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::LineSegment { points, stroke } if *points == [from, to] && stroke.width == width))
    }

    #[test]
    fn hover_preview_is_painted_without_a_pressed_button() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        app.wiring = true;
        frame(&ctx, &mut app, vec![], true);
        let pin = PinId {
            direction: PinDirection::Output,
            index: 0,
        };
        let anchor = SymbolLayout::centered_on(app.style, ComponentKind::Input, [100., 100.])
            .unwrap()
            .pin_position(pin)
            .unwrap();
        let start = Pos2::new(anchor[0] + 208., anchor[1] + 8.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(start, false)], true);
        assert!(app.wire_start.is_some());
        let hover = Pos2::new(450., 180.);
        let output = frame(&ctx, &mut app, vec![egui::Event::PointerMoved(hover)], true);
        assert!(has_line(&output, start, Pos2::new(hover.x, start.y), 1.));
        assert!(has_line(&output, Pos2::new(hover.x, start.y), hover, 1.));
        assert!(app.session.document().wires.is_empty());
    }

    #[test]
    fn drag_release_frame_paints_wire_at_committed_anchor() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        let pin = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        app.session
            .connect([pin(a, PinDirection::Output), pin(b, PinDirection::Input)])
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let start = Pos2::new(308., 108.);
        let end = Pos2::new(358., 158.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        let output = frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(
            app.session.document().components[0].position,
            Position { x: 150., y: 150. }
        );
        let input_pin = PinId {
            direction: PinDirection::Output,
            index: 0,
        };
        let output_pin = PinId {
            direction: PinDirection::Input,
            index: 0,
        };
        let anchor = |kind, center, pin| {
            let p = SymbolLayout::centered_on(app.style, kind, center)
                .unwrap()
                .pin_position(pin)
                .unwrap();
            Pos2::new(p[0] + 208., p[1] + 8.)
        };
        let moved = anchor(ComponentKind::Input, [150., 150.], input_pin);
        let old = anchor(ComponentKind::Input, [100., 100.], input_pin);
        let target = anchor(ComponentKind::Output, [200., 100.], output_pin);
        let corner = Pos2::new(target.x, moved.y);
        assert!(has_line(&output, moved, corner, 2.));
        assert!(has_line(&output, corner, target, 2.));
        assert!(!has_line(&output, old, target, 2.));
    }
    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    #[test]
    fn fast_drag_and_escape_and_focus_cancel() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::And, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        // The side panel default width is 200, plus the central margin.
        let start = Pos2::new(308., 108.);
        let end = start + Vec2::new(100., 50.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(
            app.session.document().components[0].position,
            Position { x: 200., y: 150. }
        );
        app.session.undo().unwrap();
        assert_eq!(
            app.session.document().components[0].position,
            Position { x: 100., y: 100. }
        );
        app.session.redo().unwrap();
        let snapshot = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end), button(end, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(end + Vec2::new(50., 0.)),
                key(egui::Key::Escape, egui::Modifiers::NONE),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(end + Vec2::new(50., 0.), false)],
            true,
        );
        assert_eq!(*app.session.document(), snapshot);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end), button(end, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end + Vec2::new(50., 0.))],
            false,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(end + Vec2::new(50., 0.), false)],
            true,
        );
        assert_eq!(*app.session.document(), snapshot);
        assert_eq!(app.session.document().components[0].id, id);
    }
    #[test]
    fn batched_selection_shortcuts_and_delete_drag() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p),
                button(p, true),
                button(p, false),
            ],
            true,
        );
        assert_eq!(app.selected, Some(id));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Z, egui::Modifiers::COMMAND)],
            true,
        );
        assert!(!app.session.dirty());
        let shifted = egui::Modifiers {
            shift: true,
            ..egui::Modifiers::COMMAND
        };
        frame(&ctx, &mut app, vec![key(egui::Key::Z, shifted)], true);
        assert_eq!(app.session.document().components[0].id, id);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p + Vec2::new(30., 0.))],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(p + Vec2::new(30., 0.), false)],
            true,
        );
        assert!(app.session.document().components.is_empty());
        assert!(app.error.is_none());
    }
    #[test]
    fn placement_escape_does_not_edit() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        frame(&ctx, &mut app, vec![], true);
        app.placing = Some(ComponentKind::Or);
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p + Vec2::new(30., 0.)),
                key(egui::Key::Escape, egui::Modifiers::NONE),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(p + Vec2::new(30., 0.), false)],
            true,
        );
        assert!(!app.session.dirty());
        assert!(app.session.document().components.is_empty());
    }
    #[test]
    fn palette_focus_and_placement() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        frame(&ctx, &mut app, vec![], true);
        let palette = Pos2::new(30., 29.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(palette), button(palette, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(palette, false)], true);
        assert_eq!(app.placing, Some(ComponentKind::Input));
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert_eq!(app.session.document().components.len(), 1);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(palette), button(palette, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(palette, false)], true);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document().components.len(), 1);
    }
    #[test]
    fn new_logic_gate_palette_placement_anchors_and_styles_are_shared() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        for (index, (name, kind)) in [
            ("NAND", ComponentKind::NAnd),
            ("NOR", ComponentKind::NOr),
            ("XOR", ComponentKind::XOr),
            ("XNOR", ComponentKind::XNOr),
        ]
        .into_iter()
        .enumerate()
        {
            click_text(&ctx, &mut app, name);
            let center = Pos2::new(
                350. + (index % 2) as f32 * 120.,
                180. + (index / 2) as f32 * 100.,
            );
            click_at(&ctx, &mut app, center);
            assert_eq!(app.session.document().components.last().unwrap().kind, kind);
        }
        for style in [SymbolStyle::Rect, SymbolStyle::Ieee] {
            app.style = style;
            for component in app.session.document().components.iter().filter(|c| {
                matches!(
                    c.kind,
                    ComponentKind::NAnd
                        | ComponentKind::NOr
                        | ComponentKind::XOr
                        | ComponentKind::XNOr
                )
            }) {
                let layout = SymbolLayout::centered_on(
                    style,
                    component.kind,
                    [component.position.x, component.position.y],
                )
                .unwrap();
                assert_eq!(layout.anchors().count(), 3);
                assert_eq!(
                    layout
                        .pin_position(PinId {
                            direction: PinDirection::Input,
                            index: 0
                        })
                        .unwrap()[0],
                    layout
                        .pin_position(PinId {
                            direction: PinDirection::Input,
                            index: 1
                        })
                        .unwrap()[0]
                );
                let expected = if component.kind == ComponentKind::XOr {
                    60.
                } else {
                    80.
                };
                assert_eq!(
                    layout
                        .pin_position(PinId {
                            direction: PinDirection::Output,
                            index: 0
                        })
                        .unwrap()[0]
                        - layout
                            .pin_position(PinId {
                                direction: PinDirection::Input,
                                index: 0
                            })
                            .unwrap()[0],
                    expected
                );
            }
            let painted = frame(&ctx, &mut app, vec![], true);
            for name in ["NAND", "NOR", "XOR", "XNOR"] {
                assert!(has_text(&painted, name));
            }
        }
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(app.session.document().components.len(), 3);
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert_eq!(app.session.document().components.len(), 4);
    }

    #[test]
    fn redo_y_and_error_display() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::Or, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p),
                button(p, true),
                button(p, false),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Z, egui::Modifiers::COMMAND)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Y, egui::Modifiers::COMMAND)],
            true,
        );
        assert_eq!(app.session.document().components[0].id, id);
        app.selected = Some(ComponentId(999));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.error.as_deref(), Some("Unknown component"));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("Unknown component"))));
    }
    #[test]
    fn edge_drag_and_noop_preserve_redo() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Not, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        // Just inside the right edge of the NOT symbol's bounds (the canvas origin is at x = 208).
        let edge = SymbolLayout::centered_on(DEFAULT_STYLE, ComponentKind::Not, [100., 100.])
            .unwrap()
            .bounds()
            .max[0];
        let p = Pos2::new(208. + edge - 1., 108.);
        let end = p + Vec2::new(120., 0.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(app.session.document().components[0].position.x, 220.);
        app.session.undo().unwrap();
        let before = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p + Vec2::new(20., 0.))],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(p)], true);
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert_eq!(*app.session.document(), before);
        app.session.redo().unwrap();
        assert_eq!(app.session.document().components[0].position.x, 220.);
    }
    #[test]
    fn headless_frame() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let output = ctx.run(Default::default(), |ctx| app.show(ctx));
        assert!(!output.shapes.is_empty());
    }

    fn mesh_vertices(output: &egui::FullOutput) -> Vec<(Pos2, egui::Color32)> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Mesh(mesh) => Some(mesh),
                _ => None,
            })
            .flat_map(|mesh| mesh.vertices.iter().map(|v| (v.pos, v.color)))
            .collect()
    }
    fn run_frame(ctx: &egui::Context, app: &mut DigitalApp) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            ..Default::default()
        };
        ctx.run(input, |ctx| app.show(ctx))
    }
    fn app_with(kind: ComponentKind) -> DigitalApp {
        let mut app = DigitalApp::default();
        app.session
            .place(kind, Position { x: 100., y: 100. })
            .unwrap();
        app
    }
    #[test]
    fn shape_style_defaults_to_rectangular_and_is_offered_in_the_palette() {
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::And);
        assert_eq!(app.style, SymbolStyle::Rect);
        let output = run_frame(&ctx, &mut app);
        for label in ["Rectangular", "IEEE"] {
            assert!(
                output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text.contains(label))),
                "palette lacks {label}"
            );
        }
    }
    #[test]
    fn style_toggle_changes_the_painted_shapes_but_not_the_document() {
        let ctx = egui::Context::default();
        for kind in [ComponentKind::And, ComponentKind::Or, ComponentKind::Not] {
            let mut app = app_with(kind);
            let rect = mesh_vertices(&run_frame(&ctx, &mut app));
            let document = app.session.document().clone();
            let dirty = app.session.dirty();
            app.style = SymbolStyle::Ieee;
            let ieee = mesh_vertices(&run_frame(&ctx, &mut app));
            assert!(!rect.is_empty() && !ieee.is_empty(), "{kind:?}");
            assert_ne!(rect, ieee, "{kind:?}");
            assert_eq!(*app.session.document(), document, "{kind:?}");
            assert_eq!(app.session.dirty(), dirty, "{kind:?}");
        }
    }
    #[test]
    fn symbols_are_painted_in_ink_and_selected_ones_in_the_selection_color() {
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::And);
        let visuals = egui::Visuals::dark();
        let ink = visuals.text_color();
        let selection = visuals.selection.stroke.color;
        let colors = |app: &mut DigitalApp| -> Vec<egui::Color32> {
            mesh_vertices(&run_frame(&ctx, app))
                .into_iter()
                .map(|(_, color)| color)
                .collect()
        };
        let plain = colors(&mut app);
        assert!(plain.contains(&ink) && !plain.contains(&selection));
        app.selected = Some(app.session.document().components[0].id);
        let selected = colors(&mut app);
        assert!(selected.contains(&selection) && !selected.contains(&ink));
    }
    #[test]
    fn clicks_pick_symbol_bounds_not_the_legacy_box() {
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::Input);
        frame(&ctx, &mut app, vec![], true);
        let bounds = SymbolLayout::centered_on(DEFAULT_STYLE, ComponentKind::Input, [100., 100.])
            .unwrap()
            .bounds();
        // The canvas origin is at (208, 8); the former 80 x 40 box reached 40 units right of
        // the position, beyond the much smaller input symbol.
        assert!(bounds.max[0] < 100. + 35.);
        let click = |ctx: &egui::Context, app: &mut DigitalApp, world: [f32; 2]| {
            let p = Pos2::new(208. + world[0], 8. + world[1]);
            frame(
                ctx,
                app,
                vec![egui::Event::PointerMoved(p), button(p, true)],
                true,
            );
            frame(ctx, app, vec![button(p, false)], true);
        };
        click(&ctx, &mut app, [bounds.max[0] + 10., 100.]);
        assert_eq!(app.selected, None);
        click(&ctx, &mut app, [bounds.max[0] - 1., 100.]);
        assert!(app.selected.is_some());
        click(&ctx, &mut app, [100., bounds.max[1] + 10.]);
        assert_eq!(app.selected, None);
    }
    #[test]
    fn picking_follows_the_chosen_style() {
        // An IEEE gate is wider or narrower than its rectangular twin by definition of the
        // bounds; picking must use the bounds of the style currently shown.
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::Or);
        app.style = SymbolStyle::Ieee;
        frame(&ctx, &mut app, vec![], true);
        let bounds = SymbolLayout::centered_on(SymbolStyle::Ieee, ComponentKind::Or, [100., 100.])
            .unwrap()
            .bounds();
        let p = Pos2::new(208. + bounds.min[0] + 1., 8. + 100.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert!(app.selected.is_some());
    }
    fn pin_ref(component: ComponentId, direction: PinDirection, index: u8) -> PinRef {
        PinRef {
            component,
            pin: PinId { direction, index },
        }
    }
    /// Input (100,100) -> `nots` NOT gates -> Output, wired through the engine.
    fn inverter_chain(app: &mut DigitalApp, nots: usize) -> (ComponentId, ComponentId) {
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let mut source = input;
        for n in 0..nots {
            let not = app
                .session
                .place(
                    ComponentKind::Not,
                    Position {
                        x: 200. + 20. * n as f32,
                        y: 100.,
                    },
                )
                .unwrap();
            app.session
                .connect([
                    pin_ref(source, PinDirection::Output, 0),
                    pin_ref(not, PinDirection::Input, 0),
                ])
                .unwrap();
            source = not;
        }
        let output = app
            .session
            .place(ComponentKind::Output, Position { x: 300., y: 100. })
            .unwrap();
        app.session
            .connect([
                pin_ref(source, PinDirection::Output, 0),
                pin_ref(output, PinDirection::Input, 0),
            ])
            .unwrap();
        (input, output)
    }
    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }
    fn has_text(output: &egui::FullOutput, needle: &str) -> bool {
        texts(output).iter().any(|text| text.contains(needle))
    }
    fn click_at(ctx: &egui::Context, app: &mut DigitalApp, p: Pos2) {
        frame(
            ctx,
            app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(ctx, app, vec![button(p, false)], true);
    }
    /// Clicks the widget whose text is exactly `label`, as found in a freshly painted frame.
    fn click_text(ctx: &egui::Context, app: &mut DigitalApp, label: &str) {
        let output = frame(ctx, app, vec![], true);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => Some(text.pos),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {label:?} in {:?}", texts(&output)));
        click_at(ctx, app, pos + Vec2::new(4., 4.));
    }
    fn press(
        ctx: &egui::Context,
        app: &mut DigitalApp,
        key_: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        frame(ctx, app, vec![key(key_, modifiers)], true);
    }
    fn output_level(app: &DigitalApp, output: ComponentId) -> Option<Level> {
        app.session.inspect()?.output_value(output)
    }
    fn wants_immediate_repaint(output: &egui::FullOutput) -> bool {
        output
            .viewport_output
            .values()
            .any(|viewport| viewport.repaint_delay.is_zero())
    }

    #[test]
    fn start_toggle_step_settle_inspect_stop_then_edit_and_undo() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (input, output) = inverter_chain(&mut app, 1);
        let wired = app.session.document().clone();
        click_text(&ctx, &mut app, "Start");
        assert!(app.session.simulation_active());
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: settled"), "{:?}", texts(&shown));
        assert_eq!(output_level(&app, output), Some(Level::High));
        assert!(has_text(&shown, "= 1"));
        assert!(has_text(&shown, "1 (high)"));
        // Toggle the input from the keyboard-reachable list; the inverter follows.
        click_text(&ctx, &mut app, "Input #0: 0 (low)");
        let shown = frame(&ctx, &mut app, vec![], true);
        assert_eq!(output_level(&app, output), Some(Level::Low));
        assert!(has_text(&shown, "Input #0: 1 (high)"));
        assert!(has_text(&shown, "= 0"));
        // Toggling creates no history and does not touch the document.
        assert_eq!(*app.session.document(), wired);
        assert!(app.session.dirty());
        click_text(&ctx, &mut app, "Stop");
        assert!(!app.session.simulation_active());
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: idle"));
        assert!(!has_text(&shown, "= 0"));
        // Editing works again, and undo removes exactly the edit.
        app.placing = Some(ComponentKind::And);
        click_at(&ctx, &mut app, Pos2::new(408., 300.));
        assert_eq!(app.session.document().components.len(), 4);
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(*app.session.document(), wired);
        assert_eq!(input, app.session.document().components[0].id);
    }

    #[test]
    fn gate_values_are_exposed_as_labelled_widgets() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, 2);
        click_text(&ctx, &mut app, "Start");
        let shown = frame(&ctx, &mut app, vec![], true);
        let gates: Vec<_> = app
            .session
            .document()
            .components
            .iter()
            .filter(|c| c.kind == ComponentKind::Not)
            .map(|c| c.id.0)
            .collect();
        assert_eq!(gates.len(), 2);
        // Input is low, so the first NOT is high and the second low.
        assert!(has_text(
            &shown,
            &format!("NOT gate id {}: 1 (high)", gates[0])
        ));
        assert!(has_text(
            &shown,
            &format!("NOT gate id {}: 0 (low)", gates[1])
        ));
        assert_eq!(output_level(&app, output), Some(Level::Low));
        // Standard widgets, unlike painted badges, reach the AccessKit tree.
        let accessible: Vec<String> = shown
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| update.nodes.iter())
            .filter_map(|(_, node)| node.value().or_else(|| node.label()).map(str::to_owned))
            .collect();
        for (gate, expected) in gates.iter().zip(["1 (high)", "0 (low)"]) {
            let label = format!("NOT gate id {gate}: {expected}");
            assert!(
                accessible.contains(&label),
                "{label:?} not in {accessible:?}"
            );
        }
    }

    /// Position of the first text shape equal to `label` and whether it lies inside its clip.
    fn text_position(output: &egui::FullOutput, label: &str) -> Option<(Pos2, bool)> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some((text.pos, shape.clip_rect.contains(text.pos)))
            }
            _ => None,
        })
    }

    #[test]
    fn overflowing_simulation_lists_scroll_and_keep_controls_fixed() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let inputs: Vec<_> = (0..30)
            .map(|n| {
                app.session
                    .place(
                        ComponentKind::Input,
                        Position {
                            x: 100.,
                            y: 20. * n as f32,
                        },
                    )
                    .unwrap()
            })
            .collect();
        let last = inputs[29];
        let last_label = format!("Input #{}: 0 (low)", last.0);
        click_text(&ctx, &mut app, "Start");
        let shown = frame(&ctx, &mut app, vec![], true);
        // egui culls shapes outside the clip rectangle, so "absent" also means not visible.
        assert!(
            !text_position(&shown, &last_label).is_some_and(|(_, visible)| visible),
            "30 inputs must overflow the 600-point viewport"
        );
        let (start, start_visible) = text_position(&shown, "Start").unwrap();
        assert!(start_visible);
        // Wheel over the panel scrolls the list; the controls stay where they were.
        let over_panel = Pos2::new(700., 300.);
        let mut events = vec![
            egui::Event::PointerMoved(over_panel),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0., -2_000.),
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let mut shown = frame(&ctx, &mut app, std::mem::take(&mut events), true);
        for _ in 0..30 {
            shown = frame(&ctx, &mut app, vec![], true);
        }
        let (_, visible) = text_position(&shown, &last_label).unwrap();
        assert!(visible, "last input scrolled into view");
        assert_eq!(text_position(&shown, "Start"), Some((start, true)));
        assert!(!app.session.inspect().unwrap().is_pending());
        // Hover first: a pointer that moves while pressing would drag-scroll the list.
        let shown = frame(&ctx, &mut app, vec![], true);
        let (label_pos, visible) = text_position(&shown, &last_label).unwrap();
        assert!(visible);
        let target = label_pos + Vec2::new(4., 4.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(target)],
            true,
        );
        click_at(&ctx, &mut app, target);
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(
            has_text(&shown, &format!("Input #{}: 1 (high)", last.0)),
            "{:?}",
            texts(&shown)
        );
        let runtime = app.session.inspect().unwrap();
        assert_eq!(
            runtime.pin_value(pin_ref(last, PinDirection::Output, 0)),
            Some(Level::High)
        );
        assert_eq!(
            runtime.pin_value(pin_ref(inputs[0], PinDirection::Output, 0)),
            Some(Level::Low)
        );
    }

    #[test]
    fn step_runs_one_microstep_and_settle_resumes_with_the_same_result() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, 2);
        app.settle_budget = 1;
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(app.session.inspect().unwrap().is_pending());
        assert_eq!(output_level(&app, output), Some(Level::Unavailable));
        press(&ctx, &mut app, egui::Key::F10, egui::Modifiers::NONE);
        assert!(!app.settling);
        let paused = app.session.inspect().unwrap().pending_gate_count();
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "paused"));
        assert_eq!(
            app.session.inspect().unwrap().pending_gate_count(),
            paused,
            "a paused simulation must not advance"
        );
        // One microstep cannot carry the value through both gates.
        assert_eq!(output_level(&app, output), Some(Level::Unavailable));
        press(&ctx, &mut app, egui::Key::F6, egui::Modifiers::NONE);
        for _ in 0..50 {
            frame(&ctx, &mut app, vec![], true);
        }
        assert_eq!(output_level(&app, output), Some(Level::Low));
        assert!(!app.session.inspect().unwrap().is_pending());
    }

    #[test]
    fn invalid_circuit_shows_a_located_diagnostic_and_stays_editable() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let and = app
            .session
            .place(ComponentKind::And, Position { x: 200., y: 100. })
            .unwrap();
        app.session
            .connect([
                pin_ref(input, PinDirection::Output, 0),
                pin_ref(and, PinDirection::Input, 0),
            ])
            .unwrap();
        let before = app.session.document().clone();
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(!app.session.simulation_active());
        assert_eq!(*app.session.document(), before);
        let message = app.error.clone().unwrap();
        assert!(message.contains("input pin 2 of AND #1"), "{message}");
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "input pin 2 of AND #1"));
        assert!(has_text(&shown, "State: error"));
        // Still editable; the error state ends once the document changes.
        app.session
            .place(ComponentKind::Input, Position { x: 50., y: 50. })
            .unwrap();
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: idle"));
    }

    #[test]
    fn pointer_and_keyboard_edits_are_blocked_while_active() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (input, output) = inverter_chain(&mut app, 1);
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        frame(&ctx, &mut app, vec![], true);
        assert!(app.session.simulation_active());
        let before = app.session.document().clone();
        let history_probe = before.wires.len();
        // Drag the NOT gate (canvas origin is at 208, 8).
        let start = Pos2::new(308. + 100., 108.);
        let end = start + Vec2::new(50., 50.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        assert!(app.drag.is_none());
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(*app.session.document(), before);
        // Keyboard edit paths: delete, undo, redo and wiring mode.
        for (key_, modifiers) in [
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Z, egui::Modifiers::COMMAND),
            (
                egui::Key::Z,
                egui::Modifiers {
                    shift: true,
                    ..egui::Modifiers::COMMAND
                },
            ),
            (egui::Key::Y, egui::Modifiers::COMMAND),
            (egui::Key::W, egui::Modifiers::NONE),
        ] {
            app.error = None;
            press(&ctx, &mut app, key_, modifiers);
            assert_eq!(*app.session.document(), before, "{key_:?}");
            assert!(!app.wiring);
            assert_eq!(
                app.error.as_deref(),
                Some("Stop the simulation before editing"),
                "{key_:?}"
            );
        }
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "Editing is locked"));
        // A click on an Input component toggles it instead of selecting for a drag.
        click_at(&ctx, &mut app, Pos2::new(308., 108.));
        frame(&ctx, &mut app, vec![], true);
        assert_eq!(output_level(&app, output), Some(Level::Low));
        assert_eq!(*app.session.document(), before);
        // Space on the selected Input toggles it back from the keyboard.
        app.selected = Some(input);
        press(&ctx, &mut app, egui::Key::Space, egui::Modifiers::NONE);
        frame(&ctx, &mut app, vec![], true);
        assert_eq!(output_level(&app, output), Some(Level::High));
        // The history survived untouched: after Stop, undo removes the last wire only.
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::SHIFT);
        assert!(!app.session.simulation_active());
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(app.session.document().wires.len(), history_probe - 1);
        assert_eq!(app.session.document().components.len(), 3);
    }

    #[test]
    fn starting_cancels_placement_wiring_and_drag_gestures() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        inverter_chain(&mut app, 1);
        let before = app.session.document().clone();
        // A pointer drag in progress when the simulation starts must not move anything later.
        let start = Pos2::new(408., 108.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        assert!(app.drag.is_some());
        app.placing = Some(ComponentKind::And);
        app.wiring = true;
        app.wire_start = Some(pin_ref(ComponentId(1), PinDirection::Output, 0));
        app.focused_pin = app.wire_start;
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(app.session.simulation_active());
        assert!(app.drag.is_none());
        assert!(app.placing.is_none());
        assert!(!app.wiring);
        assert!(app.wire_start.is_none());
        assert!(app.focused_pin.is_none());
        let end = start + Vec2::new(60., 0.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(*app.session.document(), before);
    }

    fn ui_file_app(dir: &std::path::Path) -> DigitalApp {
        DigitalApp::with_file_io(Some(Box::new(LocalFileIo::new(dir))), None)
    }

    struct PostPublishFailureIo(LocalFileIo);
    struct DelegatingStage(Box<dyn crate::file_session::StagedFile>);
    impl crate::file_session::StagedFile for DelegatingStage {
        fn path(&self) -> &std::path::Path {
            self.0.path()
        }
        fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.write_all(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
        fn sync_close(self: Box<Self>) -> std::io::Result<()> {
            self.0.sync_close()
        }
    }
    impl FileIo for PostPublishFailureIo {
        fn base_dir(&self) -> &std::path::Path {
            self.0.base_dir()
        }
        fn read_bounded_regular(
            &mut self,
            path: &std::path::Path,
            limit: usize,
        ) -> std::io::Result<Vec<u8>> {
            self.0.read_bounded_regular(path, limit)
        }
        fn validate_target(
            &mut self,
            target: &std::path::Path,
            intent: SaveAsIntent,
        ) -> std::io::Result<()> {
            self.0.validate_target(target, intent)
        }
        fn create_temp(
            &mut self,
            path: &std::path::Path,
        ) -> std::io::Result<Box<dyn crate::file_session::StagedFile>> {
            self.0.create_temp(path).map(|stage| {
                Box::new(DelegatingStage(stage)) as Box<dyn crate::file_session::StagedFile>
            })
        }
        fn publish(
            &mut self,
            temp: &std::path::Path,
            target: &std::path::Path,
            intent: SaveAsIntent,
        ) -> std::io::Result<()> {
            self.0.publish(temp, target, intent)
        }
        fn sync_parent(&mut self, parent: &std::path::Path) -> std::io::Result<()> {
            self.0.sync_parent(parent)
        }
        fn remove_temp(&mut self, _temp: &std::path::Path) -> std::io::Result<()> {
            Err(std::io::Error::other(
                "injected post-publish cleanup failure",
            ))
        }
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("digital-ui-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_menu_and_shortcut_share_pending_document_actions() {
        let ctx = egui::Context::default();
        let dir = test_dir("menu");
        let mut app = ui_file_app(&dir);
        let original = app
            .session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        press(&ctx, &mut app, egui::Key::N, egui::Modifiers::COMMAND);
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        assert_eq!(app.session.document().components[0].id, original);
        click_text(&ctx, &mut app, "Cancel");
        assert!(app.file_modal.is_none());
        assert!(app.session.dirty());
        // The visible File menu's New item goes through the same pending path.
        click_text(&ctx, &mut app, "New");
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        click_text(&ctx, &mut app, "Discard");
        assert!(app.session.document().components.is_empty());
        assert!(!app.session.dirty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dirty_open_save_discard_and_cancel_all_preserve_or_replace_the_document_as_chosen() {
        let ctx = egui::Context::default();
        let dir = test_dir("dirty-open-choices");
        let mut app = ui_file_app(&dir);
        frame(&ctx, &mut app, vec![], true);
        let source = app
            .session
            .place(ComponentKind::Input, Position { x: 10., y: 10. })
            .unwrap();
        app.save_as_path("source.dig".into(), false, false);

        let mut destination = Session::default();
        destination
            .place(ComponentKind::Output, Position { x: 90., y: 90. })
            .unwrap();
        destination
            .save_as(
                app.file_io.as_mut().unwrap().as_mut(),
                std::path::Path::new("destination.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();
        let old = app.session.document().clone();
        app.session
            .move_component(source, Position { x: 30., y: 30. })
            .unwrap();
        let changed = app.session.document().clone();

        app.begin_pending(PendingAction::Open(dir.join("destination.dig")));
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        click_text(&ctx, &mut app, "Cancel");
        assert_eq!(*app.session.document(), changed);

        app.begin_pending(PendingAction::Open(dir.join("destination.dig")));
        click_text(&ctx, &mut app, "Discard");
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Output
        );
        assert!(!app.session.dirty());

        let destination_id = app.session.document().components[0].id;
        app.session
            .move_component(destination_id, Position { x: 120., y: 120. })
            .unwrap();
        app.begin_pending(PendingAction::Open(dir.join("source.dig")));
        click_text(&ctx, &mut app, "Save");
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Input
        );
        assert_eq!(app.session.path(), Some(dir.join("source.dig").as_path()));
        assert!(!app.session.dirty());
        assert_eq!(*app.session.document(), old);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn path_cancel_failed_open_and_create_only_collision_never_discard_the_document() {
        let ctx = egui::Context::default();
        let dir = test_dir("file-failures");
        let mut app = ui_file_app(&dir);
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 4., y: 5. })
            .unwrap();
        app.file_path_entry = "unused.dig".into();
        app.file_modal = Some(FileModal::SavePath { continuation: true });
        click_text(&ctx, &mut app, "Cancel path");
        assert!(app.session.dirty() && app.session.path().is_none());
        assert!(!dir.join("unused.dig").exists());

        app.save_as_path("old.dig".into(), false, false);
        let before = app.session.document().clone();
        app.begin_pending(PendingAction::Open(dir.join("missing.dig")));
        assert_eq!(*app.session.document(), before);
        assert!(
            app.error
                .as_deref()
                .is_some_and(|e| e.contains("missing.dig"))
        );

        let collision = dir.join("exists.dig");
        std::fs::write(&collision, b"foreign bytes").unwrap();
        app.file_modal = Some(FileModal::SavePath {
            continuation: false,
        });
        app.file_path_entry = "exists.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert!(matches!(app.file_modal, Some(FileModal::Overwrite { .. })));
        click_text(&ctx, &mut app, "Cancel");
        assert_eq!(std::fs::read(&collision).unwrap(), b"foreign bytes");
        assert_eq!(*app.session.document(), before);
        assert_eq!(app.session.document().components[0].id, id);

        app.file_modal = Some(FileModal::SavePath {
            continuation: false,
        });
        app.file_path_entry = "exists.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert!(matches!(app.file_modal, Some(FileModal::Overwrite { .. })));
        click_text(&ctx, &mut app, "Replace");
        assert!(app.session.path().is_some());
        assert!(!app.session.dirty());
        assert_ne!(std::fs::read(&collision).unwrap(), b"foreign bytes");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_continuation_clears_pending_action_only_after_publish_and_failed_save_blocks_it() {
        let ctx = egui::Context::default();
        let dir = test_dir("save-continuation");
        let mut app = ui_file_app(&dir);
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let original = app.session.document().clone();
        app.begin_pending(PendingAction::New);
        app.begin_save(true);
        assert!(matches!(
            app.file_modal,
            Some(FileModal::SavePath { continuation: true })
        ));
        app.file_path_entry = "kept.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert!(dir.join("kept.dig").exists());
        assert!(app.session.document().components.is_empty());
        assert!(!app.session.dirty());

        app.session
            .place(ComponentKind::Output, Position { x: 1., y: 2. })
            .unwrap();
        let before_failure = app.session.document().clone();
        app.begin_pending(PendingAction::New);
        app.begin_save(true);
        app.save_as_path("missing-dir/fail.dig".into(), true, false);
        assert!(app.error.is_some());
        assert_eq!(*app.session.document(), before_failure);
        assert_eq!(app.pending_action, Some(PendingAction::New));
        assert!(app.session.dirty());
        assert_ne!(before_failure, original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dirty_close_save_continuation_approves_exit_only_after_publication() {
        let dir = test_dir("close-save");
        let mut app = ui_file_app(&dir);
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        app.save_as_path("close.dig".into(), false, false);
        app.session
            .move_component(id, Position { x: 20., y: 30. })
            .unwrap();
        app.begin_pending(PendingAction::Close);
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        app.begin_save(true);
        assert!(app.approved_close);
        assert!(!app.session.dirty());
        assert!(dir.join("close.dig").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_modal_blocks_edit_simulation_and_close_cancel_preserves_runtime() {
        let ctx = egui::Context::default();
        let dir = test_dir("modal-lock");
        let mut app = ui_file_app(&dir);
        let (input, _) = inverter_chain(&mut app, 1);
        let before = app.session.document().clone();
        app.selected = Some(input);
        app.selected_wire = Some(app.session.document().wires[0].id);
        app.placing = Some(ComponentKind::And);
        app.wiring = true;
        app.wire_start = Some(pin_ref(input, PinDirection::Output, 0));
        app.focused_pin = app.wire_start;
        app.drag = Some((input, Position { x: 0., y: 0. }, Pos2::ZERO));
        app.begin_pending(PendingAction::New);
        assert!(app.drag.is_none() && app.placing.is_none() && !app.wiring);
        assert!(app.wire_start.is_none() && app.focused_pin.is_none());
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        for (key, modifiers) in [
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Z, egui::Modifiers::COMMAND),
            (egui::Key::W, egui::Modifiers::NONE),
            (egui::Key::F5, egui::Modifiers::NONE),
        ] {
            press(&ctx, &mut app, key, modifiers);
        }
        assert_eq!(*app.session.document(), before);
        assert!(!app.session.simulation_active());
        assert!(app.wire_start.is_none());

        click_text(&ctx, &mut app, "Cancel");
        app.session.start_simulation().unwrap();
        let runtime_input =
            app.session
                .inspect()
                .unwrap()
                .pin_value(pin_ref(input, PinDirection::Output, 0));
        app.begin_pending(PendingAction::Close);
        assert!(matches!(app.file_modal, Some(FileModal::RunningClose)));
        click_text(&ctx, &mut app, "Cancel");
        assert!(app.session.simulation_active());
        assert_eq!(
            app.session
                .inspect()
                .unwrap()
                .pin_value(pin_ref(input, PinDirection::Output, 0)),
            runtime_input
        );
        assert!(!app.approved_close);
        app.begin_pending(PendingAction::Close);
        click_text(&ctx, &mut app, "Stop and continue closing");
        assert!(!app.session.simulation_active());
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        click_text(&ctx, &mut app, "Discard");
        assert!(app.approved_close);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn synthetic_close_is_cancelled_until_dirty_workflow_approves() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused: true,
            viewports: [(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        assert!(!app.approved_close);
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        click_text(&ctx, &mut app, "Cancel");
        assert!(!app.approved_close);
        assert!(app.session.dirty());

        app.begin_pending(PendingAction::Close);
        click_text(&ctx, &mut app, "Discard");
        assert!(app.approved_close);
        assert_eq!(app.session.document().components.len(), 1);
    }

    #[test]
    fn published_warning_close_keeps_original_open_continuation_until_acknowledged() {
        let ctx = egui::Context::default();
        let dir = test_dir("published-warning-close");
        let destination = dir.join("destination.dig");
        let mut destination_session = Session::default();
        destination_session
            .place(ComponentKind::Output, Position { x: 40., y: 50. })
            .unwrap();
        destination_session
            .save_as(
                &mut LocalFileIo::new(&dir),
                &destination,
                SaveAsIntent::CreateOnly,
            )
            .unwrap();

        let mut app = DigitalApp::with_file_io(
            Some(Box::new(PostPublishFailureIo(LocalFileIo::new(&dir)))),
            None,
        );
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        app.begin_pending(PendingAction::Open(destination.clone()));
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        app.begin_save(true);
        app.file_path_entry = "current.dig".into();
        click_text(&ctx, &mut app, "Save path");

        assert!(destination.exists());
        assert!(dir.join("current.dig").exists());
        assert!(
            !app.session.dirty(),
            "the published save commits the baseline"
        );
        assert_eq!(
            app.pending_action,
            Some(PendingAction::Open(destination.clone()))
        );
        assert!(matches!(
            app.file_modal,
            Some(FileModal::PublishedWarning {
                continuation: true,
                ..
            })
        ));
        let warning_frame = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(
            &warning_frame,
            "Published; durability unconfirmed"
        ));
        let close_input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused: true,
            viewports: [(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let output = ctx.run(close_input, |ctx| app.show(ctx));
        assert!(!app.approved_close);
        assert_eq!(
            app.pending_action,
            Some(PendingAction::Open(destination.clone()))
        );
        assert!(matches!(
            app.file_modal,
            Some(FileModal::PublishedWarning {
                continuation: true,
                ..
            })
        ));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Input
        );

        click_text(&ctx, &mut app, "Acknowledge and continue");
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Output
        );
        assert_eq!(app.session.path(), Some(destination.as_path()));
        assert!(app.pending_action.is_none());
        assert!(app.file_modal.is_none());
        assert!(
            !app.approved_close,
            "the original Open, not Close, continues"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn close_without_pending_warning_continuation_only_dismisses_after_ack() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp {
            file_modal: Some(FileModal::PublishedWarning {
                diagnostics: vec!["post-publish warning".into()],
                continuation: false,
            }),
            pending_action: None,
            ..Default::default()
        };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused: true,
            viewports: [(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        assert!(!app.approved_close);
        assert!(app.pending_action.is_none());
        assert!(matches!(
            app.file_modal,
            Some(FileModal::PublishedWarning {
                continuation: false,
                ..
            })
        ));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        click_text(&ctx, &mut app, "Acknowledge and continue");
        assert!(app.file_modal.is_none());
        assert!(!app.approved_close);
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn published_warning_displays_every_diagnostic_and_blocks_continuation_until_ack() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        app.pending_action = Some(PendingAction::New);
        app.file_modal = Some(FileModal::PublishedWarning {
            diagnostics: vec![
                "first post-publish warning".into(),
                "second post-publish warning".into(),
            ],
            continuation: true,
        });
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(
            matches!(app.file_modal, Some(FileModal::PublishedWarning { .. })),
            "{:?}",
            app.file_modal
        );
        assert!(
            has_text(&shown, "first post-publish warning"),
            "{:?}",
            texts(&shown)
        );
        assert!(has_text(&shown, "second post-publish warning"));
        assert_eq!(app.session.document().components.len(), 1);
        click_text(&ctx, &mut app, "Acknowledge and continue");
        assert!(app.file_modal.is_none(), "{:?}", app.file_modal);
        assert!(app.session.document().components.is_empty());
        assert!(!app.session.dirty());
    }

    #[test]
    fn label_editor_modal_applies_cancels_validates_and_blocks_every_background_route() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 100. })
            .unwrap();
        app.selected = Some(id);
        let original = app.session.document().clone();
        click_text(&ctx, &mut app, "Edit label… (F2)");
        assert!(app.label_modal.is_some());
        assert_eq!(app.label_modal.as_ref().unwrap().text, "Output");
        app.label_modal.as_mut().unwrap().text = "invalid\u{1}".into();
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "U+0001"), "{:?}", texts(&shown));
        assert!(has_text(&shown, "Apply") && has_text(&shown, "Cancel"));
        ctx.set_visuals(egui::Visuals::light());
        assert!(has_text(&frame(&ctx, &mut app, vec![], true), "U+0001"));
        ctx.set_visuals(egui::Visuals::dark());
        assert!(has_text(&frame(&ctx, &mut app, vec![], true), "U+0001"));
        let before = app.session.document().clone();
        for (pressed, modifiers) in [
            (egui::Key::F5, egui::Modifiers::NONE),
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Z, egui::Modifiers::COMMAND),
            (egui::Key::N, egui::Modifiers::COMMAND),
            (egui::Key::O, egui::Modifiers::COMMAND),
            (egui::Key::S, egui::Modifiers::COMMAND),
        ] {
            press(&ctx, &mut app, pressed, modifiers);
        }
        assert_eq!(*app.session.document(), before);
        assert!(!app.session.simulation_active());
        assert!(app.label_modal.is_some());
        assert!(app.file_modal.is_none());
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.label_modal.is_none());
        assert_eq!(*app.session.document(), original);
        press(&ctx, &mut app, egui::Key::F2, egui::Modifiers::NONE);
        assert!(
            app.label_modal.is_some(),
            "F2 should open the selected component label editor"
        );
        press(&ctx, &mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(app.label_modal.is_none());

        app.selected = Some(id);
        click_text(&ctx, &mut app, "Edit label… (F2)");
        app.label_modal.as_mut().unwrap().text = "Résultat & <Q>".into();
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::COMMAND)],
            true,
        );
        assert!(app.label_modal.is_none());
        assert_eq!(app.session.document().components[0].label, "Résultat & <Q>");
        assert!(app.session.dirty());
        app.session.undo().unwrap();
        assert_eq!(*app.session.document(), original);
        assert!(app.session.dirty());
        app.session.redo().unwrap();
        assert!(app.session.dirty());
    }

    #[test]
    fn label_modal_blocks_canvas_pointer_gestures_everywhere() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let selected = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let underlying = app
            .session
            .place(ComponentKind::Output, Position { x: 192., y: 292. })
            .unwrap();
        app.selected = Some(selected);
        app.begin_label_edit();
        assert!(app.label_modal.is_some());
        let before = app.session.document().clone();

        // The first click is on empty canvas behind the dialog; the second is on a
        // component whose symbol is underneath the dialog itself.
        for (start, end) in [
            (Pos2::new(700., 500.), Pos2::new(740., 530.)),
            (Pos2::new(400., 300.), Pos2::new(470., 350.)),
        ] {
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(start), button(start, true)],
                true,
            );
            frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
            frame(&ctx, &mut app, vec![button(end, false)], true);
            assert_eq!(app.selected, Some(selected));
            assert!(app.drag.is_none());
            assert!(app.placing.is_none());
            assert!(!app.wiring);
            assert!(app.wire_start.is_none());
            assert_eq!(*app.session.document(), before);
        }
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == underlying)
                .unwrap()
                .position,
            Position { x: 192., y: 292. }
        );
    }

    #[test]
    fn label_modal_focus_loss_cancels_and_running_simulation_blocks_open() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let id = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 0. })
            .unwrap();
        app.session
            .connect([
                pin_ref(input, PinDirection::Output, 0),
                pin_ref(id, PinDirection::Input, 0),
            ])
            .unwrap();
        app.selected = Some(id);
        app.begin_label_edit();
        app.label_modal.as_mut().unwrap().text = "draft".into();
        frame(&ctx, &mut app, vec![], false);
        assert!(app.label_modal.is_none());
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap()
                .label,
            "Output"
        );
        app.session.start_simulation().unwrap();
        app.selected = Some(id);
        app.begin_label_edit();
        assert!(app.label_modal.is_none());
        assert!(app.session.simulation_active());
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap()
                .label,
            "Output"
        );
        app.session.stop_simulation().unwrap();
    }

    #[test]
    fn label_editing_full_editor_workflow_through_production_ui_events() {
        let ctx = egui::Context::default();
        let dir = test_dir("m4-label-ui");
        let mut app = ui_file_app(&dir);
        // Construct a small inverter circuit solely with the visible palette and canvas controls.
        for (palette, canvas) in [
            ("Input", Pos2::new(308., 108.)),
            ("NOT", Pos2::new(408., 108.)),
            ("Output", Pos2::new(508., 108.)),
        ] {
            click_text(&ctx, &mut app, palette);
            click_at(&ctx, &mut app, canvas);
        }
        assert_eq!(app.session.document().components.len(), 3);
        click_text(&ctx, &mut app, "Wire (W)");
        let screen_pin = |kind, center, pin| {
            let point = SymbolLayout::centered_on(app.style, kind, center)
                .unwrap()
                .pin_position(pin)
                .unwrap();
            Pos2::new(point[0] + 208., point[1] + 8.)
        };
        let routes = [
            (
                screen_pin(
                    ComponentKind::Input,
                    [100., 100.],
                    PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                ),
                screen_pin(
                    ComponentKind::Not,
                    [200., 100.],
                    PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                ),
            ),
            (
                screen_pin(
                    ComponentKind::Not,
                    [200., 100.],
                    PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                ),
                screen_pin(
                    ComponentKind::Output,
                    [300., 100.],
                    PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                ),
            ),
        ];
        for (start, end) in routes {
            click_at(&ctx, &mut app, start);
            click_at(&ctx, &mut app, end);
        }
        click_text(&ctx, &mut app, "Wire (W)");
        assert_eq!(app.session.document().wires.len(), 2);
        let output = app
            .session
            .document()
            .components
            .iter()
            .find(|component| component.kind == ComponentKind::Output)
            .unwrap()
            .id;
        click_at(&ctx, &mut app, Pos2::new(508., 108.));
        assert_eq!(app.selected, Some(output));
        click_text(&ctx, &mut app, "Edit label… (F2)");
        assert!(app.label_modal.is_some());
        let modal = frame(&ctx, &mut app, vec![], true);
        let text_pos = modal
            .shapes
            .iter()
            .rev()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Output" => Some(text.pos),
                _ => None,
            })
            .unwrap();
        click_at(&ctx, &mut app, text_pos + Vec2::new(8., 8.));
        let document_before_text_undo = app.session.document().clone();
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(
            *app.session.document(),
            document_before_text_undo,
            "text-editor undo must not reach document history"
        );
        frame(
            &ctx,
            &mut app,
            vec![
                key(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("Résultat & <Q>".into()),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::COMMAND)],
            true,
        );
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Résultat & <Q>"
        );
        assert!(has_text(
            &frame(&ctx, &mut app, vec![], true),
            "Résultat & <Q>"
        ));
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Output"
        );
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Résultat & <Q>"
        );
        click_text(&ctx, &mut app, "Save As");
        app.file_path_entry = "ui-demo.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert_eq!(app.session.path(), Some(dir.join("ui-demo.dig").as_path()));
        assert!(!app.session.dirty());
        let xml = std::fs::read_to_string(dir.join("ui-demo.dig")).unwrap();
        assert!(xml.contains("Résultat &amp; &lt;Q&gt;"));
        click_text(&ctx, &mut app, "New");
        assert!(app.session.document().components.is_empty());
        click_text(&ctx, &mut app, "Open");
        app.file_path_entry = "ui-demo.dig".into();
        click_text(&ctx, &mut app, "Open path");
        assert_eq!(app.session.document().components.len(), 3);
        assert_eq!(app.session.document().wires.len(), 2);
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Résultat & <Q>"
        );
        click_text(&ctx, &mut app, "Start");
        let mut frames = 0;
        while app
            .session
            .inspect()
            .is_some_and(|runtime| runtime.is_pending())
        {
            let output_frame = frame(&ctx, &mut app, vec![], true);
            frames += 1;
            assert!(frames < 100, "settling stalled: {:?}", app.error);
            if app.session.inspect().unwrap().is_pending() {
                assert!(wants_immediate_repaint(&output_frame));
            }
        }
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::High)
        );
        assert!(has_text(
            &frame(&ctx, &mut app, vec![], true),
            "Résultat & <Q>"
        ));
        click_text(&ctx, &mut app, "Input #0: 0 (low)");
        let settled = frame(&ctx, &mut app, vec![], true);
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::Low)
        );
        assert!(has_text(&settled, "Résultat & <Q>"));
        let document = app.session.document().clone();
        click_at(&ctx, &mut app, Pos2::new(508., 108.));
        assert_eq!(app.selected, Some(output));
        click_text(&ctx, &mut app, "Edit label… (F2)");
        assert!(
            app.label_modal.is_none(),
            "label editing must be disabled during simulation"
        );
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(app.session.simulation_active());
        assert_eq!(*app.session.document(), document);
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::SHIFT);
        click_text(&ctx, &mut app, "Edit label… (F2)");
        let modal = frame(&ctx, &mut app, vec![], true);
        let text_pos = modal
            .shapes
            .iter()
            .rev()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Résultat & <Q>" => {
                    Some(text.pos)
                }
                _ => None,
            })
            .unwrap();
        click_at(&ctx, &mut app, text_pos + Vec2::new(8., 8.));
        frame(
            &ctx,
            &mut app,
            vec![
                key(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("Final".into()),
            ],
            true,
        );
        press(&ctx, &mut app, egui::Key::Enter, egui::Modifiers::COMMAND);
        assert!(app.session.dirty());
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert!(!app.session.dirty());
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert!(app.session.dirty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn supported_document_file_lifecycle_save_new_reopen_run_toggle_inspect_stop() {
        let dir = test_dir("m4-lifecycle");
        let mut app = ui_file_app(&dir);
        let (input, output) = inverter_chain(&mut app, 1);
        app.save_as_path("fixture.dig".into(), false, false);
        assert!(dir.join("fixture.dig").exists());
        assert!(!app.session.dirty());
        app.begin_pending(PendingAction::New);
        assert!(app.session.document().components.is_empty());
        app.begin_pending(PendingAction::Open(dir.join("fixture.dig")));
        assert_eq!(app.session.document().components.len(), 3);
        app.session.start_simulation().unwrap();
        app.session.settle_simulation(100).unwrap();
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::High)
        );
        app.session.toggle_input(input).unwrap();
        app.session.settle_simulation(100).unwrap();
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::Low)
        );
        app.session.stop_simulation().unwrap();
        assert!(!app.session.simulation_active());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_chunks_span_frames_keep_stop_responsive_and_do_not_change_the_result() {
        let ctx = egui::Context::default();
        let chain = 6;
        let mut reference = DigitalApp::default();
        let (_, reference_output) = inverter_chain(&mut reference, chain);
        press(&ctx, &mut reference, egui::Key::F5, egui::Modifiers::NONE);
        frame(&ctx, &mut reference, vec![], true);
        let expected = output_level(&reference, reference_output);
        assert_eq!(expected, Some(Level::Low));

        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, chain);
        app.settle_budget = 2;
        let started = frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::F5, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.session.inspect().unwrap().is_pending());
        assert!(wants_immediate_repaint(&started));
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: pending"));
        assert!(has_text(&shown, "= ?"));
        assert!(has_text(&shown, "? (unavailable)"));
        let mut frames = 1;
        while app.session.inspect().unwrap().is_pending() {
            let shown = frame(&ctx, &mut app, vec![], true);
            frames += 1;
            assert!(frames < 200, "settling did not finish");
            if app.session.inspect().unwrap().is_pending() {
                assert!(wants_immediate_repaint(&shown));
            }
        }
        assert!(frames > 3, "the small budget must need several frames");
        assert_eq!(output_level(&app, output), expected);
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: settled"));
        // Stop while chunks are still pending: the click is handled between chunks.
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::SHIFT);
        let before = app.session.document().clone();
        app.settle_budget = 1;
        click_text(&ctx, &mut app, "Start");
        assert!(app.session.inspect().unwrap().is_pending());
        click_text(&ctx, &mut app, "Stop");
        assert!(!app.session.simulation_active());
        assert!(!app.settling);
        assert_eq!(*app.session.document(), before);
        frame(&ctx, &mut app, vec![], true);
        let idle = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&idle, "State: idle"));
        assert!(
            !wants_immediate_repaint(&idle),
            "an idle editor must not spin repaints"
        );
    }
}
