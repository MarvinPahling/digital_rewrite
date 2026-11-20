# Digital Rust rewrite

## Goal and scope

Reimplement Digital's circuit editor and digital-logic simulator in Rust, using PhotoCraft as the architectural source of truth. Digital defines behavior and compatibility; MaterialCraft is UI inspiration, especially its node editor.

Implementation belongs in `reimplementation/`. Treat `references/` as read-only unless the task explicitly asks to update a reference. Do not change submodule revisions as part of implementation work. Reference repositories' agent files describe those projects; their organization-specific contribution policies are not this repository's policy.

## Current repository

- `reimplementation/Cargo.toml`: Rust 2024 package with an egui/eframe native editor. See `reimplementation/docs/parity.md` for the implemented editing subset and gaps.
- `references/Digital/`: Java source submodule; the behavioral reference.
- `references/Digital_release/`: runnable distribution, documentation PDFs, example circuits and component libraries.
- `references/photocraft/`: architectural reference.
- `references/materialcraft/`: UI inspiration, especially the node editor.
- Root `justfile`: `just run` launches the bundled Java application, **not** the Rust rewrite. Its Java executable path is machine-specific.

Baseline examined for this guide: Digital `d118dce079b6b8867024d40f9b14dfa667e52106`; PhotoCraft `877d58844029967b80e36a964902d309de1be158`; MaterialCraft `4ff5af9af7371613ee37b186e03caf49ee6dbd88`. The bundled Digital release identifies itself as `v0.31` in `references/Digital_release/Version.txt`, whereas the source checkout is later. Record the source revision or release version used for each compatibility result; do not conflate them. Recheck manifests and source when reference revisions change.

## Reference map and technology stacks

### Digital: behavior to reproduce

Digital is a Java 8-targeted Maven application with a Swing/AWT desktop GUI. `references/Digital/pom.xml` declares XStream 1.4.20 for serialization, JUnit 4.13.1 for tests, and dependencies including SLF4J, JSON, Apache FOP and JDOM2. These are reference implementation details, not Rust dependency requirements.

Read the relevant subsystem before implementing its counterpart:

| Work | Reference path |
| --- | --- |
| Simulation scheduling and component behavior | `references/Digital/src/main/java/de/neemann/digital/core/` |
| Circuit documents, wires, drawing and model construction | `references/Digital/src/main/java/de/neemann/digital/draw/` |
| Desktop interaction | `references/Digital/src/main/java/de/neemann/digital/gui/` |
| Boolean analysis and synthesis | `references/Digital/src/main/java/de/neemann/digital/analyse/` and `references/Digital/src/main/java/de/neemann/digital/builder/` |
| Circuit test execution | `references/Digital/src/main/java/de/neemann/digital/testing/` |
| HDL import/export and external simulation | `references/Digital/src/main/java/de/neemann/digital/hdl/` |
| State-machine tools | `references/Digital/src/main/java/de/neemann/digital/fsm/` |
| Command-line behavior | `references/Digital/src/main/java/de/neemann/digital/cli/` |
| Regression tests and circuit fixtures | `references/Digital/src/test/java/` and `references/Digital/src/test/resources/dig/test/` |
| User workflows and examples | `references/Digital/README.md`, `references/Digital_release/docu/` and `references/Digital_release/examples/` |

The feature surface extends beyond gates: buses, tri-state/weak signals, arithmetic, sequential elements, memory, hierarchical circuits, measurement, truth tables, test vectors, FSMs and HDL tooling. Implement these incrementally; a gate editor alone is not full Digital parity. External VHDL and Verilog simulation uses GHDL and Icarus Verilog respectively; those integrations are separate capabilities, not prerequisites for basic simulation.

### PhotoCraft: architecture to adapt

Before changing boundaries, read `references/photocraft/docs/architecture.md` (its current-state summary supersedes its historical plan) and `references/photocraft/AGENTS.md`. PhotoCraft separates its document, engine, I/O and UI; command IDs connect UI and headless adapters. Before native UI work, inspect `references/photocraft/apps/photocraft/src/main.rs` and `references/photocraft/crates/ui-egui/`. For node-editor interaction inspiration, inspect `references/materialcraft/crates/ui-egui/src/graph/`.

