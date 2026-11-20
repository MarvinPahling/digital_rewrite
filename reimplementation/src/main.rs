#![forbid(unsafe_code)]

// Icons are not wired into the UI yet; the module is only exercised by its tests.
#[allow(dead_code)]
mod assets;
mod components;
mod domain;
mod engine;
mod formats;
mod ui;

fn main() -> eframe::Result {
    eframe::run_native(
        "Digital Rust",
        ui::native_options(),
        Box::new(|_| Ok(Box::new(ui::DigitalApp::default()))),
    )
}
