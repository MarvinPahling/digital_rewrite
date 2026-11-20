//! Native adapter for component editing.
//!
//! Symbol drawing and picking live behind `symbol_render`; the canvas maps world units to the
//! screen through [`CanvasTransform`]. The shape style is transient UI state and never reaches
//! the engine or the document.
use crate::{
    canvas::CanvasTransform,
    components::KINDS,
    domain::{ComponentId, ComponentKind, Position},
    engine::Session,
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

pub struct DigitalApp {
    session: Session,
    selected: Option<ComponentId>,
    placing: Option<ComponentKind>,
    drag: Option<(ComponentId, Position, Pos2)>,
    error: Option<String>,
    style: SymbolStyle,
    renderer: SymbolRenderer,
}
impl Default for DigitalApp {
    fn default() -> Self {
        Self {
            session: Session::default(),
            selected: None,
            placing: None,
            drag: None,
            error: None,
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
    fn palette_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("palette").show(ctx, |ui| {
            ui.heading("Components");
            for kind in KINDS {
                if ui
                    .selectable_label(self.placing == Some(kind), kind.name())
                    .clicked()
                {
                    self.placing = Some(kind);
                }
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
        self.palette_panel(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
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
            }
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
                if self.placing.is_none() {
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
                    self.selected = hit.map(|v| v.0);
                    self.drag = hit.map(|(id, pos)| (id, pos, p));
                }
            }
            if !cancelled
                && response.clicked()
                && let Some(kind) = self.placing.take()
                && let Some(p) = pointer
            {
                let [x, y] = transform.to_world(p);
                match self.session.place(kind, Position { x, y }) {
                    Ok(id) => self.selected = Some(id),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            if response.clicked() {
                self.drag = None;
            }
            if !cancelled
                && response.drag_stopped()
                && let (Some((id, start, origin)), Some(p)) = (self.drag.take(), pointer)
            {
                let [dx, dy] = transform.delta_to_world(p - origin);
                if let Err(e) = self.session.move_component(
                    id,
                    Position {
                        x: start.x + dx,
                        y: start.y + dy,
                    },
                ) {
                    self.error = Some(e.to_string());
                }
            }
            let palette = Palette::from_visuals(ui.visuals());
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
            }
            let owns_keyboard = response.has_focus();
            if owns_keyboard
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
            let (undo, redo) = if owns_keyboard {
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
                self.session.undo();
                self.selected = None;
                self.drag = None;
            }
            if redo {
                self.session.redo();
                self.selected = None;
                self.drag = None;
            }
        });
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
    fn frame(ctx: &egui::Context, app: &mut DigitalApp, events: Vec<egui::Event>, focused: bool) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused,
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.show(ctx));
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
        app.session.undo();
        assert_eq!(
            app.session.document().components[0].position,
            Position { x: 100., y: 100. }
        );
        app.session.redo();
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
        app.session.undo();
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
        app.session.redo();
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
}
