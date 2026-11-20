# Digital Rewrite

An early Rust rewrite of [Digital](https://github.com/hneemann/Digital), using MaterialCraft as an architecture and UI reference. An early native component editor supports placing, selecting, moving and deleting five component kinds with undo/redo. Wires, simulation, `.dig` I/O, clipboard and compatibility are not implemented. See [parity notes](reimplementation/docs/parity.md).

## Quickstart

Install a Rust toolchain supporting edition 2024, Cargo, and optionally [just](https://just.systems/). A native window system and a wgpu-compatible graphics backend are required; Linux builds also require the usual Wayland/X11 development libraries.

```sh
just run-rust
just build-rust
just check-rust
```

Without just, use `cargo run --locked --manifest-path reimplementation/Cargo.toml`. The existing `just run` still launches the bundled Java reference using its machine-specific Java path. `build_reimplemtation` remains an alias for `build-rust`.

## Structure and dependencies

The single package in `reimplementation/` contains `domain` (documents and signals), `components` (definitions and simulation), `formats` (file I/O), and `engine` (sessions and commands); only component documents and editing history are implemented. `ui` is the native adapter and `main.rs` bootstraps it. Toolkit-independent layers must not depend on the UI.

Published eframe 0.31 supplies egui and wgpu, default fonts, accessibility, and X11/Wayland support. It avoids MaterialCraft's forked egui/winit patch set and widgets. Simulation, XML, automation and HDL dependencies are deferred until their requirements are understood; no compatibility is implied. The lockfile fixes resolved versions.

## Vector assets

Icon and brand artwork is original and authored as SVG with the VectorCraft MCP; sources and provenance are in `reimplementation/assets/` (`icons/manifest.toml` is the inventory). PNGs are never committed: `reimplementation/build.rs` rasterizes the delivery SVGs with `resvg` at build time into `OUT_DIR`, and fails the build for missing or unparsable SVGs, `<text>` elements or aspect-ratio mismatches. `just assets-preview` builds and copies the generated PNGs to `target/asset-preview/` for inspection. The icons are not yet wired into the UI.
