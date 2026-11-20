//! Native egui adapter. Circuit editing is not implemented yet.

pub struct DigitalApp;

pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
}

fn show(context: &eframe::egui::Context) {
    eframe::egui::CentralPanel::default().show(context, |ui| {
        ui.heading("Hello World");
    });
}

impl eframe::App for DigitalApp {
    fn update(&mut self, context: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        show(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_selects_wgpu() {
        assert!(matches!(native_options().renderer, eframe::Renderer::Wgpu));
    }

    #[test]
    fn app_renders_a_headless_frame() {
        let context = eframe::egui::Context::default();
        let output = context.run(Default::default(), |context| {
            show(context);
        });
        assert!(!output.shapes.is_empty());
    }
}
