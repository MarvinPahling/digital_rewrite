//! Validated document mutations and snapshot history.
#![allow(dead_code)] // Wire editing is exposed to the UI in a later stage.
use crate::domain::{
    Component, ComponentId, ComponentKind, Document, PinRef, Position, Wire, WireId,
};
use crate::file_session::{
    self, DiscardIntent, FileIo, FileOperationError, FileStage, PublishFailure, PublishOutcome,
    SaveAsIntent,
};
use crate::formats::{self, DigReadError, DigWriteError};
use crate::runtime::{InputChange, Runtime, RuntimeError, SettleOutcome, StepOutcome};
use crate::simulation::CompileError;
use std::path::{Path, PathBuf};
#[derive(Default)]
pub struct Session {
    document: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    saved: Document,
    next_id: u64,
    next_wire_id: u64,
    /// Transient simulation state. Never part of the document or its history.
    runtime: Option<Runtime>,
    path: Option<PathBuf>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// Editing is locked while a simulation is active; stop it first.
    SimulationActive,
    InvalidPosition,
    UnknownComponent,
    IdExhausted,
    InvalidPin,
    SameEndpoint,
    DuplicateWire,
    UnknownWire,
    WireIdExhausted,
}
impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SimulationActive => "Stop the simulation before editing",
            Self::InvalidPosition => "Invalid position",
            Self::UnknownComponent => "Unknown component",
            Self::IdExhausted => "No component IDs remaining",
            Self::InvalidPin => "Invalid pin",
            Self::SameEndpoint => "Wire endpoints must differ",
            Self::DuplicateWire => "Wire already exists",
            Self::UnknownWire => "Unknown wire",
            Self::WireIdExhausted => "No wire IDs remaining",
        })
    }
}
impl std::error::Error for EditError {}
#[derive(Debug)]
pub enum SessionDigError {
    SimulationActive,
    Read(DigReadError),
    Domain(formats::DomainCodecError),
    Write(DigWriteError),
    DirtyDiscardRequired,
    Path(FileOperationError),
    File(FileOperationError),
}
impl std::fmt::Display for SessionDigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SimulationActive => f.write_str("Stop the simulation before opening a document"),
            Self::Read(e) => e.fmt(f),
            Self::Domain(e) => e.fmt(f),
            Self::Write(e) => e.fmt(f),
            Self::DirtyDiscardRequired => {
                f.write_str("Document has unsaved changes; explicit discard intent is required")
            }
            Self::Path(e) => e.fmt(f),
            Self::File(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for SessionDigError {}
#[derive(Debug)]
pub enum SaveError {
    SimulationActive,
    PathRequired,
    Path(FileOperationError),
    Codec(SessionDigError),
    NotPublished(PublishFailure),
}
impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SimulationActive => f.write_str("Stop the simulation before saving"),
            Self::PathRequired => {
                f.write_str("Save As is required because this session has no file path")
            }
            Self::Path(e) => e.fmt(f),
            Self::Codec(e) => e.fmt(f),
            Self::NotPublished(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for SaveError {}

#[derive(Debug, PartialEq, Eq)]
pub enum SimulationError {
    AlreadyActive,
    NotActive,
    Compile(CompileError),
    Runtime(RuntimeError),
}
impl std::fmt::Display for SimulationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("Simulation is already running"),
            Self::NotActive => f.write_str("Simulation is not running"),
            Self::Compile(error) => write!(f, "Circuit cannot be simulated: {error:?}"),
            Self::Runtime(error) => write!(f, "Simulation input rejected: {error:?}"),
        }
    }
}
impl std::error::Error for SimulationError {}
impl Session {
    /// Replace this document with a clean untitled document. Dirty state requires an explicit
    /// discard intent; simulation is checked first, before any session state is changed.
    pub fn new_document(&mut self, discard: DiscardIntent) -> Result<(), SessionDigError> {
        if self.runtime.is_some() {
            return Err(SessionDigError::SimulationActive);
        }
        if self.dirty() && discard == DiscardIntent::RefuseDirty {
            return Err(SessionDigError::DirtyDiscardRequired);
        }
        self.document = Document::default();
        self.saved = Document::default();
        self.path = None;
        self.undo.clear();
        self.redo.clear();
        self.next_id = 0;
        self.next_wire_id = 0;
        Ok(())
    }

    pub fn document(&self) -> &Document {
        &self.document
    }
    /// Parse and convert completely before replacing session state.
    pub fn open_dig_bytes(&mut self, bytes: &[u8]) -> Result<(), SessionDigError> {
        if self.runtime.is_some() {
            return Err(SessionDigError::SimulationActive);
        }
        let parsed = formats::parse_v2(bytes).map_err(SessionDigError::Read)?;
        let document = formats::to_domain(&parsed).map_err(SessionDigError::Domain)?;
        let next_id = u64::try_from(document.components.len()).map_err(|_| {
            SessionDigError::Domain(formats::DomainCodecError(
                "component ID counter overflow".into(),
            ))
        })?;
        let next_wire_id = u64::try_from(document.wires.len()).map_err(|_| {
            SessionDigError::Domain(formats::DomainCodecError("wire ID counter overflow".into()))
        })?;
        self.document = document.clone();
        self.saved = document;
        self.path = None;
        self.undo.clear();
        self.redo.clear();
        self.next_id = next_id;
        self.next_wire_id = next_wire_id;
        Ok(())
    }
    /// Export is read-only and never marks the session saved.
    pub fn export_dig_bytes(&self) -> Result<Vec<u8>, SessionDigError> {
        let dig = formats::from_domain(&self.document).map_err(SessionDigError::Domain)?;
        formats::serialize_v2(&dig).map_err(SessionDigError::Write)
    }

