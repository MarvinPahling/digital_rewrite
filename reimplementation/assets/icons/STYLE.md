# Icon system

Original VectorCraft MCP artwork, derived only from action meanings; no Digital artwork or MaterialCraft branding was viewed or traced. `manifest.toml` is the machine-readable inventory consumed by `build.rs`; this file explains the style. Provenance and per-icon briefs are in `../PROVENANCE.md`.

## Sources and build outputs

- Sources are SVG only: `src/<id>.svg` (editable VectorCraft source) and `svg/<id>.svg` (delivery export). Brand equivalents live in `../brand/{src,svg}/`.
- PNG files are build outputs, not sources. `build.rs` rasterizes the delivery SVGs per `manifest.toml`. No PNG is committed (`**/*.png` is ignored). PNGs exported by VectorCraft are only ignored local previews.
- SVG text must be outlined: delivery and source SVGs contain no `<text>` elements.
- Never hand-edit SVG geometry; redraw through VectorCraft.

## Grid and drawing rules

24x24 px grid; 2 px live-area padding; 2 px strokes, round caps and joins, 2 px minimum corner radius. Optically center; simplify at 16 px and avoid fine detail. Monochrome icons use solid #000000 on transparent for UI tinting. Colored icons are limited to test-passed (#16803c disc, white glyph) and test-failed (#c62828 disc, white glyph). Brand art uses its own palette (see `../PROVENANCE.md`).

Document family: folded-corner sheet; new (+), new-sub (chip badge), open (outward arrow), open-new (window badge), save (arrow into tray), save-as (pencil).

Simulation family (unframed): outlined glyphs, no control frame. Run is an outlined play triangle; stop is an outlined rounded square. Badges sit at the bottom right of a play triangle: a check for tests, the shared gate badge for single-gate stepping. Gate step is a step-forward (triangle and bar). Run to break is a double chevron with a break bar; the gate-mode variant is smaller, with a shorter bar and the shared gate badge. The gate badge is a D-shaped AND outline with two input stubs and one output stub, identical in both gate icons, drawn with 1.5 px strokes so it stays open at 24 px.

## Exact VectorCraft recipe

`command_run file.new {width:24,height:24,units:"Pixels",backgroundContents:"transparent"}`. Draw via `draw_path`/`draw_shape` in artboard coordinates; fill none or black; stroke #000000, width 2, round cap/join. Run `render_preview` for every design. Editable source: `save_file` to `src/<id>.svg`; vector delivery: `export format:"svg"` to `svg/<id>.svg`. Optional local preview: `export format:"png"` into the ignored `png/` directory; do not commit it. Probe: the default document is 612x792 pt; create a 24x24 Pixels document explicitly or the export has wrong artboard bounds. SVG save warns that non-artwork document features (effects, symbols, swatches, styles) are not retained; these icons use plain paths.

## Inventory

| id | group | Digital resource | action meaning | metaphor |
|---|---|---|---|---|
| file-new | file | document-new | Create a new circuit document | Folded sheet with a plus |
| file-new-subcircuit | file | document-new-sub | Create a new embedded circuit (New Embedded Circuit) | Folded sheet with a chip badge |
| file-open | file | document-open | Open a circuit | Folded sheet with an outward arrow |
| file-open-new-window | file | document-open-new | Open a circuit in a new window | File-open sheet and outward arrow with a window badge |
| file-save | file | document-save | Save the current circuit | Folded sheet with a downward arrow into a tray |
| file-save-as | file | document-save-as | Save the circuit under another name | Folded sheet with a diagonal pencil |
| edit-copy | edit-view | edit-copy | Copy the selection | Two overlapping rounded sheets |
| edit-undo | edit-view | edit-undo | Undo the last edit | Counter-clockwise curved arrow |
| edit-redo | edit-view | edit-redo | Redo the undone edit | Clockwise curved arrow, mirror of undo |
| edit-delete | edit-view | delete | Delete the selection | Lidded wastebasket with two interior marks |
| view-zoom-in | edit-view | View-zoom-in | Enlarge the view | Magnifier with a plus |
| view-zoom-out | edit-view | View-zoom-out | Reduce the view | Magnifier with a minus |
| view-fit-window | edit-view | View-zoom-fit | Fit the circuit to the window (Fit to window) | Four corner brackets framing the content |
| simulation-start | simulation | media-playback-start | Run the simulation (Run) | Outlined play triangle |
| simulation-start-gate-stepping | simulation | media-playback-start-2 | Start in single-gate stepping mode (Single gate stepping) | Outlined play triangle with the gate badge |
| simulation-run-tests | simulation | media-playback-start-T | Run the circuit tests (Run Tests) | Outlined play triangle with a check badge |
| simulation-stop | simulation | media-playback-stop | Stop the simulation (Stop Simulation) | Outlined rounded square |
| simulation-gate-step | simulation | media-seek-forward | Advance the simulation by one gate step (Gate Step) | Outlined step-forward: triangle and bar |
| simulation-run-to-break-gate-mode | simulation | media-seek-forward-f | Run to a break in single-gate mode (Run To Break in Single Gate Mode) | Small double chevron, short break bar and the gate badge |
| simulation-run-to-break | simulation | media-skip-forward | Run until a break component triggers (Run to Break) | Double chevron with a vertical break bar |
| help | status-tools | help | Show help | Question mark in a circle |
| dialog-error | status-tools | dialog-error | Report an error | Exclamation mark in a circle |
| measurement-graph | status-tools | measurement-graph | Show the measurement graph | Axes with a digital square wave |
| test-passed | status-tools | testPassed | Indicate a passed test | Green disc with a white check |
| test-failed | status-tools | testFailed | Indicate a failed test | Red disc with a white cross |

Brand assets (see `manifest.toml` `[brand]`):

| id | source | build sizes | purpose |
|---|---|---|---|
| app-icon | `../brand/svg/app-icon.svg` | 512, 256, 128, 64, 48 | Window/application icon (icon16..icon128 resource) |
| app-icon-small | `../brand/svg/app-icon-small.svg` | 32, 16 | Small-size master for 16/32 px |
| app-splash | `../brand/svg/app-splash.svg` | 512x320, 1024x640 | Splash screen |

Action meanings were inferred from Java action/icon use and English language resources; Digital resource names record only the function each icon replaces.
