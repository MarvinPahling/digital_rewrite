# Icon system

Original VectorCraft MCP artwork, derived only from action meanings; no Digital artwork or MaterialCraft branding viewed or traced. Monochrome action icons use solid #000000 on transparent for UI tinting. Status/error and brand may use a small documented high-contrast accent palette (green pass, red fail/error, test contrast on light/dark backgrounds).

## Grid and drawing rules
24×24 px grid; 2 px live-area padding; 2 px strokes, round caps/joins, 2 px minimum corner radius. Optically center; simplify at 16 px, avoid fine detail. Document family: folded-corner sheet; differentiate new (+), open (outward arrow), save (inward/storage glyph), save-as (pencil/variant), new-sub (hierarchy badge), open-new (window badge). Playback family: play, pause/stop distinct; step variants one arrow, finish/fast-forward paired chevrons; test/micro variants use distinct contextual badges.

## Exact VectorCraft recipe
`command_run file.new {width:24,height:24,units:"Pixels",backgroundContents:"transparent"}`. Draw via `draw_path`/`draw_shape` in artboard coordinates; fill none or black; stroke #000000, width 2, round cap/join. Run `render_preview` for every design. Editable source: `save_file` to `src/<id>.svg`; vector delivery: `export format:"svg"`; raster deliveries: `export format:"png",scale:1` and `scale:2` to `png/<id>@1x.png`, `@2x.png`. Open each PNG with image reader and verify. Probe: default document is 612×792 pt; explicitly create 24×24 Pixels document or export has wrong artboard bounds. SVG save warns that non-artwork document features (effects, symbols, swatches, styles) are not retained; these icons use plain paths.

## Pilot inventory
| id | group | Digital resource | action meaning | metaphor brief |
|---|---|---|---|---|
| file-new | file | document-new.png | Create a new circuit document | Folded sheet plus |
| simulation-start | simulation | media-playback-start.png | Run continuous simulation | Play triangle in framed control |
| zoom-fit | edit-view | View-zoom-fit.png | Fit drawing to view | Magnifier with inward corners |
| zoom-in | edit-view | View-zoom-in.png | Enlarge view | Magnifier plus |
| zoom-out | edit-view | View-zoom-out.png | Reduce view | Magnifier minus |
| delete | edit-view | delete.png | Remove selected component | Wastebasket |
| dialog-error | status-tools | dialog-error.png | Report an error | Alert mark in circle |
| document-new-sub | file | document-new-sub.png | Create sub-circuit | Sheet plus hierarchy node |
| document-open | file | document-open.png | Open a circuit | Sheet with open-folder arrow |
| document-open-new | file | document-open-new.png | Open in new window | Sheet with window badge |
| document-save | file | document-save.png | Save current circuit | Sheet/storage mark |
| document-save-as | file | document-save-as.png | Save under another name | Sheet with pencil |
| edit-copy | edit-view | edit-copy.png | Copy selection | Overlapping sheets |
| edit-redo | edit-view | edit-redo.png | Redo operation | Forward curved arrow |
| edit-undo | edit-view | edit-undo.png | Undo operation | Backward curved arrow |
| help | status-tools | help.png | Show help | Question mark in circle |
| measurement-graph | status-tools | measurement-graph.png | Show measurement graph | Axes with plotted curve |
| simulation-start-micro | simulation | media-playback-start-2.png | Micro-step/run variant | Play with micro-step badge |
| simulation-start-test | simulation | media-playback-start-T.png | Run circuit test | Play with test badge |
| simulation-stop | simulation | media-playback-stop.png | Stop simulation | Stop square |
| simulation-step | simulation | media-seek-forward.png | Advance one simulation step | Single forward arrow |
| simulation-step-finish | simulation | media-seek-forward-f.png | Step until completion | Arrow with finish marker |
| simulation-fast-forward | simulation | media-skip-forward.png | Advance quickly | Paired forward chevrons |
| test-failed | status-tools | testFailed.png | Indicate test failure | Red cross |
| test-passed | status-tools | testPassed.png | Indicate test success | Green check |
| app-icon | brand | icon16.png–icon128.png | Identify application | Original circuit-themed mark; brand lane |
| app-splash | brand | splash resource | Identify/loading screen | Original circuit-themed composition; brand lane |

Action meanings inferred from Java action/icon use and English language resources. No brand art drawn in this pilot. Provenance: original semantic drawings made with VectorCraft MCP, 2026-11-20.
