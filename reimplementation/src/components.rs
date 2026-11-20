//! Component palette descriptors; symbols are original UI drawings.
use crate::domain::ComponentKind;
pub const KINDS: [ComponentKind; 5] = [
    ComponentKind::Input,
    ComponentKind::Output,
    ComponentKind::And,
    ComponentKind::Or,
    ComponentKind::Not,
];
impl ComponentKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Input => "Input",
            Self::Output => "Output",
            Self::And => "AND",
            Self::Or => "OR",
            Self::Not => "NOT",
        }
    }
}
