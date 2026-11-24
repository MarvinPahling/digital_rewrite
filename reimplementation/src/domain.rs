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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Orientation {
    /// Quarter-turn count, counterclockwise in screen coordinates. Only 0..=3 is valid.
    pub rotation: u8,
    pub mirror: bool,
}

impl Orientation {
    /// Mirror local Y first, then rotate counterclockwise in screen coordinates.
    pub fn transform_f32(self, x: f32, y: f32) -> Option<(f32, f32)> {
        if self.rotation > 3 {
            return None;
        }
        let y = if self.mirror { -y } else { y };
        Some(match self.rotation {
            0 => (x, y),
            1 => (y, -x),
            2 => (-x, -y),
            3 => (-y, x),
            _ => return None,
        })
    }

    pub fn transform(self, x: i32, y: i32) -> Option<(i32, i32)> {
        if self.rotation > 3 {
            return None;
        }
        let y = if self.mirror { y.checked_neg()? } else { y };
        Some(match self.rotation {
            0 => (x, y),
            1 => (y, x.checked_neg()?),
            2 => (x.checked_neg()?, y.checked_neg()?),
            3 => (y.checked_neg()?, x),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub id: ComponentId,
    pub kind: ComponentKind,
    /// Artwork bounds center in world coordinates (the historical document position).
    pub position: Position,
    pub label: String,
    /// Configured one-bit constant output; meaningful only for `Const`, defaults HIGH.
    pub constant_value: bool,
    /// Digital's generic gate shape option; meaningful only for the six binary gates.
    pub wide_shape: bool,
    pub rotation: u8,
    pub mirror: bool,
}
/// Geometric wire segments are the sole authoritative connectivity representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wiring {
    GeometricSegments(Vec<crate::geometric_wiring::Segment>),
}
impl Default for Wiring {
    fn default() -> Self {
        Self::GeometricSegments(Vec::new())
    }
}
impl Wiring {
    pub fn len(&self) -> usize {
        match self {
            Self::GeometricSegments(segments) => segments.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn geometric(
        &self,
    ) -> Result<&[crate::geometric_wiring::Segment], crate::geometric_wiring::GeometryError> {
        match self {
            Self::GeometricSegments(segments) => Ok(segments),
        }
    }
    pub fn geometric_mut(
        &mut self,
    ) -> Result<&mut Vec<crate::geometric_wiring::Segment>, crate::geometric_wiring::GeometryError>
    {
        match self {
            Self::GeometricSegments(segments) => Ok(segments),
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
