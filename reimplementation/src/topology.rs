//! Toolkit-independent geometric connectivity for orthogonal circuit segments.
//!
//! Only equal segment endpoints are vertices: crossings and pins in segment interiors do not
//! connect. Coincident pins share the same point vertex, including when no segment is present.
use crate::domain::PinRef;
use std::collections::{BTreeMap, HashMap};

pub const MAX_TOPOLOGY_ITEMS: usize = 80_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinPoint {
    pub pin: PinRef,
    pub point: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub a: [f32; 2],
    pub b: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopologyError {
    WorkLimit,
    InvalidCoordinate,
    ZeroLength,
    Diagonal,
}

impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WorkLimit => "geometric connectivity work limit exceeded",
            Self::InvalidCoordinate => "geometric connectivity has a non-finite coordinate",
            Self::ZeroLength => "geometric connectivity has a zero-length segment",
            Self::Diagonal => "geometric connectivity supports orthogonal segments only",
        })
    }
}
impl std::error::Error for TopologyError {}

/// Resolve stable, sorted pin partitions. Endpoints are unioned exactly; there is deliberately
/// no point-on-segment test, so a raw unsplit T contact and a proper crossing remain disconnected.
pub fn resolve(pins: &[PinPoint], segments: &[Segment]) -> Result<Vec<Vec<PinRef>>, TopologyError> {
    let work = pins
        .len()
        .checked_add(segments.len().saturating_mul(2))
        .ok_or(TopologyError::WorkLimit)?;
    if work > MAX_TOPOLOGY_ITEMS {
        return Err(TopologyError::WorkLimit);
    }
    let mut parent = Vec::<usize>::with_capacity(work);
    let mut vertices = HashMap::<(u32, u32), usize>::with_capacity(work);
    for pin in pins {
        intern(pin.point, &mut vertices, &mut parent)?;
    }
    for segment in segments {
        if segment.a == segment.b {
            return Err(TopologyError::ZeroLength);
        }
        if segment.a[0] != segment.b[0] && segment.a[1] != segment.b[1] {
            return Err(TopologyError::Diagonal);
        }
        let a = intern(segment.a, &mut vertices, &mut parent)?;
        let b = intern(segment.b, &mut vertices, &mut parent)?;
        union(&mut parent, a, b);
    }
    let mut groups = BTreeMap::<usize, Vec<PinRef>>::new();
    for pin in pins {
        let vertex = vertices[&(bits(pin.point[0]), bits(pin.point[1]))];
        groups
            .entry(root(&mut parent, vertex))
            .or_default()
            .push(pin.pin);
    }
    let mut result: Vec<_> = groups.into_values().collect();
    for group in &mut result {
        group.sort();
    }
    result.sort();
    Ok(result)
}

fn bits(value: f32) -> u32 {
    if value == 0.0 { 0 } else { value.to_bits() }
}
fn intern(
    point: [f32; 2],
    vertices: &mut HashMap<(u32, u32), usize>,
    parent: &mut Vec<usize>,
) -> Result<usize, TopologyError> {
    if !point[0].is_finite() || !point[1].is_finite() {
        return Err(TopologyError::InvalidCoordinate);
    }
    let key = (bits(point[0]), bits(point[1]));
    if let Some(index) = vertices.get(&key) {
        return Ok(*index);
    }
    let index = parent.len();
    parent.push(index);
    vertices.insert(key, index);
    Ok(index)
}
fn root(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
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
    fn pin(component: u64, point: [f32; 2]) -> PinPoint {
        PinPoint {
            pin: PinRef {
                component: ComponentId(component),
                pin: PinId {
                    direction: PinDirection::Input,
                    index: 0,
                },
            },
            point,
        }
    }

    #[test]
    fn crossing_t_contact_and_endpoint_incidence_follow_endpoint_only_rule() {
        let pins = [
            pin(1, [-10., 0.]),
            pin(2, [10., 0.]),
            pin(3, [0., -10.]),
            pin(4, [0., 10.]),
        ];
        let crossing = [
            Segment {
                a: [-10., 0.],
                b: [10., 0.],
            },
            Segment {
                a: [0., -10.],
                b: [0., 10.],
            },
        ];
        let crossing_nets = resolve(&pins, &crossing).unwrap();
        assert_eq!(crossing_nets.len(), 2);
        assert!(crossing_nets.contains(&vec![pins[0].pin, pins[1].pin]));
        assert!(crossing_nets.contains(&vec![pins[2].pin, pins[3].pin]));
        let split_junction = [
            Segment {
                a: [-10., 0.],
                b: [0., 0.],
            },
            Segment {
                a: [0., 0.],
                b: [10., 0.],
            },
            Segment {
                a: [0., 0.],
                b: [0., -10.],
            },
            Segment {
                a: [0., 0.],
                b: [0., 10.],
            },
        ];
        assert_eq!(
            resolve(&pins, &split_junction).unwrap(),
            vec![pins.iter().map(|p| p.pin).collect::<Vec<_>>()]
        );
        let raw_t = [
            Segment {
                a: [-10., 0.],
                b: [10., 0.],
            },
            Segment {
                a: [0., 0.],
                b: [0., 10.],
            },
        ];
        assert_eq!(resolve(&pins[..3], &raw_t).unwrap().len(), 2);
        // A pin physically on the interior is not an endpoint and must not attach.
        let on_interior = [pin(10, [-10., 0.]), pin(11, [10., 0.]), pin(12, [0., 0.])];
        let line = [Segment {
            a: [-10., 0.],
            b: [10., 0.],
        }];
        assert_eq!(resolve(&on_interior, &line).unwrap().len(), 2);
    }

    #[test]
    fn endpoint_fanout_transitivity_coincident_pins_and_permutations() {
        let pins = [
            pin(1, [0., 0.]),
            pin(2, [10., 0.]),
            pin(3, [10., 10.]),
            pin(4, [0., 0.]),
        ];
        let segments = [
            Segment {
                a: [0., 0.],
                b: [10., 0.],
            },
            Segment {
                a: [10., 0.],
                b: [10., 10.],
            },
        ];
        let expected = resolve(&pins, &segments).unwrap();
        assert_eq!(expected.len(), 1);
        let mut reversed = segments;
        reversed.reverse();
        assert_eq!(resolve(&pins, &reversed).unwrap(), expected);
        assert_eq!(resolve(&pins, &[]).unwrap()[0].len(), 2);
    }

    #[test]
    fn topology_rejects_bad_segments_and_bounds_input() {
        assert_eq!(
            resolve(
                &[],
                &[Segment {
                    a: [0., 0.],
                    b: [0., 0.]
                }]
            ),
            Err(TopologyError::ZeroLength)
        );
        assert_eq!(
            resolve(
                &[],
                &[Segment {
                    a: [0., 0.],
                    b: [1., 1.]
                }]
            ),
            Err(TopologyError::Diagonal)
        );
        assert_eq!(
            resolve(
                &[],
                &[Segment {
                    a: [f32::NAN, 0.],
                    b: [1., 0.]
                }]
            ),
            Err(TopologyError::InvalidCoordinate)
        );
        let pins = vec![pin(1, [0., 0.]); MAX_TOPOLOGY_ITEMS + 1];
        assert_eq!(resolve(&pins, &[]), Err(TopologyError::WorkLimit));
    }
}
