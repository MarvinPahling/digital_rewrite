#![forbid(unsafe_code)]

// Icons are not wired into the UI yet; the module is only exercised by its tests.
#[allow(dead_code)]
mod assets;
mod components;
mod domain;
mod engine;
mod formats;
// The renderer and layout code that consume the symbol data are the next integration step.
#[allow(
    dead_code,
    reason = "symbol data is not wired into the UI yet; exercised by tests"
)]
mod symbols;
mod ui;

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
