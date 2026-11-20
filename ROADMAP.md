# Digital Rust rewrite roadmap

## Goal and current state

Build a native Rust replacement for Digital with its complete user-facing feature set and compatible circuit workflows. Keep Digital's recognizable, canvas-first GUI layout, but give it modern typography, spacing, controls, themes and feedback. MaterialCraft is the architecture and native-UI reference, not the source of circuit semantics or branding.

The current implementation is only a Rust 2024 `digital-rust` package with a Hello World binary and no dependencies. **Every implementation checkbox below is initially incomplete.** This document is a plan, not a claim of compatibility. Implementation belongs in `reimplementation/`; reference repositories remain read-only.

### Reference baseline

| Reference | Baseline | Purpose |
| --- | --- | --- |
| Digital source | `d118dce079b6b8867024d40f9b14dfa667e52106` | Behavioral source, tests, component catalog and desktop workflows |
| Digital bundled distribution | `references/Digital_release/Version.txt`: `v0.31` | Runnable comparison, manuals, example circuits and libraries |
| MaterialCraft source | `4ff5af9af7371613ee37b186e03caf49ee6dbd88` | Rust architecture, command/session patterns and native GUI approach |

The source checkout is later than the bundled release. Record which target each test covers; never label a source-only result as v0.31 compatibility. When references change, update the inventory and rerun affected tests.

This roadmap combines source inventories, Digital's README, the v0.31 English manual's feature/component contents, and the source distribution screenshots. It is not yet an exhaustive audit of every attribute or menu action. Milestone M0 makes that audit an explicit deliverable so less visible features do not disappear from scope.

## What “all features” means

Full parity includes the editor, simulator, every registered component and its attributes, hierarchical and generic circuits, libraries, test language, analysis/synthesis, FSM tools, measurement, import/export, CLI, remote control, extensibility and hardware/toolchain workflows. An MVP is a checkpoint, not the final scope.

For each feature, track these dimensions separately:

- **Document compatibility:** read, preserve, edit and write its data without unintended loss.
- **Runtime compatibility:** reproduce observable behavior, including errors and timing boundaries.
- **Interaction compatibility:** provide the workflow through discoverable native controls and keyboard access.
- **Integration compatibility:** validate external tools, protocols and device outputs independently.

A placeholder, successful parse, or disabled button does not establish parity. Missing capabilities must produce actionable diagnostics. A platform-specific integration can be unavailable on a platform, but its supported platforms and limitations must be documented and tested.

## GUI direction: familiar layout, modern presentation

### Preserve the familiar structure

Digital's source screenshots show a top menu, compact action toolbar, a large gridded circuit canvas and a bottom status area. Analysis tools appear in separate windows. Its source also supports an optional navigation tree. Retain this spatial model rather than replacing the circuit with a material-node editor.

Proposed expanded workspace (side panels and bottom tools are collapsible):

```text
+-----------------------------------------------------------------------+
| File  Edit  View  Simulation  Analysis  Components  Windows  Help        |
| New Open Save | Undo Redo | Zoom Fit | Run Stop Step Clock | mode/state |
+--------------+-----------------------------------------+--------------+
| Components / | Circuit tabs / hierarchy breadcrumb     | Properties   |
| hierarchy    +-----------------------------------------+              |
|              |                                         | Selection /  |
| Search       |          CIRCUIT CANVAS                  | circuit /    |
| Categories   |    grid, standard symbols, wires,        | component    |
| Favorites    |    labels, signal-state overlays         | attributes   |
|              |                                         |              |
+--------------+-----------------------------------------+--------------+
| Optional tools: Diagnostics | Signals | Tests | Tables | Search        |
+-----------------------------------------------------------------------+
| Status / contextual hint                   Simulation time / frequency |
+-----------------------------------------------------------------------+
```

- Default to a spacious circuit canvas with tool panels closed or compact; do not require all panels to be open.
- Keep open/save, undo/redo, zoom, simulation and step controls in predictable top positions.
- Preserve circuit symbols, orthogonal wiring, junction semantics, pin labels and bus-width labels. Do not substitute thumbnail cards or curved material-graph wires.
- Offer searchable component/library access and optional hierarchy navigation on the left; contextual properties on the right.
- Keep truth tables, Karnaugh maps, waveforms, memory and FSM tools available as resizable panels or separate tool windows. Do not force every tool into a small bottom strip.
- Support a presentation/minimal-chrome mode, layout reset and persistent panel sizes. Modern docking must not make the main circuit canvas accidentally disappear.

