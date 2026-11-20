//! Editable component documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    Input,
    Output,
    And,
    Or,
    Not,
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
}
