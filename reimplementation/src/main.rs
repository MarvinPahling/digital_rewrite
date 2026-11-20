#![forbid(unsafe_code)]

mod components;
mod domain;
mod engine;
mod formats;
mod ui;

fn main() -> eframe::Result {
    eframe::run_native(
        "Digital Rust",
        ui::native_options(),
        Box::new(|_| Ok(Box::new(ui::DigitalApp))),
    )
}