### Modernize the visual language and feedback

- [ ] Define shared theme tokens for spacing, type, surfaces, borders, focus, selection and signal states.
- [ ] Provide light, dark and system theme choices, high-DPI scaling and legible circuit labels.
- [ ] Use original vector icons with consistent sizes, tooltips and visible shortcut hints; avoid copied branding/assets.
- [ ] Use restrained panels and borders, consistent dialogs and clear primary/destructive actions.
- [ ] Make editing, starting, running, paused/stepping, stopped and error states unambiguous.
- [ ] Explain disabled commands, especially why editing requires stopping simulation.
- [ ] Supplement signal colors with labels, patterns or inspection values; verify contrast and color-vision accessibility.
- [ ] Provide keyboard navigation, focus indicators, accessible names and screen-reader checks where the toolkit permits. Document canvas accessibility gaps explicitly.
- [ ] Keep wire errors attached to their circuit locations; clicking a diagnostic should focus the relevant element or net.
- [ ] Validate designs at small laptop sizes and large displays before committing to panel defaults.

These are proposed improvements, not claims that Digital already has these controls. Preserve familiar shortcuts where practical; document intentional changes. Approve representative mockups and interaction recordings before broad visual polish.

## Architecture and implementation rules

Start with focused modules inside the existing package; extract crates only when actual boundaries justify them. Read `references/materialcraft/docs/architecture.md` before changing boundaries and `references/materialcraft/CODE_STYLE.md` for Rust organization. Before native UI work, inspect its desktop entrypoint and `crates/ui-egui/`.

```text
Domain: document, IDs, ports, attributes, signals
    ↓
Components + simulation: catalog, connectivity/compiler, runtime, scheduler
Formats: document codecs, migrations, explicit resource resolution
    ↓
Engine: sessions, validated commands, history, simulation lifecycle
    ↓
Adapters: native egui/eframe UI, CLI, remote/integration interfaces
```

The diagram expresses responsibility/layer order, not mandatory dependencies between every neighboring box. Domain code has no GUI or filesystem dependencies; formats translate document data without owning simulation. Engine operations are shared by adapters.

- Editable documents and compiled runtime instances are separate. Simulation ticks and selection/view changes are not document edits.
- Commands validate before publishing mutation; failures leave document, history and related state unchanged.
- Start with snapshot undo, merge continuous gestures into one undo entry, and measure history memory use before scaling to large circuits or RAM images.
- Keep simulation CPU-based and independent of egui. A circuit contains feedback and state; do not reuse MaterialCraft's topological material evaluator.
- Use bounded background work, cancellation and generation IDs to reject stale results after edits, document switches, stop or close. UI repaint frequency must not determine simulation results.
- Initially prohibit document edits during simulation, as Digital does.
- Use typed errors for invalid input and unsupported operations; avoid production panics on user data.
- Choose GUI dependencies deliberately. MaterialCraft uses egui/eframe with patched forks; inspect the entire dependency/patch combination before adopting Craft widgets.
- Browser support, GPU simulation and MCP are possible later additions, not Digital parity prerequisites. Digital's existing remote interface is in scope separately.

## Milestone sequence

Complete each milestone's exit gate before calling it done. Each feature follows: reference investigation → headless tests → domain/engine implementation → native interaction → compatibility result. Write issues small enough to finish and validate independently.

| Milestone | Outcome | Depends on |
| --- | --- | --- |
| M0 | Exact scope, licensing and compatibility inventory | — |
| M1 | Domain model, command sessions and validation foundation | M0 |
| M2 | Headless combinational simulator | M1 |
| M3 | Safe `.dig` subset and basic CLI | M1–M2 |
| M4 | First usable native editor / MVP | M1–M3 |
| M5 | Complete bus/electrical/routing semantics and arithmetic | M2–M4 |
| M6 | Sequential simulation, clocks and memory | M5 |
| M7 | Hierarchy, generics, libraries and custom appearances | M3, M5–M6 |
| M8 | Full interactive I/O, measurement and debugging | M4–M7 |
| M9 | Circuit testing language and batch verification | M6–M8 |
| M10 | Analysis, synthesis and FSM tools | M7, M9 |
| M11 | HDL and programmable-device toolchains | M7, M9–M10 |
| M12 | Remaining interchange, remote control and extensions | Relevant earlier capabilities |
| M13 | Full parity audit, UX polish and release qualification | M0–M12 |

