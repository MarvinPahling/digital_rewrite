# Component symbol provenance

All eight component symbol masters are original artwork, drawn with `vectorcraft-cli` 0.24.1 through its MCP server (direct tools: `draw_shape`, `draw_path`, `add_text`, `select.set`, `type.createOutlines`, `transform`, `object.group`, `object.setProps`, `save_file`, `export`) on 2026-11-20. Each master has an editable `src/<master>.svg` and a byte-identical delivery `svg/<master>.svg`. The contract is `STYLE.md`; the inventory is `manifest.toml`.

## Statement

- No Digital or MaterialCraft images, paths or glyph outlines were viewed, traced or copied. Digital source was read only for pin-grid semantics (20-unit pitch, 40-unit input separation, output midway between the inputs).
- SVG geometry was never hand-edited. Glyphs (`&`, `≥1`, `1`) are text outlined through VectorCraft (`type.createOutlines`) from Liberation Sans Bold 2.1.5 (SIL OFL 1.1). No `<text>`, fonts, images, filters, gradients or masks are present.
- This is not a clean-room claim: the authors had read access to the reference repositories, and this file does not establish that the rewrite is free of derivative-work obligations.
- The rewrite's license is unresolved; the assets carry no license grant until the repository owner selects one.
- PNG files are never committed. Any PNG under `png/` is an ignored local preview, and build outputs are generated into `OUT_DIR`.

## Common brief

120x120 artboard (6x6 grid units, 20 world units per unit), black `#000000` ink, stroke 1.5 with round caps and joins, transparent background. Pin anchors are `<circle id="pin-…" r="1" fill="#ff00ff">` markers in `<g id="anchors" opacity="0">`. Rectangular and IEEE variants of a kind share the same anchors.

## Per-asset briefs

| master | brief |
|---|---|
| `rect-and` | Rectangular AND box x40..70, y30..90 with leads (20,40)->(40,40), (20,80)->(40,80), (70,60)->(80,60) and an outlined `&` (Bold 28 pt) centred on (55,60). |
| `rect-or` | Same box and leads as `rect-and` with an outlined `≥1` (Bold 20 pt, since 28 pt is wider than the 30-unit box) centred on (55,60). |
| `rect-not` | Narrow rectangular NOT box x30..50, y40..80 with leads (20,60)->(30,60) and (50,60)->(60,60) and an outlined `1` (Bold 20 pt) centred on (40,60). Decision: this master carries the glyph; STYLE.md left the choice to the drawing lane, and the box proportions are the drawing lane's choice. |
| `ieee-and` | Original curved AND body: straight back edge at x=40, flat top and bottom to x=55, elliptical right end to (70,60); same leads and anchors as `rect-and`; no glyph. |
| `ieee-or` | Original curved OR body `M40 30 C56 30 66 44 70 60 C66 76 56 90 40 90 C49 75 49 45 40 30 Z` with a concave back edge; input leads end on that edge at x≈44.15 (y=40 and 80), output lead (70,60)->(80,60); no glyph. |
| `ieee-not` | Original triangle (26,40), (26,80), (48,60) with an open inversion-bubble ring (`indicator-0`, centre (52,60), r=4); leads (20,60)->(26,60) and (56,60)->(60,60); no glyph. Triangle and bubble size are the drawing lane's choice. |
| `shared-input` | Small square pad (x24..44, y50..70) with a lead (44,60)->(60,60) to the sole output anchor at (60,60). Used by both styles. |
| `shared-output` | Lead (60,60)->(76,60) from the sole input anchor to a ring terminal (centre (86,60), r=10). Used by both styles. |

## Notes for the build pipeline

- VectorCraft exports some outlined or straight-sided shapes as `<polygon>` (the `1` glyphs and the `ieee-not` body) and as `<path>` otherwise; the builder must accept both. Circles export as `<circle>`.
- Element order differs between masters (for example `glyph` may precede `anchors`); extraction is by id.
- `rect-and` leads carry `stroke-miterlimit` where later masters carry `stroke-linejoin="round"`; both render identically for these straight, unjoined lines.
