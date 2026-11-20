# Asset provenance

All artwork in `reimplementation/assets/` is original, drawn with `vectorcraft-cli` 0.24.1 through its MCP server (direct tools such as `draw_path`, `draw_shape`, `set_paint`, `render_preview`, `save_file` and `export`) on 2026-11-20. Briefs were written from action meanings only. SVG geometry was never hand-edited, and text in delivery SVGs is outlined.

## Statement

- No Digital or MaterialCraft assets were viewed, traced or copied while drawing. Digital resource names appear in `icons/manifest.toml` (`replaces`) only to record which toolbar or menu function each icon stands in for.
- This is not a clean-room claim. The authors had read access to the reference repositories for other purposes, and this file does not establish that the rewrite is free of derivative-work obligations.
- The license for the rewrite is unresolved; the assets carry no license grant until the repository owner selects one.
- PNG files are generated at build time from `svg/*.svg` and `brand/svg/*.svg` per `icons/manifest.toml`. They are not sources and are not committed. Any PNG exported by VectorCraft is an ignored local preview.

## UI icons (24x24 px, black #000000 unless noted, 2 px round strokes, 2 px padding)

Each id has an editable `icons/src/<id>.svg` and a delivery `icons/svg/<id>.svg`.

| id | brief |
|---|---|
| file-new | Folded-corner sheet with a plus; pilot drawing from the meaning "create a new circuit document". |
| file-new-subcircuit | Folded sheet with a small chip badge, distinguishing an embedded circuit from the plain new sheet; from the meaning "new embedded circuit". |
| file-open | Folded sheet with an outward arrow (redrawn after an earlier open-folder draft that broke the sheet language and the 2 px padding); from the meaning "open a circuit". |
| file-open-new-window | Open-sheet bracket with cut corner, outward arrow at y=8 and a rounded window badge with title-bar line at bottom right; from the brief "open a circuit in a new window". |
| file-save | Folded sheet with a downward arrow into a tray; from the meaning "save the current circuit". |
| file-save-as | Folded sheet with a diagonal pencil, distinguishing save-as from the storage arrow of save; from the meaning "save under another name". |
| edit-copy | Two overlapping rounded sheets (redrawn so the bottom stroke respects the 2 px padding); from the meaning "copy selection". |
| edit-undo | Leftward counter-clockwise curved arrow; from the meaning "undo". |
| edit-redo | Rightward clockwise curved arrow, exact horizontal mirror of undo; from the meaning "redo". |
| edit-delete | Lidded wastebasket with two interior marks; from the meaning "delete selected components". |
| view-zoom-in | Magnifier lens and handle with a plus; from the meaning "enlarge the view". |
| view-zoom-out | Magnifier lens and handle with a minus; from the meaning "reduce the view". |
| view-fit-window | Four corner brackets framing the content, no lens; from the meaning "fit drawing to window". |
| simulation-start | Unframed outlined play triangle `M6 4 L19 12 L6 20 Z`; from the meaning "Run". |
| simulation-start-gate-stepping | Outlined play triangle at upper left (`M4 3 L15 9 L4 15 Z`) plus the shared gate badge at bottom right; from the meaning "single-gate stepping mode". |
| simulation-run-tests | Outlined play triangle at upper left (`M4 3 L15 9.5 L4 16 Z`) plus a 2 px check badge (`M13 17.5 L16 20.5 L21 14.5`); from the meaning "Run Tests". |
| simulation-stop | 14x14 outlined rounded (r2) square at (5,5); from the meaning "Stop Simulation". |
| simulation-gate-step | Outlined step-forward: triangle `M5 5 L15 12 L5 19 Z` plus bar `M19 5 V19`; from the meaning "compute one gate step". |
| simulation-run-to-break-gate-mode | Small double chevron (`M3 3 L8 8 L3 13 M9 3 L14 8 L9 13`), shortened break bar `M19 3 V11` and the shared gate badge; from the meaning "run to break in single-gate mode". |
| simulation-run-to-break | Double chevron (`M4 6 L10 12 L4 18 M10 6 L16 12 L10 18`) plus vertical break bar `M20 5 V19`; from the meaning "run until a BRK component triggers". |
| help | Circle with a curved question mark; from the meaning "show help". |
| dialog-error | Circle with a straight exclamation mark, monochrome for tinting; from the meaning "report an error". |
| measurement-graph | Axes with a digital square wave; from the meaning "show the measurement graph". |
| test-passed | Green (#16803c) disc with a white rounded check; from the meaning "test passed". |
| test-failed | Red (#c62828) disc with a white rounded cross; from the meaning "test failed". |

Unframed simulation scheme: the seven simulation icons have no control frame. The two gate-mode icons share one gate badge with identical coordinates: a D-shaped AND outline `M12.5 14.5 H15 A3.25 3.25 0 0 1 15 21 H12.5 Z` with stubs `M10 16.25 H12.5 M10 19.25 H12.5 M18.25 17.75 H21`. Badge strokes are 1.5 px so the stubs and the D interior stay open at 24 px; all main glyphs use 2 px.

## Brand

- `brand/src|svg/app-icon.svg` (512 px master) and `app-icon-small.svg` (32 px master for 16/32 px): a circuit tile on a navy (#112b46) body with a cyan (#7dd9dd) rim, gold (#f3c96a) terminal dots and a mint (#73e4ce) signal drawn as a square wave. The 512 master adds blue (#78a9c7) circuit traces; the small variant omits them and thickens the wave for legibility. Transparent outer corners and the light rim separate the tile from light and dark backgrounds. Brief: a circuit tile with a signal, no text or existing product branding. The signal was first drawn as a pulse and revised to a square wave.
- `brand/src|svg/app-splash.svg` (512x320): a landscape circuit with a stylized logic gate, branching wires that connect to the gate, gold (#f2c65a) signal nodes and a white square wave on teal (#173c46, #204b55, #376e73) with mint (#61d9bd, #71e1c6) accents. Text (the provisional name `digital-rust`) is converted to outlines; the SVGs contain no `<text>` elements. Revised from a pulse to a square wave and from mid-wire bubbles to connected wires.
