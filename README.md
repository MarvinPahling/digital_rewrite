# Digital Rewrite

<img src="reimplementation/assets/brand/svg/app-icon.svg" alt="Digital Rewrite app logo" width="128">

![Digital Rewrite splash artwork](reimplementation/assets/brand/svg/app-splash.svg)

An early Rust rewrite of [Digital](https://github.com/hneemann/Digital), using PhotoCraft as the architectural source of truth and MaterialCraft as UI inspiration, particularly its node editor. Digital is the behavioral compatibility authority. An early native component editor supports placing, selecting, moving and deleting five component kinds with undo/redo. Wires, simulation, `.dig` I/O, clipboard and compatibility are not implemented. See [parity notes](reimplementation/docs/parity.md).

## Quickstart

Install a Rust toolchain supporting edition 2024, Cargo, and optionally [just](https://just.systems/). A native window system and a wgpu-compatible graphics backend are required; Linux builds also require the usual Wayland/X11 development libraries.

```sh
just run-rust
just build-rust
just check-rust
```

Without just, use `cargo run --locked --manifest-path reimplementation/Cargo.toml`. The existing `just run` still launches the bundled Java reference using its machine-specific Java path. `build_reimplemtation` remains an alias for `build-rust`.

## Structure and dependencies

The single package in `reimplementation/` contains `domain` (documents and signals), `components` (definitions and simulation), `formats` (file I/O), and `engine` (sessions and commands); only component documents and editing history are implemented. `ui` is the native adapter and `main.rs` bootstraps it; `symbols` (generated vector data), `symbol_tessellate`, `symbol_render` and `canvas` draw and pick components without involving the engine. Toolkit-independent layers must not depend on the UI.

Published eframe 0.31 supplies egui and wgpu, default fonts, accessibility, and X11/Wayland support. It avoids MaterialCraft's forked egui/winit patch set and widgets. `lyon_tessellation` (pinned to 1.0.23, `std` only) tessellates compound symbol fills and strokes, which epaint's convex-only path fills cannot. Simulation, XML, automation and HDL dependencies are deferred until their requirements are understood; no compatibility is implied. The lockfile fixes resolved versions.

## Vector assets

Icon and brand artwork is original and authored as SVG with the VectorCraft MCP; sources and provenance are in `reimplementation/assets/` (`icons/manifest.toml` is the inventory). PNGs are never committed: `reimplementation/build.rs` rasterizes the delivery SVGs with `resvg` at build time into `OUT_DIR`, and fails the build for missing or unparsable SVGs, `<text>` elements or aspect-ratio mismatches. `just assets-preview` (requires `jq`) builds and copies the generated PNGs to `target/asset-preview/` for inspection from outside `build.rs`, so previews never invalidate Cargo's inputs; `just check-assets-preview` verifies this. The icons are not yet wired into the UI.

Component symbols (`reimplementation/assets/components/`, inventory in `manifest.toml`) are generated from the committed SVG masters at build time: `build.rs` validates each SVG (anchors on the 20-unit grid, allowed features only, outlined text, black ink), normalizes it with `resvg::usvg` and writes toolkit-neutral vector data (`PathOp`, `SymbolPath`, `SymbolDef`) to `OUT_DIR/component_symbols.rs`, exposed by `src/symbols.rs`. No SVG is parsed at run time. The canvas draws and picks these symbols through `lyon`-tessellated, anti-aliased meshes cached per style, kind and scale bucket; the palette offers a Rectangular (default) / IEEE style toggle. `just symbol-sheet` writes a software-rasterized preview of all ten bindings to `reimplementation/target/symbol-sheet.png`. There is no pan/zoom UI or wiring yet.
