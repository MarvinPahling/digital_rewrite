//! Editable component documents.
#![allow(dead_code)] // Wire editing is exposed to the UI in a later stage.

pub const MAX_LABEL_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelError {
    TooLong { bytes: usize },
    InvalidXmlCharacter { character: char },
}
impl std::fmt::Display for LabelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { bytes } => write!(
                f,
                "Label is {bytes} UTF-8 bytes; maximum is {MAX_LABEL_BYTES}"
            ),
            Self::InvalidXmlCharacter { character } => write!(
                f,
                "Label contains character U+{:04X}, which XML 1.0 cannot represent",
                *character as u32
            ),
        }
    }
}
impl std::error::Error for LabelError {}

/// Validate exactly the XML 1.0 character repertoire used by `.dig` labels. Unicode whitespace
/// is meaningful and is deliberately neither trimmed nor rejected.
pub fn validate_label(label: &str) -> Result<(), LabelError> {
    if label.len() > MAX_LABEL_BYTES {
        return Err(LabelError::TooLong { bytes: label.len() });
    }
    for character in label.chars() {
        let code = character as u32;
        let allowed = matches!(code, 0x9 | 0xA | 0xD)
            || (0x20..=0xD7FF).contains(&code)
            || (0xE000..=0xFFFD).contains(&code)
            || (0x10000..=0x10FFFF).contains(&code);
        if !allowed {
            return Err(LabelError::InvalidXmlCharacter { character });
        }
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    Input,
    Output,
    And,
    NAnd,
    Or,
    NOr,
    XOr,
    XNOr,
    Not,
    Const,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PinDirection {
    Input,
    Output,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PinId {
    pub direction: PinDirection,
    pub index: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PinRef {
    pub component: ComponentId,
    pub pin: PinId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WireId(pub u64);
/// An explicit logical pin-pair connection. Component positions and rendered route geometry do
/// not add endpoints or join nets in the current compiler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub id: WireId,
    pub endpoints: [PinRef; 2],
}
impl ComponentKind {
    pub fn has_pin(self, pin: PinId) -> bool {
        let count = match (self, pin.direction) {
            (Self::Input | Self::Const, PinDirection::Output)
            | (Self::Output, PinDirection::Input)
            | (Self::Not, _)
            | (
                Self::And | Self::NAnd | Self::Or | Self::NOr | Self::XOr | Self::XNOr,
                PinDirection::Output,
            ) => 1,
            (
                Self::And | Self::NAnd | Self::Or | Self::NOr | Self::XOr | Self::XNOr,
                PinDirection::Input,
            ) => 2,
            _ => 0,
        };
        pin.index < count
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}
impl Position {
    pub fn valid(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub id: ComponentId,
    pub kind: ComponentKind,
    pub position: Position,
    pub label: String,
    /// Configured one-bit constant output; meaningful only for `Const`, defaults HIGH.
    pub constant_value: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wiring {
    LegacyPinPairs(Vec<Wire>),
    GeometricSegments(Vec<crate::geometric_wiring::Segment>),
}
impl Default for Wiring {
    fn default() -> Self {
        Self::GeometricSegments(Vec::new())
    }
}
impl From<Vec<Wire>> for Wiring {
    fn from(wires: Vec<Wire>) -> Self {
        Self::LegacyPinPairs(wires)
    }
}
impl FromIterator<Wire> for Wiring {
    fn from_iter<T: IntoIterator<Item = Wire>>(iter: T) -> Self {
        Self::LegacyPinPairs(iter.into_iter().collect())
    }
}
impl Wiring {
    pub fn len(&self) -> usize {
        match self {
            Self::LegacyPinPairs(w) => w.len(),
            Self::GeometricSegments(s) => s.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn iter_legacy(
        &self,
    ) -> Result<std::slice::Iter<'_, Wire>, crate::geometric_wiring::GeometryError> {
        Ok(self.legacy()?.iter())
    }
    pub fn legacy_mut(&mut self) -> Result<&mut Vec<Wire>, crate::geometric_wiring::GeometryError> {
        match self {
            Self::LegacyPinPairs(wires) => Ok(wires),
            Self::GeometricSegments(_) => Err(crate::geometric_wiring::GeometryError::WrongMode),
        }
    }
    pub fn legacy(&self) -> Result<&[Wire], crate::geometric_wiring::GeometryError> {
        match self {
            Self::LegacyPinPairs(wires) => Ok(wires),
            Self::GeometricSegments(_) => Err(crate::geometric_wiring::GeometryError::WrongMode),
        }
    }
    pub fn geometric(
        &self,
    ) -> Result<&[crate::geometric_wiring::Segment], crate::geometric_wiring::GeometryError> {
        match self {
            Self::GeometricSegments(segments) => Ok(segments),
            Self::LegacyPinPairs(_) => Err(crate::geometric_wiring::GeometryError::WrongMode),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub components: Vec<Component>,
    pub wires: Wiring,
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn label_limits_are_utf8_bytes_and_xml_10_characters_without_trimming() {
        assert!(validate_label("").is_ok());
        assert!(validate_label(" \u{2003}\t\n\r").is_ok());
        assert!(validate_label(&"é".repeat(2048)).is_ok());
        assert!(matches!(
            validate_label(&format!("{}x", "é".repeat(2048))),
            Err(LabelError::TooLong { bytes: 4097 })
        ));
        assert!(matches!(
            validate_label(&"é".repeat(2049)),
            Err(LabelError::TooLong { bytes: 4098 })
        ));
        assert!(matches!(
            validate_label(&"x".repeat(4097)),
            Err(LabelError::TooLong { bytes: 4097 })
        ));
        for forbidden in [
            '\u{0}', '\u{1}', '\u{8}', '\u{B}', '\u{C}', '\u{1F}', '\u{FFFE}', '\u{FFFF}',
        ] {
            assert!(matches!(
                validate_label(&forbidden.to_string()),
                Err(LabelError::InvalidXmlCharacter { .. })
            ));
        }
        assert!(validate_label("tab\tline\nreturn\r & < > 💡").is_ok());
    }
}
