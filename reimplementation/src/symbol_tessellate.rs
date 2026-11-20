//! Tessellation of symbol vector data into anti-aliased triangles (no GUI types).
//!
//! Geometry is produced in world units, relative to the symbol origin, with a per-vertex
//! coverage in `0..=1` that the renderer turns into a vertex alpha. Fills and strokes are
//! tessellated with `lyon_tessellation`, which resolves non-zero and even-odd compound fills
//! (including the holes of outlined glyphs) and stroke caps and joins.
//!
//! ## Curve error
//!
//! Curves are flattened by lyon with a tolerance of [`MAX_CURVE_ERROR_PX`] physical pixels,
//! converted to world units with the physical scale (`zoom * pixels_per_point`) the geometry is
//! built for. The renderer builds for the upper bound of a scale bucket and only draws at or
//! below it, so the bound holds for every drawn scale of the bucket.
//!
//! ## Anti-aliasing
//!
//! The meshes are drawn through egui, which gives raw meshes no edge anti-aliasing. Each
//! tessellated layer therefore gets a coverage fringe, the same scheme egui uses for its own
//! fills: the triangle soup is welded, the boundary edges (edges used by exactly one triangle,
//! which includes the edges of holes) are found, and every boundary vertex is moved half a
//! feather width inwards with full coverage and copied half a feather width outwards with zero
//! coverage. A strip between the two copies ramps coverage linearly across one physical pixel
//! centered on the true edge, so the edge keeps its exact position and a straight edge shows
//! 50% coverage on its boundary. Outward directions come from the triangle owning the edge,
//! so holes and any winding order are handled alike; corners use a miter offset limited to
//! twice the feather width.
//!
//! Strokes narrower than one physical pixel are tessellated at one pixel and their coverage is
//! scaled by the true width, as egui does for thin lines. Features narrower than a pixel
//! (glyph stems at very small zoom) are not specially handled.

use crate::symbols::{FillRule, PathOp, Point, StrokeCap, StrokeJoin, SymbolPath};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, LineCap, LineJoin, StrokeOptions,
    StrokeTessellator, StrokeVertex, TessellationError, VertexBuffers,
    math::point,
    path::{FillRule as LyonFillRule, Path},
};
use std::collections::HashMap;

/// Upper bound of the distance between a flattened curve and the true curve, in physical pixels.
pub const MAX_CURVE_ERROR_PX: f32 = 0.25;

/// Width of the anti-aliasing ramp in physical pixels, centered on the true edge.
pub const FEATHER_PX: f32 = 1.0;

/// A vertex in world units relative to the symbol origin with its coverage in `0..=1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageVertex {
    pub position: Point,
    pub coverage: f32,
}

/// Indexed triangles with coverage; one fill or one stroke of a symbol path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoverageMesh {
    pub vertices: Vec<CoverageVertex>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaintKind {
    Fill,
    Stroke,
}

/// The triangles for one fill or stroke of one symbol path.
#[derive(Clone, Debug, PartialEq)]
pub struct PathLayer {
    pub path_id: &'static str,
    pub role: crate::symbols::SymbolRole,
    pub paint: PaintKind,
    pub mesh: CoverageMesh,
}

#[derive(Debug)]
pub enum TessellateError {
    /// A curve or line op without a preceding `MoveTo`, or a non-finite coordinate.
    MalformedPath(&'static str),
    /// The physical scale is not a positive finite number.
    InvalidScale(f32),
    Lyon(TessellationError),
}

impl std::fmt::Display for TessellateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedPath(id) => write!(f, "symbol path {id} is malformed"),
            Self::InvalidScale(scale) => write!(f, "invalid symbol scale {scale}"),
            Self::Lyon(error) => write!(f, "tessellation failed: {error}"),
        }
    }
}

impl std::error::Error for TessellateError {}

impl From<TessellationError> for TessellateError {
    fn from(error: TessellationError) -> Self {
        Self::Lyon(error)
    }
}