Some investigation and independent adapters can proceed in parallel. Do not delay tests, accessibility or error handling until M13. External-tool availability must not block ordinary circuit editing or simulation.

### M0 — Establish the compatibility contract

- [ ] Select the project's license/reuse policy with the owner before copying source, tests, fixtures, fonts, icons or documentation. Digital is GPLv3; MaterialCraft is MIT OR Apache-2.0 with additional notices. Reading either does not establish a clean-room implementation.
- [ ] Create a versioned feature ledger, for example `reimplementation/docs/parity.md`, covering registered component identifiers, variants, attributes/defaults, symbol styles, menu actions, shortcuts, settings, CLI options, file versions, protocols and exporters.
- [ ] Inventory source registrations, all bundled libraries (including DIL/74xx chips and RAMs), examples, test fixtures and localized resources. Class files alone are not the user-visible catalog.
- [ ] Give every ledger row a milestone, reference path/version, implementation location, test evidence and status: not started, partial, implemented, verified or blocked.
- [ ] Separate legacy migration targets, v0.31 behavior and later source features. Decide how conflicting behavior is selected or reported.
- [ ] Define resource budgets and supported operating-system/toolchain targets. Measure reference workloads rather than adopting advertised performance figures as requirements.
- [ ] Record unresolved decisions: extension/JVM compatibility strategy, hardware available for validation, dependency/fork policy and distribution license obligations.

**Exit gate:** every discovered feature has a tracked destination; no unassigned “miscellaneous” bucket. Licensing blockers and platform limits are visible. The ledger remains open to newly discovered features throughout implementation.

### M1 — Domain, editing commands and session foundation

- [ ] Model circuit documents, stable component identities, ports/directions, wire segments/junctions, positions, rotations, typed attributes, labels, annotations and metadata.
- [ ] Define width-aware signal values and a per-bit high-impedance mask; distinguish signed formatting from stored bits. Confirm supported widths from the reference rather than assuming unlimited integers.
- [ ] Define component descriptors shared by document validation, simulation, properties and library UI.
- [ ] Implement sessions, dirty state, create/delete/move/rotate/property/wire commands, selection/view state, clipboard remapping and undo/redo.
- [ ] Validate pin existence, attributes, geometry and widths without requiring every incomplete editing state to simulate successfully.
- [ ] Establish structured diagnostics with component/net/source locations and separate document validation from runtime compilation errors.
- [ ] Add formatting, linting, tests and basic CI. Introduce dependency-layer checks when crate extraction occurs.

**Exit gate:** headless editing tests cover valid edits, invalid edits leaving state unchanged, clipboard identity remapping, undo/redo and dirty-state restoration.

### M2 — First correct headless simulator

- [ ] Compile document connectivity into runtime nets, including crossing-versus-junction behavior and disconnected-pin errors.
- [ ] Implement input, output, constants and AND/NAND/OR/NOR/XOR/XNOR/NOT; begin with an explicitly documented one-bit subset.
- [ ] Implement event scheduling with the reference's normal read-then-publish propagation boundaries, not arbitrary immediate output updates.
- [ ] Establish startup settling as a distinct lifecycle: randomized gatewise updates, permitted undefined initial states, then reset behavior. Expand feedback coverage in M6.
- [ ] Implement start/stop, input changes, bounded propagation and step inspection with useful non-settling diagnostics.
- [ ] Make event traces and headless inspection available to tests; define repeatable seed controls without promising identical Java random traces.

**Exit gate:** exhaustive small gate truth tables, connectivity cases, propagation-order regression tests and bounded feedback/oscillation tests pass. Review `core/Model.java` and relevant component tests before fixing simulator semantics.

### M3 — Safe circuit persistence and headless entrypoint

