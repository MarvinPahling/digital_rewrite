//! Integer endpoint-only geometric connectivity. Crossings and pin-interior contacts never join.
use crate::{
    domain::PinRef,
    geometric_wiring::{Point, Segment},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const MAX_TOPOLOGY_ITEMS: usize = 80_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinPoint {
    pub pin: PinRef,
    pub point: Point,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedNet {
    pub pins: Vec<PinRef>,
    pub segment_ids: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopologyError {
    WorkLimit,
    ZeroLength(u64),
    DuplicateSegment(u64),
}
impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TopologyError {}

/// Return deterministic pin partitions, including empty partitions for wire-only connected nets.
pub fn resolve(pins: &[PinPoint], segments: &[Segment]) -> Result<Vec<Vec<PinRef>>, TopologyError> {
    Ok(resolve_detailed(pins, segments)?
        .into_iter()
        .map(|net| net.pins)
        .collect())
}
pub fn resolve_detailed(
    pins: &[PinPoint],
    segments: &[Segment],
) -> Result<Vec<ResolvedNet>, TopologyError> {
    let work = pins
        .len()
        .checked_add(segments.len().saturating_mul(2))
        .ok_or(TopologyError::WorkLimit)?;
    if work > MAX_TOPOLOGY_ITEMS {
        return Err(TopologyError::WorkLimit);
    }
    let mut ids = BTreeSet::new();
    for s in segments {
        if s.a == s.b {
            return Err(TopologyError::ZeroLength(s.id));
        }
        if !ids.insert(s.id) {
            return Err(TopologyError::DuplicateSegment(s.id));
        }
    }
    let mut parent = Vec::with_capacity(work);
    let mut vertices = HashMap::<Point, usize>::with_capacity(work);
    for pin in pins {
        intern(pin.point, &mut vertices, &mut parent);
    }
    for s in segments {
        let a = intern(s.a, &mut vertices, &mut parent);
        let b = intern(s.b, &mut vertices, &mut parent);
        union(&mut parent, a, b);
    }
    let mut groups = BTreeMap::<usize, ResolvedNet>::new();
    for pin in pins {
        let n = root(&mut parent, vertices[&pin.point]);
        groups
            .entry(n)
            .or_insert_with(|| ResolvedNet {
                pins: Vec::new(),
                segment_ids: Vec::new(),
            })
            .pins
            .push(pin.pin);
    }
    for s in segments {
        let n = root(&mut parent, vertices[&s.a]);
        groups
            .entry(n)
            .or_insert_with(|| ResolvedNet {
                pins: Vec::new(),
                segment_ids: Vec::new(),
            })
            .segment_ids
            .push(s.id);
    }
    let mut result: Vec<_> = groups.into_values().collect();
    for net in &mut result {
        net.pins.sort();
        net.segment_ids.sort();
    }
    result.sort_by(|a, b| a.pins.cmp(&b.pins).then(a.segment_ids.cmp(&b.segment_ids)));
    Ok(result)
}
fn intern(point: Point, vertices: &mut HashMap<Point, usize>, parent: &mut Vec<usize>) -> usize {
    if let Some(&index) = vertices.get(&point) {
        return index;
    }
    let index = parent.len();
    parent.push(index);
    vertices.insert(point, index);
    index
}
fn root(parent: &mut [usize], mut n: usize) -> usize {
    while parent[n] != n {
        parent[n] = parent[parent[n]];
        n = parent[n];
    }
    n
}
fn union(parent: &mut [usize], a: usize, b: usize) {
    let a = root(parent, a);
    let b = root(parent, b);
    if a != b {
        parent[a] = b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ComponentId, PinDirection, PinId};
    fn pin(id: u64, x: i32, y: i32) -> PinPoint {
        PinPoint {
            pin: PinRef {
                component: ComponentId(id),
                pin: PinId {
                    direction: PinDirection::Input,
                    index: 0,
                },
            },
            point: Point::new(x, y),
        }
    }
    fn s(id: u64, a: (i32, i32), b: (i32, i32)) -> Segment {
        Segment {
            id,
            a: Point::new(a.0, a.1),
            b: Point::new(b.0, b.1),
        }
    }
    #[test]
    fn endpoint_only_crossings_t_contacts_and_explicit_fourway() {
        let p = [pin(1, -10, 0), pin(2, 10, 0), pin(3, 0, -10), pin(4, 0, 10)];
        assert_eq!(
            resolve(&p, &[s(1, (-10, 0), (10, 0)), s(2, (0, -10), (0, 10))])
                .unwrap()
                .len(),
            2
        );
        let t_pins = [p[0], p[1], pin(9, 0, 0)];
        assert_eq!(
            resolve(&t_pins, &[s(1, (-10, 0), (10, 0)), s(2, (0, 0), (0, 10))])
                .unwrap()
                .len(),
            2
        );
        let fourway = [
            s(1, (-10, 0), (0, 0)),
            s(2, (0, 0), (10, 0)),
            s(3, (0, 0), (0, -10)),
            s(4, (0, 0), (0, 10)),
        ];
        let expected = resolve(&p, &fourway).unwrap();
        assert_eq!(expected.len(), 1);
        let reversed = [fourway[3], fourway[2], fourway[1], fourway[0]];
        assert_eq!(resolve(&p, &reversed).unwrap(), expected);
        assert_eq!(
            resolve(
                &[pin(1, -10, 0), pin(2, 10, 0), pin(3, 0, 0)],
                &[s(1, (-10, 0), (10, 0))]
            )
            .unwrap()
            .len(),
            2
        );
    }
    #[test]
    fn diagonal_endpoint_edges_coincident_pins_and_permutations() {
        let p = [pin(1, 0, 0), pin(2, 10, 10), pin(3, 0, 0)];
        let segments = [s(7, (0, 0), (10, 10))];
        assert_eq!(
            resolve(&p, &segments).unwrap(),
            resolve(&p, &[segments[0]]).unwrap()
        );
        assert_eq!(
            resolve(&p, &[]).unwrap(),
            vec![vec![p[0].pin, p[2].pin], vec![p[1].pin]]
        );
        assert_eq!(
            resolve(&[], &[s(1, (0, 0), (10, 10))]).unwrap(),
            vec![vec![]]
        );
    }
    #[test]
    fn overlapping_raw_segments_are_not_silently_normalized() {
        let overlapping = [s(1, (0, 0), (10, 0)), s(2, (5, 0), (15, 0))];
        assert_eq!(resolve(&[], &overlapping).unwrap(), vec![vec![], vec![]]);
    }

    #[test]
    fn raw_t_is_not_normalized_and_invalid_segments_are_located() {
        assert_eq!(
            resolve(&[], &[s(4, (0, 0), (0, 0))]),
            Err(TopologyError::ZeroLength(4))
        );
        assert_eq!(
            resolve(&[], &[s(4, (0, 0), (1, 0)), s(4, (2, 0), (3, 0))]),
            Err(TopologyError::DuplicateSegment(4))
        );
        assert_eq!(
            resolve(&vec![pin(1, 0, 0); MAX_TOPOLOGY_ITEMS + 1], &[]),
            Err(TopologyError::WorkLimit)
        );
    }
}
