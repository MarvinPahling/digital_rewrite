//! Drawing of component symbols: resolved layout, palette and cached mesh geometry.
//!
//! The seam to the rest of the program is deliberately small:
//!
//! - [`SymbolLayout`] is a symbol placed in world units. Drawing, hit testing and (later) wire
//!   endpoints all read the same placement, so pin anchors never come from painted pixels.
//! - [`Palette`] maps semantic roles ([`PaintState`]) to colors; artwork carries no colors.
//!   Signal channels (low, high, high-Z) are reserved: they will recolor lead and indicator
//!   parts without changing any geometry.
//! - [`SymbolRenderer`] turns a layout into one egui [`Mesh`]. Geometry is tessellated once per
//!   (style, kind, scale bucket) by [`crate::symbol_tessellate`] and reused.
//!
//! ## Scale buckets
//!
//! Tessellation depends on the physical scale (`zoom * pixels_per_point`) through the curve
//! error bound and the one-pixel anti-aliasing fringe. Scales are grouped into quarter-octave
//! buckets (ratio about 1.19) and geometry is built for the bucket's upper bound, so drawn
//! curves stay within the error bound and the fringe is at most one pixel wide and no narrower
//! than about 0.84 px inside a bucket. The cache is bounded; it is cleared when it overflows.
use crate::domain::{ComponentKind, PinDirection, PinId};
use crate::{
    canvas::CanvasTransform,
    symbol_tessellate::{PathLayer, TessellateError, tessellate_path},
    symbols::{
        Point, PortDirection, SymbolBounds, SymbolDef, SymbolStyle, origin_from_center, symbol,
    },
};
use eframe::egui::{
    Color32, Mesh, Painter, Shape, Visuals,
    epaint::{Vertex, WHITE_UV},
};
use std::{collections::HashMap, sync::Arc};

/// Semantic color role of a whole symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintState {
    Ink,
    Selection,
    #[allow(
        dead_code,
        reason = "no error source exists in the editor yet; used by tests"
    )]
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub ink: Color32,
    pub selection: Color32,
    pub error: Color32,
}

impl Palette {
    pub fn from_visuals(visuals: &Visuals) -> Self {
        Self {
            ink: visuals.text_color(),
            selection: visuals.selection.stroke.color,
            error: visuals.error_fg_color,
        }
    }

    pub fn color(&self, state: PaintState) -> Color32 {
        match state {
            PaintState::Ink => self.ink,
            PaintState::Selection => self.selection,
            PaintState::Error => self.error,
        }
    }
}

/// An anchor in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedAnchor {
    pub name: &'static str,
    pub position: Point,
    pub direction: PortDirection,
}

/// A symbol placed in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolLayout {
    def: &'static SymbolDef,
    /// World position of the symbol's logical origin.
    origin: Point,
}

impl SymbolLayout {
    /// Places the symbol so that the center of its bounds is at `center`. This is how the
    /// editor interprets a component position, preserving the earlier meaning of the position
    /// as the center of the drawn component. Returns `None` if no symbol is drawn for the pair.
    pub fn centered_on(style: SymbolStyle, kind: ComponentKind, center: Point) -> Option<Self> {
        let def = symbol(style, kind)?;
        Some(Self {
            def,
            origin: origin_from_center(style, kind, center)?,
        })
    }

    #[cfg(test)]
    pub fn origin(&self) -> Point {
        self.origin
    }

    /// External connection points in world coordinates.
    #[allow(
        dead_code,
        reason = "wiring is not implemented yet; exercised by tests"
    )]
    pub fn anchors(&self) -> impl Iterator<Item = PlacedAnchor> + '_ {
        self.def.anchors.iter().map(|a| PlacedAnchor {
            name: a.name,
            position: [
                self.origin[0] + a.position[0],
                self.origin[1] + a.position[1],
            ],
            direction: a.direction,
        })
    }

    /// Resolve a typed document pin to its artwork anchor.
    #[allow(dead_code)]
    pub fn pin_position(&self, pin: PinId) -> Option<Point> {
        let name = match (pin.direction, pin.index) {
            (PinDirection::Input, 0) => "pin-in-0",
            (PinDirection::Input, 1) => "pin-in-1",
            (PinDirection::Output, 0) => "pin-out-0",
            _ => return None,
        };
        self.anchors()
            .find(|anchor| anchor.name == name)
            .map(|anchor| anchor.position)
    }

    /// Extent of the drawn symbol including strokes, in world units.
    pub fn bounds(&self) -> SymbolBounds {
        let b = self.def.bounds;
        SymbolBounds {
            min: [self.origin[0] + b.min[0], self.origin[1] + b.min[1]],
            max: [self.origin[0] + b.max[0], self.origin[1] + b.max[1]],
        }
    }

    /// Whether `world` lies within the symbol bounds grown by `tolerance` world units.
    pub fn hit(&self, world: Point, tolerance: f32) -> bool {
        let b = self.bounds();
        (b.min[0] - tolerance..=b.max[0] + tolerance).contains(&world[0])
            && (b.min[1] - tolerance..=b.max[1] + tolerance).contains(&world[1])
    }
}

