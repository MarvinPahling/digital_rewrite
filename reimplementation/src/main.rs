#![forbid(unsafe_code)]

// Icons are not wired into the UI yet; the module is only exercised by its tests.
#[allow(dead_code)]
mod assets;
#[allow(dead_code, reason = "wired into the UI by the next commit")]
mod canvas;
mod components;
mod domain;
mod engine;
mod formats;
#[allow(dead_code, reason = "wired into the UI by the next commit")]
mod symbol_render;
#[cfg(test)]
mod symbol_sheet;
#[allow(dead_code, reason = "wired into the UI by the next commit")]
mod symbol_tessellate;
#[allow(dead_code, reason = "wired into the UI by the next commit")]
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