/// Tessellates every fill and stroke of `path` for `physical_scale` physical pixels per world
/// unit. A path that is both filled and stroked yields the fill layer first.
pub fn tessellate_path(
    path: &SymbolPath,
    physical_scale: f32,
) -> Result<Vec<PathLayer>, TessellateError> {
    if !(physical_scale.is_finite() && physical_scale > 0.0) {
        return Err(TessellateError::InvalidScale(physical_scale));
    }
    let tolerance = MAX_CURVE_ERROR_PX / physical_scale;
    let half_feather = 0.5 * FEATHER_PX / physical_scale;
    let lyon_path = build_path(path)?;
    let mut layers = Vec::new();
    if path.filled {
        let raw = fill_triangles(&lyon_path, path.fill_rule, tolerance)?;
        layers.push(PathLayer {
            path_id: path.id,
            role: path.role,
            paint: PaintKind::Fill,
            mesh: feather(&raw, half_feather, 1.0),
        });
    }
    if path.stroked {
        let width_px = path.stroke_width * physical_scale;
        // Thin strokes keep a one pixel footprint and lose coverage instead.
        let (width, coverage) = if width_px < FEATHER_PX {
            (FEATHER_PX / physical_scale, width_px / FEATHER_PX)
        } else {
            (path.stroke_width, 1.0)
        };
        let raw = stroke_triangles(&lyon_path, path, width, tolerance)?;
        layers.push(PathLayer {
            path_id: path.id,
            role: path.role,
            paint: PaintKind::Stroke,
            mesh: feather(&raw, half_feather, coverage),
        });
    }
    Ok(layers)
}

/// Triangles without any fringe: positions and indices only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawTriangles {
    pub positions: Vec<Point>,
    pub indices: Vec<u32>,
}

impl RawTriangles {
    /// Sum of the triangle areas, in squared world units.
    pub fn area(&self) -> f32 {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| self.positions[i as usize]);
                0.5 * ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs()
            })
            .sum()
    }
}

fn build_path(path: &SymbolPath) -> Result<Path, TessellateError> {
    let malformed = || TessellateError::MalformedPath(path.id);
    let finite = |p: &Point| p.iter().all(|c| c.is_finite());
    let to = |p: Point| point(p[0], p[1]);
    let mut builder = Path::builder();
    let mut open = false;
    for op in path.ops {
        match *op {
            PathOp::MoveTo(p) if finite(&p) => {
                if open {
                    builder.end(false);
                }
                builder.begin(to(p));
                open = true;
            }
            PathOp::LineTo(p) if open && finite(&p) => {
                builder.line_to(to(p));
            }
            PathOp::QuadTo(c, p) if open && finite(&c) && finite(&p) => {
                builder.quadratic_bezier_to(to(c), to(p));
            }
            PathOp::CubicTo(c1, c2, p) if open && finite(&c1) && finite(&c2) && finite(&p) => {
                builder.cubic_bezier_to(to(c1), to(c2), to(p));
            }
            PathOp::Close if open => {
                builder.end(true);
                open = false;
            }
            _ => return Err(malformed()),
        }
    }
    if open {
        builder.end(false);
    }
    Ok(builder.build())
}

/// Fill triangles of the compound `path` under `rule`, without fringe.
pub fn fill_triangles(
    path: &Path,
    rule: FillRule,
    tolerance: f32,
) -> Result<RawTriangles, TessellateError> {
    let options = FillOptions::tolerance(tolerance).with_fill_rule(match rule {
        FillRule::NonZero => LyonFillRule::NonZero,
        FillRule::EvenOdd => LyonFillRule::EvenOdd,
    });
    let mut buffers: VertexBuffers<Point, u32> = VertexBuffers::new();
    FillTessellator::new().tessellate_path(
        path,
        &options,
        &mut BuffersBuilder::new(&mut buffers, |vertex: FillVertex| {
            vertex.position().to_array()
        }),
    )?;
    Ok(RawTriangles {
        positions: buffers.vertices,
        indices: buffers.indices,
    })
}