/// Tessellated layers of one symbol for one scale bucket, relative to the symbol origin.
#[derive(Debug, PartialEq)]
pub struct SymbolGeometry {
    pub layers: Vec<PathLayer>,
}

#[derive(Debug)]
pub enum RenderError {
    Tessellate(&'static str, TessellateError),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tessellate(id, error) => write!(f, "cannot draw symbol {id}: {error}"),
        }
    }
}

impl std::error::Error for RenderError {}

const BUCKETS_PER_OCTAVE: f32 = 4.0;
const MIN_BUCKET: i32 = -32;
const MAX_BUCKET: i32 = 40;
/// Upper bound of cached geometries; ten symbols times the reachable buckets stay far below.
const MAX_CACHED_GEOMETRIES: usize = 256;

/// Bucket index for a physical scale, and the scale geometry is built for in that bucket.
fn scale_bucket(physical_scale: f32) -> (i32, f32) {
    let bucket =
        ((physical_scale.log2() * BUCKETS_PER_OCTAVE).ceil() as i32).clamp(MIN_BUCKET, MAX_BUCKET);
    (bucket, (bucket as f32 / BUCKETS_PER_OCTAVE).exp2())
}

#[derive(Default)]
pub struct SymbolRenderer {
    cache: HashMap<(&'static str, i32), Arc<SymbolGeometry>>,
    tessellations: usize,
}

impl SymbolRenderer {
    /// Geometry of `def` for the bucket containing `physical_scale` (physical pixels per world
    /// unit), tessellating it on first use.
    pub fn geometry(
        &mut self,
        def: &'static SymbolDef,
        physical_scale: f32,
    ) -> Result<Arc<SymbolGeometry>, RenderError> {
        let physical_scale = if physical_scale.is_finite() && physical_scale > 0.0 {
            physical_scale
        } else {
            1.0
        };
        let (bucket, build_scale) = scale_bucket(physical_scale);
        if let Some(found) = self.cache.get(&(def.id, bucket)) {
            return Ok(Arc::clone(found));
        }
        let mut layers = Vec::new();
        for path in def.paths {
            layers.extend(
                tessellate_path(path, build_scale)
                    .map_err(|e| RenderError::Tessellate(def.id, e))?,
            );
        }
        self.tessellations += 1;
        if self.cache.len() >= MAX_CACHED_GEOMETRIES {
            self.cache.clear();
        }
        let geometry = Arc::new(SymbolGeometry { layers });
        self.cache.insert((def.id, bucket), Arc::clone(&geometry));
        Ok(geometry)
    }

    /// Number of tessellations performed so far; cache hits do not count.
    #[cfg(test)]
    pub fn tessellations(&self) -> usize {
        self.tessellations
    }

    /// One mesh in screen space for `layout`, with vertex alpha following the fringe coverage.
    pub fn mesh(
        &mut self,
        layout: &SymbolLayout,
        transform: &CanvasTransform,
        pixels_per_point: f32,
        color: Color32,
    ) -> Result<Mesh, RenderError> {
        let geometry = self.geometry(layout.def, transform.zoom() * pixels_per_point)?;
        let origin = layout.origin;
        let mut mesh = Mesh::default();
        for layer in &geometry.layers {
            let base = mesh.vertices.len() as u32;
            mesh.vertices
                .extend(layer.mesh.vertices.iter().map(|v| Vertex {
                    pos:
                        transform.to_screen([origin[0] + v.position[0], origin[1] + v.position[1]]),
                    uv: WHITE_UV,
                    color: color.gamma_multiply(v.coverage),
                }));
            mesh.indices
                .extend(layer.mesh.indices.iter().map(|i| base + i));
        }
        Ok(mesh)
    }