- [ ] Read `references/Digital/src/main/java/de/neemann/digital/draw/elements/Circuit.java` and representative files before implementing the codec.
- [ ] Parse/write an explicit `.dig` subset including visual elements, typed attributes, positions, wires, circuit metadata and embedded test payload preservation.
- [ ] Support version 2 output and v0/v1 migrations with semantic tests; retain the original version/provenance for diagnostics.
- [ ] Preserve unknown elements/attributes/data losslessly where possible. Otherwise make the document read-only or refuse a lossy save with a precise explanation; never report placeholder success.
- [ ] Bound XML size/depth and disallow unintended external entity/resource access. Define explicit I/O interfaces for owner-relative linked resources.
- [ ] Implement atomic saves, Save As, failure injection and original-file preservation. Keep external resources intact on failure.
- [ ] Add basic CLI open/validate/inspect/simulate commands through the engine; distinguish these initial commands from final Digital CLI compatibility.

**Exit gate:** semantic round trips, migration fixtures, malformed/oversized XML, unsupported-content preservation and interrupted-save tests pass. A Rust-saved supported circuit opens correctly in the declared Digital target; record that check separately from Rust round trips.

### M4 — First usable native editor (MVP)

- [ ] Introduce the egui/eframe shell, central circuit canvas, top menus/toolbar and bottom status area using the GUI direction above.
- [ ] Implement pan, pointer-centered zoom, fit, grid/snap, pin/wire hit testing and selection without encoding topology rules in drawing code.
- [ ] Expose component insertion/search, wire creation/deletion, move/rotate, multi-selection, copy/cut/paste, delete, attribute editing and undo/redo through commands.
- [ ] Support cancelled gestures and focus loss without partial edits; preview gestures transiently and commit one history entry.
- [ ] Implement new/open/save/save-as, recent files, unsaved-change prompts and visible errors.
- [ ] Expose run/stop/step and interactive inputs; visualize output values and signal states. Clearly disable editing while running.
- [ ] Add first light/dark theme, keyboard/focus support and UI interaction/snapshot tests.

**Exit gate / MVP demo:** create a small combinational circuit, wire it, edit attributes, undo/redo, save, reopen, run, change inputs, inspect outputs and stop to edit again. Compare the result with Digital. The UI stays responsive and no UI code implements simulation behavior.

### M5 — Buses, electrical behavior, routing and arithmetic

- [ ] Expand every applicable component to the reference width and inversion/format attributes.
- [ ] Implement splitters/mergers, bidirectional splitters, tunnels, bus mappings, supply/ground/constants, no-connect markers and delay/asynchronous timing elements.
- [ ] Implement mux/demux, decoder, priority encoder, bit selector and lookup-table behavior with all catalog variants.
- [ ] Implement tri-state drivers including inverted select, pull-up/down and weak-driver resolution. Cover per-bit high-Z, contention and driver conflicts.
- [ ] Specify ordinary high-Z reads versus zero-substituting reads as distinct operations. Digital can randomize ordinary high-Z reads; do not impose an assumed four-state logic model.
- [ ] Implement switches, double-throw switches, relays, FETs including floating-gate variants, transmission gates, fuses and rail diodes according to the reference's digital model—not a new analog simulator.
- [ ] Implement add/subtract/multiply/divide, compare, negate, shift/rotate variants, sign extension and bit count, with exact width/signedness/carry/overflow and divide-by-zero behavior from the reference.

**Exit gate:** table-driven width/mask/signed boundary tests, high-Z splitter regressions, weak/strong contention cases and CMOS/NMOS-style feedback examples pass. Illegal configurations produce located diagnostics, not panics.

### M6 — Sequential behavior, clocks and memory

- [ ] Implement RS, clocked RS, D, JK and T flip-flops, asynchronous variants and monoflop behavior; cover edge polarity, enable, set/reset precedence and initialization attributes.
- [ ] Implement registers, counters/preset counters, program-counter/catalog variants and random generators with a documented seed policy.
- [ ] Implement clock controls, power/reset, break/stop and asynchronous timing lifecycle. Distinguish propagation step, single-gate debugging and clock step.
- [ ] Match startup/reset and oscillation handling with realistic feedback; test permitted startup outcomes rather than requiring identical random sequences.
- [ ] Implement ROM/dual-port ROM, RAM variants (separate/bidirectional/chip-select, synchronous/block, asynchronous and dual-port), register files and EEPROM.
- [ ] Define read/write collision, enable, edge, address, out-of-range and high-Z behavior for each memory variant.
- [ ] Add memory editing, initial contents, program loading and documented binary/Intel HEX/Logisim data import/export variants after catalog verification.
- [ ] Keep large runtime memories outside ordinary document undo unless the edit explicitly changes persisted memory contents.

