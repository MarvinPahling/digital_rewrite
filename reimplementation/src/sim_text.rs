//! Human-readable text for simulation diagnostics and values.
//!
//! Compile errors are rendered against the document so every message names the components and
//! pins involved. Values carry a symbol and a word, so the state never depends on color.
use crate::{
    domain::{ComponentId, Document, PinDirection, PinRef},
    engine::SimulationError,
    runtime::{Level, OscillationDiagnostic, RuntimeError},
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
        CompileError::DuplicateComponent(id) => {
            format!("Component id #{} is used more than once.", id.0)
        }
        CompileError::DuplicateWire(id) => format!("Wire id #{} is used more than once.", id.0),
        CompileError::InvalidConstantConfiguration { component } => format!(
            "Component {} has a non-default constant configuration, but only Const components support it.",
            component_name(document, *component)
        ),
        CompileError::InvalidEndpoint { wire, pin } => format!(
            "Wire #{} is attached to {}, which does not exist.",
            wire.0,
            pin_name(document, *pin)
        ),
        CompileError::MissingDriver { pins } => format!(
            "Nothing drives {}. Wire an output pin to it.",
            pin_list(document, pins)
        ),
        CompileError::MultipleDrivers { pins } => format!(
            "Several outputs drive one net: {}. A net may have only one output.",
            pin_list(document, pins)
        ),
        CompileError::InvalidGeometry(error) => {
            format!("Circuit geometry cannot be resolved: {error}.")
        }
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

/// A one-bit value as symbol plus word.
pub fn level_text(level: Level) -> &'static str {
    match level {
        Level::Low => "0 (low)",
        Level::High => "1 (high)",
        Level::Unavailable => "? (unavailable)",
    }
}

/// Short form for on-canvas badges.
pub fn level_badge(level: Level) -> &'static str {
    match level {
        Level::Low => "= 0",
        Level::High => "= 1",
        Level::Unavailable => "= ?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Component, ComponentKind, PinId, Position, WireId};

    fn document() -> Document {
        let component = |id, kind: ComponentKind, label: &str| Component {
            id: ComponentId(id),
            kind,
            position: Position { x: 0., y: 0. },
            label: label.into(),
            constant_value: true,
        };
        Document {
            components: vec![
                component(1, ComponentKind::And, "AND"),
                component(2, ComponentKind::Or, "carry"),
            ],
            wires: vec![],
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
    fn compile_errors_name_the_offending_pins_and_components() {
        let document = document();
        let missing = describe_compile_error(
            &document,
            &CompileError::MissingDriver {
                pins: vec![pin(1, PinDirection::Input, 0)],
            },
        );
        assert!(missing.contains("input pin 1 of AND #1"), "{missing}");
        let multiple = describe_compile_error(
            &document,
            &CompileError::MultipleDrivers {
                pins: vec![
                    pin(1, PinDirection::Output, 0),
                    pin(2, PinDirection::Output, 0),
                ],
            },
        );
        assert!(multiple.contains("output pin 1 of AND #1"), "{multiple}");
        assert!(
            multiple.contains("output pin 1 of OR \"carry\" #2"),
            "{multiple}"
        );
        let endpoint = describe_compile_error(
            &document,
            &CompileError::InvalidEndpoint {
                wire: WireId(4),
                pin: pin(7, PinDirection::Output, 0),
            },
        );
        assert!(
            endpoint.contains("Wire #4") && endpoint.contains("unknown component #7"),
            "{endpoint}"
        );
    }

    #[test]
    fn values_name_their_state_in_words() {
        assert_eq!(level_text(Level::Low), "0 (low)");
        assert_eq!(level_text(Level::High), "1 (high)");
        assert_eq!(level_text(Level::Unavailable), "? (unavailable)");
        assert_eq!(level_badge(Level::Unavailable), "= ?");
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
