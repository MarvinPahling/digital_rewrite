//! Shared component catalog metadata; component behavior remains in the simulator.
use crate::domain::ComponentKind;

/// Public typed attribute IDs; unknown strings never enter the editing engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AttributeId {
    Label,
    Value,
    Rotation,
    Mirror,
    WideShape,
    Inputs,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttributeValue {
    Label(String),
    Value(bool),
    Rotation(u8),
    Mirror(bool),
    WideShape(bool),
    Inputs(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeType {
    Text,
    Boolean,
    QuarterTurns,
    InputCount,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributeDescriptor {
    pub id: AttributeId,
    pub name: &'static str,
    pub value_type: AttributeType,
    pub editable: bool,
}
const COMPONENT_ATTRIBUTES: &[AttributeDescriptor] = &[
    AttributeDescriptor {
        id: AttributeId::Label,
        name: "Label",
        value_type: AttributeType::Text,
        editable: true,
    },
    AttributeDescriptor {
        id: AttributeId::Value,
        name: "Value",
        value_type: AttributeType::Boolean,
        editable: true,
    },
    AttributeDescriptor {
        id: AttributeId::Rotation,
        name: "rotation",
        value_type: AttributeType::QuarterTurns,
        editable: true,
    },
    AttributeDescriptor {
        id: AttributeId::Mirror,
        name: "mirror",
        value_type: AttributeType::Boolean,
        editable: true,
    },
    AttributeDescriptor {
        id: AttributeId::WideShape,
        name: "wideShape",
        value_type: AttributeType::Boolean,
        editable: true,
    },
    AttributeDescriptor {
        id: AttributeId::Inputs,
        name: "Inputs",
        value_type: AttributeType::InputCount,
        editable: false,
    },
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttributeError {
    Unsupported(AttributeId),
    WrongType(AttributeId),
    WrongKind(AttributeId),
    Invalid(AttributeId, String),
    Duplicate(AttributeId),
}
impl std::fmt::Display for AttributeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid component attribute: {self:?}")
    }
}
impl std::error::Error for AttributeError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeChange {
    pub id: AttributeId,
    pub value: AttributeValue,
}
impl AttributeChange {
    pub fn new(id: AttributeId, value: AttributeValue) -> Self {
        Self { id, value }
    }
}
pub fn attribute_descriptors(kind: ComponentKind) -> Vec<AttributeDescriptor> {
    let supported = descriptor(kind).attributes;
    COMPONENT_ATTRIBUTES
        .iter()
        .copied()
        .filter(|d| supported.contains(&d.id))
        .collect()
}
pub fn has_attribute(kind: ComponentKind, id: AttributeId) -> bool {
    descriptor(kind).attributes.contains(&id)
}
pub fn supports_wide_shape(kind: ComponentKind) -> bool {
    matches!(
        kind,
        ComponentKind::And
            | ComponentKind::NAnd
            | ComponentKind::Or
            | ComponentKind::NOr
            | ComponentKind::XOr
            | ComponentKind::XNOr
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentDescriptor {
    pub kind: ComponentKind,
    pub palette_name: &'static str,
    pub digital_name: &'static str,
    pub input_pins: u8,
    pub output_pins: u8,
    /// Digital defaults are recorded for compatibility; the strict v2 subset omits them.
    pub defaults: &'static [(&'static str, &'static str)],
    pub attributes: &'static [AttributeId],
}
const CONST_DEFAULTS: &[(&str, &str)] = &[("Bits", "1"), ("Value", "1")];
const ONE_BIT: &[(&str, &str)] = &[("Bits", "1")];
const COMMON_ATTRIBUTES: &[AttributeId] = &[
    AttributeId::Label,
    AttributeId::Rotation,
    AttributeId::Mirror,
];
const CONST_ATTRIBUTES: &[AttributeId] = &[
    AttributeId::Label,
    AttributeId::Value,
    AttributeId::Rotation,
    AttributeId::Mirror,
];
const BINARY_ATTRIBUTES: &[AttributeId] = &[
    AttributeId::Label,
    AttributeId::Rotation,
    AttributeId::Mirror,
    AttributeId::WideShape,
    AttributeId::Inputs,
];
const TWO_INPUT_GATE: &[(&str, &str)] = &[("Bits", "1"), ("Inputs", "2"), ("wideShape", "false")];
pub const DESCRIPTORS: [ComponentDescriptor; 10] = [
    ComponentDescriptor {
        kind: ComponentKind::Input,
        palette_name: "Input",
        digital_name: "In",
        input_pins: 0,
        output_pins: 1,
        defaults: ONE_BIT,
        attributes: COMMON_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::Output,
        palette_name: "Output",
        digital_name: "Out",
        input_pins: 1,
        output_pins: 0,
        defaults: ONE_BIT,
        attributes: COMMON_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::And,
        palette_name: "AND",
        digital_name: "And",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
        attributes: BINARY_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::NAnd,
        palette_name: "NAND",
        digital_name: "NAnd",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
        attributes: BINARY_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::Or,
        palette_name: "OR",
        digital_name: "Or",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
        attributes: BINARY_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::NOr,
        palette_name: "NOR",
        digital_name: "NOr",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
        attributes: BINARY_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::XOr,
        palette_name: "XOR",
        digital_name: "XOr",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
        attributes: BINARY_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::XNOr,
        palette_name: "XNOR",
        digital_name: "XNOr",
        input_pins: 2,
        output_pins: 1,
        defaults: TWO_INPUT_GATE,
        attributes: BINARY_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::Not,
        palette_name: "NOT",
        digital_name: "Not",
        input_pins: 1,
        output_pins: 1,
        defaults: ONE_BIT,
        attributes: COMMON_ATTRIBUTES,
    },
    ComponentDescriptor {
        kind: ComponentKind::Const,
        palette_name: "Const",
        digital_name: "Const",
        input_pins: 0,
        output_pins: 1,
        defaults: CONST_DEFAULTS,
        attributes: CONST_ATTRIBUTES,
    },
];
pub const KINDS: [ComponentKind; 10] = [
    ComponentKind::Input,
    ComponentKind::Output,
    ComponentKind::And,
    ComponentKind::NAnd,
    ComponentKind::Or,
    ComponentKind::NOr,
    ComponentKind::XOr,
    ComponentKind::XNOr,
    ComponentKind::Not,
    ComponentKind::Const,
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
        for kind in KINDS {
            let attrs = attribute_descriptors(kind);
            assert!(
                attrs
                    .iter()
                    .any(|attribute| attribute.id == AttributeId::Label && attribute.editable)
            );
            assert_eq!(
                attrs
                    .iter()
                    .any(|attribute| attribute.id == AttributeId::Value),
                kind == ComponentKind::Const
            );
            assert_eq!(
                attrs
                    .iter()
                    .any(|attribute| attribute.id == AttributeId::WideShape),
                supports_wide_shape(kind)
            );
            assert_eq!(
                attrs
                    .iter()
                    .find(|attribute| attribute.id == AttributeId::Inputs)
                    .is_some(),
                supports_wide_shape(kind)
            );
            assert!(
                attrs
                    .iter()
                    .filter(|attribute| attribute.id == AttributeId::Inputs)
                    .all(|attribute| !attribute.editable)
            );
        }
    }
}