**Exit gate:** edge/reset/enable traces, independent memories, read/write collisions, loader errors, feedback circuits and stop/cancel tests pass. Headless execution and UI stepping agree.

### M7 — Hierarchy, generics, libraries and appearance compatibility

- [ ] Resolve subcircuits/resources relative to their owning document, with useful missing-file and recursive-dependency diagnostics.
- [ ] Compile separate runtime state per instance while preserving Digital's flattening/connectivity and propagation behavior, including bidirectional pins.
- [ ] Add hierarchy navigation, open-child/return-to-parent, pin ordering/naming and interface-change validation.
- [ ] Implement generic parameters, initialization/code elements, expression/script evaluation and generated circuit attributes using a bounded execution model. Port documented behavior; do not execute arbitrary imported code by default.
- [ ] Implement library discovery, user libraries, DIL/74xx catalog, RAM library entries, insertion history and dependency-safe project packaging.
- [ ] Implement custom component shapes/SVG templates, pin placement, symbol styles, labels/text formatting, rectangles and other decorations from the feature ledger.
- [ ] Preserve unsupported custom appearance data and diagnose unsupported execution separately.

**Exit gate:** multiple independent stateful instances, nested/bidirectional circuits, generic width variants, moved projects, recursion/resource limits and shape round trips pass. Library distribution includes only assets cleared for reuse.

### M8 — Interactive I/O, measurement and debugging

- [ ] Complete inputs/outputs, buttons, DIP switches, probes, LEDs/RGB/two-terminal LEDs, illuminated buttons, bulbs, seven/hex/16-segment displays and LED matrices.
- [ ] Add keyboard/terminal, rotary encoders, unipolar/bipolar stepper motors, graphics RAM and VGA display workflows.
- [ ] Implement signal graphs and triggered capture, timing/waveform views, value tables, radix/signed display choices, capture export and memory/program inspection.
- [ ] Add breakpoints/break components, fast run without per-tick repaint, single-gate pending-change highlighting and diagnostic navigation.
- [ ] Integrate MIDI and Telnet peripherals behind explicit permission/availability boundaries; test protocol behavior independently of physical devices.
- [ ] Bound capture buffers and peripheral queues, decouple wall-clock pacing from logical simulation and release resources on stop/close.

**Exit gate:** scripted interactions and captured traces agree with the reference; display/terminal examples are usable; slow UI rendering does not change logical results. Devices/network failures are visible and cancellable.

### M9 — Circuit test language and batch execution

- [ ] Implement embedded test-case components/data, parsing and diagnostics with source line/column locations.
- [ ] Match test values, don't-care matching, virtual signals, clock/transition handling, expressions/functions, loops/conditionals and random generation from the registered language inventory.
- [ ] Implement per-circuit and folder-level execution, progress, cancellation, failure locations and deterministic replay metadata.
- [ ] Expose test editing/results in the UI and batch execution in the CLI with documented exit status and machine-readable reporting.
- [ ] Bound test work and report exhaustion distinctly from an assertion failure.

**Exit gate:** positive/negative language fixtures and sequential timing tests pass; the declared supported example corpus has no unexplained failures. Java differential results include source/release revision and intentional deviations.

### M10 — Analysis, synthesis and FSM workspace

- [ ] Derive combinational truth tables and sequential transition tables for reference-supported D/JK circuits; diagnose unsupported feedback structures rather than fabricating a table.
- [ ] Implement Boolean expression parsing/editing/formatting, CSV/table import/export, don't-care handling, minimization and Karnaugh maps.
- [ ] Cover Quine–McCluskey variants, prime-implicant selection and expression/circuit generation options identified in the ledger.
- [ ] Implement dependency/cycle/path-length analysis, label numbering and circuit/statistics views.
- [ ] Generate combinational/sequential circuits through engine commands, retaining explicit generation settings and undo boundaries.
- [ ] Implement FSM create/open/save, state/transition editing, conditions/outputs, initial states, state encoding/optimization, transition-table conversion and circuit generation.
- [ ] Provide table, expression, map and FSM tools as full-size views/windows with keyboard editing, export and actionable errors.
- [ ] Add size/work bounds, cancellation and progress for exponential analysis/minimization tasks.

**Exit gate:** exhaustive small-table equivalence, expression round trips, minimized/generated circuit equivalence and FSM transition/encoding equivalence pass. Test unsupported sequential analysis and complexity-limit errors explicitly.

