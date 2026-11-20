//! Native adapter for component editing.
use crate::{
    components::KINDS,
    domain::{ComponentId, ComponentKind, Position},
    engine::Session,
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
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
        egui::SidePanel::left("palette").show(ctx, |ui| { ui.heading("Components"); for kind in KINDS { if ui.selectable_label(self.placing == Some(kind),kind.name()).clicked() { self.placing = Some(kind); } } ui.separator(); ui.label("Click canvas to place. Drag to move. Escape cancels. Delete removes; Ctrl/Cmd-Z undoes; Shift-Ctrl/Cmd-Z redoes."); ui.label(if self.session.dirty() { "Modified" } else { "Unmodified" }); if let Some(error)= &self.error { ui.colored_label(Color32::RED,error); } });
        egui::CentralPanel::default().show(ctx, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            let grid = 20.0;
            let mut x = rect.left();
            while x < rect.right() {
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    Stroke::new(0.5_f32, Color32::DARK_GRAY),
                );
                x += grid;
            }
            let mut y = rect.top();
            while y < rect.bottom() {
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    Stroke::new(0.5_f32, Color32::DARK_GRAY),
                );
                y += grid;
            }
            let pointer = response.interact_pointer_pos();
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.drag = None;
                self.placing = None;
            }
            if (response.drag_started() || response.clicked())
                && let Some(p) = pointer
            {
                let hit = self
                    .session
                    .document
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
                if let Some(kind) = self.placing.take() {
                    let pos = Position {
                        x: p.x - rect.left(),
                        y: p.y - rect.top(),
                    };
                    match self.session.place(kind, pos) {
                        Ok(id) => self.selected = Some(id),
                        Err(e) => self.error = Some(e.into()),
                    }
                } else {
                    self.selected = hit.map(|v| v.0);
                    if response.drag_started()
                        && let Some((id, pos)) = hit
                    {
                        self.drag = Some((id, pos, p));
                    }
                }
            }
            if response.drag_stopped()
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
                    self.error = Some(e.into());
                }
            }
            for c in &self.session.document.components {
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
                    Color32::YELLOW
                } else {
                    ui.visuals().text_color()
                };
                painter.rect_stroke(
                    bounds,
                    3.,
                    Stroke::new(1.5_f32, color),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    &c.label,
                    egui::FontId::proportional(14.),
                    color,
                );
            }
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Delete))
            && let Some(id) = self.selected
        {
            match self.session.delete(id) {
                Ok(()) => self.selected = None,
                Err(e) => self.error = Some(e.into()),
            }
        }
        let (undo, redo) = ctx.input(|i| {
            (
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                i.modifiers.command
                    && ((i.modifiers.shift && i.key_pressed(egui::Key::Z))
                        || i.key_pressed(egui::Key::Y)),
            )
        });
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
    fn headless_frame() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let output = ctx.run(Default::default(), |ctx| app.show(ctx));
        assert!(!output.shapes.is_empty());
    }
}
