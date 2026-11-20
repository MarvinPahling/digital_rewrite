# Component symbol system

Original VectorCraft MCP artwork for the circuit canvas. Only the function of each symbol (AND, OR, NOT, input, output) was used as the brief; no Digital or MaterialCraft artwork, paths or glyph outlines were viewed, traced or copied. Digital source was read only for pin-grid semantics. This is the contract for the drawing lanes and for `build.rs`. `manifest.toml` is the machine-readable inventory; provenance is recorded at the end of this file.

Decision record: build-time SVG normalization into generated vector data, rendered with egui meshes (resvg stays build-only). SVGs here are therefore a *source format*, not something loaded at runtime.

## Files

```text
assets/components/
  STYLE.md  manifest.toml
  src/<master>.svg   editable VectorCraft source (save_file)
  svg/<master>.svg   delivery export (export format:"svg")
  png/               ignored local previews only (never committed)
```

- Masters (eight): `rect-and`, `rect-or`, `rect-not`, `ieee-and`, `ieee-or`, `ieee-not`, `shared-input`, `shared-output`. Name pattern `<style>-<kind>.svg`, styles `rect`, `ieee`, `shared`.
- Bindings (ten): the six gate masters plus `rect-input`, `ieee-input` (both use `shared-input`) and `rect-output`, `ieee-output` (both use `shared-output`). Runtime selects a binding from (style, kind); `manifest.toml` says which master and which origin/anchors it uses.
- `src/` and `svg/` are byte-identical for these assets (VectorCraft writes the same SVG for save and export). Both are committed; PNG is never committed (`**/*.png` is ignored).
- No `<text>`, fonts, images, filters, gradients, masks, clip paths, transforms, `non-scaling-stroke` or external references in any SVG.
- Never hand-edit SVG geometry; redraw through VectorCraft.

## Artboard, grid and units

- 1 grid unit = 20 world units = 20 egui points at canvas zoom 1.
- Every master has a 6x6-grid-unit artboard: `file.new {width:120, height:120, units:"Pixels", backgroundContents:"transparent"}`, exported with `width="120" height="120" viewBox="0 0 120 120"`. Artboard px = world units. Padding is never connectivity data.
- Wider future variants (many inputs, flip-flops) may use a larger artboard but must keep the 20-unit grid and record it in the manifest.
- Ink is solid `#000000`, no background. Runtime recolors by role, so SVG RGB is never used for theming.
- Stroke 1.5 world units on all line art, round caps and round joins (set `cap:"round", join:"round"` on every `draw_shape`/`draw_path`; a `draw_shape` line otherwise exports with the default miter join). Strokes scale with zoom; no hairlines.
- Filled shapes (outlined glyphs, bubbles) have fill `#000000` and stroke none.

## Origin and anchors

The manifest records the logical origin; never infer it from artwork bounds. Anchors are in artboard coordinates in the SVG (markers) and relative to the origin in the manifest.

| Kind | Origin (artboard) | Anchor ids | Artboard coords | Relative to origin |
|---|---|---|---|---|
| AND / OR | (20,40) | `pin-in-0`, `pin-in-1`, `pin-out-0` | (20,40), (20,80), (80,60) | (0,0), (0,40), (60,20) |
| NOT | (20,60) | `pin-in-0`, `pin-out-0` | (20,60), (60,60) | (0,0), (40,0) |
| Input | (60,60) | `pin-out-0` | (60,60) | (0,0) |
| Output | (60,60) | `pin-in-0` | (60,60) | (0,0) |

Rectangular and IEEE variants of a kind have identical origin and anchors. Leads must reach their anchor point exactly (a lead ends at the anchor center; round caps overshoot by half the stroke width, which is acceptable).

## Anchor marker convention (verified)

Anchors are small circles in a group, extracted by `build.rs` from the raw XML **before** usvg and then stripped from the artwork.

```xml
<g id="anchors" opacity="0">
  <circle id="pin-in-0" cx="20" cy="40" r="1" fill="#ff00ff"/>
  <circle id="pin-in-1" cx="20" cy="80" r="1" fill="#ff00ff"/>
  <circle id="pin-out-0" cx="80" cy="60" r="1" fill="#ff00ff"/>
</g>
```

