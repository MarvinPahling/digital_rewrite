//! Native adapter for component editing.
use crate::{
    components::KINDS,
    domain::{ComponentId, ComponentKind, Position},
    engine::Session,
};
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, Vec2};
#[derive(Default)]
pub struct DigitalApp {
    session: Session,
    selected: Option<ComponentId>,
    placing: Option<ComponentKind>,
    drag: Option<(ComponentId, Position, Pos2)>,
    error: Option<String>,
}
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
}
impl DigitalApp {
    fn show(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("palette").show(ctx, |ui| { ui.heading("Components"); for kind in KINDS { if ui.selectable_label(self.placing == Some(kind),kind.name()).clicked() { self.placing = Some(kind); } } ui.separator(); ui.label("Click canvas to place. Drag to move. Escape cancels. Delete removes; Ctrl/Cmd-Z undoes; Shift-Ctrl/Cmd-Z redoes."); ui.label(if self.session.dirty() { "Modified" } else { "Unmodified" }); if let Some(error)= &self.error { ui.colored_label(ui.visuals().error_fg_color,error); } });
        egui::CentralPanel::default().show(ctx, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            let grid = 20.0;
            let mut x = rect.left();
            while x < rect.right() {
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    Stroke::new(0.5_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
                );
                x += grid;
            }
            let mut y = rect.top();
            while y < rect.bottom() {
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    Stroke::new(0.5_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
                );
                y += grid;
            }
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
                    let hit = self
                        .session
                        .document()
                        .components
                        .iter()
                        .rev()
                        .find(|c| {
                            Rect::from_center_size(
                                rect.min + Vec2::new(c.position.x, c.position.y),
                                Vec2::new(80., 40.),
                            )
                            .contains(p)
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
                match self.session.place(
                    kind,
                    Position {
                        x: p.x - rect.left(),
                        y: p.y - rect.top(),
                    },
                ) {
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
                let d = p - origin;
                if let Err(e) = self.session.move_component(
                    id,
                    Position {
                        x: start.x + d.x,
                        y: start.y + d.y,
                    },
                ) {
                    self.error = Some(e.to_string());
                }
            }
            for c in &self.session.document().components {
                let mut pos = c.position;
                if let (Some((id, start, origin)), Some(p)) = (self.drag, pointer)
                    && id == c.id
                {
                    let d = p - origin;
                    pos = Position {
                        x: start.x + d.x,
                        y: start.y + d.y,
                    };
                }
                let center = rect.min + Vec2::new(pos.x, pos.y);
                let bounds = Rect::from_center_size(center, Vec2::new(80., 40.));
                let color = if self.selected == Some(c.id) {
                    ui.visuals().selection.stroke.color
                } else {
                    ui.visuals().text_color()
                };
                let stroke = Stroke::new(1.5_f32, color);
                // Distinct, intentionally simple original geometric symbols.
                match c.kind {
                    ComponentKind::Input => {
                        painter.circle_stroke(center, 13., stroke);
                    }
                    ComponentKind::Output => {
                        painter.rect_stroke(
                            Rect::from_center_size(center, Vec2::splat(26.)),
                            0.,
                            stroke,
                            egui::StrokeKind::Inside,
                        );
                    }
                    ComponentKind::And => {
                        painter.line_segment(
                            [
                                center + Vec2::new(-20., -15.),
                                center + Vec2::new(-20., 15.),
                            ],
                            stroke,
                        );
                        painter.line_segment(
                            [center + Vec2::new(-20., -15.), center + Vec2::new(0., -15.)],
                            stroke,
                        );
                        painter.line_segment(
                            [center + Vec2::new(-20., 15.), center + Vec2::new(0., 15.)],
                            stroke,
                        );
                        painter.circle_stroke(center, 15., stroke);
                    }
                    ComponentKind::Or => {
                        painter.add(egui::Shape::closed_line(
                            vec![
                                center + Vec2::new(-20., -15.),
                                center + Vec2::new(20., 0.),
                                center + Vec2::new(-20., 15.),
                                center + Vec2::new(-12., 0.),
                            ],
                            stroke,
                        ));
                    }
                    ComponentKind::Not => {
                        painter.add(egui::Shape::closed_line(
                            vec![
                                center + Vec2::new(-18., -15.),
                                center + Vec2::new(12., 0.),
                                center + Vec2::new(-18., 15.),
                            ],
                            stroke,
                        ));
                        painter.circle_stroke(center + Vec2::new(16., 0.), 4., stroke);
                    }
                }
                if self.selected == Some(c.id) {
                    painter.rect_stroke(
                        bounds,
                        3.,
                        ui.visuals().selection.stroke,
                        egui::StrokeKind::Inside,
                    );
                }
                painter.text(
                    center + Vec2::new(0., 24.),
                    egui::Align2::CENTER_TOP,
                    &c.label,
                    egui::FontId::proportional(14.),
                    color,
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
        let p = Pos2::new(347., 108.);
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
}