Adapt architectural principles, not PhotoCraft's image-processing semantics, compositing, pixel tiles or formats. Digital determines circuit behavior, including feedback and simulation timing. PhotoCraft's workspace and dependencies are reference evidence, not a mandate to clone them.

## Target architecture (direction, not existing implementation)

Use Rust with a native egui/eframe UI as the default direction. Keep the simulator CPU-based and toolkit-independent. Choose dependency versions when introducing the GUI; the reference's versions and forks are evidence, not dependencies already adopted here.

Start with focused modules in the existing package. Extract crates when boundaries justify them; do not generate an empty copy of PhotoCraft's workspace. Preserve these responsibilities and dependency direction:

1. **Domain:** circuit document, component identities, ports, wires, typed attributes and signal values. No GUI or filesystem dependencies.
2. **Components and simulation:** component definitions and runtime behavior; circuit compilation/connectivity, event scheduling, clocks and diagnostics. Runtime state is separate from the editable document.
3. **Formats:** `.dig` parsing, migration, validation and serialization; resource/subcircuit resolution through explicit I/O boundaries.
4. **Engine:** document sessions, editing commands, undo/redo, simulation lifecycle and headless inspection. Commands validate before mutation and leave state unchanged on failure.
5. **Adapters:** native UI, CLI and later automation use the same engine operations. GUI panels must not implement their own simulation or bypass editing commands.

Keep document edits separate from transient selection/view changes and simulation ticks. Start with snapshot undo if appropriate, but measure memory cost before applying it to large circuit histories or RAM state. Long-running simulation must not block the UI; define cancellation, bounded work and stale-result rejection when adding workers.

Reuse PhotoCraft's separation of concerns, not its image-processing implementation. In particular, a topological material evaluator is not a circuit simulator: circuits include feedback and state. Browser support, MCP and GPU work are later decisions, not prerequisites for the first native slice. Keep core interfaces portable without claiming untested platform support.

## Behavioral and file-compatibility guardrails

Before changing simulation semantics, inspect `references/Digital/src/main/java/de/neemann/digital/core/Model.java` and the relevant component tests.

- Normal propagation reads affected gates' inputs before updating their outputs together. Preserve step boundaries; arbitrary immediate updates can change observable behavior.
- Startup differs from normal propagation: randomized gatewise updates settle feedback, and initial state can remain undefined. Reset behavior occurs after settling. Test controlled outcomes or permitted outcomes rather than assuming identical random traces.
- Signals are width-aware, not booleans. `references/Digital/src/main/java/de/neemann/digital/core/ObservableValue.java` represents value bits and a per-bit high-impedance mask; its ordinary read randomizes high-Z bits, while another read treats them as zero. Specify which semantics each operation needs. Cover masking, signed interpretation, high-Z, contention and weak drivers explicitly rather than imposing an assumed four-state model.
- Hierarchical circuits need separate runtime state per instance. Keep source documents and compiled instances distinct, and diagnose missing or recursive dependencies.
- Digital disallows circuit edits during simulation. Match this initially; changing it requires an explicit lifecycle design and tests.
- Bound settling/oscillation work and return useful diagnostics. Match single-step behavior and investigate the reference's oscillation detection before selecting limits.

Before implementing `.dig` I/O, read `references/Digital/src/main/java/de/neemann/digital/draw/elements/Circuit.java` and representative fixtures. The format is XStream XML containing visual elements, positions, typed attributes, wires and potentially test data—not just a netlist. The examined implementation writes version 2 and migrates versions 0 and 1.

Define the supported import/export subset explicitly. Preserve unsupported data losslessly where possible; otherwise report the limitation and prevent a misleading successful save that discards it. Bound XML input size and nesting, hierarchy expansion and resource access. Resolve linked circuits relative to their owning document. Save atomically, with failure leaving the original intact. Test semantic round trips, not only successful parsing.

## Implementation and validation workflow

For each feature:

1. Identify the Digital behavior, supporting source/test paths and reference version. State the supported subset and unresolved cases.
2. Add headless behavior tests and implement the domain/engine path before exposing it through the UI.
3. Wire the native interaction to that path. Preserve keyboard access, visible errors and theme-based styling.
4. Run applicable checks and report remaining parity gaps. Unsupported components, formats or integrations must produce explicit diagnostics, not placeholder success.