- The anchor position is the circle center `cx, cy`, in root coordinates. Markers are plain circles (radius 1, fill `#ff00ff`, no stroke, no transform). Extraction rule for `build.rs`: find `<g id="anchors">`, read each child `<circle id=… cx=… cy=…>`, then remove the whole group before usvg.
- Marker names are `pin-in-<n>` (input port n, counted top to bottom) and `pin-out-<n>`. Optional later guides use the `join-` prefix (for example `join-in-0`, a non-grid body attachment point). Anything else in `anchors` is a build error.
- `opacity="0"` on the group makes the markers invisible in any viewer or a naive renderer, so a forgotten strip cannot show magenta dots. The `#ff00ff` fill is kept so a marker that loses its opacity is obviously wrong.

### What VectorCraft 0.24.1 does on export (probed)

- Object and group names become SVG ids verbatim (`name:"body"` -> `<rect id="body">`; a group named `glyph` -> `<g id="glyph">`). Unnamed objects get no id. The layer becomes `<g id="Layer_1" data-name="Layer 1">`. The document title becomes `<title>` from the file stem of the last save/export path.
- A `draw_path` named `body` exports as `<path id="body" d="M… L… C… Z">`; a rectangle shape as `<rect id=…>`; a line as `<line id=…>`; an ellipse as `<circle id=… cx cy r>` when width=height.
- **Hidden objects do not survive `export format:"svg"`** (they are dropped). They are kept only by `save_file` as `display="none"`. So a hidden marker group would be lost from the delivery SVG; do not use `object.hide`.
- **Zero-opacity objects survive export** (`opacity="0"` on the element, including a whole group). That is why the markers use a group opacity of 0.
- The group's children keep their own ids.

## Part ids and roles

