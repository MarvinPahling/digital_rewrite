//! Editable component documents.
#![allow(dead_code)] // Wire editing is exposed to the UI in a later stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    Input,
    Output,
    And,
    Or,
    Not,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub id: WireId,
    pub endpoints: [PinRef; 2],
}
impl ComponentKind {
    pub fn has_pin(self, pin: PinId) -> bool {
        let count = match (self, pin.direction) {
            (Self::Input, PinDirection::Output)
            | (Self::Output, PinDirection::Input)
            | (Self::Not, _)
            | (Self::And | Self::Or, PinDirection::Output) => 1,
            (Self::And | Self::Or, PinDirection::Input) => 2,
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
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub components: Vec<Component>,
    pub wires: Vec<Wire>,
}
