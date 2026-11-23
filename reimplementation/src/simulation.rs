//! Bounded compiler for the supported one-bit circuits with explicit-pin connectivity.
//! Geometry never joins nets. Floating receiving pins and multiple drivers are errors.
use crate::domain::*;
use std::collections::{BTreeMap, HashMap, HashSet};

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
    InvalidConstantConfiguration { component: ComponentId },
    InvalidEndpoint { wire: WireId, pin: PinRef },
    MissingDriver { pins: Vec<PinRef> },
    MultipleDrivers { pins: Vec<PinRef> },
}
#[derive(Debug, PartialEq, Eq)]
pub struct Net {
    pub driver: Option<PinRef>,
    pub receivers: Vec<PinRef>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct CompiledCircuit {
    pub nets: Vec<Net>,
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
    for component in &document.components {
        if component.kind != ComponentKind::Const && !component.constant_value {
            return Err(CompileError::InvalidConstantConfiguration {
                component: component.id,
            });
        }
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

        nets.push(Net {
            driver: drivers.first().copied(),
            receivers,
        });
    }
    Ok(CompiledCircuit { nets })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn component(id: u64, kind: ComponentKind) -> Component {
        Component {
            id: ComponentId(id),
            kind,
            position: Position {
                x: id as f32 * 300.0,
                y: (id % 7) as f32 * 180.0,
            },
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
    fn non_const_non_default_value_is_rejected_at_compile_boundary() {
        let mut invalid = component(7, ComponentKind::Input);
        invalid.constant_value = false;
        assert_eq!(
            compile(&Document {
                components: vec![invalid],
                wires: vec![]
            }),
            Err(CompileError::InvalidConstantConfiguration {
                component: ComponentId(7)
            })
        );
        assert!(
            compile(&Document {
                components: vec![component(7, ComponentKind::Input)],
                wires: vec![]
            })
            .is_ok()
        );
    }

    #[test]
    fn transitive_explicit_pin_fanout() {
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
    fn feedback_and_disconnected_regions_compile() {
        assert!(compile(&Document::default()).is_ok());
        let i = |n| pin(n, PinDirection::Input, 0);
        let o = |n| pin(n, PinDirection::Output, 0);
        let self_loop = Document {
            components: vec![component(1, ComponentKind::Not)],
            wires: vec![wire(1, i(1), o(1))],
        };
        assert!(compile(&self_loop).is_ok());

        // Two cross-coupled inverters, a downstream output, and an independent region.
        let multi = Document {
            components: vec![
                component(1, ComponentKind::Not),
                component(2, ComponentKind::Not),
                component(3, ComponentKind::Output),
                component(4, ComponentKind::Input),
                component(5, ComponentKind::Output),
            ],
            wires: vec![
                wire(1, o(1), i(2)),
                wire(2, o(2), i(1)),
                wire(3, o(2), pin(3, PinDirection::Input, 0)),
                wire(4, o(4), pin(5, PinDirection::Input, 0)),
            ],
        };
        assert!(compile(&multi).is_ok());
    }

    #[test]
    fn feedback_does_not_bypass_driver_validation() {
        let i = |n| pin(n, PinDirection::Input, 0);
        let o = |n| pin(n, PinDirection::Output, 0);
        let mut missing = Document {
            components: vec![
                component(1, ComponentKind::Not),
                component(2, ComponentKind::Not),
            ],
            wires: vec![wire(1, o(1), i(1))],
        };
        assert!(matches!(
            compile(&missing),
            Err(CompileError::MissingDriver { .. })
        ));

        missing.wires = vec![
            wire(1, o(1), i(1)),
            wire(2, o(1), i(2)),
            wire(3, o(2), i(2)),
        ];
        assert!(matches!(
            compile(&missing),
            Err(CompileError::MultipleDrivers { .. })
        ));
    }
    #[test]
    fn coincident_pin_anchors_do_not_join_without_an_explicit_wire() {
        let source_a = component(1, ComponentKind::Input);
        let mut source_b = component(2, ComponentKind::Input);
        source_b.position = source_a.position;
        let document = Document {
            components: vec![source_a, source_b],
            wires: vec![],
        };
        let compiled = compile(&document).unwrap();
        assert_eq!(compiled.nets.len(), 2);
        assert!(compiled.nets.iter().all(|net| net.receivers.is_empty()));
    }

    #[test]
    fn synthesized_route_bend_does_not_attach_an_unwired_pin() {
        let at_pin = |id, kind, pin, point: [f32; 2]| {
            let layout = crate::symbol_render::SymbolLayout::centered_on(
                crate::symbols::SymbolStyle::Rect,
                kind,
                [0.0, 0.0],
            )
            .unwrap();
            let anchor = layout.pin_position(pin).unwrap();
            Component {
                id: ComponentId(id),
                kind,
                position: Position {
                    x: point[0] - anchor[0],
                    y: point[1] - anchor[1],
                },
                label: String::new(),
                constant_value: true,
            }
        };
        let source = PinRef {
            component: ComponentId(1),
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        };
        let receiver_b = PinRef {
            component: ComponentId(2),
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        let receiver_c = PinRef {
            component: ComponentId(3),
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        // The synthesized A(0,0) -> B(100,100) path bends at C(100,0),
        // but the editable document names only the A-to-B pin pair.
        let document = Document {
            components: vec![
                at_pin(1, ComponentKind::Input, source.pin, [0.0, 0.0]),
                at_pin(2, ComponentKind::Output, receiver_b.pin, [100.0, 100.0]),
                at_pin(3, ComponentKind::Output, receiver_c.pin, [100.0, 0.0]),
            ],
            wires: vec![wire(1, source, receiver_b)],
        };
        assert_eq!(
            compile(&document),
            Err(CompileError::MissingDriver {
                pins: vec![receiver_c]
            })
        );
        // Export checks its geometric reconstruction and refuses the unsupported short;
        // it must not claim a successful save of this explicit-edge document.
        assert!(crate::formats::from_domain(&document).is_err());
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
    fn floating_pins_are_checked_independent_of_geometry() {
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
    fn explicit_pin_pairs_compile_without_geometric_inference() {
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