Runtime colors by role, derived from the element id prefix (the artwork's RGB is ignored):

| Role | Id prefix | Used for | Notes |
|---|---|---|---|
| body | `body` (`body`, `body-0`…) | gate body outline, box | stroke only, fill none |
| glyph | `glyph` (`glyph`, `glyph-0`…) | internal `&`, `≥1`, `1` | filled outlined compound path; holes through even-odd/nonzero winding |
| lead | `lead-in-<n>`, `lead-out-<n>` | stubs from the body to the anchors | stroke only |
| indicator | `indicator` (`indicator-0`…) | inversion bubble, clock marker, other markers | filled or stroked; bubbles are reusable parts |

`anchors` and `pin-*`/`join-*` are not artwork. Unknown prefixes are a build error, so no part is silently drawn in the wrong color. Fill/stroke attributes tell the builder whether a part is filled or stroked; the colors do not matter.

Prototype ids produced by VectorCraft for `rect-and`: `Layer_1`, `body` (rect), `lead-in-0`, `lead-in-1`, `lead-out-0` (lines), `glyph` (group containing one compound `<path>` with three subpaths), `anchors` > `pin-in-0`, `pin-in-1`, `pin-out-0`. For `ieee-and`: `Layer_1`, `body` (path with a cubic curve), the same three leads, the same `anchors`. The inner `<path>` of `glyph` has no id; the group id is the part id (nested elements inherit the role of the nearest ancestor with a part id).

## Outlined text

- Glyphs (`&`, `≥1`, `1`) are produced with `add_text` and then `select.set {ids:[text id]}` followed by `type.createOutlines {}` (the command takes no parameters; it works on the selection and returns a group id). The result is a group holding a compound path with its holes, which exports as a single `<path>` with no fill-rule attribute (nonzero) whose hole contours have opposite winding. The render of `rect-and` confirmed the `&` holes are open.
- Font: `Liberation Sans Bold` 2.1.5 (SIL OFL 1.1; installed at `~/Library/Fonts`), size 28 pt, black fill. Use this same font and weight for every glyph so they match. Do not use system or commercial fonts (Helvetica, SF, Arial); outlines made from them inherit their license. A first probe with Helvetica was discarded for that reason.
- After outlining, recenter by reading the group bounds from `doc_inspect {ids:[group], depth:1}` and moving with `transform`. For the `&` in `rect-and` the offset was dx -2.29, dy -0.45 so the glyph is centered on the box center (55,60). Rename the group `glyph`.
- Labels, pin names and changing numeric values are runtime text outside the artwork.

## Rectangular style (default)

- Explicit rectangular is the default style; IEEE is a runtime option. Same anchors in both.
- Box 30 wide x 60 tall (x 40..70, y 30..90), 1.5 stroke, round joins. Input leads (20,40)->(40,40), (20,80)->(40,80); output lead (70,60)->(80,60). Glyph centered at (55,60).
- Rectangular NOT: Digital's narrow form omits the `1`; the drawing lane decides between a narrow box with no glyph and a glyph, and records the choice in the NOT master. Inversion bubbles are `indicator` parts.
- Rect OR uses `≥1`.

## IEEE style

- Original curved body, no internal glyph. `ieee-and`: left edge x=40 from y=30 to 90, flat top and bottom to x=55, elliptical right end to (70,60) (cubics with control offset 0.5523 x radius, rx 15, ry 30). Leads are identical to `rect-and`; input leads meet the straight left edge at y=40 and y=80.
- OR/NOT in IEEE keep the same origin and anchors. IEEE OR needs a concave back edge: the input leads must still start at x=20 and end on the curved back edge at the same y values, so the lead length differs; the anchors do not.

## Variable inputs and dynamic composition

A fixed two-input asset is the first supported instance, not the layout model.

- Rectangular boxes: extend by moving the top/bottom caps and keep the glyph size and stroke. Leads and anchors come from the shared port-layout recipe (inputs on the 20-unit grid with the center slot left open for an even count; output midway between the first and last input).
- IEEE: fixed curved body plus generated input-bank extension of the straight back edge. Do not stretch the curve.
- Inversion bubbles are `indicator` parts. A per-input inversion moves that external anchor outward by one grid unit; output inversion reserves an extra cell.
- Until a layout recipe exists, other input counts are rejected with an explicit diagnostic. Never emit a PNG per variant.
- Flip-flops and muxes add semantic port roles, clock markers and layout parameters later; they get their own masters.

## Exact VectorCraft recipe (per master)

1. `command_run file.new {width:120, height:120, units:"Pixels", backgroundContents:"transparent"}`.
2. Draw body, leads with `draw_shape`/`draw_path`: `fill:"none"` (or `#000000` for filled parts), `stroke:"#000000"`, `strokeWidth:1.5`, `cap:"round"`, `join:"round"`, and `name` set to the part id (`body`, `lead-in-0`, `lead-in-1`, `lead-out-0`, `indicator-0`, …). Example IEEE body: `draw_path d:"M40 30 H55 C63.28 30 70 43.43 70 60 C70 76.57 63.28 90 55 90 H40 Z"`.
3. Glyph (rectangular only): `add_text {text:"&", x:47, y:70, size:28, font:"Liberation Sans Bold", color:"#000000"}` -> `select.set {ids:[id]}` -> `type.createOutlines {}` -> `doc_inspect {ids:[group id], depth:1}` -> `transform {ids:[group], dx, dy}` to center on the box center -> `command_run object.setProps {ids:[group], name:"glyph"}`.
4. Anchors: for each anchor, `draw_shape {shape:"ellipse", x:cx-1, y:cy-1, width:2, height:2, fill:"#ff00ff", stroke:"none", name:"pin-in-0"}`; `select.set {ids:[markers]}`; `object.group {}` (returns the group id); `command_run object.setProps {ids:[group], name:"anchors", opacity:0}`.
5. `render_preview` and check; `inspect_document` for geometry.
6. `save_file {path:".../src/<master>.svg"}`; `export {format:"svg", path:".../svg/<master>.svg"}`. Optional preview: `export {format:"png", scale:6, path:".../png/<master>.png"}` (ignored). Open the previews with the read tool to check holes and curves.
7. Read the exported SVG text: ids present, `anchors` group with `opacity="0"`, three markers at the exact anchor coordinates, no `<text>`.

Notes: VectorCraft writes each file asynchronously, so wait about a second before reading an exported file from the shell. The title is the file stem of the last save/export, so save the `src` file first, then export, to get the right `<title>`.

Drawing-lane checklist: names exact; anchors at the table coordinates; stroke 1.5 round; no hidden objects; no text; gate masters of the same kind share origin and anchors; re-read the SVG after export.

## Provenance

- Drawn with `vectorcraft-cli` 0.24.1 through its MCP server on 2026-11-20: `draw_shape`, `draw_path`, `add_text`, `type.createOutlines`, `object.group`, `object.setProps`, `transform`, `save_file`, `export`. Briefs: "rectangular AND box with outlined & glyph and pin leads" and "original curved AND body with leads", on the contract above.
- Drawn so far: `rect-and`, `ieee-and`. The other masters are pending.
- The `&` is Liberation Sans Bold outlined through VectorCraft (SIL OFL 1.1, permits embedding outlines). IEEE body and rectangular box are plain geometric primitives drawn from numbers, not from any reference outline.
- This is not a clean-room claim: the authors had read access to the reference repositories; Digital source was consulted only for pin-grid semantics (20-unit pitch, 40-unit input separation, output midway). The rewrite's license is unresolved and the assets carry no license grant until the owner decides.