### M11 — HDL and programmable-device workflows

- [ ] Implement the HDL intermediate model, VHDL/Verilog generation, naming/pin mapping, hierarchy, clock handling, external blocks, templates and testbench generation.
- [ ] Implement embedded/external-file HDL components and Pin Control behavior. Determine the precise accepted HDL subset; do not promise general-purpose HDL import merely because external co-simulation exists.
- [ ] Add GHDL and Icarus Verilog co-simulation adapters with version detection, process isolation, timeouts, cancellation, captured stderr and located errors.
- [ ] Implement GAL16V8/GAL22V10 JEDEC workflows, pin/fuse mapping, CUPL/TT2/ATF150x export/fitter paths and constraint errors from the reference inventory.
- [ ] Implement board/toolchain configurations including BASYS3 and TinyFPGA BX workflows, configuration files and program/build actions where supported by the baseline.
- [ ] Require confirmation for external commands/programming; report missing tools explicitly. Ordinary simulation must remain usable without these tools installed.

**Exit gate:** generated HDL is compiled and behaviorally compared using available tools; device output has structural/golden and fitter validation. Record physical-board checks separately. Export success alone does not prove hardware correctness; unavailable hardware remains a visible validation gap.

### M12 — Remaining interchange, remote control and extensibility

- [ ] Complete graphical export workflows: SVG, LaTeX/Inkscape-compatible SVG, raster/animated GIF and printing paths actually exposed by the audited reference.
- [ ] Complete project/dependency ZIP packaging, data/CSV and memory formats, CLI test/SVG/statistics commands, help/options/output and exit-code compatibility.
- [ ] Implement Digital's TCP remote/assembler integration: program loading, start/stop, stepping, break/run and program-address responses according to the observed protocol.
- [ ] Bind networking to safe defaults; require explicit opt-in for broader exposure, limit payloads and test disconnects/lifecycle. Digital TCP control, Telnet peripheral I/O and any future MCP API are separate interfaces.
- [ ] Resolve Java JAR custom-component compatibility explicitly. A Rust plugin API is not a compatible replacement for existing Java components. Investigate a JVM bridge/helper or another owner-approved strategy, then test a representative extension and its failure modes.
- [ ] Complete preferences/settings, localization, help/tutorials, examples, window management, search, presentation mode, numbering utilities and release-notification behavior identified by M0.
- [ ] Document platform substitutions without silently dropping features; keep unimplemented integrations marked partial/blocked in the ledger.

**Exit gate:** each registered importer/exporter/CLI/protocol/extension path has a test or an explicit unresolved compatibility record. Full-parity status is blocked by unresolved required paths, including Java-extension compatibility.

### M13 — Parity closure, modern UI polish and distribution

- [ ] Reconcile every feature-ledger row against component registrations, menus, settings, CLI, languages, libraries and example inventories; assign newly found gaps before declaring completion.
- [ ] Run a versioned compatibility corpus covering combinational logic, buses/high-Z, feedback, sequential circuits, independent hierarchy, generic circuits, RAM, I/O, analysis/FSM and HDL/toolchain fixtures.
- [ ] Finish visual consistency, iconography, layout persistence, onboarding, component search, theme coverage, accessibility and shortcut documentation across all tools—not only the main canvas.
- [ ] Benchmark large circuits/processor workloads, compile/load/save, simulation events and clock throughput, UI frame times, cancellation latency and memory/history/capture growth. Publish hardware/build settings and agreed budgets.
- [ ] Add fuzz/property testing for XML, expressions, test scripts, memory importers and protocol input, plus save-failure and resource-limit regression tests.
- [ ] Package and manually validate declared Windows/macOS/Linux targets: launch, file dialogs/associations, fonts/DPI, clipboard, open/save, external tools, settings paths and shutdown. Do not claim untested platform support.
- [ ] Provide versioned release notes, migration/compatibility notes, supported tool versions, user documentation, recovery procedures and reproducible build instructions.
- [ ] Complete license/provenance notices and distribution review before bundling reference-derived code or resources. Add update delivery only with an explicit security/distribution design.

**Exit gate / full release:** no required ledger row is missing, partial or blocked for the declared support matrix; all remaining deviations are explicit and approved. Publish automated, manual, external-tool and physical-hardware evidence separately. A smaller release can ship earlier, but must name its supported subset rather than claim full Digital parity.

