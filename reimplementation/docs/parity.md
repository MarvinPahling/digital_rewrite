# Component-editing subset

- EDIT-1 implemented: empty document and in-memory Input, Output, AND, OR and NOT descriptors, positions, labels, validated commands, snapshot undo/redo and dirty state. Headless engine tests exercise history, IDs, invalid commands and redo preservation.
- EDIT-2 implemented: palette, canvas grid, selection, movement, deletion, shortcuts and visible errors. Original simple kind-specific vector geometry and separate labels are not Digital artwork. Headless egui tests exercise placement, focus ownership, rendered errors, selection, drag cancellation, history and shortcuts; native interaction, all palette choices and light/dark appearance remain unverified.
- EDIT-3 unsupported: wires, simulation, clipboard, `.dig` import/export and saving. This is not a milestone completion or exhaustive feature inventory.

Reference investigation: Digital source revision d118dce079b6b8867024d40f9b14dfa667e52106, `references/Digital/src/main/java/de/neemann/digital/draw/elements/Circuit.java` and `references/Digital/src/main/java/de/neemann/digital/gui/`; MaterialCraft revision 4ff5af9af7371613ee37b186e03caf49ee6dbd88, `references/materialcraft/docs/architecture.md` and `references/materialcraft/crates/engine/src/session.rs`. No reference code, fixtures, tests or assets were copied; this is not a clean-room claim. Licensing/reuse policy remains unresolved and blocks distribution pending owner and legal review.

## Vector assets

- 25 UI icons and the application icon and splash are original SVGs made with the VectorCraft MCP (`reimplementation/assets/`, inventory in `icons/manifest.toml`, provenance in `assets/PROVENANCE.md`); no Digital artwork was copied or traced.
- PNGs are generated at build time by `reimplementation/build.rs` using `resvg` (icons at 24 and 48 px, app icon at 16-512 px, splash at 512x320 and 1024x640) and are never committed. `just assets-preview` copies them to `target/asset-preview/`.
- Not yet done: the icons and brand images are exposed through `src/assets.rs` but are not wired into the UI; no toolbar, menu, window icon or splash screen uses them.
