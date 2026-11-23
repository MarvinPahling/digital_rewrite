//! Bounded compiler for the supported one-bit circuits and geometric connectivity.
use crate::domain::*;
use std::collections::{HashMap, HashSet};

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
    InvalidGeometry(crate::topology::TopologyError),
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
    let mut wire_ids = HashSet::new();
    for wire in &document.wires {
        if !wire_ids.insert(wire.id) {
            return Err(CompileError::DuplicateWire(wire.id));
        }
        for pin in wire.endpoints {
            if !indices.contains_key(&pin) {
                return Err(CompileError::InvalidEndpoint { wire: wire.id, pin });
            }
        }
    }
    let mut pin_points = Vec::with_capacity(pins.len());
    let mut point_by_pin = HashMap::with_capacity(pins.len());
    for component in &document.components {
        let layout = crate::symbol_render::SymbolLayout::centered_on(
            crate::symbols::SymbolStyle::Rect,
            component.kind,
            [component.position.x, component.position.y],
        )
        .ok_or(CompileError::InvalidGeometry(
            crate::topology::TopologyError::InvalidCoordinate,
        ))?;
        for direction in [PinDirection::Input, PinDirection::Output] {
            for index in 0..2 {
                let id = PinId { direction, index };
                if !component.kind.has_pin(id) {
                    continue;
                }
                let pin = PinRef {
                    component: component.id,
                    pin: id,
                };
                let point = layout
                    .pin_position(id)
                    .ok_or(CompileError::InvalidEndpoint {
                        wire: WireId(0),
                        pin,
                    })?;
                point_by_pin.insert(pin, point);
                pin_points.push(crate::topology::PinPoint { pin, point });
            }
        }
    }
    let mut segments = Vec::with_capacity(document.wires.len().saturating_mul(2));
    for wire in &document.wires {
        let [a, b] = wire.endpoints;
        let path = crate::wire_geometry::path((a, point_by_pin[&a]), (b, point_by_pin[&b]));
        segments.extend(path.windows(2).map(|pair| crate::topology::Segment {
            a: pair[0],
            b: pair[1],
        }));
    }
    let groups =
        crate::topology::resolve(&pin_points, &segments).map_err(CompileError::InvalidGeometry)?;
    let mut nets = Vec::new();
    for pins in groups {
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
    fn coincident_pin_anchors_share_a_net_without_an_explicit_wire() {
        let source_a = component(1, ComponentKind::Input);
        let mut source_b = component(2, ComponentKind::Input);
        let mut receiver = component(3, ComponentKind::Output);
        source_b.position = source_a.position;
        receiver.position = source_a.position;
        let document = Document {
            components: vec![source_a, source_b, receiver],
            wires: vec![],
        };
        assert!(matches!(
            compile(&document),
            Err(CompileError::MultipleDrivers { .. })
        ));
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