fn stroke_triangles(
    path: &Path,
    source: &SymbolPath,
    width: f32,
    tolerance: f32,
) -> Result<RawTriangles, TessellateError> {
    let options = StrokeOptions::tolerance(tolerance)
        .with_line_width(width)
        .with_line_cap(match source.cap {
            StrokeCap::Butt => LineCap::Butt,
            StrokeCap::Round => LineCap::Round,
            StrokeCap::Square => LineCap::Square,
        })
        .with_line_join(match source.join {
            StrokeJoin::Miter => LineJoin::Miter,
            StrokeJoin::Round => LineJoin::Round,
            StrokeJoin::Bevel => LineJoin::Bevel,
        })
        .with_miter_limit(source.miter_limit);
    let mut buffers: VertexBuffers<Point, u32> = VertexBuffers::new();
    StrokeTessellator::new().tessellate_path(
        path,
        &options,
        &mut BuffersBuilder::new(&mut buffers, |vertex: StrokeVertex| {
            vertex.position().to_array()
        }),
    )?;
    Ok(RawTriangles {
        positions: buffers.vertices,
        indices: buffers.indices,
    })
}

/// Tessellates one filled `path` (ignoring its stroke) without fringe, for area checks.
pub fn tessellate_fill_raw(
    path: &SymbolPath,
    physical_scale: f32,
) -> Result<RawTriangles, TessellateError> {
    if !(physical_scale.is_finite() && physical_scale > 0.0) {
        return Err(TessellateError::InvalidScale(physical_scale));
    }
    fill_triangles(
        &build_path(path)?,
        path.fill_rule,
        MAX_CURVE_ERROR_PX / physical_scale,
    )
}

fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1]]
}

fn dot(a: Point, b: Point) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn length(a: Point) -> f32 {
    dot(a, a).sqrt()
}

/// Welded triangles of a raw tessellation and the outward normals of their boundary edges.
struct Boundary {
    positions: Vec<Point>,
    triangles: Vec<[u32; 3]>,
    /// Vertex pairs (smaller index first) with the outward unit normal, sorted by pair.
    edges: Vec<((u32, u32), Point)>,
}

fn find_boundary(raw: &RawTriangles) -> Boundary {
    // Weld bit-identical positions so shared edges are recognized as shared.
    let mut welded: Vec<Point> = Vec::new();
    let mut by_bits: HashMap<[u32; 2], u32> = HashMap::new();
    let remap: Vec<u32> = raw
        .positions
        .iter()
        .map(|p| {
            *by_bits
                .entry([p[0].to_bits(), p[1].to_bits()])
                .or_insert_with(|| {
                    welded.push(*p);
                    (welded.len() - 1) as u32
                })
        })
        .collect();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for t in raw.indices.as_chunks::<3>().0 {
        let tri = [
            remap[t[0] as usize],
            remap[t[1] as usize],
            remap[t[2] as usize],
        ];
        if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            triangles.push(tri);
        }
    }

    // An edge used by exactly one triangle is a boundary edge; remember its opposite vertex.
    struct EdgeUse {
        count: u32,
        opposite: u32,
    }
    let mut uses: HashMap<(u32, u32), EdgeUse> = HashMap::new();
    for tri in &triangles {
        for k in 0..3 {
            let (a, b, opposite) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            uses.entry((a.min(b), a.max(b)))
                .and_modify(|e| e.count += 1)
                .or_insert(EdgeUse { count: 1, opposite });
        }
    }

    let mut edges: Vec<((u32, u32), Point)> = Vec::new();
    for (&(a, b), edge) in &uses {
        if edge.count != 1 {
            continue;
        }
        let (pa, pb) = (welded[a as usize], welded[b as usize]);
        let d = sub(pb, pa);
        let len = length(d);
        if len == 0.0 {
            continue;
        }
        let mut n = [d[1] / len, -d[0] / len];
        if dot(n, sub(welded[edge.opposite as usize], pa)) > 0.0 {
            n = [-n[0], -n[1]];
        }
        edges.push(((a, b), n));
    }
    // Deterministic order independent of hash iteration.
    edges.sort_by_key(|(edge, _)| *edge);
    Boundary {
        positions: welded,
        triangles,
        edges,
    }
}

