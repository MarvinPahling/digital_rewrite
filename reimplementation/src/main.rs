#![forbid(unsafe_code)]

// App icon/splash exports remain intentionally unused by the native shell.
#[allow(dead_code)]
mod assets;
mod canvas;
use digital_rust::{components, domain, engine, file_session, runtime, selection, sim_text};
mod preferences;
mod symbol_render;
#[cfg(test)]
mod symbol_sheet;
mod symbol_tessellate;
mod symbols;
mod ui;
mod wire_geometry;

// The component symbol pipeline is compiled into test builds so its diagnostics are tested.
#[cfg(test)]
#[path = "../build_support/component_symbols.rs"]
mod component_symbol_pipeline;

fn main() -> eframe::Result {
    eframe::run_native(
        "Digital Rust",
        ui::native_options(),
        Box::new(|cc| Ok(Box::new(ui::DigitalApp::native_with_storage(cc.storage)))),
    )
}
