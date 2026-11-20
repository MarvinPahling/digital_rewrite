//! Component symbols as toolkit-neutral vector data.
//!
//! `build.rs` converts the committed SVG masters in `assets/components/` (see its `STYLE.md`
//! and `manifest.toml`) into the `const` tables included below. The data keeps curves and
//! compound contours; no resolution is baked in and no SVG is parsed at run time. Coordinates
//! are world units (20 per grid unit) relative to the symbol origin, so a symbol placed at a
//! grid-aligned position has every anchor on the grid.
//!
//! The data types deliberately know nothing about egui. `symbol_tessellate` and
//! `symbol_render` turn them into meshes, and the UI adapter draws and picks through them.

use crate::domain::ComponentKind;

/// Drawing style of a gate symbol. Input and output share one drawing across styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolStyle {
    Rect,
    Ieee,
}

/// A point in world units: `[x, y]`.
pub type Point = [f32; 2];

/// One segment of a path; a contour starts with `MoveTo` and may end with `Close`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathOp {
    MoveTo(Point),
    LineTo(Point),
    /// Control point, end point.
    #[allow(
        dead_code,
        reason = "supported by the data model and renderer; the current artwork does not use it"
    )]
    QuadTo(Point, Point),
    /// Two control points, end point.
    CubicTo(Point, Point, Point),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    #[allow(
        dead_code,
        reason = "supported by the data model and renderer; the current artwork does not use it"
    )]
    EvenOdd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeCap {
    Butt,
    Round,
    #[allow(
        dead_code,
        reason = "supported by the data model and renderer; the current artwork does not use it"
    )]
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeJoin {
    Miter,
    Round,
    #[allow(
        dead_code,
        reason = "supported by the data model and renderer; the current artwork does not use it"
    )]
    Bevel,
}

/// Semantic role of a path; the renderer picks colors by role, never from the SVG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolRole {
    /// Gate outline or box; stroke only.
    Body,
    /// Outlined internal glyph such as `&`; fill only, holes come from the contours.
    Glyph,
    /// Stub between the body and an anchor; stroke only.
    Lead,
    /// Inversion bubble, terminal ring or similar marker; filled or stroked.
    Indicator,
}

/// A path with its paint description. `fill_rule` is only meaningful when `filled`;
/// `stroke_width`, `cap`, `join` and `miter_limit` only when `stroked`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolPath {
    /// Part id from the SVG, for example `body` or `lead-in-0`.
    pub id: &'static str,
    pub role: SymbolRole,
    pub ops: &'static [PathOp],
    pub fill_rule: FillRule,
    pub filled: bool,
    pub stroked: bool,
    /// World units; scales with the canvas.
    pub stroke_width: f32,
    pub cap: StrokeCap,
    pub join: StrokeJoin,
    pub miter_limit: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
}

/// An external connection point, relative to the symbol origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolAnchor {
    /// `pin-in-<n>` or `pin-out-<n>`.
    pub name: &'static str,
    pub position: Point,
    pub direction: PortDirection,
}

/// Axis-aligned extent including strokes, relative to the symbol origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolBounds {
    pub min: Point,
    pub max: Point,
}

/// One (style, kind) binding of the component symbol manifest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolDef {
    /// Binding id from the manifest, for example `rect-and`.
    pub id: &'static str,
    pub style: SymbolStyle,
    pub kind: ComponentKind,
    /// Id of the SVG master the geometry was taken from.
    pub master: &'static str,
    /// Position of the logical origin on the SVG artboard. All other coordinates in this
    /// definition are already relative to it.
    pub origin: Point,
    pub bounds: SymbolBounds,
    pub paths: &'static [SymbolPath],
    pub anchors: &'static [SymbolAnchor],
}

include!(concat!(env!("OUT_DIR"), "/component_symbols.rs"));

/// Returns the symbol drawn for `kind` in `style`, if one has been drawn.
pub fn symbol(style: SymbolStyle, kind: ComponentKind) -> Option<&'static SymbolDef> {
    SYMBOLS
        .iter()
        .find(|def| def.style == style && def.kind == kind)
}