A useful first vertical slice is a small editable combinational circuit with inputs, gates, wires and outputs, engine commands, simulation, undo and a tested `.dig` subset. Then expand to buses/high-Z, sequential state, hierarchy, memory and the broader analysis/tooling surface. This is suggested sequencing, not a declaration that later features are out of scope.

From the repository root, current Rust commands are:

```sh
cargo run --manifest-path reimplementation/Cargo.toml
cargo fmt --manifest-path reimplementation/Cargo.toml --all -- --check
cargo check --manifest-path reimplementation/Cargo.toml --all-targets
cargo test --manifest-path reimplementation/Cargo.toml
cargo clippy --manifest-path reimplementation/Cargo.toml --all-targets -- -D warnings
```

These validate the current editing subset, not Digital compatibility. After a workspace exists, update commands and add a dependency-layer check. MaterialCraft's `cargo xtask layers`, `cargo xtask wasm` and snapshot tests exist only in that reference; do not present them as available here.

For Java reference checks, use `mvn -f references/Digital/pom.xml test` with the required Java/Maven environment. Its Maven configuration excludes fragile GUI tests; passing unit tests is not full UI parity. `just run` is useful for manual comparison if its configured JDK exists; alternatively launch `java -jar references/Digital_release/Digital.jar` with an appropriate Java installation. Record manual observations separately from automated results.

Prioritize regression coverage for propagation order, feedback/startup, clocks, bus widths, high-Z splitters, independent hierarchy instances, RAM and format migrations. Start investigation with `references/Digital/src/test/java/de/neemann/digital/core/ObservableValueTest.java`, `references/Digital/src/test/java/de/neemann/digital/core/wiring/SplitterHighZTest.java` and `references/Digital/src/test/java/de/neemann/digital/draw/elements/CircuitTest.java`. Use differential comparisons where licensing permits; use self-authored fixtures for newly encoded behavior. Never claim full parity from a successful build or a few example circuits.

Use typed errors for malformed documents and unsupported operations; avoid production panics on user input. Prefer safe Rust. Keep reusable commands in the root `justfile` as workflows stabilize. Report checks actually run, failures and checks skipped; preserve unrelated working-tree changes.

## Icon generation (VectorCraft MCP)

Use the `vectorcraft` MCP server to generate icons and other vector assets (toolbar, component and application icons). It is configured user-level in `~/.pi/agent/mcp.json` and runs `vectorcraft-cli mcp` (Homebrew `vectorcraft-cli`, 0.24.1). Docs: <https://craft.wiki/apps/vectorcraft/ai-agents/>.

- Tools are reachable from `codemode` as `mcp__vectorcraft`. Main tools: `draw_shape`, `draw_path`, `add_text`, `set_paint`, `transform`, `pathfinder`, `render_preview`, `export`, `save_file`. The `icon-set` prompt draws matching icons on a grid.
- Without a window the server works headless on documents. To watch it draw, quit VectorCraft, start it with `open -a VectorCraft --args --control 7979`, then restart the agent session.
- Draw as live shapes on a fixed grid (for example 24×24 px). Check each icon with `render_preview` before export.
- Save the editable `.svg` source and the exported assets in the rewrite's asset directory, for example `reimplementation/assets/icons/`. Do not write into `references/`.
- Generated icons are self-authored assets. Do not copy or trace Digital's icons or fonts (GPLv3) or MaterialCraft's branding. Record provenance (VectorCraft MCP, prompt or brief) next to the asset.

## Licensing and provenance

Digital's source is GPLv3 (`references/Digital/LICENSE`); PhotoCraft and MaterialCraft code is MIT OR Apache-2.0, with additional asset/brand notices. This repository has not yet selected a rewrite license. Reading source for this investigation does not establish that a subsequent implementation is clean-room or that copied code can be distributed under a permissive license.

Before copying code, tests, fixtures, icons, fonts or documentation into the rewrite, establish the intended license/reuse policy with the owner and record provenance plus applicable notices. Before distribution, resolve derivative-work and license obligations with appropriate review. Prefer original implementation and self-authored assets; do not transplant MaterialCraft's branding or its organization-wide clean-room policy as if it resolved this project's GPL questions.
