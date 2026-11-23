//! Bounded compiler for one-bit acyclic circuits with explicit-pin connectivity.
//! Geometry never joins nets. Floating receiving pins and multiple drivers are errors.
use crate::domain::*;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Limits are checked before allocating compiler work proportional to the document.
pub const MAX_COMPONENTS: usize = 10_000;
pub const MAX_WIRES: usize = 30_000;
/// Upper bound on pins plus wire endpoints plus component dependency edges.
pub const MAX_WORK: usize = 80_000;

#[derive(Debug, PartialEq, Eq)]
pub enum CompileError {
    ComponentLimit,
    WireLimit,
    WorkLimit,
    DuplicateComponent(ComponentId),
    DuplicateWire(WireId),
    InvalidEndpoint { wire: WireId, pin: PinRef },
    MissingDriver { pins: Vec<PinRef> },
    MultipleDrivers { pins: Vec<PinRef> },
    Cycle { components: Vec<ComponentId> },
}
#[derive(Debug, PartialEq, Eq)]
pub struct Net {
    pub driver: Option<PinRef>,
    pub receivers: Vec<PinRef>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct CompiledCircuit {
    pub nets: Vec<Net>,
    pub order: Vec<ComponentId>,
}

fn root(parent: &mut [usize], mut n: usize) -> usize {
    while parent[n] != n {
        parent[n] = parent[parent[n]];
        n = parent[n];
    }
    n
}

pub fn compile(document: &Document) -> Result<CompiledCircuit, CompileError> {
    if document.components.len() > MAX_COMPONENTS {
        return Err(CompileError::ComponentLimit);
    }
    if document.wires.len() > MAX_WIRES {
        return Err(CompileError::WireLimit);
    }
    // Each component has at most three pins and two incoming dependencies.
    let work = document
        .components
        .len()
        .saturating_mul(5)
        .saturating_add(document.wires.len().saturating_mul(2));
    if work > MAX_WORK {
        return Err(CompileError::WorkLimit);
    }
    let mut components = HashMap::new();
    let mut pins = Vec::new();
    let mut indices = HashMap::new();
    for (i, component) in document.components.iter().enumerate() {
        if components.insert(component.id, i).is_some() {
            return Err(CompileError::DuplicateComponent(component.id));
        }
        for direction in [PinDirection::Input, PinDirection::Output] {
            for index in 0..2 {
                let pin = PinId { direction, index };
                if component.kind.has_pin(pin) {
                    let pin = PinRef {
                        component: component.id,
                        pin,
                    };
                    indices.insert(pin, pins.len());
                    pins.push(pin);
                }
            }
        }
    }
    let mut parent: Vec<_> = (0..pins.len()).collect();
    let mut wire_ids = HashSet::new();
    for wire in &document.wires {
        if !wire_ids.insert(wire.id) {
            return Err(CompileError::DuplicateWire(wire.id));
        }
        let mut ends = [0; 2];
        for (i, pin) in wire.endpoints.iter().enumerate() {
            ends[i] = *indices.get(pin).ok_or(CompileError::InvalidEndpoint {
                wire: wire.id,
                pin: *pin,
            })?;
        }
        let a = root(&mut parent, ends[0]);
        let b = root(&mut parent, ends[1]);
        parent[a] = b;
    }
    let mut groups: BTreeMap<usize, Vec<PinRef>> = BTreeMap::new();
    for (i, pin) in pins.iter().enumerate() {
        groups.entry(root(&mut parent, i)).or_default().push(*pin);
    }
    let mut nets = Vec::new();
    let mut edges = vec![Vec::new(); document.components.len()];
    let mut degrees = vec![0usize; document.components.len()];
    for pins in groups.into_values() {
        let drivers: Vec<_> = pins
            .iter()
            .copied()
            .filter(|p| p.pin.direction == PinDirection::Output)
            .collect();
        let receivers: Vec<_> = pins
            .iter()
            .copied()
            .filter(|p| p.pin.direction == PinDirection::Input)
            .collect();
        if drivers.len() > 1 {
            return Err(CompileError::MultipleDrivers { pins });
        }
        if drivers.is_empty() && !receivers.is_empty() {
            return Err(CompileError::MissingDriver { pins });
        }
        if let Some(driver) = drivers.first() {
            for receiver in &receivers {
                let from = components[&driver.component];
                let to = components[&receiver.component];
                edges[from].push(to);
                degrees[to] += 1;
            }
        }
        nets.push(Net {
            driver: drivers.first().copied(),
            receivers,
        });
    }
    let mut queue: VecDeque<_> = degrees
        .iter()
        .enumerate()
        .filter_map(|(i, d)| (*d == 0).then_some(i))
        .collect();
    let mut order = Vec::new();
    while let Some(i) = queue.pop_front() {
        order.push(document.components[i].id);
        for &next in &edges[i] {
            degrees[next] -= 1;
            if degrees[next] == 0 {
                queue.push_back(next);
            }
        }
    }
    if order.len() != document.components.len() {
        // Iterative Kosaraju traversal avoids recursion on large user documents.
        let mut reverse = vec![Vec::new(); edges.len()];
        for (from, targets) in edges.iter().enumerate() {
            for &to in targets {
                reverse[to].push(from);
            }
        }
        let mut seen = vec![false; edges.len()];
        let mut finish = Vec::new();
        for start in 0..edges.len() {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![(start, 0)];
            while let Some((node, next)) = stack.last_mut() {
                if *next == edges[*node].len() {
                    finish.push(*node);
                    stack.pop();
                } else {
                    let target = edges[*node][*next];
                    *next += 1;
                    if !seen[target] {
                        seen[target] = true;
                        stack.push((target, 0));
                    }
                }
            }
        }
        seen.fill(false);
        let mut cyclic = vec![false; edges.len()];
        for &start in finish.iter().rev() {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![start];
            let mut group = Vec::new();
            while let Some(node) = stack.pop() {
                group.push(node);
                for &target in &reverse[node] {
                    if !seen[target] {
                        seen[target] = true;
                        stack.push(target);
                    }
                }
            }
            if group.len() > 1 || edges[start].contains(&start) {
                for node in group {
                    cyclic[node] = true;
                }
            }
        }
        return Err(CompileError::Cycle {
            components: cyclic
                .iter()
                .enumerate()
                .filter_map(|(i, &yes)| yes.then_some(document.components[i].id))
                .collect(),
        });
    }
    Ok(CompiledCircuit { nets, order })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn component(id: u64, kind: ComponentKind) -> Component {
        Component {
            id: ComponentId(id),
            kind,
            position: Position { x: 0.0, y: 0.0 },
            label: String::new(),
            constant_value: true,
        }
    }
    fn pin(id: u64, direction: PinDirection, index: u8) -> PinRef {
        PinRef {
            component: ComponentId(id),
            pin: PinId { direction, index },
        }
    }
    fn wire(id: u64, a: PinRef, b: PinRef) -> Wire {
        Wire {
            id: WireId(id),
            endpoints: [a, b],
        }
    }
    #[test]
    fn transitive_fanout_and_geometry() {
        let o = pin(1, PinDirection::Output, 0);
        let a = pin(2, PinDirection::Input, 0);
        let b = pin(3, PinDirection::Input, 0);
        let mut d = Document {
            components: vec![
                component(1, ComponentKind::Input),
                component(2, ComponentKind::Output),
                component(3, ComponentKind::Output),
            ],
            wires: vec![wire(1, a, o), wire(2, b, a)],
        };
        assert_eq!(compile(&d).unwrap().nets[0].receivers.len(), 2);
        d.wires.pop();
        assert!(matches!(
            compile(&d),
            Err(CompileError::MissingDriver { .. })
        ));
    }
    #[test]
    fn invalid_nets_and_identities() {
        let a = pin(1, PinDirection::Output, 0);
        let b = pin(2, PinDirection::Output, 0);
        let mut d = Document {
            components: vec![
                component(1, ComponentKind::Input),
                component(2, ComponentKind::Input),
            ],
            wires: vec![wire(1, a, b)],
        };
        assert!(matches!(
            compile(&d),
            Err(CompileError::MultipleDrivers { .. })
        ));
        d.wires[0].endpoints[1].pin.index = 4;
        assert!(matches!(
            compile(&d),
            Err(CompileError::InvalidEndpoint { .. })
        ));
        d.components.push(component(1, ComponentKind::Input));
        assert!(matches!(
            compile(&d),
            Err(CompileError::DuplicateComponent(_))
        ));
        d.components.pop();
        d.wires[0].endpoints[1] = a;
        d.wires.push(d.wires[0].clone());
        assert!(matches!(compile(&d), Err(CompileError::DuplicateWire(_))));
    }
    #[test]
    fn cycles_and_empty() {
        assert!(compile(&Document::default()).is_ok());
        assert!(
            compile(&Document {
                components: vec![component(1, ComponentKind::Input)],
                wires: vec![]
            })
            .is_ok()
        );
        let input = |n| pin(n, PinDirection::Input, 0);
        let output = |n| pin(n, PinDirection::Output, 0);
        let mut d = Document {
            components: vec![component(1, ComponentKind::Not)],
            wires: vec![wire(1, input(1), output(1))],
        };
        assert!(matches!(compile(&d), Err(CompileError::Cycle { .. })));
        d.components.push(component(2, ComponentKind::Not));
        d.wires = vec![wire(1, output(1), input(2)), wire(2, output(2), input(1))];
        assert!(matches!(compile(&d), Err(CompileError::Cycle { .. })));
    }
    #[test]
    fn limits() {
        let mut d = Document {
            components: (0..MAX_COMPONENTS as u64)
                .map(|id| component(id, ComponentKind::Input))
                .collect(),
            wires: vec![],
        };
        assert!(compile(&d).is_ok());
        d.components
            .push(component(MAX_COMPONENTS as u64, ComponentKind::Input));
        assert_eq!(compile(&d), Err(CompileError::ComponentLimit));
        d.components.clear();
        let w = wire(
            0,
            pin(0, PinDirection::Output, 0),
            pin(0, PinDirection::Output, 0),
        );
        d.wires = vec![w; MAX_WIRES + 1];
        assert_eq!(compile(&d), Err(CompileError::WireLimit));
        d.wires.pop();
        assert!(matches!(
            compile(&d),
            Err(CompileError::InvalidEndpoint { .. })
        ));
        d.components = (0..MAX_COMPONENTS as u64)
            .map(|id| component(id, ComponentKind::Input))
            .collect();
        assert_eq!(compile(&d), Err(CompileError::WorkLimit));
    }
    #[test]
    fn exact_work_and_wire_boundaries() {
        let output = pin(0, PinDirection::Output, 0);
        let mut d = Document {
            components: vec![component(0, ComponentKind::Input)],
            wires: (0..MAX_WIRES as u64)
                .map(|i| wire(i, output, output))
                .collect(),
        };
        assert!(compile(&d).is_ok());
        d.wires.push(wire(MAX_WIRES as u64, output, output));
        assert_eq!(compile(&d), Err(CompileError::WireLimit));
        d.components = (0..MAX_COMPONENTS as u64)
            .map(|i| component(i, ComponentKind::Input))
            .collect();
        d.wires.truncate((MAX_WORK - MAX_COMPONENTS * 5) / 2);
        assert!(compile(&d).is_ok());
        d.components.pop();
        d.wires
            .extend((20000..20003).map(|id| wire(id, output, output)));
        assert_eq!(compile(&d), Err(CompileError::WorkLimit));
    }
    #[test]
    fn only_cycle_members_reported() {
        let i = |n| pin(n, PinDirection::Input, 0);
        let o = |n| pin(n, PinDirection::Output, 0);
        let mut d = Document {
            components: vec![
                component(1, ComponentKind::Not),
                component(2, ComponentKind::Not),
                component(3, ComponentKind::Output),
            ],
            wires: vec![
                wire(1, o(1), i(2)),
                wire(2, o(2), i(1)),
                wire(3, o(2), i(3)),
            ],
        };
        assert_eq!(
            compile(&d),
            Err(CompileError::Cycle {
                components: vec![ComponentId(1), ComponentId(2)]
            })
        );
        d.wires = vec![
            wire(1, o(1), i(1)),
            wire(2, o(1), i(2)),
            wire(3, o(2), i(3)),
        ];
        assert_eq!(
            compile(&d),
            Err(CompileError::Cycle {
                components: vec![ComponentId(1)]
            })
        );
    }
    #[test]
    fn floating_pins_and_coincident_geometry() {
        let i = |n| pin(n, PinDirection::Input, 0);
        let o = |n| pin(n, PinDirection::Output, 0);
        let mut d = Document {
            components: vec![
                component(1, ComponentKind::Output),
                component(2, ComponentKind::Output),
            ],
            wires: vec![wire(1, i(1), i(2))],
        };
        assert_eq!(
            compile(&d),
            Err(CompileError::MissingDriver {
                pins: vec![i(1), i(2)]
            })
        );
        d.components = vec![
            component(1, ComponentKind::Input),
            component(2, ComponentKind::Output),
            component(3, ComponentKind::Output),
        ];
        d.wires = vec![wire(1, o(1), i(2))];
        assert_eq!(
            compile(&d),
            Err(CompileError::MissingDriver { pins: vec![i(3)] })
        );
        d.components = vec![component(1, ComponentKind::Not)];
        d.wires.clear();
        assert!(matches!(
            compile(&d),
            Err(CompileError::MissingDriver { .. })
        ));
    }
    #[test]
    fn crossing_wires_do_not_join() {
        let mut components = vec![
            component(1, ComponentKind::Input),
            component(2, ComponentKind::Output),
            component(3, ComponentKind::Input),
            component(4, ComponentKind::Output),
        ];
        for (component, (x, y)) in
            components
                .iter_mut()
                .zip([(-1., 0.), (1., 0.), (0., -1.), (0., 1.)])
        {
            component.position = Position { x, y };
        }
        let d = Document {
            components,
            wires: vec![
                wire(
                    1,
                    pin(1, PinDirection::Output, 0),
                    pin(2, PinDirection::Input, 0),
                ),
                wire(
                    2,
                    pin(3, PinDirection::Output, 0),
                    pin(4, PinDirection::Input, 0),
                ),
            ],
        };
        let compiled = compile(&d).unwrap();
        assert_eq!(compiled.nets.len(), 2);
        assert!(compiled.nets.iter().all(|net| net.receivers.len() == 1));
    }
}
