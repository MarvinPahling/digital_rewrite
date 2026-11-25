//! Human-readable text for simulation diagnostics and values.
//!
//! Compile errors are rendered against the document so every message names the components and
//! pins involved. Values carry a symbol and a word, so the state never depends on color.
use crate::{
    domain::{ComponentId, Document, PinDirection, PinRef},
    engine::SimulationError,
    runtime::{OscillationDiagnostic, RuntimeError, RuntimeValue},
    simulation::{CompileError, MAX_COMPONENTS, MAX_WIRES, MAX_WORK},
};

/// Kind, label (when it differs from the kind name) and id, e.g. `AND "carry" #3`.
pub fn component_name(document: &Document, id: ComponentId) -> String {
    let Some(component) = document.components.iter().find(|c| c.id == id) else {
        return format!("unknown component #{}", id.0);
    };
    let kind = component.kind.name();
    if component.label == kind {
        format!("{kind} #{}", id.0)
    } else {
        format!("{kind} \"{}\" #{}", component.label, id.0)
    }
}

pub fn pin_name(document: &Document, pin: PinRef) -> String {
    let direction = match pin.pin.direction {
        PinDirection::Input => "input",
        PinDirection::Output => "output",
    };
    format!(
        "{direction} pin {} of {}",
        u16::from(pin.pin.index) + 1,
        component_name(document, pin.component)
    )
}