    /// The associated absolute path, if this session has been opened/saved from disk.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Bounded open through an explicitly based, injectable filesystem adapter. Symlinks,
    /// directories and other non-regular files are refused by the local adapter.
    pub fn open_path(
        &mut self,
        io: &mut dyn FileIo,
        path: &Path,
        discard: DiscardIntent,
    ) -> Result<(), SessionDigError> {
        if self.runtime.is_some() {
            return Err(SessionDigError::SimulationActive);
        }
        if self.dirty() && matches!(discard, DiscardIntent::RefuseDirty) {
            return Err(SessionDigError::DirtyDiscardRequired);
        }
        let path = file_session::resolve_path(io.base_dir(), path).map_err(|source| {
            SessionDigError::Path(FileOperationError {
                stage: FileStage::ResolvePath,
                path: path.to_path_buf(),
                source,
            })
        })?;
        let bytes = io
            .read_bounded_regular(&path, file_session::MAX_OPEN_BYTES)
            .map_err(|source| {
                SessionDigError::File(FileOperationError {
                    stage: FileStage::Read,
                    path: path.clone(),
                    source,
                })
            })?;
        if bytes.len() > file_session::MAX_OPEN_BYTES {
            return Err(SessionDigError::Read(DigReadError {
                kind: formats::DigReadErrorKind::InputLimit,
                offset: file_session::MAX_OPEN_BYTES,
                context: "document".into(),
                message: "input exceeds 4 MiB".into(),
            }));
        }
        self.open_bytes_at(&bytes, Some(path))
    }

    fn open_bytes_at(
        &mut self,
        bytes: &[u8],
        path: Option<PathBuf>,
    ) -> Result<(), SessionDigError> {
        let parsed = formats::parse_v2(bytes).map_err(SessionDigError::Read)?;
        let document = formats::to_domain(&parsed).map_err(SessionDigError::Domain)?;
        let next_id = u64::try_from(document.components.len()).map_err(|_| {
            SessionDigError::Domain(formats::DomainCodecError(
                "component ID counter overflow".into(),
            ))
        })?;
        let next_wire_id = u64::try_from(document.wires.len()).map_err(|_| {
            SessionDigError::Domain(formats::DomainCodecError("wire ID counter overflow".into()))
        })?;
        self.document = document.clone();
        self.saved = document;
        self.undo.clear();
        self.redo.clear();
        self.next_id = next_id;
        self.next_wire_id = next_wire_id;
        self.path = path;
        Ok(())
    }

    /// Save to the current association. All encoding finishes before the filesystem is touched.
    pub fn save(&mut self, io: &mut dyn FileIo) -> Result<PublishOutcome, SaveError> {
        self.ensure_file_editable()?;
        let target = self.path.clone().ok_or(SaveError::PathRequired)?;
        self.save_encoded(io, target, SaveAsIntent::Replace)
    }

    /// Save As requires explicit intent: create-only is atomically no-clobber; replacement uses
    /// same-directory atomic replacement. Association and saved baseline change only after publish.
    pub fn save_as(
        &mut self,
        io: &mut dyn FileIo,
        path: &Path,
        intent: SaveAsIntent,
    ) -> Result<PublishOutcome, SaveError> {
        self.ensure_file_editable()?;
        let target = file_session::resolve_path(io.base_dir(), path).map_err(|source| {
            SaveError::Path(FileOperationError {
                stage: FileStage::ResolvePath,
                path: path.to_path_buf(),
                source,
            })
        })?;
        self.save_encoded(io, target, intent)
    }

    fn ensure_file_editable(&self) -> Result<(), SaveError> {
        if self.runtime.is_some() {
            Err(SaveError::SimulationActive)
        } else {
            Ok(())
        }
    }

    fn save_encoded(
        &mut self,
        io: &mut dyn FileIo,
        target: PathBuf,
        intent: SaveAsIntent,
    ) -> Result<PublishOutcome, SaveError> {
        let bytes = self.export_dig_bytes().map_err(SaveError::Codec)?;
        let outcome = file_session::publish_bytes(io, &target, &bytes, intent)
            .map_err(SaveError::NotPublished)?;
        self.saved = self.document.clone();
        self.path = Some(target);
        Ok(outcome)
    }

