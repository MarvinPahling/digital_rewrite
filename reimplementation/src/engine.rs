//! Validated document mutations and snapshot history.
use crate::domain::{Component, ComponentId, ComponentKind, Document, Position};
#[derive(Default)]
pub struct Session {
    pub document: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    saved: Document,
    next_id: u64,
}
impl Session {
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
    ) -> Result<ComponentId, &'static str> {
        if !position.valid() {
            return Err("Invalid position");
        }
        let id = ComponentId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("No component IDs remaining")?;
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
    pub fn move_component(
        &mut self,
        id: ComponentId,
        position: Position,
    ) -> Result<(), &'static str> {
        if !position.valid() {
            return Err("Invalid position");
        }
        let mut next = self.document.clone();
        let component = next
            .components
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or("Unknown component")?;
        if component.position != position {
            component.position = position;
            self.edit(next);
        }
        Ok(())
    }
    pub fn delete(&mut self, id: ComponentId) -> Result<(), &'static str> {
        let mut next = self.document.clone();
        let index = next
            .components
            .iter()
            .position(|c| c.id == id)
            .ok_or("Unknown component")?;
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