fn pin_list(document: &Document, pins: &[PinRef]) -> String {
    pins.iter()
        .map(|pin| pin_name(document, *pin))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn describe_compile_error(document: &Document, error: &CompileError) -> String {
    match error {
        CompileError::ComponentLimit => {
            format!("Too many components to simulate (limit {MAX_COMPONENTS}).")
        }
        CompileError::WireLimit => format!("Too many wires to simulate (limit {MAX_WIRES})."),
        CompileError::WorkLimit => {
            format!("The circuit is too large to simulate (work limit {MAX_WORK}).")
        }
        CompileError::FractionalPlacement { component } => format!(
            "{} has a fractional position; geometric compilation requires integer coordinates.",
            component_name(document, *component)
        ),
        CompileError::InvalidPlacement { component } => format!(
            "{} has an out-of-range or unsupported pin placement.",
            component_name(document, *component)
        ),
        CompileError::ZeroLengthSegment { segment } => {
            format!("Wire segment #{segment} has zero length.")
        }
        CompileError::DuplicateSegmentId { segment } => {
            format!("Wire segment id #{segment} is duplicated.")
        }
        CompileError::GeometricWorkLimit => {
            "Geometric connectivity exceeds its bounded work limit.".into()
        }
        CompileError::DuplicateComponent(id) => {
            format!("Component id #{} is used more than once.", id.0)
        }
        CompileError::InvalidConstantConfiguration { component } => format!(
            "Component {} has a non-default constant configuration, but only Const components support it.",
            component_name(document, *component)
        ),
        CompileError::InvalidWideShapeConfiguration { component } => format!(
            "Component {} has unsupported wideShape attributes; wide shapes apply only to binary gates.",
            component_name(document, *component)
        ),
        CompileError::DisconnectedReceiver { pin } => {
            format!("{} is disconnected.", pin_name(document, *pin))
        }
        CompileError::FloatingReceiverNet { pins, segment_ids } => format!(
            "Receiver net has no driver: {} (segments {}).",
            pin_list(document, pins),
            segment_ids
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CompileError::GeometricMultipleDrivers { pins, segment_ids } => format!(
            "Several outputs drive one geometric net: {} (segments {}). A net may have only one output.",
            pin_list(document, pins),
            segment_ids
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn describe_simulation_error(document: &Document, error: &SimulationError) -> String {
    match error {
        SimulationError::AlreadyActive => "The simulation is already running.".into(),
        SimulationError::NotActive => "The simulation is not running.".into(),
        SimulationError::Compile(error) => {
            format!(
                "Cannot start the simulation: {}",
                describe_compile_error(document, error)
            )
        }
        SimulationError::Runtime(RuntimeError::Pending) => {
            "Inputs can change only after the simulation has settled.".into()
        }
        SimulationError::Runtime(RuntimeError::UnknownComponent(id)) => {
            format!("Unknown component #{}.", id.0)
        }
        SimulationError::Runtime(RuntimeError::NotAnInput(id)) => {
            format!("{} is not an input.", component_name(document, *id))
        }
        SimulationError::Runtime(RuntimeError::InvalidOscillationLimit(limit)) => {
            format!("Oscillation limit {limit} is invalid.")
        }
        SimulationError::Runtime(RuntimeError::TraceSequenceExhausted) => {
            "Trace sequence exhausted; operation was not advanced.".into()
        }
        SimulationError::InvalidOscillationLimit(limit) => format!(
            "Oscillation limit {limit} is invalid; choose 1–{}.",
            crate::simulation::MAX_WORK
        ),
        SimulationError::TraceConfig(error) => format!("Trace configuration is invalid: {error}"),
        SimulationError::TraceSequenceExhausted => {
            "Trace sequence exhausted; operation was not advanced.".into()
        }
        SimulationError::SessionGenerationExhausted => {
            "Simulation session generation exhausted.".into()
        }
        SimulationError::Oscillation { diagnostic, work } => {
            describe_oscillation(document, diagnostic, *work)
        }
    }
}

pub fn describe_latched_oscillation(
    document: &Document,
    diagnostic: &OscillationDiagnostic,
) -> String {
    let components = diagnostic
        .components
        .iter()
        .map(|component| {
            let label = document
                .components
                .iter()
                .find(|item| item.id == component.id)
                .map(|item| item.label.as_str())
                .unwrap_or(&component.label);
            format!(
                "{} \"{}\" #{}",
                component.kind.name(),
                label,
                component.id.0
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Oscillation detected after {} microsteps (limit {}). Affected components: {}.",
        diagnostic.steps,
        diagnostic.threshold,
        if components.is_empty() {
            "none"
        } else {
            &components
        }
    )
}

pub fn describe_oscillation(
    document: &Document,
    diagnostic: &OscillationDiagnostic,
    work: usize,
) -> String {
    let components = diagnostic
        .components
        .iter()
        .map(|component| {
            let label = document
                .components
                .iter()
                .find(|item| item.id == component.id)
                .map(|item| item.label.as_str())
                .unwrap_or(&component.label);
            format!(
                "{} \"{}\" #{}",
                component.kind.name(),
                label,
                component.id.0
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Circuit appears to oscillate after {} microsteps (limit {}); consumed {work} work units. Affected components: {}.",
        diagnostic.steps,
        diagnostic.threshold,
        if components.is_empty() {
            "none"
        } else {
            &components
        }
    )
}

/// Signal display without implicitly sampling high-impedance bits.
pub fn level_text(level: RuntimeValue) -> String {
    match level {
        RuntimeValue::Unavailable => "? (unavailable)".to_owned(),
        RuntimeValue::Value(value) if value.high_z() != 0 => format!(
            "{}-bit {} (high-Z mask 0x{:X})",
            value.width().get(),
            value.format_hex(),
            value.high_z()
        ),
        RuntimeValue::Value(value) if value.width().get() == 1 && value.bits() == 0 => {
            "0 (low)".to_owned()
        }
        RuntimeValue::Value(value) if value.width().get() == 1 => "1 (high)".to_owned(),
        RuntimeValue::Value(value) => format!(
            "{}-bit {} (unsigned {})",
            value.width().get(),
            value.format_hex(),
            value.format_unsigned()
        ),
    }
}

/// Short form for on-canvas badges; width and high-Z information are retained.
pub fn level_badge(level: RuntimeValue) -> String {
    match level {
        RuntimeValue::Unavailable => "= ?".to_owned(),
        RuntimeValue::Value(value) if value.high_z() != 0 => {
            format!(
                "= {}-bit {} Z:{:X}",
                value.width().get(),
                value.format_hex(),
                value.high_z()
            )
        }
        RuntimeValue::Value(value) if value.width().get() == 1 => format!("= {}", value.bits()),
        RuntimeValue::Value(value) => {
            format!("= {}-bit {}", value.width().get(), value.format_hex())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Component, ComponentKind, PinId, Position};
    use crate::runtime::Level;

    fn document() -> Document {
        let component = |id, kind: ComponentKind, label: &str| Component {
            id: ComponentId(id),
            kind,
            position: Position { x: 0., y: 0. },
            label: label.into(),
            constant_value: true,
            wide_shape: false,
            rotation: 0,
            mirror: false,
        };
        Document {
            components: vec![
                component(1, ComponentKind::And, "AND"),
                component(2, ComponentKind::Or, "carry"),
            ],
            wires: crate::domain::Wiring::GeometricSegments(vec![]),
        }
    }

    fn pin(component: u64, direction: PinDirection, index: u8) -> PinRef {
        PinRef {
            component: ComponentId(component),
            pin: PinId { direction, index },
        }
    }

    #[test]
    fn components_and_pins_are_located_by_kind_label_and_id() {
        let document = document();
        assert_eq!(component_name(&document, ComponentId(1)), "AND #1");
        assert_eq!(component_name(&document, ComponentId(2)), "OR \"carry\" #2");
        assert_eq!(
            component_name(&document, ComponentId(9)),
            "unknown component #9"
        );
        assert_eq!(
            pin_name(&document, pin(2, PinDirection::Input, 1)),
            "input pin 2 of OR \"carry\" #2"
        );
    }

    #[test]
    fn geometric_compile_errors_name_the_offending_pins_and_components() {
        let document = document();
        let missing = describe_compile_error(
            &document,
            &CompileError::DisconnectedReceiver {
                pin: pin(1, PinDirection::Input, 0),
            },
        );
        assert!(missing.contains("input pin 1 of AND #1"), "{missing}");
        let multiple = describe_compile_error(
            &document,
            &CompileError::GeometricMultipleDrivers {
                pins: vec![
                    pin(1, PinDirection::Output, 0),
                    pin(2, PinDirection::Output, 0),
                ],
                segment_ids: vec![4],
            },
        );
        assert!(multiple.contains("output pin 1 of AND #1"), "{multiple}");
        assert!(
            multiple.contains("output pin 1 of OR \"carry\" #2"),
            "{multiple}"
        );
    }

    #[test]
    fn values_name_their_state_in_words() {
        assert_eq!(level_text(Level::Low), "0 (low)");
        assert_eq!(level_text(Level::High), "1 (high)");
        assert_eq!(level_text(Level::Unavailable), "? (unavailable)");
        assert_eq!(level_badge(Level::Unavailable), "= ?");
        let width = crate::signal::SignalWidth::new(4).unwrap();
        let z_value = RuntimeValue::Value(crate::signal::SignalValue::new(width, 0b1010, 0b0010));
        assert_eq!(level_text(z_value), "4-bit 0x8 (high-Z mask 0x2)");
        assert_eq!(level_badge(z_value), "= 4-bit 0x8 Z:2");
    }

    #[test]
    fn simulation_errors_are_described_against_the_document() {
        let document = document();
        assert_eq!(
            describe_simulation_error(&document, &SimulationError::NotActive),
            "The simulation is not running."
        );
        let rejected = describe_simulation_error(
            &document,
            &SimulationError::Runtime(RuntimeError::NotAnInput(ComponentId(1))),
        );
        assert_eq!(rejected, "AND #1 is not an input.");
    }
}
