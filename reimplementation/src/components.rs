//! Shared component catalog metadata; component behavior remains in the simulator.
use crate::domain::ComponentKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentDescriptor {
    pub kind: ComponentKind,
    pub palette_name: &'static str,
    pub digital_name: &'static str,
    pub input_pins: u8,
    pub output_pins: u8,
    /// Digital defaults are recorded for compatibility; the strict v2 subset omits them.
    pub defaults: &'static [(&'static str, &'static str)],
}
const ONE_BIT: &[(&str, &str)] = &[("Bits", "1")];
const TWO_INPUT_GATE: &[(&str, &str)] = &[("Bits", "1"), ("Inputs", "2"), ("wideShape", "false")];
pub const DESCRIPTORS: [ComponentDescriptor; 9] = [
    ComponentDescriptor {
        kind: ComponentKind::Input,
        palette_name: "Input",
        digital_name: "In",
        input_pins: 0,
        output_pins: 1,
        defaults: ONE_BIT,
    },
    ComponentDescriptor {
        kind: ComponentKind::Output,
        palette_name: "Output",
        digital_name: "Out",
        input_pins: 1,
        output_pins: 0,
        defaults: ONE_BIT,
    },
    ComponentDescriptor {
        kind: ComponentKind::And,
        palette_name: "AND",
        digital_name: "And",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
    },
    ComponentDescriptor {
        kind: ComponentKind::NAnd,
        palette_name: "NAND",
        digital_name: "NAnd",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
    },
    ComponentDescriptor {
        kind: ComponentKind::Or,
        palette_name: "OR",
        digital_name: "Or",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
    },
    ComponentDescriptor {
        kind: ComponentKind::NOr,
        palette_name: "NOR",
        digital_name: "NOr",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
    },
    ComponentDescriptor {
        kind: ComponentKind::XOr,
        palette_name: "XOR",
        digital_name: "XOr",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
    },
    ComponentDescriptor {
        kind: ComponentKind::XNOr,
        palette_name: "XNOR",
        digital_name: "XNOr",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
    },
    ComponentDescriptor {
        kind: ComponentKind::Not,
        palette_name: "NOT",
        digital_name: "Not",
        input_pins: 1,
        output_pins: 1,
        defaults: ONE_BIT,
    },
];
pub const KINDS: [ComponentKind; 9] = [
    ComponentKind::Input,
    ComponentKind::Output,
    ComponentKind::And,
    ComponentKind::NAnd,
    ComponentKind::Or,
    ComponentKind::NOr,
    ComponentKind::XOr,
    ComponentKind::XNOr,
    ComponentKind::Not,
];
pub fn descriptor(kind: ComponentKind) -> &'static ComponentDescriptor {
    DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.kind == kind)
        .expect("every component kind has a catalog descriptor")
}
pub fn from_digital_name(name: &str) -> Option<ComponentKind> {
    DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.digital_name == name)
        .map(|descriptor| descriptor.kind)
}
impl ComponentKind {
    pub fn name(self) -> &'static str {
        descriptor(self).palette_name
    }
    pub fn digital_name(self) -> &'static str {
        descriptor(self).digital_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PinDirection, PinId};

    #[test]
    fn descriptor_names_defaults_and_pin_counts_are_consistent() {
        assert_eq!(DESCRIPTORS.len(), KINDS.len());
        for descriptor in DESCRIPTORS {
            assert!(KINDS.contains(&descriptor.kind));
            assert_eq!(
                from_digital_name(descriptor.digital_name),
                Some(descriptor.kind)
            );
            assert_eq!(descriptor.kind.name(), descriptor.palette_name);
            for (direction, expected) in [
                (PinDirection::Input, descriptor.input_pins),
                (PinDirection::Output, descriptor.output_pins),
            ] {
                for index in 0..=expected {
                    assert_eq!(
                        descriptor.kind.has_pin(PinId { direction, index }),
                        index < expected
                    );
                }
            }
        }
        for kind in [
            ComponentKind::NAnd,
            ComponentKind::NOr,
            ComponentKind::XOr,
            ComponentKind::XNOr,
        ] {
            assert_eq!(
                descriptor(kind).defaults,
                &[("Bits", "1"), ("Inputs", "2"), ("wideShape", "false")]
            );
            assert_eq!(descriptor(kind).defaults, TWO_INPUT_GATE);
            assert_eq!(descriptor(kind).input_pins, 2);
            assert_eq!(descriptor(kind).output_pins, 1);
        }
        for wrong_case in ["nand", "NOR", "xor", "XNOR"] {
            assert_eq!(from_digital_name(wrong_case), None);
        }
    }
}