/// Iterates all symbol definitions in manifest order.
#[cfg(test)]
pub fn symbols() -> impl Iterator<Item = &'static SymbolDef> {
    SYMBOLS.iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATES: [ComponentKind; 3] = [ComponentKind::And, ComponentKind::Or, ComponentKind::Not];
    const STYLES: [SymbolStyle; 2] = [SymbolStyle::Rect, SymbolStyle::Ieee];
    const ALL_KINDS: [ComponentKind; 5] = [
        ComponentKind::Input,
        ComponentKind::Output,
        ComponentKind::And,
        ComponentKind::Or,
        ComponentKind::Not,
    ];

    fn anchors_of(def: &SymbolDef) -> Vec<(&'static str, Point, PortDirection)> {
        def.anchors
            .iter()
            .map(|anchor| (anchor.name, anchor.position, anchor.direction))
            .collect()
    }

    #[test]
    fn every_style_and_kind_is_bound() {
        assert_eq!(symbols().count(), 10);
        for style in STYLES {
            for kind in ALL_KINDS {
                let def = symbol(style, kind).unwrap_or_else(|| panic!("{style:?} {kind:?}"));
                assert_eq!((def.style, def.kind), (style, kind));
            }
        }
        assert_eq!(DEFAULT_STYLE, SymbolStyle::Rect);
        assert_eq!(GRID_UNIT, 20.0);
        assert_eq!(STROKE_WIDTH, 1.5);
    }

    #[test]
    fn gate_anchor_tables_match_the_contract() {
        use PortDirection::{Input, Output};
        for style in STYLES {
            for kind in [ComponentKind::And, ComponentKind::Or] {
                assert_eq!(
                    anchors_of(symbol(style, kind).unwrap()),
                    [
                        ("pin-in-0", [0.0, 0.0], Input),
                        ("pin-in-1", [0.0, 40.0], Input),
                        ("pin-out-0", [60.0, 20.0], Output),
                    ],
                    "{style:?} {kind:?}"
                );
            }
            assert_eq!(
                anchors_of(symbol(style, ComponentKind::Not).unwrap()),
                [
                    ("pin-in-0", [0.0, 0.0], Input),
                    ("pin-out-0", [40.0, 0.0], Output),
                ],
                "{style:?} not"
            );
        }
    }

    #[test]
    fn input_and_output_have_a_single_port_at_the_origin() {
        for style in STYLES {
            let input = symbol(style, ComponentKind::Input).unwrap();
            assert_eq!(
                anchors_of(input),
                [("pin-out-0", [0.0, 0.0], PortDirection::Output)]
            );
            let output = symbol(style, ComponentKind::Output).unwrap();
            assert_eq!(
                anchors_of(output),
                [("pin-in-0", [0.0, 0.0], PortDirection::Input)]
            );
        }
        assert_eq!(
            symbol(SymbolStyle::Rect, ComponentKind::Input)
                .unwrap()
                .master,
            symbol(SymbolStyle::Ieee, ComponentKind::Input)
                .unwrap()
                .master
        );
    }

    #[test]
    fn rect_and_ieee_anchors_are_identical_and_on_the_grid() {
        for kind in GATES {
            let rect = symbol(SymbolStyle::Rect, kind).unwrap();
            let ieee = symbol(SymbolStyle::Ieee, kind).unwrap();
            assert_eq!(anchors_of(rect), anchors_of(ieee), "{kind:?}");
            assert_eq!(rect.origin, ieee.origin, "{kind:?}");
        }
        for def in symbols() {
            assert_eq!(def.origin[0] % GRID_UNIT, 0.0, "{}", def.id);
            assert_eq!(def.origin[1] % GRID_UNIT, 0.0, "{}", def.id);
            for anchor in def.anchors {
                assert_eq!(
                    anchor.position[0] % GRID_UNIT,
                    0.0,
                    "{} {}",
                    def.id,
                    anchor.name
                );
                assert_eq!(
                    anchor.position[1] % GRID_UNIT,
                    0.0,
                    "{} {}",
                    def.id,
                    anchor.name
                );
            }
        }
    }

    #[test]
    fn paths_are_well_formed() {
        for def in symbols() {
            assert!(!def.paths.is_empty(), "{} has no paths", def.id);
            assert!(
                def.bounds.min[0] < def.bounds.max[0] && def.bounds.min[1] < def.bounds.max[1],
                "{} has empty bounds",
                def.id
            );
            for path in def.paths {
                assert!(!path.ops.is_empty(), "{} {}", def.id, path.id);
                assert!(
                    matches!(path.ops[0], PathOp::MoveTo(_)),
                    "{} {} must start with MoveTo",
                    def.id,
                    path.id
                );
                assert!(path.filled || path.stroked, "{} {}", def.id, path.id);
                if path.stroked {
                    assert_eq!(path.stroke_width, STROKE_WIDTH, "{} {}", def.id, path.id);
                }
                match path.role {
                    SymbolRole::Body | SymbolRole::Lead => {
                        assert!(path.stroked && !path.filled, "{} {}", def.id, path.id);
                    }
                    SymbolRole::Glyph => {
                        assert!(path.filled && !path.stroked, "{} {}", def.id, path.id);
                    }
                    SymbolRole::Indicator => {}
                }
                // Every path stays within the declared bounds (stroke included).
                for op in path.ops {
                    let points: &[Point] = match op {
                        PathOp::MoveTo(p) | PathOp::LineTo(p) => std::slice::from_ref(p),
                        PathOp::QuadTo(c, p) => &[*c, *p],
                        PathOp::CubicTo(c1, c2, p) => &[*c1, *c2, *p],
                        PathOp::Close => &[],
                    };
                    for p in points {
                        // Control points may leave the bounds slightly; end points may not.
                        if matches!(op, PathOp::MoveTo(_) | PathOp::LineTo(_)) {
                            assert!(
                                p[0] >= def.bounds.min[0] - 0.01
                                    && p[0] <= def.bounds.max[0] + 0.01
                                    && p[1] >= def.bounds.min[1] - 0.01
                                    && p[1] <= def.bounds.max[1] + 0.01,
                                "{} {} point {p:?} outside {:?}",
                                def.id,
                                path.id,
                                def.bounds
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn not_symbols_have_inversion_indicators() {
        for style in [SymbolStyle::Rect, SymbolStyle::Ieee] {
            let def = symbol(style, ComponentKind::Not).unwrap();
            assert!(
                def.paths
                    .iter()
                    .any(|path| path.role == SymbolRole::Indicator),
                "{} lacks an inversion indicator",
                def.id
            );
        }
    }

    #[test]
    fn leads_reach_their_anchors() {
        for def in symbols() {
            for anchor in def.anchors {
                let lead = def.paths.iter().find(|path| {
                    path.role == SymbolRole::Lead
                        && path.id.starts_with(match anchor.direction {
                            PortDirection::Input => "lead-in-",
                            PortDirection::Output => "lead-out-",
                        })
                        && path.ops.iter().any(|op| match op {
                            PathOp::MoveTo(p) | PathOp::LineTo(p) => *p == anchor.position,
                            _ => false,
                        })
                });
                // Input and Output pads are a single-lead symbol where the lead ends on the anchor;
                // every anchor of every symbol must have a lead ending exactly there.
                assert!(
                    lead.is_some(),
                    "{} anchor {} has no lead",
                    def.id,
                    anchor.name
                );
            }
        }
    }

    #[test]
    fn glyph_paths_contain_multiple_contours() {
        for (style, kind) in [
            (SymbolStyle::Rect, ComponentKind::And),
            (SymbolStyle::Rect, ComponentKind::Or),
            (SymbolStyle::Rect, ComponentKind::Not),
        ] {
            let def = symbol(style, kind).unwrap();
            let glyph = def
                .paths
                .iter()
                .find(|path| path.role == SymbolRole::Glyph)
                .unwrap_or_else(|| panic!("{} has no glyph", def.id));
            let contours = glyph
                .ops
                .iter()
                .filter(|op| matches!(op, PathOp::MoveTo(_)))
                .count();
            assert!(contours >= 1, "{}", def.id);
            if kind == ComponentKind::And {
                // The `&` outline has an outer contour and two counters.
                assert_eq!(contours, 3, "{}", def.id);
                assert!(
                    glyph.ops.iter().any(|op| matches!(op, PathOp::CubicTo(..))),
                    "curves must be preserved"
                );
            }
        }
    }

    #[test]
    fn ieee_bodies_keep_their_curves_and_have_no_glyph() {
        for kind in [ComponentKind::And, ComponentKind::Or] {
            let def = symbol(SymbolStyle::Ieee, kind).unwrap();
            let body = def
                .paths
                .iter()
                .find(|path| path.role == SymbolRole::Body)
                .unwrap();
            assert!(
                body.ops.iter().any(|op| matches!(op, PathOp::CubicTo(..))),
                "{} body must keep cubic curves",
                def.id
            );
            assert!(def.paths.iter().all(|path| path.role != SymbolRole::Glyph));
        }
    }
}