## Validation workflow

For every implementation issue, record:

1. Feature/ledger IDs, reference revision/version, source/tests and supported subset.
2. Self-authored headless behavior tests before GUI exposure; provenance for any reused material.
3. Engine commands, error behavior, history behavior and compatibility/migration impacts.
4. UI interaction and accessibility checks, where applicable.
5. Checks actually run, results, manual observations, skipped checks and unresolved gaps.

Current Rust commands from the repository root:

```sh
cargo fmt --manifest-path reimplementation/Cargo.toml --all -- --check
cargo check --manifest-path reimplementation/Cargo.toml --all-targets
cargo test --manifest-path reimplementation/Cargo.toml
cargo clippy --manifest-path reimplementation/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path reimplementation/Cargo.toml
```

These currently exercise only the scaffold. Add focused simulator/codec/command tests, UI interaction and snapshot suites, differential comparisons and platform/tool integration jobs as those capabilities exist. Introduce reusable root `justfile` workflows without relabeling the current `just run`: it launches Java, not the Rust rewrite. MaterialCraft's `cargo xtask` commands do not exist here.

For comparison, run the bundled application with `java -jar references/Digital_release/Digital.jar` when an appropriate Java runtime exists. Java source tests use `mvn -f references/Digital/pom.xml test`; their exclusions mean they do not prove GUI or external-HDL parity. Use only cleared fixtures for redistribution and report manual comparison separately.

## Reference navigation

| Area | Starting points |
| --- | --- |
| Features/manual/examples | `references/Digital/README.md`, `references/Digital_release/docu/Documentation_en.pdf`, `references/Digital_release/examples/`, `references/Digital_release/lib/` |
| Scheduler and signals | `references/Digital/src/main/java/de/neemann/digital/core/Model.java`, `references/Digital/src/main/java/de/neemann/digital/core/ObservableValue.java` |
| Component families/catalog | `references/Digital/src/main/java/de/neemann/digital/core/`, `references/Digital/src/main/java/de/neemann/digital/draw/library/`, `references/Digital/src/main/java/de/neemann/digital/draw/shapes/` |
| Circuit codec/connectivity | `references/Digital/src/main/java/de/neemann/digital/draw/elements/Circuit.java`, `references/Digital/src/main/java/de/neemann/digital/draw/model/` |
| Editor/layout | `references/Digital/src/main/java/de/neemann/digital/gui/Main.java`, `references/Digital/src/main/java/de/neemann/digital/gui/components/`, `references/Digital/distribution/screenshot.png`, `references/Digital/distribution/screenshot2.png` |
| Analysis/synthesis/FSM | `references/Digital/src/main/java/de/neemann/digital/analyse/`, `references/Digital/src/main/java/de/neemann/digital/builder/`, `references/Digital/src/main/java/de/neemann/digital/fsm/` |
| Tests/HDL/toolchains/CLI | `references/Digital/src/main/java/de/neemann/digital/testing/`, `references/Digital/src/main/java/de/neemann/digital/hdl/`, `references/Digital/src/main/java/de/neemann/digital/toolchain/`, `references/Digital/src/main/java/de/neemann/digital/cli/` |
| High-risk regression tests | `references/Digital/src/test/java/de/neemann/digital/core/ObservableValueTest.java`, `references/Digital/src/test/java/de/neemann/digital/core/wiring/SplitterHighZTest.java`, `references/Digital/src/test/java/de/neemann/digital/draw/elements/CircuitTest.java` |
| Rust architecture/UI | `references/materialcraft/docs/architecture.md`, `references/materialcraft/CODE_STYLE.md`, `references/materialcraft/apps/materialcraft/src/main.rs`, `references/materialcraft/crates/ui-egui/` |

## Recommended first implementation issues

1. Establish the license/reuse decision and create the component/feature compatibility ledger (M0).
2. Add the domain model and validated edit/undo command tests (M1).
3. Implement and test the one-bit combinational scheduler and startup boundary (M2).
4. Add safe `.dig` subset round trips and a headless inspection path (M3).
5. Build the modernized native shell around that tested engine and complete the MVP workflow (M4).

Do not estimate the entire rewrite from the MVP alone. Re-estimate after M0 and after the first sequential/hierarchical compatibility results, when the largest semantic and integration risks have evidence.