/// Adds the coverage fringe described in the module documentation. `coverage` scales the
/// coverage of the solid part and of the fringe alike.
fn feather(raw: &RawTriangles, half_width: f32, coverage: f32) -> CoverageMesh {
    let Boundary {
        positions: welded,
        triangles,
        edges: boundary,
    } = find_boundary(raw);

    let mut normals: HashMap<u32, Vec<Point>> = HashMap::new();
    for ((a, b), n) in &boundary {
        normals.entry(*a).or_default().push(*n);
        normals.entry(*b).or_default().push(*n);
    }
    let offsets: HashMap<u32, Point> = normals
        .iter()
        .map(|(&vertex, ns)| {
            let sum = ns
                .iter()
                .fold([0.0, 0.0], |s, n| [s[0] + n[0], s[1] + n[1]]);
            let len = length(sum);
            if len < 1e-6 {
                return (vertex, [0.0, 0.0]);
            }
            let dir = [sum[0] / len, sum[1] / len];
            // Miter factor, limited so sharp corners do not spike.
            let cos = ns.iter().map(|n| dot(dir, *n)).fold(1.0_f32, f32::min);
            let scale = half_width / cos.max(0.5);
            (vertex, [dir[0] * scale, dir[1] * scale])
        })
        .collect();

    let mut mesh = CoverageMesh::default();
    let mut inner: Vec<u32> = Vec::with_capacity(welded.len());
    let mut outer: HashMap<u32, u32> = HashMap::new();
    for (index, p) in welded.iter().enumerate() {
        let index = index as u32;
        let (position, outer_position) = match offsets.get(&index) {
            Some(o) => ([p[0] - o[0], p[1] - o[1]], Some([p[0] + o[0], p[1] + o[1]])),
            None => (*p, None),
        };
        inner.push(mesh.vertices.len() as u32);
        mesh.vertices.push(CoverageVertex { position, coverage });
        if let Some(position) = outer_position {
            outer.insert(index, mesh.vertices.len() as u32);
            mesh.vertices.push(CoverageVertex {
                position,
                coverage: 0.0,
            });
        }
    }
    for tri in &triangles {
        mesh.indices.extend(tri.iter().map(|&i| inner[i as usize]));
    }
    for ((a, b), _) in &boundary {
        let (ia, ib) = (inner[*a as usize], inner[*b as usize]);
        let (oa, ob) = (outer[a], outer[b]);
        mesh.indices.extend([ia, ib, ob, ia, ob, oa]);
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ComponentKind;
    use crate::symbols::{SymbolRole, SymbolStyle, symbol};

    fn glyph() -> &'static SymbolPath {
        symbol(SymbolStyle::Rect, ComponentKind::And)
            .unwrap()
            .paths
            .iter()
            .find(|p| p.role == SymbolRole::Glyph)
            .unwrap()
    }

    fn contours(path: &SymbolPath) -> Vec<Vec<Point>> {
        let mut out: Vec<Vec<Point>> = Vec::new();
        for op in path.ops {
            match *op {
                PathOp::MoveTo(p) => out.push(vec![p]),
                PathOp::LineTo(p) | PathOp::QuadTo(_, p) | PathOp::CubicTo(_, _, p) => {
                    out.last_mut().unwrap().push(p)
                }
                PathOp::Close => {}
            }
        }
        out
    }

    fn polygon_area(points: &[Point]) -> f32 {
        let mut sum = 0.0;
        for i in 0..points.len() {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            sum += a[0] * b[1] - b[0] * a[1];
        }
        0.5 * sum.abs()
    }

    #[test]
    fn compound_glyph_is_triangulated_with_holes() {
        let path = glyph();
        let raw = tessellate_fill_raw(path, 4.0).unwrap();
        assert!(!raw.indices.is_empty() && raw.indices.len().is_multiple_of(3));
        let rings = contours(path);
        assert_eq!(rings.len(), 3, "outer contour and two counters");
        let outer = rings
            .iter()
            .map(|r| polygon_area(r))
            .fold(0.0_f32, f32::max);
        let holes: f32 = rings.iter().map(|r| polygon_area(r)).sum::<f32>() - outer;
        let area = raw.area();
        // Control polygons only approximate the curves, so allow a generous margin; what
        // matters is that the counters are cut out instead of covered.
        assert!(
            area < outer,
            "filled area {area} must be below the outer area {outer}"
        );
        assert!(holes > 0.5, "counters must have area, got {holes}");
        // Fill the outer contour alone for a direct with/without-hole comparison.
        let mut ops = vec![];
        let mut first_ring_done = false;
        for op in path.ops {
            if matches!(op, PathOp::MoveTo(_)) && first_ring_done {
                break;
            }
            ops.push(*op);
            if matches!(op, PathOp::Close) {
                first_ring_done = true;
            }
        }
        let outer_only = SymbolPath {
            ops: Box::leak(ops.into_boxed_slice()),
            ..*path
        };
        let without_holes = tessellate_fill_raw(&outer_only, 4.0).unwrap().area();
        assert!(
            area < without_holes - 1.0,
            "with holes {area} vs without {without_holes}"
        );
    }

    #[test]
    fn feathered_fill_leaves_the_counters_uncovered() {
        let path = glyph();
        let layers = tessellate_path(path, 4.0).unwrap();
        let layer = &layers[0];
        assert_eq!(layer.paint, PaintKind::Fill);
        let rings = contours(path);
        let (counter, _) = rings
            .iter()
            .skip(1)
            .map(|ring| {
                let n = ring.len() as f32;
                let c = ring
                    .iter()
                    .fold([0.0, 0.0], |s, p| [s[0] + p[0] / n, s[1] + p[1] / n]);
                (c, polygon_area(ring))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let solid_cover = |p: Point| -> bool {
            layer.mesh.indices.as_chunks::<3>().0.iter().any(|t| {
                let v = t.map(|i| layer.mesh.vertices[i as usize]);
                v.iter().all(|v| v.coverage >= 1.0) && point_in_triangle(p, v.map(|v| v.position))
            })
        };
        assert!(
            !solid_cover(counter),
            "counter centre {counter:?} is filled"
        );
        // A point on the glyph's stem must be solid; use the outer contour's first vertex nudged
        // inwards is unreliable, so check that some solid area exists at all.
        assert!(layer.mesh.indices.as_chunks::<3>().0.iter().any(|t| {
            t.iter()
                .all(|&i| layer.mesh.vertices[i as usize].coverage >= 1.0)
        }));
    }

    fn point_in_triangle(p: Point, t: [Point; 3]) -> bool {
        let side =
            |a: Point, b: Point| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        let (d1, d2, d3) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    }

    #[test]
    fn fringe_ramps_coverage_across_one_pixel_around_the_edge() {
        // A 10 x 10 square at 2 px per unit: the fringe is 1 px = 0.5 units wide, centered.
        let square = SymbolPath {
            id: "square",
            role: SymbolRole::Glyph,
            ops: &[
                PathOp::MoveTo([0.0, 0.0]),
                PathOp::LineTo([10.0, 0.0]),
                PathOp::LineTo([10.0, 10.0]),
                PathOp::LineTo([0.0, 10.0]),
                PathOp::Close,
            ],
            fill_rule: FillRule::NonZero,
            filled: true,
            stroked: false,
            stroke_width: 0.0,
            cap: StrokeCap::Butt,
            join: StrokeJoin::Miter,
            miter_limit: 4.0,
        };
        let layer = &tessellate_path(&square, 2.0).unwrap()[0];
        let solid: Vec<_> = layer
            .mesh
            .vertices
            .iter()
            .filter(|v| v.coverage == 1.0)
            .collect();
        let faded: Vec<_> = layer
            .mesh
            .vertices
            .iter()
            .filter(|v| v.coverage == 0.0)
            .collect();
        assert_eq!((solid.len(), faded.len()), (4, 4));
        let extent = |vs: &[&CoverageVertex]| {
            let min = vs.iter().map(|v| v.position[0]).fold(f32::MAX, f32::min);
            let max = vs.iter().map(|v| v.position[0]).fold(f32::MIN, f32::max);
            (min, max)
        };
        let (s_min, s_max) = extent(&solid);
        let (f_min, f_max) = extent(&faded);
        assert!((s_min - 0.25).abs() < 1e-5 && (s_max - 9.75).abs() < 1e-5);
        assert!((f_min + 0.25).abs() < 1e-5 && (f_max - 10.25).abs() < 1e-5);
    }

    #[test]
    fn strokes_keep_world_width_and_thin_strokes_lose_coverage() {
        let lead = symbol(SymbolStyle::Rect, ComponentKind::And)
            .unwrap()
            .paths
            .iter()
            .find(|p| p.id == "lead-in-0")
            .unwrap();
        let half_extent = |scale: f32| {
            let layer = tessellate_path(lead, scale).unwrap().remove(0);
            let ys = |c: f32| {
                let v: Vec<f32> = layer
                    .mesh
                    .vertices
                    .iter()
                    .filter(|v| v.coverage == c)
                    .map(|v| v.position[1])
                    .collect();
                v.iter().cloned().fold(f32::MIN, f32::max)
                    - v.iter().cloned().fold(f32::MAX, f32::min)
            };
            (ys(1.0), ys(0.0), layer)
        };
        // Wide enough: the 50% coverage width equals the stroke width in world units.
        let (solid, faded, _) = half_extent(4.0);
        assert!(
            ((solid + faded) / 2.0 - 1.5).abs() < 1e-4,
            "{solid} {faded}"
        );
        // Thin: one pixel wide at 0.4 px/unit (2.5 units), with coverage 0.6.
        let (_, _, layer) = half_extent(0.4);
        let max_cov = layer
            .mesh
            .vertices
            .iter()
            .map(|v| v.coverage)
            .fold(0.0_f32, f32::max);
        assert!((max_cov - 0.6).abs() < 1e-4, "{max_cov}");
    }

    #[test]
    fn symbol_layers_are_finite_and_fringed() {
        for def in crate::symbols::symbols() {
            for path in def.paths {
                for layer in tessellate_path(path, 3.0).unwrap() {
                    let vertices = &layer.mesh.vertices;
                    assert!(!layer.mesh.indices.is_empty(), "{} {}", def.id, path.id);
                    assert!(
                        layer
                            .mesh
                            .indices
                            .iter()
                            .all(|&i| (i as usize) < vertices.len()),
                        "{} {}",
                        def.id,
                        path.id
                    );
                    assert!(vertices.iter().all(|v| {
                        v.position.iter().all(|c| c.is_finite())
                            && (0.0..=1.0).contains(&v.coverage)
                    }));
                    assert!(
                        vertices.iter().any(|v| v.coverage == 0.0),
                        "{} {} has no fringe",
                        def.id,
                        path.id
                    );
                }
            }
        }
    }

    #[test]
    fn raw_fill_boundaries_are_closed_loops_without_t_junctions() {
        // Every boundary vertex of a fill must have exactly two boundary edges; a vertex with
        // more or fewer would indicate a T-junction or a dangling edge that breaks the fringe.
        for def in crate::symbols::symbols() {
            for path in def.paths.iter().filter(|p| p.filled) {
                let raw = tessellate_fill_raw(path, 3.0).unwrap();
                let boundary = find_boundary(&raw);
                let mut degree: HashMap<u32, usize> = HashMap::new();
                for ((a, b), _) in &boundary.edges {
                    *degree.entry(*a).or_default() += 1;
                    *degree.entry(*b).or_default() += 1;
                }
                assert!(!degree.is_empty());
                assert!(
                    degree.values().all(|&d| d == 2),
                    "{} {}: {degree:?}",
                    def.id,
                    path.id
                );
            }
        }
    }

    #[test]
    fn rejects_malformed_paths_and_scales() {
        let bad = SymbolPath {
            ops: &[PathOp::LineTo([1.0, 1.0])],
            ..*glyph()
        };
        assert!(matches!(
            tessellate_path(&bad, 1.0),
            Err(TessellateError::MalformedPath(_))
        ));
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(matches!(
                tessellate_path(glyph(), scale),
                Err(TessellateError::InvalidScale(_))
            ));
        }
    }
}
