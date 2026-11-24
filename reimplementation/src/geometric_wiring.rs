//! Integer geometric wiring and transactional editing primitives.
//!
//! Geometric segments are the authoritative document representation. Pin-to-pin editing helpers
//! materialize segments; connectivity is resolved from endpoints, never from a parallel edge list.
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SEGMENTS: usize = 30_000;
/// Conservative bound covering source-pair checks, generated pieces and collinear merge scans.
pub const MAX_NORMALIZATION_WORK: usize = 5_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub id: u64,
    pub a: Point,
    pub b: Point,
}

pub use crate::domain::Wiring;

/// Explicit, stable wire-routing orientation. The planner is toolkit independent and emits an
/// orthogonal L (or a single segment when either coordinate already matches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteOrientation {
    HorizontalFirst,
    VerticalFirst,
}

pub fn plan_orthogonal_route(
    start: Point,
    end: Point,
    orientation: RouteOrientation,
) -> Vec<Point> {
    if start == end {
        return vec![start];
    }
    let corner = match orientation {
        RouteOrientation::HorizontalFirst => Point::new(end.x, start.y),
        RouteOrientation::VerticalFirst => Point::new(start.x, end.y),
    };
    if corner == start || corner == end {
        vec![start, end]
    } else {
        vec![start, corner, end]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    ZeroLength,
    DuplicateId(u64),
    IdExhausted,
    SegmentLimit,
    WorkLimit,
    SimulationActive,
    InvalidGeometry(&'static str),
}
impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GeometryError {}

/// Canonicalizes orthogonal intervals and endpoint contacts. Proper interior crossings do not
/// create vertices. Diagonal segments are retained (except duplicate edges) and only their
/// endpoints can split another segment.
pub fn normalize(input: &[Segment]) -> Result<Vec<Segment>, GeometryError> {
    normalize_with_high_water(input, input.iter().map(|s| s.id).max().unwrap_or(0))
}

/// Normalize while reserving every ID at or below `high_water`, including retired IDs.
pub fn normalize_with_high_water(
    input: &[Segment],
    high_water: u64,
) -> Result<Vec<Segment>, GeometryError> {
    normalize_with_high_water_preserving(input, high_water, &[])
}

/// Normalize while retaining vertices at protected coordinates, such as component pin contacts.
pub fn normalize_with_high_water_preserving(
    input: &[Segment],
    high_water: u64,
    protected_points: &[Point],
) -> Result<Vec<Segment>, GeometryError> {
    if input.len() > MAX_SEGMENTS {
        return Err(GeometryError::SegmentLimit);
    }
    let mut ids = BTreeSet::new();
    for s in input {
        if s.a == s.b {
            return Err(GeometryError::ZeroLength);
        }
        if !ids.insert(s.id) {
            return Err(GeometryError::DuplicateId(s.id));
        }
    }
    let pairs = input
        .len()
        .checked_mul(input.len().saturating_sub(1))
        .and_then(|n| n.checked_div(2))
        .ok_or(GeometryError::WorkLimit)?;
    // Four endpoint predicates are evaluated per pair. Before allocating generated pieces, bound
    // the worst-case cuts (two endpoints from each other segment) and later quadratic merge scan.
    let max_pieces = input
        .len()
        .checked_mul(input.len().saturating_mul(2).saturating_add(1))
        .ok_or(GeometryError::WorkLimit)?;
    let preflight = pairs
        .checked_mul(5)
        .and_then(|n| n.checked_add(max_pieces))
        .ok_or(GeometryError::WorkLimit)?;
    if preflight > MAX_NORMALIZATION_WORK {
        return Err(GeometryError::WorkLimit);
    }
    let mut cuts: Vec<BTreeSet<Point>> = input.iter().map(|s| BTreeSet::from([s.a, s.b])).collect();
    for i in 0..input.len() {
        for j in i + 1..input.len() {
            let a = input[i];
            let b = input[j];
            // Endpoints lying on another segment are the only cross-segment split contacts.
            for p in [a.a, a.b] {
                if on_segment(p, b) && (is_orthogonal(b) || p == b.a || p == b.b) {
                    cuts[j].insert(p);
                }
            }
            for p in [b.a, b.b] {
                if on_segment(p, a) && (is_orthogonal(a) || p == a.a || p == a.b) {
                    cuts[i].insert(p);
                }
            }
            // Orthogonal proper crossings remain unconnected. Collinear overlap boundaries are
            // already endpoint cuts; adjacent intervals will be merged below.
        }
    }
    let piece_upper = cuts
        .iter()
        .try_fold(0usize, |count, points| {
            count.checked_add(points.len().saturating_sub(1))
        })
        .ok_or(GeometryError::WorkLimit)?;
    if piece_upper > MAX_SEGMENTS {
        return Err(GeometryError::SegmentLimit);
    }
    let mut pieces: BTreeMap<(Point, Point), Vec<u64>> = BTreeMap::new();
    for (s, points) in input.iter().zip(cuts) {
        let mut ordered: Vec<_> = points.into_iter().collect();
        ordered.sort_by_key(|p| along_key(*p, *s));
        for pair in ordered.windows(2) {
            if pair[0] != pair[1] {
                pieces
                    .entry(canonical(pair[0], pair[1]))
                    .or_default()
                    .push(s.id);
            }
        }
    }
    // Bound generated pieces and merge scans before invoking the merge's quadratic junction pass.
    let merge_work = pieces
        .len()
        .checked_mul(pieces.len())
        .ok_or(GeometryError::WorkLimit)?;
    if pairs
        .saturating_mul(5)
        .saturating_add(pieces.len())
        .saturating_add(merge_work)
        > MAX_NORMALIZATION_WORK
    {
        return Err(GeometryError::WorkLimit);
    }
    // Join touching/overlapping horizontal and vertical intervals unless a different-orientation
    // endpoint establishes a junction there. Pieces remain distinct at branch points.
    let mut out = pieces
        .into_iter()
        .map(|((a, b), source)| Segment {
            id: *source.iter().min().unwrap_or(&0),
            a,
            b,
        })
        .collect::<Vec<_>>();
    merge_collinear(&mut out, protected_points);
    out.sort_by_key(|s| (s.a, s.b, s.id));
    if out.len() > MAX_SEGMENTS {
        return Err(GeometryError::SegmentLimit);
    }
    // Ensure IDs are unique while preserving unchanged IDs and deterministic source ownership.
    let mut used = BTreeSet::new();
    let mut next = high_water.max(input.iter().map(|s| s.id).max().unwrap_or(0));
    for s in &mut out {
        if !used.insert(s.id) {
            next = next.checked_add(1).ok_or(GeometryError::IdExhausted)?;
            while used.contains(&next) {
                next = next.checked_add(1).ok_or(GeometryError::IdExhausted)?;
            }
            s.id = next;
            used.insert(next);
        }
    }
    Ok(out)
}
fn canonical(a: Point, b: Point) -> (Point, Point) {
    if a <= b { (a, b) } else { (b, a) }
}
fn along_key(p: Point, s: Segment) -> i128 {
    if s.a.y == s.b.y {
        i128::from(p.x) * (i128::from(s.b.x) - i128::from(s.a.x)).signum()
    } else if s.a.x == s.b.x {
        i128::from(p.y) * (i128::from(s.b.y) - i128::from(s.a.y)).signum()
    } else {
        let dx = i128::from(s.b.x) - i128::from(s.a.x);
        let dy = i128::from(s.b.y) - i128::from(s.a.y);
        (i128::from(p.x) - i128::from(s.a.x)) * dx + (i128::from(p.y) - i128::from(s.a.y)) * dy
    }
}
fn is_orthogonal(s: Segment) -> bool {
    s.a.x == s.b.x || s.a.y == s.b.y
}
fn on_segment(p: Point, s: Segment) -> bool {
    let dx = i128::from(s.b.x) - i128::from(s.a.x);
    let dy = i128::from(s.b.y) - i128::from(s.a.y);
    let px = i128::from(p.x) - i128::from(s.a.x);
    let py = i128::from(p.y) - i128::from(s.a.y);
    px * dy == py * dx
        && p.x >= s.a.x.min(s.b.x)
        && p.x <= s.a.x.max(s.b.x)
        && p.y >= s.a.y.min(s.b.y)
        && p.y <= s.a.y.max(s.b.y)
}
fn merge_collinear(segments: &mut Vec<Segment>, protected_points: &[Point]) {
    // Preserve branches and all diagonal segments. Merge only orthogonal connected intervals
    // having no non-collinear segment endpoint at their common location.
    let mut protected = protected_points.iter().copied().collect::<BTreeSet<_>>();
    for a in segments.iter() {
        for b in segments.iter() {
            if a.id != b.id && !same_line(a, b) {
                for p in [a.a, a.b] {
                    if p == b.a || p == b.b {
                        protected.insert(p);
                    }
                }
            }
        }
    }
    let mut groups: BTreeMap<(u8, i32), Vec<Segment>> = BTreeMap::new();
    let mut rest = Vec::new();
    for s in segments.drain(..) {
        if s.a.y == s.b.y {
            groups.entry((0, s.a.y)).or_default().push(s);
        } else if s.a.x == s.b.x {
            groups.entry((1, s.a.x)).or_default().push(s);
        } else {
            rest.push(s);
        }
    }
    for ((axis, line), mut group) in groups {
        group.sort_by_key(|s| {
            if axis == 0 {
                s.a.x.min(s.b.x)
            } else {
                s.a.y.min(s.b.y)
            }
        });
        let mut current: Option<Segment> = None;
        for s in group {
            if let Some(mut c) = current.take() {
                let end = if axis == 0 {
                    c.a.x.max(c.b.x)
                } else {
                    c.a.y.max(c.b.y)
                };
                let start = if axis == 0 {
                    s.a.x.min(s.b.x)
                } else {
                    s.a.y.min(s.b.y)
                };
                let point = if axis == 0 {
                    Point::new(start, line)
                } else {
                    Point::new(line, start)
                };
                if start <= end && !protected.contains(&point) {
                    let far = if axis == 0 {
                        s.a.x.max(s.b.x)
                    } else {
                        s.a.y.max(s.b.y)
                    };
                    c.b = if axis == 0 {
                        Point::new(end.max(far), line)
                    } else {
                        Point::new(line, end.max(far))
                    };
                    c.id = c.id.min(s.id);
                    current = Some(c);
                } else {
                    rest.push(c);
                    current = Some(s);
                }
            } else {
                current = Some(s);
            }
        }
        if let Some(c) = current {
            rest.push(c);
        }
    }
    *segments = rest;
}
fn same_line(a: &Segment, b: &Segment) -> bool {
    let (adx, ady) = (
        i128::from(a.b.x) - i128::from(a.a.x),
        i128::from(a.b.y) - i128::from(a.a.y),
    );
    let (bdx, bdy) = (
        i128::from(b.b.x) - i128::from(b.a.x),
        i128::from(b.b.y) - i128::from(b.a.y),
    );
    adx * bdy == ady * bdx
        && (i128::from(b.a.x) - i128::from(a.a.x)) * ady
            == (i128::from(b.a.y) - i128::from(a.a.y)) * adx
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(id: u64, a: (i32, i32), b: (i32, i32)) -> Segment {
        Segment {
            id,
            a: Point::new(a.0, a.1),
            b: Point::new(b.0, b.1),
        }
    }
    #[test]
    fn orthogonal_planner_has_both_orientations_and_no_redundant_corner() {
        let a = Point::new(1, 2);
        let b = Point::new(8, 9);
        assert_eq!(
            plan_orthogonal_route(a, b, RouteOrientation::HorizontalFirst),
            vec![a, Point::new(8, 2), b]
        );
        assert_eq!(
            plan_orthogonal_route(a, b, RouteOrientation::VerticalFirst),
            vec![a, Point::new(1, 9), b]
        );
        assert_eq!(
            plan_orthogonal_route(a, Point::new(8, 2), RouteOrientation::VerticalFirst),
            vec![a, Point::new(8, 2)]
        );
        assert_eq!(
            plan_orthogonal_route(a, a, RouteOrientation::VerticalFirst),
            vec![a]
        );
    }

    #[test]
    fn normalization_splits_endpoint_contacts_but_not_proper_crossings() {
        let t = normalize(&[segment(1, (0, 0), (10, 0)), segment(2, (5, 0), (5, 5))]).unwrap();
        assert!(
            t.iter()
                .any(|s| s.a == Point::new(5, 0) || s.b == Point::new(5, 0))
        );
        let x = normalize(&[segment(1, (0, 0), (10, 0)), segment(2, (5, -5), (5, 5))]).unwrap();
        assert_eq!(x.len(), 2);
    }
    #[test]
    fn normalization_retains_protected_pin_contact_vertices() {
        let pieces = [segment(1, (0, 0), (10, 0)), segment(2, (10, 0), (20, 0))];
        assert_eq!(normalize(&pieces).unwrap().len(), 1);
        let preserved =
            normalize_with_high_water_preserving(&pieces, 2, &[Point::new(10, 0)]).unwrap();
        assert_eq!(preserved.len(), 2);
        assert!(
            preserved
                .iter()
                .any(|s| s.a == Point::new(10, 0) || s.b == Point::new(10, 0))
        );
    }

    #[test]
    fn canonical_overlap_is_idempotent_and_bounded() {
        let input = [segment(8, (0, 0), (10, 0)), segment(9, (5, 0), (15, 0))];
        let once = normalize(&input).unwrap();
        assert_eq!(once.len(), 1);
        assert_eq!(normalize(&once).unwrap(), once);
        let over = (0..2500)
            .map(|i| segment(i, (0, i as i32), (1, i as i32)))
            .collect::<Vec<_>>();
        assert_eq!(normalize(&over), Err(GeometryError::WorkLimit));
    }
}
