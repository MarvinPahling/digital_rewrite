//! Integer geometric wiring and transactional editing primitives.
//!
//! This is an opt-in staged model. The existing document/compiler/codec still consume legacy
//! pin-pair wiring; callers must explicitly select a mode and cannot infer geometric behavior.
use crate::domain::Wire;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SEGMENTS: usize = 30_000;
pub const MAX_NORMALIZATION_WORK: usize = 80_000;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wiring {
    LegacyPinPairs(Vec<Wire>),
    GeometricSegments(Vec<Segment>),
}
impl Default for Wiring {
    fn default() -> Self {
        Self::LegacyPinPairs(Vec::new())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    WrongMode,
    ZeroLength,
    DuplicateId(u64),
    IdExhausted,
    SegmentLimit,
    WorkLimit,
    SimulationActive,
    InvalidMigration(&'static str),
}
impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GeometryError {}

impl Wiring {
    pub fn geometric(&self) -> Result<&[Segment], GeometryError> {
        match self {
            Self::GeometricSegments(s) => Ok(s),
            Self::LegacyPinPairs(_) => Err(GeometryError::WrongMode),
        }
    }
    /// Legacy pin-pair documents do not carry the canonical route coordinates required for
    /// migration. Refuse rather than silently replace their wires with an empty geometric model.
    pub fn migrate(&self) -> Result<Self, GeometryError> {
        match self {
            Self::LegacyPinPairs(_) => Err(GeometryError::InvalidMigration(
                "legacy pin-pair routes are unavailable",
            )),
            Self::GeometricSegments(_) => Err(GeometryError::WrongMode),
        }
    }
}

/// Canonicalizes orthogonal intervals and endpoint contacts. Proper interior crossings do not
/// create vertices. Diagonal segments are retained (except duplicate edges) and only their
/// endpoints can split another segment.
pub fn normalize(input: &[Segment]) -> Result<Vec<Segment>, GeometryError> {
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
    if pairs > MAX_NORMALIZATION_WORK {
        return Err(GeometryError::WorkLimit);
    }
    let mut cuts: Vec<BTreeSet<Point>> = input.iter().map(|s| BTreeSet::from([s.a, s.b])).collect();
    for i in 0..input.len() {
        for j in i + 1..input.len() {
            let a = input[i];
            let b = input[j];
            // Endpoints lying on another segment are the only cross-segment split contacts.
            for p in [a.a, a.b] {
                if on_segment(p, b) {
                    cuts[j].insert(p);
                }
            }
            for p in [b.a, b.b] {
                if on_segment(p, a) {
                    cuts[i].insert(p);
                }
            }
            // Orthogonal proper crossings remain unconnected. Collinear overlap boundaries are
            // already endpoint cuts; adjacent intervals will be merged below.
        }
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
    merge_collinear(&mut out);
    out.sort_by_key(|s| (s.a, s.b, s.id));
    if out.len() > MAX_SEGMENTS {
        return Err(GeometryError::SegmentLimit);
    }
    // Ensure IDs are unique while preserving unchanged IDs and deterministic source ownership.
    let mut used = BTreeSet::new();
    let mut next = input.iter().map(|s| s.id).max().unwrap_or(0);
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
fn merge_collinear(segments: &mut Vec<Segment>) {
    // Preserve branches and all diagonal segments. Merge only orthogonal connected intervals
    // having no non-collinear segment endpoint at their common location.
    let mut protected = BTreeSet::new();
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

/// Small validated command/history owner used by headless and adapter clients.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GeometricSession {
    wiring: Wiring,
    undo: Vec<Wiring>,
    redo: Vec<Wiring>,
    locked: bool,
    next_id: u64,
}
impl GeometricSession {
    pub fn new() -> Self {
        Self {
            wiring: Wiring::GeometricSegments(Vec::new()),
            ..Self::default()
        }
    }
    fn ensure_editable(&self) -> Result<(), GeometryError> {
        if self.locked {
            Err(GeometryError::SimulationActive)
        } else {
            Ok(())
        }
    }
    pub fn wiring(&self) -> &Wiring {
        &self.wiring
    }
    pub fn set_locked(&mut self, value: bool) {
        self.locked = value;
    }
    fn commit(&mut self, candidate: Wiring) -> Result<bool, GeometryError> {
        if self.locked {
            return Err(GeometryError::SimulationActive);
        }
        if candidate == self.wiring {
            return Ok(false);
        }
        self.undo.push(self.wiring.clone());
        self.wiring = candidate;
        self.redo.clear();
        Ok(true)
    }
    pub fn add_batch(&mut self, segments: &[(Point, Point)]) -> Result<bool, GeometryError> {
        self.ensure_editable()?;
        let mut all = self.wiring.geometric()?.to_vec();
        let mut next = self.next_id;
        for &(a, b) in segments {
            if a == b {
                return Err(GeometryError::ZeroLength);
            }
            all.push(Segment { id: next, a, b });
            next = next.checked_add(1).ok_or(GeometryError::IdExhausted)?;
        }
        let norm = normalize(&all)?;
        if all.len() == self.wiring.geometric()?.len() || norm.is_empty() {
            return Ok(false);
        }
        let changed = self.commit(Wiring::GeometricSegments(norm.clone()))?;
        if changed {
            if let Some(following) = norm.iter().map(|s| s.id).max().unwrap_or(0).checked_add(1) {
                self.next_id = following.max(next);
            } else {
                self.next_id = u64::MAX;
            }
        }
        Ok(changed)
    }
    /// Splitting a bare straight segment normalizes back to its original shape (a no-op).
    /// Add the branch with `add_batch` in the same edit to commit an actual junction.
    pub fn split_segment(&mut self, id: u64, point: Point) -> Result<bool, GeometryError> {
        self.ensure_editable()?;
        let mut all = self.wiring.geometric()?.to_vec();
        let Some(index) = all.iter().position(|s| s.id == id) else {
            return Ok(false);
        };
        let segment = all[index];
        if !on_segment(point, segment) {
            return Err(GeometryError::InvalidMigration(
                "split point is not on segment",
            ));
        }
        if point == segment.a || point == segment.b {
            return Ok(false);
        }
        let new_id = self.next_id;
        let next = new_id.checked_add(1).ok_or(GeometryError::IdExhausted)?;
        all[index] = Segment {
            id,
            a: segment.a,
            b: point,
        };
        all.push(Segment {
            id: new_id,
            a: point,
            b: segment.b,
        });
        let candidate_segments = normalize(&all)?;
        let maximum = candidate_segments.iter().map(|s| s.id).max().unwrap_or(0);
        let following = if let Some(following) = maximum.checked_add(1) {
            following.max(next)
        } else {
            u64::MAX
        };
        let changed = self.commit(Wiring::GeometricSegments(candidate_segments))?;
        if changed {
            self.next_id = following;
        }
        Ok(changed)
    }
    /// Apply canonical overlap/contact merging as one document edit. Already-normalized input is
    /// a no-op and therefore preserves redo history and identifiers.
    pub fn merge_collinear(&mut self) -> Result<bool, GeometryError> {
        self.ensure_editable()?;
        let candidate = Wiring::GeometricSegments(normalize(self.wiring.geometric()?)?);
        self.commit(candidate)
    }
    pub fn delete(&mut self, id: u64) -> Result<bool, GeometryError> {
        self.ensure_editable()?;
        let mut all = self.wiring.geometric()?.to_vec();
        let n = all.len();
        all.retain(|s| s.id != id);
        if n == all.len() {
            return Ok(false);
        }
        let candidate = Wiring::GeometricSegments(normalize(&all)?);
        self.commit(candidate)
    }
    pub fn move_segment(&mut self, id: u64, dx: i32, dy: i32) -> Result<bool, GeometryError> {
        self.ensure_editable()?;
        let mut all = self.wiring.geometric()?.to_vec();
        let Some(s) = all.iter_mut().find(|s| s.id == id) else {
            return Ok(false);
        };
        let shift = |p: Point| Some(Point::new(p.x.checked_add(dx)?, p.y.checked_add(dy)?));
        s.a = shift(s.a).ok_or(GeometryError::InvalidMigration("coordinate overflow"))?;
        s.b = shift(s.b).ok_or(GeometryError::InvalidMigration("coordinate overflow"))?;
        self.commit(Wiring::GeometricSegments(normalize(&all)?))
    }
    pub fn move_endpoint(
        &mut self,
        id: u64,
        endpoint: usize,
        p: Point,
    ) -> Result<bool, GeometryError> {
        self.ensure_editable()?;
        if endpoint > 1 {
            return Err(GeometryError::InvalidMigration("endpoint index"));
        }
        let mut all = self.wiring.geometric()?.to_vec();
        let Some(s) = all.iter_mut().find(|s| s.id == id) else {
            return Ok(false);
        };
        if endpoint == 0 {
            s.a = p
        } else {
            s.b = p
        }
        self.commit(Wiring::GeometricSegments(normalize(&all)?))
    }
    pub fn undo(&mut self) -> Result<bool, GeometryError> {
        if self.locked {
            return Err(GeometryError::SimulationActive);
        }
        if let Some(p) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.wiring, p));
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn redo(&mut self) -> Result<bool, GeometryError> {
        if self.locked {
            return Err(GeometryError::SimulationActive);
        }
        if let Some(n) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.wiring, n));
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(id: u64, a: (i32, i32), b: (i32, i32)) -> Segment {
        Segment {
            id,
            a: Point::new(a.0, a.1),
            b: Point::new(b.0, b.1),
        }
    }
    #[test]
    fn splits_endpoint_contacts_but_not_proper_crossings_and_preserves_diagonals() {
        let t = normalize(&[s(1, (0, 0), (10, 0)), s(2, (5, 0), (5, 5))]).unwrap();
        assert!(
            t.iter()
                .any(|x| x.a == Point::new(5, 0) || x.b == Point::new(5, 0))
        );
        let cross = normalize(&[s(1, (0, 0), (10, 0)), s(2, (5, -5), (5, 5))]).unwrap();
        assert_eq!(cross.len(), 2);
        assert_eq!(
            normalize(&[s(4, (0, 0), (5, 7))]).unwrap()[0].b,
            Point::new(5, 7)
        );
    }
    #[test]
    fn diagonal_endpoint_is_a_branch_and_extreme_integer_predicates_do_not_overflow() {
        let branch = normalize(&[
            s(1, (i32::MIN, 0), (i32::MAX, 0)),
            s(2, (0, 0), (i32::MAX, i32::MAX)),
        ])
        .unwrap();
        assert_eq!(branch.len(), 3);
        assert_eq!(
            normalize(&[s(1, (i32::MIN, i32::MIN), (i32::MAX, i32::MAX))])
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn overlap_normalizes_deterministically_and_is_idempotent() {
        let a = [s(8, (0, 0), (10, 0)), s(9, (5, 0), (15, 0))];
        let n = normalize(&a).unwrap();
        assert_eq!(n, normalize(&n).unwrap());
        assert_eq!(n.len(), 1);
        assert_eq!((n[0].a, n[0].b), (Point::new(0, 0), Point::new(15, 0)));
    }
    #[test]
    fn transactional_operations_reject_zero_overflow_and_lock_without_history_change() {
        let mut e = GeometricSession::new();
        assert_eq!(
            e.add_batch(&[(Point::new(0, 0), Point::new(0, 0))]),
            Err(GeometryError::ZeroLength)
        );
        assert!(
            e.add_batch(&[(Point::new(0, 0), Point::new(10, 0))])
                .unwrap()
        );
        let before = e.clone();
        assert!(e.move_segment(0, i32::MAX, 0).is_err());
        assert_eq!(e, before);
        e.set_locked(true);
        assert_eq!(
            e.add_batch(&[(Point::new(1, 1), Point::new(1, 1))]),
            Err(GeometryError::SimulationActive)
        );
        assert!(e.delete(0).is_err());
        assert_eq!(e.wiring(), before.wiring());
        e.set_locked(false);
        assert!(e.undo().unwrap());
        assert!(e.redo().unwrap());
    }
    #[test]
    fn straight_split_is_noop_and_branch_insertion_is_atomic() {
        let mut editor = GeometricSession::new();
        editor
            .add_batch(&[(Point::new(0, 0), Point::new(10, 0))])
            .unwrap();
        let before = editor.clone();
        assert!(!editor.split_segment(0, Point::new(5, 0)).unwrap());
        assert_eq!(editor, before);
        assert!(
            editor
                .add_batch(&[(Point::new(5, 0), Point::new(5, 5))])
                .unwrap()
        );
        assert_eq!(editor.wiring().geometric().unwrap().len(), 3);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.wiring().geometric().unwrap().len(), 1);
    }
    #[test]
    fn checked_limits() {
        let too_many = vec![s(0, (0, 0), (1, 0)); MAX_SEGMENTS + 1];
        assert_eq!(normalize(&too_many), Err(GeometryError::SegmentLimit));
        let pair_work = (0..401)
            .map(|i| s(i, (0, i as i32), (1, i as i32)))
            .collect::<Vec<_>>();
        assert_eq!(normalize(&pair_work), Err(GeometryError::WorkLimit));
    }
}
