//! Bounded one-bit compiler over exact integer geometric wire endpoints.
use crate::domain::*;
use std::collections::HashMap;

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
    InvalidConstantConfiguration {
        component: ComponentId,
    },
    InvalidWideShapeConfiguration {
        component: ComponentId,
    },
    DisconnectedReceiver {
        pin: PinRef,
    },
    FloatingReceiverNet {
        pins: Vec<PinRef>,
        segment_ids: Vec<u64>,
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

/// Connectivity only, without electrical validation. Useful for edit-time duplicate-connection checks.
pub fn geometric_pin_partitions(document: &Document) -> Result<Vec<Vec<PinRef>>, CompileError> {
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
    let mut components = HashMap::new();
    let mut placed = Vec::new();
    for component in &document.components {
        if component.wide_shape && !crate::components::supports_wide_shape(component.kind) {
            return Err(CompileError::InvalidWideShapeConfiguration {
                component: component.id,
            });
        }
        if components.insert(component.id, ()).is_some() {
            return Err(CompileError::DuplicateComponent(component.id));
        }
        for direction in [PinDirection::Input, PinDirection::Output] {
            for index in 0..2 {
                let pin = PinId { direction, index };
                if component.kind.has_pin(pin) {
                    let reference = PinRef {
                        component: component.id,
                        pin,
                    };
                    let point = crate::pin_geometry::pin_point(component, pin).map_err(|e| {
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
                    placed.push(crate::topology::PinPoint {
                        pin: reference,
                        point,
                    });
                }
            }
        }
    }
    let mut result = crate::topology::resolve(&placed, document.wires.geometric().unwrap())
        .map_err(|e| match e {
            crate::topology::TopologyError::ZeroLength(id) => {
                CompileError::ZeroLengthSegment { segment: id }
            }
            crate::topology::TopologyError::DuplicateSegment(id) => {
                CompileError::DuplicateSegmentId { segment: id }
            }
            crate::topology::TopologyError::WorkLimit => CompileError::GeometricWorkLimit,
        })?;
    for partition in &mut result {
        partition.sort_unstable();
    }
    result.sort_unstable();
    Ok(result)
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
        if component.wide_shape && !crate::components::supports_wide_shape(component.kind) {
            return Err(CompileError::InvalidWideShapeConfiguration {
                component: component.id,
            });
        }
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
    let segments = document.wires.geometric().unwrap();
    let mut placed = Vec::with_capacity(pins.len());
    for pin in &pins {
        let component_index =
            *components
                .get(&pin.component)
                .ok_or(CompileError::InvalidPlacement {
                    component: pin.component,
                })?;
        let component = &document.components[component_index];
        let point = crate::pin_geometry::pin_point(component, pin.pin).map_err(|e| match e {
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
    let partitions = crate::topology::resolve_detailed(&placed, segments)
        .map_err(|e| match e {
            crate::topology::TopologyError::ZeroLength(id) => {
                CompileError::ZeroLengthSegment { segment: id }
            }
            crate::topology::TopologyError::DuplicateSegment(id) => {
                CompileError::DuplicateSegmentId { segment: id }
            }
            crate::topology::TopologyError::WorkLimit => CompileError::GeometricWorkLimit,
        })?
        .into_iter()
        .map(|net| (net.pins, net.segment_ids))
        .collect::<Vec<_>>();
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
            return Err(CompileError::GeometricMultipleDrivers { pins, segment_ids });
        }
        if drivers.is_empty() && !receivers.is_empty() {
            {
                if segment_ids.is_empty() && receivers.len() == 1 {
                    return Err(CompileError::DisconnectedReceiver { pin: receivers[0] });
                }
                return Err(CompileError::FloatingReceiverNet { pins, segment_ids });
            }
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
            wide_shape: false,
            rotation: 0,
            mirror: false,
        }
    }
    fn pin(id: u64, direction: PinDirection, index: u8) -> PinRef {
        PinRef {
            component: ComponentId(id),
            pin: PinId { direction, index },
        }
    }
    #[test]
    fn wide_shape_on_unsupported_component_is_rejected_at_compile_boundary() {
        let mut doc = Document {
            components: vec![component(4, ComponentKind::Not)],
            wires: crate::domain::Wiring::default(),
        };
        doc.components[0].wide_shape = true;
        assert_eq!(
            compile(&doc),
            Err(CompileError::InvalidWideShapeConfiguration {
                component: ComponentId(4)
            })
        );
        assert_eq!(
            geometric_pin_partitions(&doc),
            Err(CompileError::InvalidWideShapeConfiguration {
                component: ComponentId(4)
            })
        );
    }

    #[test]
    fn non_const_non_default_value_is_rejected_at_compile_boundary() {
        let mut invalid = component(7, ComponentKind::Input);
        invalid.constant_value = false;
        assert_eq!(
            compile(&Document {
                components: vec![invalid],
                wires: crate::domain::Wiring::GeometricSegments(vec![])
            }),
            Err(CompileError::InvalidConstantConfiguration {
                component: ComponentId(7)
            })
        );
        assert!(
            compile(&Document {
                components: vec![component(7, ComponentKind::Input)],
                wires: crate::domain::Wiring::GeometricSegments(vec![])
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
    fn routed_geometric_fanout_joins_receivers_and_disconnects_after_segment_removal() {
        let components = vec![
            component(1, ComponentKind::Input),
            component(2, ComponentKind::Output),
            component(3, ComponentKind::Output),
        ];
        let output = |id| pin(id, PinDirection::Output, 0);
        let input = |id| pin(id, PinDirection::Input, 0);
        let mut document = Document {
            wires: crate::test_support::routed_wires(
                &components,
                &[(output(1), input(2)), (output(1), input(3))],
            ),
            components,
        };
        let compiled = compile(&document).unwrap();
        assert_eq!(
            compiled
                .nets
                .iter()
                .find(|net| net.driver == Some(output(1)))
                .unwrap()
                .receivers
                .len(),
            2
        );
        document.wires.geometric_mut().unwrap().pop();
        assert!(
            matches!(compile(&document), Err(CompileError::DisconnectedReceiver { pin }) if pin == input(3))
        );
    }

    #[test]
    fn feedback_and_disconnected_regions_compile_from_real_pin_geometry() {
        let self_components = vec![component(1, ComponentKind::Not)];
        let input = pin(1, PinDirection::Input, 0);
        let output = pin(1, PinDirection::Output, 0);
        let self_loop = Document {
            wires: crate::test_support::routed_wires(&self_components, &[(output, input)]),
            components: self_components,
        };
        assert!(compile(&self_loop).is_ok());
        let components = vec![
            component(1, ComponentKind::Not),
            component(2, ComponentKind::Not),
            component(3, ComponentKind::Output),
            component(4, ComponentKind::Input),
            component(5, ComponentKind::Output),
        ];
        let pin = |id, direction| pin(id, direction, 0);
        let pairs = [
            (pin(1, PinDirection::Output), pin(2, PinDirection::Input)),
            (pin(2, PinDirection::Output), pin(1, PinDirection::Input)),
            (pin(2, PinDirection::Output), pin(3, PinDirection::Input)),
            (pin(4, PinDirection::Output), pin(5, PinDirection::Input)),
        ];
        let multi = Document {
            wires: crate::test_support::routed_wires(&components, &pairs),
            components,
        };
        assert!(compile(&multi).is_ok());
    }

    #[test]
    fn geometric_feedback_does_not_bypass_driver_validation() {
        let components = vec![
            component(1, ComponentKind::Not),
            component(2, ComponentKind::Not),
        ];
        let input = |id| pin(id, PinDirection::Input, 0);
        let output = |id| pin(id, PinDirection::Output, 0);
        let missing = Document {
            wires: crate::test_support::routed_wires(&components, &[(output(1), input(1))]),
            components: components.clone(),
        };
        assert!(
            matches!(compile(&missing), Err(CompileError::DisconnectedReceiver { pin }) if pin == input(2))
        );
        let multi = Document {
            wires: crate::test_support::routed_wires(
                &components,
                &[
                    (output(1), input(1)),
                    (output(1), input(2)),
                    (output(2), input(2)),
                ],
            ),
            components,
        };
        assert!(matches!(
            compile(&multi),
            Err(CompileError::GeometricMultipleDrivers { .. })
        ));
    }

    #[test]
    fn coincident_or_nearby_components_are_not_used_as_implicit_test_wires() {
        let mut a = component(1, ComponentKind::Input);
        let mut b = component(2, ComponentKind::Input);
        a.position = Position {
            x: -300.0,
            y: -180.0,
        };
        b.position = Position { x: 300.0, y: 180.0 };
        let compiled = compile(&Document {
            components: vec![a, b],
            wires: Wiring::default(),
        })
        .unwrap();
        assert_eq!(compiled.nets.len(), 2);
        assert!(compiled.nets.iter().all(|net| net.receivers.is_empty()));
    }

    #[test]
    fn independent_geometric_segment_crossings_do_not_attach_an_unwired_pin() {
        use crate::geometric_wiring::{Point, Segment};
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
                wide_shape: false,
                rotation: 0,
                mirror: false,
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
        let components = vec![
            at_pin(1, ComponentKind::Input, source.pin, [0., 0.]),
            at_pin(2, ComponentKind::Output, receiver_b.pin, [100., 100.]),
            at_pin(3, ComponentKind::Output, receiver_c.pin, [100., 0.]),
        ];
        let document = Document {
            components,
            wires: Wiring::GeometricSegments(vec![Segment {
                id: 1,
                a: Point::new(0, 0),
                b: Point::new(100, 100),
            }]),
        };
        assert!(
            matches!(compile(&document), Err(CompileError::DisconnectedReceiver { pin }) if pin == receiver_c)
        );
        assert!(crate::formats::from_domain(&document).is_ok());
    }

    #[test]
    fn limits_apply_to_geometric_segments_and_document_work() {
        let mut document = Document {
            components: (0..MAX_COMPONENTS as u64)
                .map(|id| component(id, ComponentKind::Input))
                .collect(),
            wires: Wiring::default(),
        };
        assert!(compile(&document).is_ok());
        document
            .components
            .push(component(MAX_COMPONENTS as u64, ComponentKind::Input));
        assert_eq!(compile(&document), Err(CompileError::ComponentLimit));
        document.components.clear();
        document.wires = Wiring::GeometricSegments(
            (0..MAX_WIRES as u64 + 1)
                .map(|id| crate::geometric_wiring::Segment {
                    id,
                    a: crate::geometric_wiring::Point::new(id as i32 * 2, 0),
                    b: crate::geometric_wiring::Point::new(id as i32 * 2 + 1, 0),
                })
                .collect(),
        );
        assert_eq!(compile(&document), Err(CompileError::WireLimit));
        document.wires.geometric_mut().unwrap().pop();
        document.components = (0..MAX_COMPONENTS as u64)
            .map(|id| component(id, ComponentKind::Input))
            .collect();
        assert_eq!(compile(&document), Err(CompileError::WorkLimit));
    }

    #[test]
    fn floating_receivers_are_reported_from_geometric_net_shape() {
        let input = |id| pin(id, PinDirection::Input, 0);
        let output = |id| pin(id, PinDirection::Output, 0);
        let components = vec![
            component(1, ComponentKind::Output),
            component(2, ComponentKind::Output),
        ];
        let floating = Document {
            wires: crate::test_support::routed_wires(&components, &[(input(1), input(2))]),
            components,
        };
        assert!(matches!(
            compile(&floating),
            Err(CompileError::FloatingReceiverNet { .. })
        ));
        let components = vec![
            component(1, ComponentKind::Input),
            component(2, ComponentKind::Output),
            component(3, ComponentKind::Output),
        ];
        let disconnected = Document {
            wires: crate::test_support::routed_wires(&components, &[(output(1), input(2))]),
            components,
        };
        assert!(
            matches!(compile(&disconnected), Err(CompileError::DisconnectedReceiver { pin }) if pin == input(3))
        );
    }
}
