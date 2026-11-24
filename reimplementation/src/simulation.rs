//! Bounded one-bit compiler. Legacy documents use explicit pin pairs; geometric documents use exact integer endpoints.
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
    FractionalPlacement {
        component: ComponentId,
    },
    InvalidPlacement {
        component: ComponentId,
    },
    ZeroLengthSegment {
        segment: u64,
    },
    DuplicateSegmentId {
        segment: u64,
    },
    GeometricWorkLimit,
    DuplicateComponent(ComponentId),
    DuplicateWire(WireId),
    InvalidConstantConfiguration {
        component: ComponentId,
    },
    InvalidEndpoint {
        wire: WireId,
        pin: PinRef,
    },
    MissingDriver {
        pins: Vec<PinRef>,
    },
    DisconnectedReceiver {
        pin: PinRef,
    },
    FloatingReceiverNet {
        pins: Vec<PinRef>,
        segment_ids: Vec<u64>,
    },
    MultipleDrivers {
        pins: Vec<PinRef>,
    },
    GeometricMultipleDrivers {
        pins: Vec<PinRef>,
        segment_ids: Vec<u64>,
    },
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

/// Connectivity only, without electrical validation. Used by transactional topology migration so
/// incomplete circuits can be compared without requiring a successful simulation compile.
pub fn logical_pin_partitions(document: &Document) -> Result<Vec<Vec<PinRef>>, CompileError> {
    if document.components.len() > MAX_COMPONENTS {
        return Err(CompileError::ComponentLimit);
    }
    if document.wires.len() > MAX_WIRES {
        return Err(CompileError::WireLimit);
    }
    if document
        .components
        .len()
        .saturating_mul(5)
        .saturating_add(document.wires.len().saturating_mul(2))
        > MAX_WORK
    {
        return Err(CompileError::WorkLimit);
    }
    let mut pins = Vec::new();
    let mut indices = HashMap::new();
    let mut component_indices = HashMap::new();
    for (component_index, component) in document.components.iter().enumerate() {
        if component_indices
            .insert(component.id, component_index)
            .is_some()
        {
            return Err(CompileError::DuplicateComponent(component.id));
        }
        for direction in [PinDirection::Input, PinDirection::Output] {
            for index in 0..2 {
                let pin = PinId { direction, index };
                if component.kind.has_pin(pin) {
                    let r = PinRef {
                        component: component.id,
                        pin,
                    };
                    indices.insert(r, pins.len());
                    pins.push(r);
                }
            }
        }
    }
    let mut result = match &document.wires {
        Wiring::LegacyPinPairs(wires) => {
            let mut parent: Vec<_> = (0..pins.len()).collect();
            let mut ids = HashSet::new();
            for wire in wires {
                if !ids.insert(wire.id) {
                    return Err(CompileError::DuplicateWire(wire.id));
                }
                let a = *indices
                    .get(&wire.endpoints[0])
                    .ok_or(CompileError::InvalidEndpoint {
                        wire: wire.id,
                        pin: wire.endpoints[0],
                    })?;
                let b = *indices
                    .get(&wire.endpoints[1])
                    .ok_or(CompileError::InvalidEndpoint {
                        wire: wire.id,
                        pin: wire.endpoints[1],
                    })?;
                let a = root(&mut parent, a);
                let b = root(&mut parent, b);
                parent[a] = b;
            }
            let mut groups = BTreeMap::<usize, Vec<PinRef>>::new();
            for (i, p) in pins.iter().enumerate() {
                groups.entry(root(&mut parent, i)).or_default().push(*p);
            }
            groups.into_values().collect()
        }
        Wiring::GeometricSegments(segments) => {
            let mut placed = Vec::new();
            for pin in &pins {
                let component_index = *component_indices.get(&pin.component).ok_or(
                    CompileError::InvalidPlacement {
                        component: pin.component,
                    },
                )?;
                let component = &document.components[component_index];
                let point = crate::pin_geometry::pin_point(component, pin.pin).map_err(|e| {
                    if matches!(e, crate::pin_geometry::PinGeometryError::Fractional(_)) {
                        CompileError::FractionalPlacement {
                            component: component.id,
                        }
                    } else {
                        CompileError::InvalidPlacement {
                            component: component.id,
                        }
                    }
                })?;
                placed.push(crate::topology::PinPoint { pin: *pin, point });
            }
            crate::topology::resolve(&placed, segments).map_err(|e| match e {
                crate::topology::TopologyError::ZeroLength(id) => {
                    CompileError::ZeroLengthSegment { segment: id }
                }
                crate::topology::TopologyError::DuplicateSegment(id) => {
                    CompileError::DuplicateSegmentId { segment: id }
                }
                crate::topology::TopologyError::WorkLimit => CompileError::GeometricWorkLimit,
            })?
        }
    };
    // Partition enumeration differs by adapter: legacy union-find exposes roots in wire
    // insertion order, while geometric resolution emits nets in geometric order. Canonicalize
    // both the pins and the partition list so connectivity comparisons are representation-neutral.
    for partition in &mut result {
        partition.sort_unstable();
    }
    result.sort_unstable();
    Ok(result)
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
    let (partitions, geometric) = match &document.wires {
        Wiring::LegacyPinPairs(wires) => {
            let mut parent: Vec<_> = (0..pins.len()).collect();
            let mut wire_ids = HashSet::new();
            for wire in wires {
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
            (
                groups
                    .into_values()
                    .map(|pins| (pins, Vec::<u64>::new()))
                    .collect::<Vec<_>>(),
                false,
            )
        }
        Wiring::GeometricSegments(segments) => {
            let mut placed = Vec::with_capacity(pins.len());
            for pin in &pins {
                let component_index =
                    *components
                        .get(&pin.component)
                        .ok_or(CompileError::InvalidPlacement {
                            component: pin.component,
                        })?;
                let component = &document.components[component_index];
                let point =
                    crate::pin_geometry::pin_point(component, pin.pin).map_err(|e| match e {
                        crate::pin_geometry::PinGeometryError::Fractional(_) => {
                            CompileError::FractionalPlacement {
                                component: component.id,
                            }
                        }
                        _ => CompileError::InvalidPlacement {
                            component: component.id,
                        },
                    })?;
                placed.push(crate::topology::PinPoint { pin: *pin, point });
            }
            (
                crate::topology::resolve_detailed(&placed, segments)
                    .map_err(|e| match e {
                        crate::topology::TopologyError::ZeroLength(id) => {
                            CompileError::ZeroLengthSegment { segment: id }
                        }
                        crate::topology::TopologyError::DuplicateSegment(id) => {
                            CompileError::DuplicateSegmentId { segment: id }
                        }
                        crate::topology::TopologyError::WorkLimit => {
                            CompileError::GeometricWorkLimit
                        }
                    })?
                    .into_iter()
                    .map(|net| (net.pins, net.segment_ids))
                    .collect::<Vec<_>>(),
                true,
            )
        }
    };
    let mut nets = Vec::new();
    for (pins, segment_ids) in partitions {
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
            return Err(if geometric {
                CompileError::GeometricMultipleDrivers { pins, segment_ids }
            } else {
                CompileError::MultipleDrivers { pins }
            });
        }
        if drivers.is_empty() && !receivers.is_empty() {
            if geometric {
                if segment_ids.is_empty() && receivers.len() == 1 {
                    return Err(CompileError::DisconnectedReceiver { pin: receivers[0] });
                }
                return Err(CompileError::FloatingReceiverNet { pins, segment_ids });
            }
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
                wires: crate::domain::Wiring::LegacyPinPairs(vec![])
            }),
            Err(CompileError::InvalidConstantConfiguration {
                component: ComponentId(7)
            })
        );
        assert!(
            compile(&Document {
                components: vec![component(7, ComponentKind::Input)],
                wires: crate::domain::Wiring::LegacyPinPairs(vec![])
            })
            .is_ok()
        );
    }

    #[test]
    fn geometric_compilation_and_diagnostics_use_integer_endpoint_topology() {
        use crate::geometric_wiring::{Point, Segment};
        let input = component(1, ComponentKind::Input);
        let output = component(2, ComponentKind::Output);
        let input_pin = pin(1, PinDirection::Output, 0);
        let output_pin = pin(2, PinDirection::Input, 0);
        let a = crate::pin_geometry::pin_point(&input, input_pin.pin).unwrap();
        let b = crate::pin_geometry::pin_point(&output, output_pin.pin).unwrap();
        let mut doc = Document {
            components: vec![input, output],
            wires: Wiring::GeometricSegments(vec![Segment { id: 9, a, b }]),
        };
        assert!(compile(&doc).is_ok());
        doc.wires = Wiring::GeometricSegments(vec![]);
        assert_eq!(
            compile(&doc),
            Err(CompileError::DisconnectedReceiver { pin: output_pin })
        );
        doc.wires = Wiring::GeometricSegments(vec![
            Segment {
                id: 11,
                a: Point::new(0, 0),
                b: Point::new(10, 10),
            },
            Segment {
                id: 12,
                a: Point::new(10, 10),
                b: Point::new(20, 20),
            },
        ]);
        assert!(compile(&doc).is_err()); // the receiver pin is not at this remote wire
        doc.wires = Wiring::GeometricSegments(vec![Segment {
            id: 11,
            a: Point::new(b.x + 10, b.y),
            b,
        }]);
        assert!(matches!(
            compile(&doc),
            Err(CompileError::FloatingReceiverNet { .. })
        ));
    }

    #[test]
    fn geometric_multiple_driver_diagnostics_are_stably_located() {
        use crate::geometric_wiring::Segment;
        let a = component(4, ComponentKind::Input);
        let mut b = component(2, ComponentKind::Input);
        let pa = pin(4, PinDirection::Output, 0);
        let pb = pin(2, PinDirection::Output, 0);
        let point = crate::pin_geometry::pin_point(&a, pa.pin).unwrap();
        b.position = a.position;
        let doc = Document {
            components: vec![a, b],
            wires: Wiring::GeometricSegments(vec![Segment {
                id: 44,
                a: point,
                b: crate::geometric_wiring::Point::new(point.x + 10, point.y),
            }]),
        };
        assert!(
            matches!(compile(&doc),Err(CompileError::GeometricMultipleDrivers{pins,segment_ids}) if pins==vec![pb,pa] && segment_ids==vec![44])
        );
    }

    #[test]
    fn geometric_validation_rejects_fractional_placements_and_bad_segments() {
        let mut c = component(1, ComponentKind::Input);
        c.position.x = 0.5;
        let d = Document {
            components: vec![c],
            wires: Wiring::GeometricSegments(vec![]),
        };
        assert_eq!(
            compile(&d),
            Err(CompileError::FractionalPlacement {
                component: ComponentId(1)
            })
        );
        let c = component(1, ComponentKind::Input);
        let d = Document {
            components: vec![c],
            wires: Wiring::GeometricSegments(vec![crate::geometric_wiring::Segment {
                id: 7,
                a: crate::geometric_wiring::Point::new(1, 1),
                b: crate::geometric_wiring::Point::new(1, 1),
            }]),
        };
        assert_eq!(
            compile(&d),
            Err(CompileError::ZeroLengthSegment { segment: 7 })
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![wire(1, a, o), wire(2, b, a)]),
        };
        assert_eq!(compile(&d).unwrap().nets[0].receivers.len(), 2);
        d.wires.legacy_mut().unwrap().pop();
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![wire(1, a, b)]),
        };
        assert!(matches!(
            compile(&d),
            Err(CompileError::MultipleDrivers { .. })
        ));
        d.wires.legacy_mut().unwrap()[0].endpoints[1].pin.index = 4;
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
        d.wires.legacy_mut().unwrap()[0].endpoints[1] = a;
        let duplicate = d.wires.legacy().unwrap()[0].clone();
        d.wires.legacy_mut().unwrap().push(duplicate);
        assert!(matches!(compile(&d), Err(CompileError::DuplicateWire(_))));
    }
    #[test]
    fn feedback_and_disconnected_regions_compile() {
        assert!(compile(&Document::default()).is_ok());
        let i = |n| pin(n, PinDirection::Input, 0);
        let o = |n| pin(n, PinDirection::Output, 0);
        let self_loop = Document {
            components: vec![component(1, ComponentKind::Not)],
            wires: crate::domain::Wiring::LegacyPinPairs(vec![wire(1, i(1), o(1))]),
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![
                wire(1, o(1), i(2)),
                wire(2, o(2), i(1)),
                wire(3, o(2), pin(3, PinDirection::Input, 0)),
                wire(4, o(4), pin(5, PinDirection::Input, 0)),
            ]),
        };
        assert!(compile(&multi).is_ok());
    }

    #[test]
    fn geometric_feedback_cycles_remain_legal_for_existing_scheduler() {
        let not = component(8, ComponentKind::Not);
        let input = crate::pin_geometry::pin_point(
            &not,
            PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        )
        .unwrap();
        let output = crate::pin_geometry::pin_point(
            &not,
            PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        )
        .unwrap();
        let document = Document {
            components: vec![not],
            wires: Wiring::GeometricSegments(vec![crate::geometric_wiring::Segment {
                id: 81,
                a: input,
                b: output,
            }]),
        };
        assert!(compile(&document).is_ok());
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![wire(1, o(1), i(1))]),
        };
        assert!(matches!(
            compile(&missing),
            Err(CompileError::MissingDriver { .. })
        ));

        missing.wires = Wiring::LegacyPinPairs(vec![
            wire(1, o(1), i(1)),
            wire(2, o(1), i(2)),
            wire(3, o(2), i(2)),
        ]);
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![]),
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![wire(1, source, receiver_b)]),
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![]),
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
        d.wires = Wiring::LegacyPinPairs(vec![w; MAX_WIRES + 1]);
        assert_eq!(compile(&d), Err(CompileError::WireLimit));
        d.wires.legacy_mut().unwrap().pop();
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
        d.wires
            .legacy_mut()
            .unwrap()
            .push(wire(MAX_WIRES as u64, output, output));
        assert_eq!(compile(&d), Err(CompileError::WireLimit));
        d.components = (0..MAX_COMPONENTS as u64)
            .map(|i| component(i, ComponentKind::Input))
            .collect();
        d.wires
            .legacy_mut()
            .unwrap()
            .truncate((MAX_WORK - MAX_COMPONENTS * 5) / 2);
        assert!(compile(&d).is_ok());
        d.components.pop();
        d.wires
            .legacy_mut()
            .unwrap()
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![wire(1, i(1), i(2))]),
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
        d.wires = Wiring::LegacyPinPairs(vec![wire(1, o(1), i(2))]);
        assert_eq!(
            compile(&d),
            Err(CompileError::MissingDriver { pins: vec![i(3)] })
        );
        d.components = vec![component(1, ComponentKind::Not)];
        d.wires.legacy_mut().unwrap().clear();
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
            wires: crate::domain::Wiring::LegacyPinPairs(vec![
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
            ]),
        };
        let compiled = compile(&d).unwrap();
        assert_eq!(compiled.nets.len(), 2);
        assert!(compiled.nets.iter().all(|net| net.receivers.len() == 1));
    }
}