    pub fn draw(
        &mut self,
        painter: &Painter,
        layout: &SymbolLayout,
        transform: &CanvasTransform,
        palette: &Palette,
        state: PaintState,
    ) -> Result<(), RenderError> {
        let mesh = self.mesh(
            layout,
            transform,
            painter.pixels_per_point(),
            palette.color(state),
        )?;
        painter.add(Shape::mesh(mesh));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Pos2;

    const STYLES: [SymbolStyle; 2] = [SymbolStyle::Rect, SymbolStyle::Ieee];
    const KINDS: [ComponentKind; 10] = [
        ComponentKind::Const,
        ComponentKind::Input,
        ComponentKind::Output,
        ComponentKind::And,
        ComponentKind::NAnd,
        ComponentKind::Or,
        ComponentKind::NOr,
        ComponentKind::XOr,
        ComponentKind::XNOr,
        ComponentKind::Not,
    ];

    fn layout(style: SymbolStyle, kind: ComponentKind, center: Point) -> SymbolLayout {
        SymbolLayout::centered_on(style, kind, center).unwrap()
    }

    fn unit_transform(zoom: f32) -> CanvasTransform {
        CanvasTransform::new(Pos2::ZERO, zoom).unwrap()
    }

    #[test]
    fn centered_layout_puts_the_bounds_center_on_the_position() {
        for style in STYLES {
            for kind in KINDS {
                let b = layout(style, kind, [100., 60.]).bounds();
                assert!(
                    ((b.min[0] + b.max[0]) / 2. - 100.).abs() < 1e-4,
                    "{style:?} {kind:?}"
                );
                assert!(
                    ((b.min[1] + b.max[1]) / 2. - 60.).abs() < 1e-4,
                    "{style:?} {kind:?}"
                );
            }
        }
    }

    #[test]
    fn anchors_are_reported_in_world_coordinates() {
        for style in STYLES {
            for kind in KINDS {
                let l = layout(style, kind, [300., 200.]);
                let o = l.origin();
                let expected: Vec<(&str, Point)> = match kind {
                    ComponentKind::And | ComponentKind::Or => vec![
                        ("pin-in-0", [o[0], o[1]]),
                        ("pin-in-1", [o[0], o[1] + 40.]),
                        ("pin-out-0", [o[0] + 60., o[1] + 20.]),
                    ],
                    ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr => vec![
                        ("pin-in-0", [o[0], o[1]]),
                        ("pin-in-1", [o[0], o[1] + 40.]),
                        ("pin-out-0", [o[0] + 80., o[1] + 20.]),
                    ],
                    ComponentKind::XOr => vec![
                        ("pin-in-0", [o[0], o[1]]),
                        ("pin-in-1", [o[0], o[1] + 40.]),
                        ("pin-out-0", [o[0] + 60., o[1] + 20.]),
                    ],
                    ComponentKind::Not => {
                        vec![
                            ("pin-in-0", [o[0], o[1]]),
                            ("pin-out-0", [o[0] + 40., o[1]]),
                        ]
                    }
                    ComponentKind::Input => vec![("pin-out-0", [o[0], o[1]])],
                    ComponentKind::Output => vec![("pin-in-0", [o[0], o[1]])],
                    ComponentKind::Const => vec![("pin-out-0", [o[0], o[1]])],
                };
                let got: Vec<(&str, Point)> = l.anchors().map(|a| (a.name, a.position)).collect();
                assert_eq!(got, expected, "{style:?} {kind:?}");
            }
        }
    }

    #[test]
    fn gate_pins_of_both_styles_coincide_when_placed_alike() {
        for kind in [ComponentKind::And, ComponentKind::Or, ComponentKind::Not] {
            let pins = |style| -> Vec<Point> {
                SymbolLayout {
                    origin: [40., 80.],
                    ..layout(style, kind, [0., 0.])
                }
                .anchors()
                .map(|a| a.position)
                .collect()
            };
            assert_eq!(pins(SymbolStyle::Rect), pins(SymbolStyle::Ieee), "{kind:?}");
        }
    }

    #[test]
    fn hit_testing_uses_the_symbol_bounds_in_both_styles() {
        for style in STYLES {
            for kind in KINDS {
                let l = layout(style, kind, [100., 100.]);
                let b = l.bounds();
                assert!(l.hit([100., 100.], 0.), "{style:?} {kind:?} center");
                assert!(
                    l.hit([b.min[0] + 0.5, b.min[1] + 0.5], 0.),
                    "{style:?} {kind:?} corner"
                );
                assert!(
                    l.hit([b.max[0] - 0.5, b.max[1] - 0.5], 0.),
                    "{style:?} {kind:?} corner"
                );
                // Just outside misses unless the tolerance reaches it.
                assert!(
                    !l.hit([b.max[0] + 1.0, 100.], 0.5),
                    "{style:?} {kind:?} right"
                );
                assert!(
                    l.hit([b.max[0] + 1.0, 100.], 2.0),
                    "{style:?} {kind:?} right tolerance"
                );
                assert!(
                    !l.hit([100., b.min[1] - 1.0], 0.5),
                    "{style:?} {kind:?} top"
                );
                assert!(!l.hit([100. + 200., 100.], 2.0), "{style:?} {kind:?} far");
                // The universal 80 x 40 box is gone: small symbols no longer claim its area.
                if matches!(kind, ComponentKind::Input | ComponentKind::Output) {
                    assert!(
                        !l.hit([100. + 35., 100.], 2.0),
                        "{style:?} {kind:?} legacy box"
                    );
                }
            }
        }
    }

    #[test]
    fn selection_and_error_roles_differ_from_ink_in_both_themes() {
        for visuals in [Visuals::dark(), Visuals::light()] {
            let palette = Palette::from_visuals(&visuals);
            assert_eq!(palette.color(PaintState::Ink), visuals.text_color());
            assert_ne!(
                palette.color(PaintState::Selection),
                palette.color(PaintState::Ink)
            );
            assert_ne!(
                palette.color(PaintState::Error),
                palette.color(PaintState::Ink)
            );
            assert_ne!(
                palette.color(PaintState::Error),
                palette.color(PaintState::Selection)
            );
        }
    }

    #[test]
    fn geometry_is_cached_per_style_kind_and_scale_bucket() {
        let mut renderer = SymbolRenderer::default();
        let t = unit_transform(1.0);
        let and = layout(SymbolStyle::Rect, ComponentKind::And, [100., 100.]);
        let first = renderer.mesh(&and, &t, 1.0, Color32::WHITE).unwrap();
        assert_eq!(renderer.tessellations(), 1);
        // Same symbol elsewhere, other color: reused, only the vertices move.
        let moved = layout(SymbolStyle::Rect, ComponentKind::And, [300., 40.]);
        let second = renderer.mesh(&moved, &t, 1.0, Color32::RED).unwrap();
        assert_eq!(renderer.tessellations(), 1);
        assert_eq!(first.indices, second.indices);
        // A scale in the same quarter-octave bucket reuses the geometry too.
        renderer
            .mesh(&and, &unit_transform(0.9), 1.0, Color32::WHITE)
            .unwrap();
        assert_eq!(renderer.tessellations(), 1);
        // Different style, kind and scale bucket each tessellate once.
        let ieee = layout(SymbolStyle::Ieee, ComponentKind::And, [100., 100.]);
        renderer.mesh(&ieee, &t, 1.0, Color32::WHITE).unwrap();
        assert_eq!(renderer.tessellations(), 2);
        let or = layout(SymbolStyle::Rect, ComponentKind::Or, [100., 100.]);
        renderer.mesh(&or, &t, 1.0, Color32::WHITE).unwrap();
        assert_eq!(renderer.tessellations(), 3);
        renderer
            .mesh(&and, &unit_transform(2.0), 1.0, Color32::WHITE)
            .unwrap();
        assert_eq!(renderer.tessellations(), 4);
        // Pixels per point select the bucket as well.
        renderer.mesh(&and, &t, 2.0, Color32::WHITE).unwrap();
        assert_eq!(renderer.tessellations(), 4);
        renderer.mesh(&and, &t, 1.5, Color32::WHITE).unwrap();
        assert_eq!(renderer.tessellations(), 5);
    }

    #[test]
    fn scale_buckets_bound_the_build_scale_from_above() {
        for scale in [0.07_f32, 0.5, 0.99, 1.0, 1.01, 1.3, 2.0, 7.7, 64.0] {
            let (_, build) = scale_bucket(scale);
            assert!(build >= scale * 0.999_9, "{scale} -> {build}");
            assert!(build <= scale * 1.19, "{scale} -> {build}");
        }
    }

    /// Width and height at 50% coverage (midway between solid and faded vertices).
    fn true_extent(mesh: &Mesh) -> (f32, f32) {
        let span = |select: &dyn Fn(&Vertex) -> bool, axis: usize| {
            let values: Vec<f32> = mesh
                .vertices
                .iter()
                .filter(|v| select(v))
                .map(|v| if axis == 0 { v.pos.x } else { v.pos.y })
                .collect();
            values.iter().cloned().fold(f32::MIN, f32::max)
                - values.iter().cloned().fold(f32::MAX, f32::min)
        };
        let solid = |v: &Vertex| v.color.a() == 255;
        let faded = |v: &Vertex| v.color.a() == 0;
        (
            (span(&solid, 0) + span(&faded, 0)) / 2.,
            (span(&solid, 1) + span(&faded, 1)) / 2.,
        )
    }

    #[test]
    fn stroke_width_scales_with_zoom() {
        let mut renderer = SymbolRenderer::default();
        let l = layout(SymbolStyle::Rect, ComponentKind::And, [0., 0.]);
        let b = l.bounds();
        let (bw, bh) = (b.max[0] - b.min[0], b.max[1] - b.min[1]);
        for zoom in [1.0_f32, 2.0, 3.0] {
            let mesh = renderer
                .mesh(&l, &unit_transform(zoom), 1.0, Color32::WHITE)
                .unwrap();
            let (w, h) = true_extent(&mesh);
            // The declared bounds include half a stroke on each side; a screen-space hairline
            // would make the extent fall short by 0.75 * (zoom - 1) on each side.
            assert!(
                (w - bw * zoom).abs() < 0.05,
                "zoom {zoom}: {w} vs {}",
                bw * zoom
            );
            assert!(
                (h - bh * zoom).abs() < 0.05,
                "zoom {zoom}: {h} vs {}",
                bh * zoom
            );
        }
    }

    #[test]
    fn style_changes_the_rendered_shapes() {
        let mut renderer = SymbolRenderer::default();
        let t = unit_transform(1.0);
        for kind in [ComponentKind::And, ComponentKind::Or, ComponentKind::Not] {
            let rect = renderer
                .mesh(
                    &layout(SymbolStyle::Rect, kind, [50., 50.]),
                    &t,
                    1.0,
                    Color32::WHITE,
                )
                .unwrap();
            let ieee = renderer
                .mesh(
                    &layout(SymbolStyle::Ieee, kind, [50., 50.]),
                    &t,
                    1.0,
                    Color32::WHITE,
                )
                .unwrap();
            let positions = |m: &Mesh| {
                m.vertices
                    .iter()
                    .map(|v| (v.pos.x, v.pos.y))
                    .collect::<Vec<_>>()
            };
            assert_ne!(positions(&rect), positions(&ieee), "{kind:?}");
        }
        // Input and output share their drawing across styles.
        for kind in [ComponentKind::Input, ComponentKind::Output] {
            let rect = renderer
                .mesh(
                    &layout(SymbolStyle::Rect, kind, [50., 50.]),
                    &t,
                    1.0,
                    Color32::WHITE,
                )
                .unwrap();
            let ieee = renderer
                .mesh(
                    &layout(SymbolStyle::Ieee, kind, [50., 50.]),
                    &t,
                    1.0,
                    Color32::WHITE,
                )
                .unwrap();
            assert_eq!(rect.vertices.len(), ieee.vertices.len(), "{kind:?}");
        }
    }

    #[test]
    fn mesh_carries_fringe_alpha_and_ink_color() {
        let mut renderer = SymbolRenderer::default();
        let mesh = renderer
            .mesh(
                &layout(SymbolStyle::Rect, ComponentKind::Not, [0., 0.]),
                &unit_transform(1.0),
                1.0,
                Color32::from_rgb(200, 100, 50),
            )
            .unwrap();
        assert!(
            mesh.vertices
                .iter()
                .any(|v| v.color == Color32::from_rgb(200, 100, 50))
        );
        assert!(mesh.vertices.iter().any(|v| v.color.a() == 0));
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len())
        );
        assert_eq!(mesh.indices.len() % 3, 0);
    }
}
