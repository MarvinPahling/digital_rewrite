#![forbid(unsafe_code)]

// Icons are not wired into the UI yet; the module is only exercised by its tests.
#[allow(dead_code)]
mod assets;
mod canvas;
use digital_rust::components;
use digital_rust::domain;
use digital_rust::runtime;
use digital_rust::simulation;
mod engine;
mod formats;
mod sim_text;
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
        Box::new(|_| Ok(Box::new(ui::DigitalApp::default()))),
    )
}
