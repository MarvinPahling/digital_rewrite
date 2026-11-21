//! Native adapter for component editing.
//!
//! Symbol drawing and picking live behind `symbol_render`; the canvas maps world units to the
//! screen through [`CanvasTransform`]. The shape style is transient UI state and never reaches
//! the engine or the document.
use crate::{
    canvas::CanvasTransform,
    components::KINDS,
    domain::{ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position, WireId},
    engine::{EditError, Session, SimulationError},
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
}
impl Default for DigitalApp {
    fn default() -> Self {
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
            let mut action = None;
            ui.horizontal_wrapped(|ui| {
                for (label, enabled, hint, command) in [
                    ("Start", !active, "Start (F5)", SimAction::Start),
                    ("Stop", active, "Stop (Shift+F5)", SimAction::Stop),
                    ("Step", active, "One microstep (F10)", SimAction::Step),
                    ("Settle", active, "Run until stable (F6)", SimAction::Settle),
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
                        .add_enabled(!runtime.is_pending(), egui::Button::new(text))
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
            }
            if let Some(action) = action {
                self.apply_simulation_action(action);
            }
        });
    }
    fn palette_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("palette").show(ctx, |ui| {
            ui.heading("Components");
            let locked = self.session.simulation_active();
            ui.add_enabled_ui(!locked, |ui| {
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
            if locked {
                ui.label(EDIT_LOCK_NOTE);
            }
            ui.separator();
            ui.label("Shape style");
            ui.radio_value(&mut self.style, SymbolStyle::Rect, "Rectangular");
            ui.radio_value(&mut self.style, SymbolStyle::Ieee, "IEEE");
            ui.separator();
            ui.label("Click canvas to place. Drag to move. Escape cancels. Delete removes; Ctrl/Cmd-Z undoes; Shift-Ctrl/Cmd-Z redoes.");
            ui.label(if self.session.dirty() { "Modified" } else { "Unmodified" });
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }
    fn show(&mut self, ctx: &egui::Context) {
        self.simulation_shortcuts(ctx);
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
            let pointer = response.interact_pointer_pos();
            let cancelled = ctx.input(|i| !i.focused || i.key_pressed(egui::Key::Escape));
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
            if response.clicked() {
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
                if let Some(start) = self.wire_start
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
                if let Err(e) = self
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
            let owns_keyboard = response.has_focus();
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
        self.advance_simulation(ctx);
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
