//! Validated document mutations and snapshot history.
use crate::domain::{Component, ComponentId, ComponentKind, Document, Position};
#[derive(Default)]
pub struct Session {
    document: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    saved: Document,
    next_id: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    InvalidPosition,
    UnknownComponent,
    IdExhausted,
}
impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPosition => "Invalid position",
            Self::UnknownComponent => "Unknown component",
            Self::IdExhausted => "No component IDs remaining",
        })
    }
}
impl std::error::Error for EditError {}
impl Session {
    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn dirty(&self) -> bool {
        self.document != self.saved
    }
    fn edit(&mut self, next: Document) {
        self.undo.push(std::mem::replace(&mut self.document, next));
        self.redo.clear();
    }
    pub fn place(
        &mut self,
        kind: ComponentKind,
        position: Position,
    ) -> Result<ComponentId, EditError> {
        if !position.valid() {
            return Err(EditError::InvalidPosition);
        }
        let id = ComponentId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(EditError::IdExhausted)?;
        let mut next = self.document.clone();
        next.components.push(Component {
            id,
            kind,
            position,
            label: kind.name().into(),
        });
        self.edit(next);
        Ok(id)
    }
    pub fn move_component(&mut self, id: ComponentId, position: Position) -> Result<(), EditError> {
        if !position.valid() {
            return Err(EditError::InvalidPosition);
        }
        let mut next = self.document.clone();
        let component = next
            .components
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or(EditError::UnknownComponent)?;
        if component.position != position {
            component.position = position;
            self.edit(next);
        }
        Ok(())
    }
    pub fn delete(&mut self, id: ComponentId) -> Result<(), EditError> {
        let mut next = self.document.clone();
        let index = next
            .components
            .iter()
            .position(|c| c.id == id)
            .ok_or(EditError::UnknownComponent)?;
        next.components.remove(index);
        self.edit(next);
        Ok(())
    }
    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.document, previous));
        }
    }
    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.document, next));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_errors_and_id_exhaustion_leave_state_unchanged() {
        let mut session = Session::default();
        let p = Position { x: 1., y: 2. };
        let id = session.place(ComponentKind::Not, p).unwrap();
        let snapshot = session.document().clone();
        assert_eq!(
            session.move_component(
                id,
                Position {
                    x: f32::INFINITY,
                    y: 0.
                }
            ),
            Err(EditError::InvalidPosition)
        );
        assert_eq!(
            session.delete(ComponentId(999)),
            Err(EditError::UnknownComponent)
        );
        session.next_id = u64::MAX;
        assert_eq!(
            session.place(ComponentKind::And, p),
            Err(EditError::IdExhausted)
        );
        assert_eq!(*session.document(), snapshot);
        session.undo();
        assert!(!session.dirty());
        session.redo();
        assert_eq!(*session.document(), snapshot);
    }
    #[test]
    fn history_and_branch_ids() {
        let mut s = Session::default();
        let p = Position { x: 1., y: 2. };
        let id = s.place(ComponentKind::And, p).unwrap();
        assert!(s.dirty());
        s.undo();
        assert!(!s.dirty());
        assert!(s.move_component(id, p).is_err());
        s.redo();
        assert_eq!(s.document.components[0].id, id);
        s.move_component(id, p).unwrap();
        s.undo();
        let other = s.place(ComponentKind::Or, p).unwrap();
        assert_ne!(id, other);
        s.redo();
        assert_eq!(s.document.components.len(), 1);
    }
    #[test]
    fn all_kinds_and_failed_edits_preserve_redo() {
        let mut session = Session::default();
        let position = Position { x: 10.0, y: 20.0 };
        for kind in crate::components::KINDS {
            let id = session.place(kind, position).unwrap();
            session
                .move_component(id, Position { x: 30.0, y: 40.0 })
                .unwrap();
            session.delete(id).unwrap();
        }
        assert!(session.document.components.is_empty());
        session.undo();
        let previous = session.document.clone();
        let id = previous.components[0].id;
        assert!(
            session
                .move_component(
                    id,
                    Position {
                        x: f32::NAN,
                        y: 0.0
                    }
                )
                .is_err()
        );
        assert!(session.delete(ComponentId(u64::MAX)).is_err());
        session
            .move_component(id, previous.components[0].position)
            .unwrap();
        assert_eq!(session.document, previous);
        session.redo();
        assert!(session.document.components.is_empty());
    }
}