    pub fn dirty(&self) -> bool {
        self.document != self.saved
    }
    pub fn simulation_active(&self) -> bool {
        self.runtime.is_some()
    }
    /// Read-only view of the running simulation, if any.
    pub fn inspect(&self) -> Option<&Runtime> {
        self.runtime.as_ref()
    }
    /// Compiles and starts a runtime. On failure nothing changes and no simulation is active.
    pub fn start_simulation(&mut self) -> Result<(), SimulationError> {
        if self.runtime.is_some() {
            return Err(SimulationError::AlreadyActive);
        }
        self.runtime = Some(Runtime::start(&self.document).map_err(SimulationError::Compile)?);
        Ok(())
    }
    /// Discards the runtime; the document, history, dirty state and ids were never touched.
    pub fn stop_simulation(&mut self) -> Result<(), SimulationError> {
        self.runtime
            .take()
            .map(drop)
            .ok_or(SimulationError::NotActive)
    }
    fn runtime_mut(&mut self) -> Result<&mut Runtime, SimulationError> {
        self.runtime.as_mut().ok_or(SimulationError::NotActive)
    }
    pub fn step_simulation(&mut self) -> Result<StepOutcome, SimulationError> {
        Ok(self.runtime_mut()?.step())
    }
    pub fn settle_simulation(&mut self, budget: usize) -> Result<SettleOutcome, SimulationError> {
        Ok(self.runtime_mut()?.settle(budget))
    }
    pub fn toggle_input(&mut self, id: ComponentId) -> Result<InputChange, SimulationError> {
        self.runtime_mut()?
            .toggle_input(id)
            .map_err(SimulationError::Runtime)
    }
    /// Set a one-bit input without changing the editable document or history.
    pub fn set_input(
        &mut self,
        id: ComponentId,
        value: bool,
    ) -> Result<InputChange, SimulationError> {
        self.runtime_mut()?
            .set_input(id, value)
            .map_err(SimulationError::Runtime)
    }
    fn ensure_editable(&self) -> Result<(), EditError> {
        if self.runtime.is_some() {
            Err(EditError::SimulationActive)
        } else {
            Ok(())
        }
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
        self.ensure_editable()?;
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
        self.ensure_editable()?;
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
        self.ensure_editable()?;
        let mut next = self.document.clone();
        let index = next
            .components
            .iter()
            .position(|c| c.id == id)
            .ok_or(EditError::UnknownComponent)?;
        next.components.remove(index);
        next.wires
            .retain(|wire| wire.endpoints.iter().all(|pin| pin.component != id));
        self.edit(next);
        Ok(())
    }
    pub fn connect(&mut self, endpoints: [PinRef; 2]) -> Result<WireId, EditError> {
        self.ensure_editable()?;
        for endpoint in endpoints {
            let component = self
                .document
                .components
                .iter()
                .find(|c| c.id == endpoint.component)
                .ok_or(EditError::UnknownComponent)?;
            if !component.kind.has_pin(endpoint.pin) {
                return Err(EditError::InvalidPin);
            }
        }
        if endpoints[0] == endpoints[1] {
            return Err(EditError::SameEndpoint);
        }
        if self
            .document
            .wires
            .iter()
            .any(|w| w.endpoints == endpoints || w.endpoints == [endpoints[1], endpoints[0]])
        {
            return Err(EditError::DuplicateWire);
        }
        let id = WireId(self.next_wire_id);
        let next_id = self
            .next_wire_id
            .checked_add(1)
            .ok_or(EditError::WireIdExhausted)?;
        let mut next = self.document.clone();
        next.wires.push(Wire { id, endpoints });
        self.next_wire_id = next_id;
        self.edit(next);
        Ok(id)
    }
    pub fn delete_wire(&mut self, id: WireId) -> Result<(), EditError> {
        self.ensure_editable()?;
        let mut next = self.document.clone();
        let index = next
            .wires
            .iter()
            .position(|w| w.id == id)
            .ok_or(EditError::UnknownWire)?;
        next.wires.remove(index);
        self.edit(next);
        Ok(())
    }
    pub fn undo(&mut self) -> Result<(), EditError> {
        self.ensure_editable()?;
        if let Some(previous) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.document, previous));
        }
        Ok(())
    }
    pub fn redo(&mut self) -> Result<(), EditError> {
        self.ensure_editable()?;
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.document, next));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn set_input_changes_only_runtime_not_document_history_or_dirty_state() {
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let document = session.document().clone();
        let undo_len = session.undo.len();
        let dirty = session.dirty();
        session.start_simulation().unwrap();
        assert_eq!(
            session.settle_simulation(10).unwrap(),
            SettleOutcome::Settled { work: 0 }
        );
        assert_eq!(session.set_input(id, true).unwrap(), InputChange::Changed);
        assert_eq!(
            session.settle_simulation(10).unwrap(),
            SettleOutcome::Settled { work: 0 }
        );
        assert_eq!(
            session.inspect().unwrap().pin_value(PinRef {
                component: id,
                pin: crate::domain::PinId {
                    direction: crate::domain::PinDirection::Output,
                    index: 0
                }
            }),
            Some(crate::runtime::Level::High)
        );
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo.len(), undo_len);
        assert_eq!(session.dirty(), dirty);
        session.stop_simulation().unwrap();
    }

    #[test]
    fn new_document_is_clean_transactional_and_requires_discard() {
        let mut session = Session::default();
        session
            .place(ComponentKind::Input, Position { x: 1., y: 2. })
            .unwrap();
        session.saved = Document::default();
        session.path = Some(PathBuf::from("/captured/document.dig"));
        session.undo().unwrap();
        session.redo().unwrap();
        assert!(session.dirty());
        assert!(matches!(
            session.new_document(DiscardIntent::RefuseDirty),
            Err(SessionDigError::DirtyDiscardRequired)
        ));
        assert_eq!(session.document().components.len(), 1);
        session.new_document(DiscardIntent::DiscardDirty).unwrap();
        assert!(session.document().components.is_empty());
        assert!(!session.dirty());
        assert!(session.path().is_none());
        assert!(session.undo.is_empty() && session.redo.is_empty());
        assert_eq!(
            session
                .place(ComponentKind::Output, Position { x: 0., y: 0. })
                .unwrap(),
            ComponentId(0)
        );
    }

    #[test]
    fn new_document_checks_simulation_before_dirty_validation_and_preserves_state() {
        let mut session = Session::default();
        session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        session.start_simulation().unwrap();
        let before = snapshot(&session);
        assert!(matches!(
            session.new_document(DiscardIntent::DiscardDirty),
            Err(SessionDigError::SimulationActive)
        ));
        assert_unchanged(&session, &before);
        session.stop_simulation().unwrap();
    }

    #[test]
    fn open_is_transactional_and_success_resets_history_ids_and_dirty_state() {
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::Input, Position { x: 7.0, y: -9.0 })
            .unwrap();
        session.undo().unwrap();
        session.redo().unwrap();
        let before = snapshot(&session);
        assert!(session.open_dig_bytes(b"not xml").is_err());
        assert_unchanged(&session, &before);
        let noncanonical = b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement><visualElement><elementName>Out</elementName><elementAttributes/><pos x=\"100\" y=\"100\"/></visualElement></visualElements><wires><wire><p1 x=\"0\" y=\"0\"/><p2 x=\"0\" y=\"100\"/></wire><wire><p1 x=\"0\" y=\"100\"/><p2 x=\"100\" y=\"100\"/></wire></wires></circuit>";
        assert!(matches!(
            session.open_dig_bytes(noncanonical),
            Err(SessionDigError::Domain(_))
        ));
        assert_unchanged(&session, &before);
        let xml=b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement></visualElements><wires/></circuit>";
        session.open_dig_bytes(xml).unwrap();
        assert!(!session.dirty());
        assert!(session.undo.is_empty());
        assert!(session.redo.is_empty());
        let new = session
            .place(ComponentKind::Output, Position { x: 1.0, y: 2.0 })
            .unwrap();
        assert_eq!(new, ComponentId(1));
        assert_ne!(new, id);
    }
    #[test]
    fn export_is_read_only_and_open_refuses_simulation_without_mutation() {
        let mut session = Session::default();
        session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let before = snapshot(&session);
        let bytes = session.export_dig_bytes().unwrap();
        assert_eq!(bytes,b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes><entry><string>Label</string><string>Input</string></entry></elementAttributes><pos x=\"18\" y=\"0\"/></visualElement></visualElements><wires/></circuit>");
        assert_unchanged(&session, &before);
        session.start_simulation().unwrap();
        let active = snapshot(&session);
        assert!(matches!(
            session.open_dig_bytes(b"bad"),
            Err(SessionDigError::SimulationActive)
        ));
        assert_unchanged(&session, &active);
        session.stop_simulation().unwrap();
    }
    struct OversizedIo;
    impl crate::file_session::FileIo for OversizedIo {
        fn base_dir(&self) -> &Path {
            Path::new("/")
        }
        fn read_bounded_regular(&mut self, _: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
            Ok(vec![b'x'; limit + 1])
        }
        fn validate_target(
            &mut self,
            _: &Path,
            _: crate::file_session::SaveAsIntent,
        ) -> std::io::Result<()> {
            panic!("unexpected target inspection")
        }
        fn create_temp(
            &mut self,
            _: &Path,
        ) -> std::io::Result<Box<dyn crate::file_session::StagedFile>> {
            panic!("unexpected temp create")
        }
        fn publish(
            &mut self,
            _: &Path,
            _: &Path,
            _: crate::file_session::SaveAsIntent,
        ) -> std::io::Result<()> {
            panic!("unexpected publish")
        }
        fn sync_parent(&mut self, _: &Path) -> std::io::Result<()> {
            panic!("unexpected directory sync")
        }
        fn remove_temp(&mut self, _: &Path) -> std::io::Result<()> {
            panic!("unexpected temp cleanup")
        }
    }

    #[test]
    fn adapter_cannot_return_more_than_the_session_open_bound() {
        let mut session = Session::default();
        session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let before = snapshot(&session);
        assert!(matches!(
            session.open_path(
                &mut OversizedIo,
                Path::new("unchecked.dig"),
                crate::file_session::DiscardIntent::DiscardDirty
            ),
            Err(SessionDigError::Read(_))
        ));
        assert_unchanged(&session, &before);
    }

    #[test]
    fn file_open_failures_and_dirty_or_locked_rejections_are_transactional() {
        use crate::file_session::{DiscardIntent, LocalFileIo};
        let dir = std::env::temp_dir().join(format!("digital-rust-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement></visualElements><wires/></circuit>";
        let path = dir.join("valid.dig");
        std::fs::write(&path, good).unwrap();
        let mut io = LocalFileIo::new(&dir);
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::Not, Position { x: 1., y: 2. })
            .unwrap();
        let before = snapshot(&session);
        assert!(matches!(
            session.open_path(
                &mut io,
                Path::new("missing.dig"),
                DiscardIntent::DiscardDirty
            ),
            Err(SessionDigError::File(_))
        ));
        assert_unchanged(&session, &before);
        assert!(matches!(
            session.open_path(&mut io, Path::new("valid.dig"), DiscardIntent::RefuseDirty),
            Err(SessionDigError::DirtyDiscardRequired)
        ));
        assert_unchanged(&session, &before);
        session
            .open_path(&mut io, Path::new("valid.dig"), DiscardIntent::DiscardDirty)
            .unwrap();
        assert_eq!(session.path(), Some(path.as_path()));
        assert!(!session.dirty());
        assert!(session.undo.is_empty());
        assert!(session.redo.is_empty());
        let next = session
            .place(ComponentKind::Output, Position { x: 4., y: 5. })
            .unwrap();
        assert!(next.0 > id.0);
        for (name, bytes) in [
            ("malformed.dig", b"bad".as_slice()),
            (
                "unsupported.dig",
                b"<circuit><version>1</version></circuit>".as_slice(),
            ),
            (
                "oversized.dig",
                vec![b' '; crate::file_session::MAX_OPEN_BYTES + 1].as_slice(),
            ),
        ] {
            let bad = dir.join(name);
            std::fs::write(&bad, bytes).unwrap();
            let before = snapshot(&session);
            assert!(
                session
                    .open_path(&mut io, &bad, DiscardIntent::DiscardDirty)
                    .is_err()
            );
            assert_unchanged(&session, &before);
        }
        session.delete(next).unwrap();
        session.start_simulation().unwrap();
        let before = snapshot(&session);
        assert!(matches!(
            session.open_path(
                &mut io,
                Path::new("missing.dig"),
                DiscardIntent::DiscardDirty
            ),
            Err(SessionDigError::SimulationActive)
        ));
        assert_unchanged(&session, &before);
        session.stop_simulation().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct NoFileAccess;
    impl crate::file_session::FileIo for NoFileAccess {
        fn base_dir(&self) -> &Path {
            Path::new("/")
        }
        fn read_bounded_regular(&mut self, _: &Path, _: usize) -> std::io::Result<Vec<u8>> {
            panic!("unexpected file read")
        }
        fn validate_target(
            &mut self,
            _: &Path,
            _: crate::file_session::SaveAsIntent,
        ) -> std::io::Result<()> {
            panic!("unexpected target inspection")
        }
        fn create_temp(
            &mut self,
            _: &Path,
        ) -> std::io::Result<Box<dyn crate::file_session::StagedFile>> {
            panic!("unexpected temp create")
        }
        fn publish(
            &mut self,
            _: &Path,
            _: &Path,
            _: crate::file_session::SaveAsIntent,
        ) -> std::io::Result<()> {
            panic!("unexpected publish")
        }
        fn sync_parent(&mut self, _: &Path) -> std::io::Result<()> {
            panic!("unexpected directory sync")
        }
        fn remove_temp(&mut self, _: &Path) -> std::io::Result<()> {
            panic!("unexpected temp cleanup")
        }
    }

    #[test]
    fn save_path_required_and_simulation_locks_precede_any_file_operation() {
        let mut io = NoFileAccess;
        let mut session = Session::default();
        assert!(matches!(
            session.save(&mut io),
            Err(SaveError::PathRequired)
        ));
        session.start_simulation().unwrap();
        assert!(matches!(
            session.save(&mut io),
            Err(SaveError::SimulationActive)
        ));
        assert!(matches!(
            session.save_as(
                &mut io,
                Path::new("x.dig"),
                crate::file_session::SaveAsIntent::Replace
            ),
            Err(SaveError::SimulationActive)
        ));
        assert!(matches!(
            session.open_path(
                &mut io,
                Path::new("x.dig"),
                crate::file_session::DiscardIntent::DiscardDirty
            ),
            Err(SessionDigError::SimulationActive)
        ));
        session.stop_simulation().unwrap();
    }

    #[test]
    fn codec_refusal_does_not_reach_filesystem() {
        let mut io = NoFileAccess;
        let mut session = Session::default();
        session
            .place(
                ComponentKind::Input,
                Position {
                    x: f32::MAX,
                    y: 0.0,
                },
            )
            .unwrap();
        assert!(matches!(
            session.save_as(
                &mut io,
                Path::new("unsupported.dig"),
                crate::file_session::SaveAsIntent::Replace
            ),
            Err(SaveError::Codec(_))
        ));
    }

    #[test]
    fn save_and_save_as_update_association_only_after_publish_and_keep_history() {
        use crate::file_session::{LocalFileIo, PublishOutcome, SaveAsIntent};
        let dir =
            std::env::temp_dir().join(format!("digital-rust-save-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut io = LocalFileIo::new(&dir);
        let mut session = Session::default();
        assert!(matches!(
            session.save(&mut io),
            Err(SaveError::PathRequired)
        ));
        let id = session
            .place(ComponentKind::Input, Position { x: 2., y: 3. })
            .unwrap();
        assert_eq!(
            session
                .save_as(&mut io, Path::new("first.dig"), SaveAsIntent::CreateOnly)
                .unwrap(),
            PublishOutcome::Published
        );
        let first_path = dir.join("first.dig");
        assert_eq!(session.path(), Some(first_path.as_path()));
        assert!(!session.dirty());
        let next = session
            .place(ComponentKind::Not, Position { x: 40., y: 0. })
            .unwrap();
        let undo_len = session.undo.len();
        let before_path = session.path().unwrap().to_path_buf();
        assert!(
            session
                .save_as(&mut io, Path::new("first.dig"), SaveAsIntent::CreateOnly)
                .is_err()
        );
        assert_eq!(session.path(), Some(before_path.as_path()));
        assert!(session.dirty());
        assert_eq!(session.undo.len(), undo_len);
        let second_path = dir.join("second.dig");
        session
            .save_as(&mut io, Path::new("second.dig"), SaveAsIntent::CreateOnly)
            .unwrap();
        assert_eq!(session.path(), Some(second_path.as_path()));
        assert!(!session.dirty());
        assert_eq!(session.undo.len(), undo_len);
        session.undo().unwrap();
        assert!(session.dirty());
        session.redo().unwrap();
        assert!(!session.dirty());
        assert_eq!(session.document.components.last().unwrap().id, next);
        let mut reopened = Session::default();
        reopened
            .open_path(
                &mut io,
                Path::new("second.dig"),
                crate::file_session::DiscardIntent::RefuseDirty,
            )
            .unwrap();
        assert_eq!(reopened.document(), session.document());
        assert!(reopened.path().is_some());
        assert_eq!(id, crate::domain::ComponentId(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn imported_geometric_netlist_roundtrips_and_simulates_truth_table() {
        let xml=b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement><visualElement><elementName>Not</elementName><elementAttributes/><pos x=\"100\" y=\"0\"/></visualElement><visualElement><elementName>Out</elementName><elementAttributes/><pos x=\"200\" y=\"0\"/></visualElement></visualElements><wires><wire><p1 x=\"0\" y=\"0\"/><p2 x=\"100\" y=\"0\"/></wire><wire><p1 x=\"140\" y=\"0\"/><p2 x=\"200\" y=\"0\"/></wire></wires></circuit>";
        let mut session = Session::default();
        session.open_dig_bytes(xml).unwrap();
        let output = ComponentId(2);
        let input = ComponentId(0);
        session.start_simulation().unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::High)
        );
        session.toggle_input(input).unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::Low)
        );
        session.stop_simulation().unwrap();
        let exported = session.export_dig_bytes().unwrap();
        let reopened =
            crate::formats::to_domain(&crate::formats::parse_v2(&exported).unwrap()).unwrap();
        assert_eq!(
            reopened
                .components
                .iter()
                .map(|c| (c.kind, c.position))
                .collect::<Vec<_>>(),
            session
                .document()
                .components
                .iter()
                .map(|c| (c.kind, c.position))
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn wiring_validation_history_and_cascade() {
        use crate::domain::{PinDirection, PinId};
        let mut s = Session::default();
        let p = Position { x: 0.0, y: 0.0 };
        let a = s.place(ComponentKind::Input, p).unwrap();
        let b = s.place(ComponentKind::And, p).unwrap();
        let pin = |component, direction, index| PinRef {
            component,
            pin: PinId { direction, index },
        };
        let x = pin(a, PinDirection::Output, 0);
        let y = pin(b, PinDirection::Input, 0);
        let z = pin(b, PinDirection::Input, 1);
        let first = s.connect([x, y]).unwrap();
        let second = s.connect([x, z]).unwrap();
        let snapshot = s.document.clone();
        assert_eq!(s.connect([y, x]), Err(EditError::DuplicateWire));
        assert_eq!(s.connect([x, x]), Err(EditError::SameEndpoint));
        assert_eq!(
            s.connect([x, pin(b, PinDirection::Input, 2)]),
            Err(EditError::InvalidPin)
        );
        assert_eq!(
            s.connect([x, pin(ComponentId(999), PinDirection::Input, 0)]),
            Err(EditError::UnknownComponent)
        );
        assert_eq!(s.delete_wire(WireId(999)), Err(EditError::UnknownWire));
        assert_eq!(s.document, snapshot);
        s.delete(b).unwrap();
        assert!(s.document.wires.is_empty());
        s.undo().unwrap();
        assert_eq!(s.document, snapshot);
        s.redo().unwrap();
        s.undo().unwrap();
        s.delete_wire(first).unwrap();
        s.undo().unwrap();
        s.undo().unwrap();
        s.next_wire_id = u64::MAX;
        assert_eq!(s.connect([x, z]), Err(EditError::WireIdExhausted));
        s.redo().unwrap();
        assert_eq!(s.document, snapshot);
        s.undo().unwrap();
        s.next_wire_id = 2;
        let third = s.connect([y, z]).unwrap();
        assert_ne!(third, second);
        s.redo().unwrap();
        assert_eq!(s.document.wires.len(), 2);
    }
    #[test]
    fn pin_sets_are_kind_specific() {
        use crate::domain::{PinDirection::*, PinId};
        for (kind, inputs, outputs) in [
            (ComponentKind::Input, 0, 1),
            (ComponentKind::Output, 1, 0),
            (ComponentKind::And, 2, 1),
            (ComponentKind::Or, 2, 1),
            (ComponentKind::Not, 1, 1),
        ] {
            for (direction, count) in [(Input, inputs), (Output, outputs)] {
                for index in 0..3 {
                    assert_eq!(kind.has_pin(PinId { direction, index }), index < count);
                }
            }
        }
    }
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
        session.undo().unwrap();
        assert!(!session.dirty());
        session.redo().unwrap();
        assert_eq!(*session.document(), snapshot);
    }
    #[test]
    fn history_and_branch_ids() {
        let mut s = Session::default();
        let p = Position { x: 1., y: 2. };
        let id = s.place(ComponentKind::And, p).unwrap();
        assert!(s.dirty());
        s.undo().unwrap();
        assert!(!s.dirty());
        assert!(s.move_component(id, p).is_err());
        s.redo().unwrap();
        assert_eq!(s.document.components[0].id, id);
        s.move_component(id, p).unwrap();
        s.undo().unwrap();
        let other = s.place(ComponentKind::Or, p).unwrap();
        assert_ne!(id, other);
        s.redo().unwrap();
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
        session.undo().unwrap();
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
        session.redo().unwrap();
        assert!(session.document.components.is_empty());
    }
    /// Input -> Not -> Output, wired with explicit pins.
    fn wired_not(s: &mut Session) -> (ComponentId, ComponentId, ComponentId) {
        use crate::domain::{PinDirection, PinId};
        let p = Position { x: 0.0, y: 0.0 };
        let input = s.place(ComponentKind::Input, p).unwrap();
        let not = s.place(ComponentKind::Not, p).unwrap();
        let output = s.place(ComponentKind::Output, p).unwrap();
        let pin = |component, direction, index| PinRef {
            component,
            pin: PinId { direction, index },
        };
        s.connect([
            pin(input, PinDirection::Output, 0),
            pin(not, PinDirection::Input, 0),
        ])
        .unwrap();
        s.connect([
            pin(not, PinDirection::Output, 0),
            pin(output, PinDirection::Input, 0),
        ])
        .unwrap();
        (input, not, output)
    }
    struct Snapshot {
        document: Document,
        saved: Document,
        undo: Vec<Document>,
        redo: Vec<Document>,
        next_id: u64,
        next_wire_id: u64,
        dirty: bool,
        path: Option<PathBuf>,
    }
    fn snapshot(s: &Session) -> Snapshot {
        Snapshot {
            document: s.document.clone(),
            saved: s.saved.clone(),
            undo: s.undo.clone(),
            redo: s.redo.clone(),
            next_id: s.next_id,
            next_wire_id: s.next_wire_id,
            dirty: s.dirty(),
            path: s.path.clone(),
        }
    }
    fn assert_unchanged(s: &Session, before: &Snapshot) {
        assert_eq!(s.document, before.document);
        assert_eq!(s.saved, before.saved);
        assert_eq!(s.undo, before.undo);
        assert_eq!(s.redo, before.redo);
        assert_eq!(s.next_id, before.next_id);
        assert_eq!(s.next_wire_id, before.next_wire_id);
        assert_eq!(s.dirty(), before.dirty);
        assert_eq!(s.path, before.path);
    }
    #[test]
    fn failed_start_preserves_everything_and_does_not_activate() {
        let mut s = Session::default();
        let p = Position { x: 0.0, y: 0.0 };
        s.place(ComponentKind::Not, p).unwrap(); // unconnected input: missing driver
        s.undo().unwrap();
        s.redo().unwrap();
        let before = snapshot(&s);
        assert!(matches!(
            s.start_simulation(),
            Err(SimulationError::Compile(CompileError::MissingDriver { .. }))
        ));
        assert!(!s.simulation_active());
        assert!(s.inspect().is_none());
        assert_unchanged(&s, &before);
        assert_eq!(s.stop_simulation(), Err(SimulationError::NotActive));
        assert_eq!(s.step_simulation(), Err(SimulationError::NotActive));
        assert_eq!(s.settle_simulation(8), Err(SimulationError::NotActive));
        assert_eq!(
            s.toggle_input(ComponentId(0)),
            Err(SimulationError::NotActive)
        );
        s.place(ComponentKind::And, p).unwrap(); // editing still works
    }
    #[test]
    fn every_mutating_entry_point_is_locked_without_side_effects() {
        use crate::domain::{PinDirection, PinId};
        let mut s = Session::default();
        let (input, not, _) = wired_not(&mut s);
        let wire = s.document.wires[0].id;
        let spare = s
            .place(ComponentKind::Input, Position { x: 9.0, y: 9.0 })
            .unwrap();
        s.undo().unwrap(); // leaves a redo entry; the document still compiles
        s.start_simulation().unwrap();
        let before = snapshot(&s);
        assert!(s.dirty());
        let endpoints = [
            PinRef {
                component: input,
                pin: PinId {
                    direction: PinDirection::Output,
                    index: 0,
                },
            },
            PinRef {
                component: not,
                pin: PinId {
                    direction: PinDirection::Input,
                    index: 0,
                },
            },
        ];
        let p = Position { x: 5.0, y: 5.0 };
        let locked = EditError::SimulationActive;
        assert_eq!(s.place(ComponentKind::And, p), Err(locked));
        assert_eq!(s.move_component(input, p), Err(EditError::SimulationActive));
        assert_eq!(s.delete(input), Err(EditError::SimulationActive));
        assert_eq!(s.connect(endpoints), Err(EditError::SimulationActive));
        assert_eq!(s.delete_wire(wire), Err(EditError::SimulationActive));
        assert_eq!(s.undo(), Err(EditError::SimulationActive));
        assert_eq!(s.redo(), Err(EditError::SimulationActive));
        // Locked even when the operation would otherwise be invalid.
        assert_eq!(s.delete(ComponentId(999)), Err(EditError::SimulationActive));
        assert_eq!(
            s.place(
                ComponentKind::And,
                Position {
                    x: f32::NAN,
                    y: 0.0
                }
            ),
            Err(EditError::SimulationActive)
        );
        assert_unchanged(&s, &before);
        assert_eq!(before.redo.len(), 1);
        assert!(s.simulation_active());
        s.stop_simulation().unwrap();
        s.redo().unwrap();
        assert!(s.document.components.iter().any(|c| c.id == spare));
    }
    #[test]
    fn locked_redo_history_is_preserved() {
        let mut s = Session::default();
        wired_not(&mut s);
        s.undo().unwrap();
        s.undo().unwrap();
        s.undo().unwrap();
        s.undo().unwrap();
        s.undo().unwrap();
        s.start_simulation().unwrap(); // empty document compiles
        let before = snapshot(&s);
        assert_eq!(before.redo.len(), 5);
        assert_eq!(s.redo(), Err(EditError::SimulationActive));
        assert_eq!(s.undo(), Err(EditError::SimulationActive));
        assert_unchanged(&s, &before);
        s.stop_simulation().unwrap();
        s.redo().unwrap();
        assert_eq!(s.document.components.len(), 1);
    }
    #[test]
    fn stop_restores_editing_exactly_and_runtime_activity_is_not_history() {
        let mut s = Session::default();
        let (input, _, output) = wired_not(&mut s);
        let before = snapshot(&s);
        s.start_simulation().unwrap();
        assert_eq!(s.start_simulation(), Err(SimulationError::AlreadyActive));
        assert_eq!(
            s.settle_simulation(1000),
            Ok(SettleOutcome::Settled { work: 2 })
        );
        let runtime = s.inspect().unwrap();
        assert_eq!(
            runtime.output_value(output),
            Some(crate::runtime::Level::High)
        );
        assert_eq!(s.toggle_input(input), Ok(InputChange::Changed));
        assert!(matches!(
            s.step_simulation(),
            Ok(StepOutcome::Stepped { .. })
        ));
        assert_eq!(
            s.toggle_input(ComponentId(999)),
            Err(SimulationError::Runtime(RuntimeError::UnknownComponent(
                ComponentId(999)
            )))
        );
        s.settle_simulation(1000).unwrap();
        assert_eq!(
            s.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::Low)
        );
        assert_unchanged(&s, &before);
        s.stop_simulation().unwrap();
        assert!(!s.simulation_active());
        assert_unchanged(&s, &before);
        let id = s
            .place(ComponentKind::And, Position { x: 1.0, y: 1.0 })
            .unwrap();
        assert_eq!(id, ComponentId(before.next_id));
        s.undo().unwrap();
        assert_eq!(s.document, before.document);
        // A fresh start begins from the initial state, not the discarded one.
        s.start_simulation().unwrap();
        assert_eq!(
            s.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::Unavailable)
        );
    }
}
