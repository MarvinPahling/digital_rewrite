//! Validated document mutations and snapshot history.
#![allow(dead_code)] // Wire editing is exposed to the UI in a later stage.
use crate::domain::{
    Component, ComponentId, ComponentKind, Document, LabelError, PinRef, Position, Wire, WireId,
};
use crate::file_session::{
    self, DiscardIntent, FileIo, FileOperationError, FileStage, PublishFailure, PublishOutcome,
    SaveAsIntent,
};
use crate::formats::{self, DigReadError, DigWriteError};
use crate::runtime::{
    InputChange, OscillationDiagnostic, Runtime, RuntimeError, SettleOutcome, SimulationOptions,
    StartupPhase, StepOutcome,
};
use crate::simulation::CompileError;
use std::path::{Path, PathBuf};
pub struct TerminalSimulationContext {
    pub startup_seed: u64,
    pub phase: StartupPhase,
}
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
    oscillation_diagnostic: Option<OscillationDiagnostic>,
    terminal_simulation_context: Option<TerminalSimulationContext>,
    path: Option<PathBuf>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// Editing is locked while a simulation is active; stop it first.
    SimulationActive,
    InvalidPosition,
    UnknownComponent,
    WrongComponentKind,
    InvalidLabel(LabelError),
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
            Self::WrongComponentKind => "Operation requires a Const component",
            Self::InvalidLabel(error) => return write!(f, "Invalid component label: {error}"),
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
    InvalidOscillationLimit(usize),
    Oscillation {
        diagnostic: OscillationDiagnostic,
        work: usize,
    },
}
impl std::fmt::Display for SimulationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("Simulation is already running"),
            Self::NotActive => f.write_str("Simulation is not running"),
            Self::Compile(error) => write!(f, "Circuit cannot be simulated: {error:?}"),
            Self::Runtime(error) => write!(f, "Simulation input rejected: {error:?}"),
            Self::InvalidOscillationLimit(limit) => write!(
                f,
                "Oscillation limit must be between 1 and {} (got {limit})",
                crate::simulation::MAX_WORK
            ),
            Self::Oscillation { diagnostic, .. } => write!(
                f,
                "Circuit appears to oscillate after {} microsteps (limit {})",
                diagnostic.steps, diagnostic.threshold
            ),
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
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
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
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
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
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
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
    pub fn startup_phase(&self) -> Option<StartupPhase> {
        self.runtime.as_ref().map(Runtime::startup_phase)
    }
    pub fn startup_seed(&self) -> Option<u64> {
        self.runtime.as_ref().map(Runtime::startup_seed)
    }
    pub fn startup_policy(&self) -> Option<&'static str> {
        (self.runtime.is_some() || self.terminal_simulation_context.is_some())
            .then_some(crate::runtime::STARTUP_RANDOM_POLICY)
    }
    #[cfg(test)]
    fn start_running_for_test(&mut self) -> Result<(), SimulationError> {
        self.start_simulation()?;
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.force_running_for_test();
        }
        Ok(())
    }
    #[cfg(test)]
    fn start_running_with_limit_for_test(&mut self, limit: usize) -> Result<(), SimulationError> {
        self.start_simulation_with_oscillation_limit(limit)?;
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.force_running_for_test();
        }
        Ok(())
    }
    /// Read-only view of the running simulation, if any.
    pub fn inspect(&self) -> Option<&Runtime> {
        self.runtime.as_ref()
    }
    /// Compiles and starts a runtime. On failure nothing changes and no simulation is active.
    pub fn start_simulation(&mut self) -> Result<(), SimulationError> {
        self.start_simulation_with_options(SimulationOptions::default())
    }
    pub fn start_simulation_with_oscillation_limit(
        &mut self,
        limit: usize,
    ) -> Result<(), SimulationError> {
        self.start_simulation_with_options(SimulationOptions {
            startup_seed: 0,
            oscillation_limit: limit,
        })
    }
    pub fn start_simulation_with_options(
        &mut self,
        options: SimulationOptions,
    ) -> Result<(), SimulationError> {
        if self.runtime.is_some() {
            return Err(SimulationError::AlreadyActive);
        }
        if options.oscillation_limit == 0 || options.oscillation_limit > crate::simulation::MAX_WORK
        {
            return Err(SimulationError::InvalidOscillationLimit(
                options.oscillation_limit,
            ));
        }
        let mut runtime = Runtime::start_with_seed(&self.document, options.startup_seed)
            .map_err(SimulationError::Compile)?;
        runtime
            .set_oscillation_limit(options.oscillation_limit)
            .map_err(|_| SimulationError::InvalidOscillationLimit(options.oscillation_limit))?;
        self.runtime = Some(runtime);
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
        Ok(())
    }
    pub fn oscillation_diagnostic(&self) -> Option<&OscillationDiagnostic> {
        self.oscillation_diagnostic.as_ref()
    }
    pub fn terminal_simulation_context(&self) -> Option<&TerminalSimulationContext> {
        self.terminal_simulation_context.as_ref()
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
    /// Legacy one-work-unit stepping. Use `micro_step_simulation` for one complete frontier.
    pub fn step_simulation(&mut self) -> Result<StepOutcome, SimulationError> {
        if let Some(diagnostic) = self.oscillation_diagnostic.clone() {
            return Err(SimulationError::Oscillation {
                diagnostic,
                work: 0,
            });
        }
        let outcome = self.runtime_mut()?.step();
        if let StepOutcome::Oscillating { diagnostic } = outcome {
            let runtime = self.runtime.as_ref().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.runtime = None;
            self.oscillation_diagnostic = Some(diagnostic.clone());
            self.terminal_simulation_context = Some(context);
            return Err(SimulationError::Oscillation {
                diagnostic,
                work: 0,
            });
        }
        Ok(outcome)
    }
    /// Advances one logical propagation frontier without consuming its successor.
    pub fn micro_step_simulation(&mut self) -> Result<StepOutcome, SimulationError> {
        if let Some(diagnostic) = self.oscillation_diagnostic.clone() {
            return Err(SimulationError::Oscillation {
                diagnostic,
                work: 0,
            });
        }
        let outcome = self.runtime_mut()?.micro_step();
        self.retain_terminal(outcome)
    }

    fn retain_terminal(&mut self, outcome: StepOutcome) -> Result<StepOutcome, SimulationError> {
        if let StepOutcome::Oscillating { diagnostic } = outcome {
            let runtime = self.runtime.as_ref().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.runtime = None;
            self.oscillation_diagnostic = Some(diagnostic.clone());
            self.terminal_simulation_context = Some(context);
            Err(SimulationError::Oscillation {
                diagnostic,
                work: 0,
            })
        } else {
            Ok(outcome)
        }
    }

    pub fn settle_simulation(&mut self, budget: usize) -> Result<SettleOutcome, SimulationError> {
        if let Some(diagnostic) = self.oscillation_diagnostic.clone() {
            return Err(SimulationError::Oscillation {
                diagnostic,
                work: 0,
            });
        }
        let outcome = self.runtime_mut()?.settle(budget);
        if let SettleOutcome::Oscillating { work, diagnostic } = outcome {
            let runtime = self.runtime.as_ref().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.runtime = None;
            self.oscillation_diagnostic = Some(diagnostic.clone());
            self.terminal_simulation_context = Some(context);
            return Err(SimulationError::Oscillation { diagnostic, work });
        }
        Ok(outcome)
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
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
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
            constant_value: true,
        });
        self.edit(next);
        Ok(id)
    }
    /// Set a constant's configured one-bit level; runtime values are independent.
    pub fn set_constant_value(&mut self, id: ComponentId, value: bool) -> Result<(), EditError> {
        self.ensure_editable()?;
        let Some(component) = self
            .document
            .components
            .iter()
            .find(|component| component.id == id)
        else {
            return Err(EditError::UnknownComponent);
        };
        if component.kind != ComponentKind::Const {
            return Err(EditError::WrongComponentKind);
        }
        if component.constant_value == value {
            return Ok(());
        }
        let mut next = self.document.clone();
        next.components
            .iter_mut()
            .find(|component| component.id == id)
            .ok_or(EditError::UnknownComponent)?
            .constant_value = value;
        self.edit(next);
        Ok(())
    }
    /// Change only the selected component's label. Lock and identity checks precede validation;
    /// validation completes before any document/history mutation.
    pub fn set_label(&mut self, id: ComponentId, label: &str) -> Result<(), EditError> {
        self.ensure_editable()?;
        let Some(component) = self
            .document
            .components
            .iter()
            .find(|component| component.id == id)
        else {
            return Err(EditError::UnknownComponent);
        };
        crate::domain::validate_label(label).map_err(EditError::InvalidLabel)?;
        if component.label == label {
            return Ok(());
        }
        let mut next = self.document.clone();
        let component = next
            .components
            .iter_mut()
            .find(|component| component.id == id)
            .ok_or(EditError::UnknownComponent)?;
        component.label = label.to_owned();
        self.edit(next);
        Ok(())
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
            self.oscillation_diagnostic = None;
            self.terminal_simulation_context = None;
            self.redo
                .push(std::mem::replace(&mut self.document, previous));
        }
        Ok(())
    }
    pub fn redo(&mut self) -> Result<(), EditError> {
        self.ensure_editable()?;
        if let Some(next) = self.redo.pop() {
            self.oscillation_diagnostic = None;
            self.terminal_simulation_context = None;
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
        session.start_running_for_test().unwrap();
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
        session.start_running_for_test().unwrap();
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
        session.start_running_for_test().unwrap();
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
        session.start_running_for_test().unwrap();
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
        session.start_running_for_test().unwrap();
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
    fn public_file_and_history_operations_remain_locked_at_reset_release_barrier() {
        use crate::runtime::StartupPhase;
        let mut session = Session::default();
        session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let mut io = NoFileAccess;
        session
            .start_simulation_with_options(crate::runtime::SimulationOptions::default())
            .unwrap();
        assert_eq!(
            session.settle_simulation(1),
            Ok(SettleOutcome::BudgetExhausted { work: 1 })
        );
        assert_eq!(session.startup_phase(), Some(StartupPhase::ResetRelease));
        assert!(matches!(session.undo(), Err(EditError::SimulationActive)));
        assert!(matches!(session.redo(), Err(EditError::SimulationActive)));
        assert!(matches!(
            session.save(&mut io),
            Err(SaveError::SimulationActive)
        ));
        assert!(matches!(
            session.save_as(
                &mut io,
                Path::new("blocked.dig"),
                crate::file_session::SaveAsIntent::Replace
            ),
            Err(SaveError::SimulationActive)
        ));
        assert!(matches!(
            session.open_path(
                &mut io,
                Path::new("blocked.dig"),
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
    fn invalid_nonconst_configuration_fails_export_and_save_before_io() {
        let mut session = Session::default();
        session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        session.document.components[0].constant_value = false;
        let error = session.export_dig_bytes().unwrap_err();
        assert!(matches!(error, SessionDigError::Domain(_)));
        assert!(matches!(
            session.start_running_for_test(),
            Err(SimulationError::Compile(
                CompileError::InvalidConstantConfiguration {
                    component: ComponentId(0)
                }
            ))
        ));
        assert!(!session.simulation_active());
        let mut io = NoFileAccess;
        assert!(matches!(
            session.save_as(&mut io, Path::new("unsupported.dig"), SaveAsIntent::Replace),
            Err(SaveError::Codec(SessionDigError::Domain(_)))
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
        session.start_running_for_test().unwrap();
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
            (ComponentKind::NAnd, 2, 1),
            (ComponentKind::Or, 2, 1),
            (ComponentKind::NOr, 2, 1),
            (ComponentKind::XOr, 2, 1),
            (ComponentKind::XNOr, 2, 1),
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
    fn constant_configuration_is_typed_history_and_simulation_locked() {
        let mut session = Session::default();
        let constant = session
            .place(ComponentKind::Const, Position { x: -20.0, y: 0.0 })
            .unwrap();
        let input = session
            .place(ComponentKind::Input, Position { x: 40.0, y: 0.0 })
            .unwrap();
        assert!(session.document().components[0].constant_value);
        session.undo().unwrap();
        let redo_len = session.redo.len();
        assert_eq!(session.set_constant_value(constant, true), Ok(()));
        assert_eq!(
            session.redo.len(),
            redo_len,
            "same-value edit preserves redo"
        );
        session.redo().unwrap();
        let saved = session.document().clone();
        session.set_constant_value(constant, false).unwrap();
        assert!(!session.document().components[0].constant_value);
        assert_eq!(session.undo.len(), 3);
        session.undo().unwrap();
        assert_eq!(session.document(), &saved);
        session.redo().unwrap();
        assert!(!session.document().components[0].constant_value);
        let before = snapshot(&session);
        assert_eq!(
            session.set_constant_value(ComponentId(u64::MAX), false),
            Err(EditError::UnknownComponent)
        );
        assert_eq!(
            session.set_constant_value(input, false),
            Err(EditError::WrongComponentKind)
        );
        assert_unchanged(&session, &before);
        session.start_running_for_test().unwrap();
        let active = snapshot(&session);
        assert_eq!(
            session.set_constant_value(constant, true),
            Err(EditError::SimulationActive)
        );
        assert_unchanged(&session, &active);
        session.stop_simulation().unwrap();
    }

    #[test]
    fn label_edits_validate_atomically_and_have_one_undo_snapshot() {
        let mut session = Session::default();
        let ids: Vec<_> = crate::components::KINDS
            .iter()
            .map(|kind| session.place(*kind, Position { x: 10.0, y: 20.0 }).unwrap())
            .collect();
        let id = ids[1];
        session.saved = session.document.clone();
        let original = session.document.clone();
        let before = snapshot(&session);
        assert_eq!(
            session.set_label(ComponentId(u64::MAX), "bad\0"),
            Err(EditError::UnknownComponent)
        );
        assert_unchanged(&session, &before);
        for label in ["x".repeat(4097), "bad\u{1}".into(), "bad\u{FFFE}".into()] {
            assert!(matches!(
                session.set_label(id, &label),
                Err(EditError::InvalidLabel(_))
            ));
            assert_unchanged(&session, &before);
        }
        let unicode = "é".repeat(2048);
        session.set_label(id, &unicode).unwrap();
        assert_eq!(
            session
                .document
                .components
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .label,
            unicode
        );
        assert!(session.dirty());
        assert_eq!(
            session
                .document
                .components
                .iter()
                .map(|c| (c.id, c.kind, c.position))
                .collect::<Vec<_>>(),
            original
                .components
                .iter()
                .map(|c| (c.id, c.kind, c.position))
                .collect::<Vec<_>>()
        );
        assert_eq!(session.document.wires, original.wires);
        session.undo().unwrap();
        assert_eq!(session.document, original);
        assert!(!session.dirty());
        session.redo().unwrap();
        assert_eq!(
            session
                .document
                .components
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .label,
            unicode
        );
        assert!(session.dirty());
    }

    #[test]
    fn labels_for_every_supported_kind_export_and_import_without_loss() {
        let mut session = Session::default();
        let ids: Vec<_> = crate::components::KINDS
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                session
                    .place(
                        *kind,
                        Position {
                            x: index as f32 * 100.0,
                            y: 60.0,
                        },
                    )
                    .unwrap()
            })
            .collect();
        for (id, label) in
            ids.iter()
                .zip(["", "\u{2003} ", "Résultat & <Q>", "λ input", "line\nfeed"])
        {
            session.set_label(*id, label).unwrap();
        }
        let bytes = session.export_dig_bytes().unwrap();
        let mut imported = Session::default();
        imported.open_dig_bytes(&bytes).unwrap();
        assert_eq!(
            session
                .document()
                .components
                .iter()
                .map(|component| (&component.kind, &component.label))
                .collect::<Vec<_>>(),
            imported
                .document()
                .components
                .iter()
                .map(|component| (&component.kind, &component.label))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn label_noop_preserves_redo_and_edit_after_undo_clears_redo() {
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::Output, Position { x: 0.0, y: 0.0 })
            .unwrap();
        session.set_label(id, "  \u{2003}").unwrap();
        session.undo().unwrap();
        assert_eq!(session.document.components[0].label, "Output");
        assert_eq!(session.redo.len(), 1);
        let before = snapshot(&session);
        session.set_label(id, "Output").unwrap();
        assert_unchanged(&session, &before);
        session.redo().unwrap();
        assert_eq!(session.document.components[0].label, "  \u{2003}");
        session.undo().unwrap();
        session.set_label(id, "Renamed").unwrap();
        assert!(session.redo.is_empty());
        assert_eq!(session.document.components[0].label, "Renamed");
    }

    #[test]
    fn label_mutation_is_locked_before_validation_and_preserves_runtime() {
        let mut session = Session::default();
        let (input, _, output) = wired_not(&mut session);
        session.start_running_for_test().unwrap();
        session.settle_simulation(100).unwrap();
        let value = session.inspect().unwrap().output_value(output);
        let before = snapshot(&session);
        assert_eq!(
            session.set_label(input, "ok"),
            Err(EditError::SimulationActive)
        );
        assert_eq!(
            session.set_label(input, "Input"),
            Err(EditError::SimulationActive)
        );
        assert_eq!(
            session.set_label(ComponentId(u64::MAX), "\0"),
            Err(EditError::SimulationActive)
        );
        assert_eq!(
            session.set_label(input, "\0"),
            Err(EditError::SimulationActive)
        );
        assert_unchanged(&session, &before);
        assert!(session.simulation_active());
        assert_eq!(session.inspect().unwrap().output_value(output), value);
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
    #[test]
    fn seeded_startup_is_a_transactional_edit_and_input_barrier_until_ready() {
        use crate::runtime::{SimulationOptions, StartupPhase};
        let mut session = Session::default();
        let (input, _, output) = wired_not(&mut session);
        let doc = session.document().clone();
        let undo = session.undo.clone();
        session
            .start_simulation_with_options(SimulationOptions {
                startup_seed: u64::MAX,
                oscillation_limit: 100,
            })
            .unwrap();
        assert_eq!(session.startup_seed(), Some(u64::MAX));
        assert_eq!(
            session.startup_policy(),
            Some(crate::runtime::STARTUP_RANDOM_POLICY)
        );
        assert_eq!(session.startup_phase(), Some(StartupPhase::StartupSettling));
        assert_eq!(
            session.set_input(input, true),
            Err(SimulationError::Runtime(RuntimeError::Pending))
        );
        assert_eq!(
            session.place(ComponentKind::Input, Position { x: 2., y: 3. }),
            Err(EditError::SimulationActive)
        );
        assert_eq!(
            session.settle_simulation(0),
            Ok(SettleOutcome::BudgetExhausted { work: 0 })
        );
        assert_eq!(session.startup_phase(), Some(StartupPhase::StartupSettling));
        assert_eq!(session.document(), &doc);
        assert_eq!(session.undo, undo);
        loop {
            match session.settle_simulation(1).unwrap() {
                SettleOutcome::Settled { .. } => break,
                SettleOutcome::BudgetExhausted { .. } => {}
                other => panic!("unexpected startup result {other:?}"),
            }
        }
        assert_eq!(session.startup_phase(), Some(StartupPhase::Running));
        assert_eq!(session.set_input(input, true), Ok(InputChange::Changed));
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::Low)
        );
        session.stop_simulation().unwrap();
        session
            .start_simulation_with_options(SimulationOptions {
                startup_seed: u64::MAX,
                oscillation_limit: 100,
            })
            .unwrap();
        while session.startup_phase() != Some(StartupPhase::Running) {
            let _ = session.settle_simulation(4096).unwrap();
        }
        assert_eq!(
            session.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::High)
        );
        session.stop_simulation().unwrap();
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
    fn oscillation_is_terminal_transactional_and_unlocks_edits() {
        use crate::domain::{PinDirection, PinId};
        let mut s = Session::default();
        let mut ids = Vec::new();
        for n in 0..2 {
            ids.push(
                s.place(ComponentKind::Not, Position { x: n as f32, y: 0. })
                    .unwrap(),
            );
        }
        let pin = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        s.connect([
            pin(ids[0], PinDirection::Output),
            pin(ids[1], PinDirection::Input),
        ])
        .unwrap();
        s.connect([
            pin(ids[1], PinDirection::Output),
            pin(ids[0], PinDirection::Input),
        ])
        .unwrap();
        let before = snapshot(&s);
        s.start_running_with_limit_for_test(1).unwrap();
        for id in &ids {
            s.runtime
                .as_mut()
                .unwrap()
                .seed_output_for_test(*id, crate::runtime::Level::Low);
        }
        let result = s.settle_simulation(10_000);
        assert!(matches!(
            result,
            Err(SimulationError::Oscillation { work: 303, .. })
        ));
        assert!(!s.simulation_active());
        assert!(s.oscillation_diagnostic().is_some());
        assert!(matches!(
            s.settle_simulation(1),
            Err(SimulationError::Oscillation { work: 0, .. })
        ));
        assert_unchanged(&s, &before);
        assert_eq!(s.set_label(ids[0], "NOT"), Ok(()));
        assert!(
            s.oscillation_diagnostic().is_some(),
            "no-op keeps the located failure"
        );
        assert_eq!(
            s.set_label(ComponentId(999), "bad"),
            Err(EditError::UnknownComponent)
        );
        assert!(
            s.oscillation_diagnostic().is_some(),
            "failed edits keep the located failure"
        );
        s.undo().unwrap();
        assert!(
            s.oscillation_diagnostic().is_none(),
            "real history mutation clears stale locations"
        );
        assert_eq!(s.set_label(ids[0], "editable after failure"), Ok(()));
        assert_eq!(s.undo.len(), before.undo.len());
        assert!(s.dirty());
    }

    #[test]
    fn startup_oscillation_is_bounded_on_a_real_self_not_ring() {
        use crate::domain::{PinDirection, PinId};
        let id = ComponentId(0);
        let pin = PinRef {
            component: id,
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        let output = PinRef {
            component: id,
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        };
        let document = Document {
            components: vec![Component {
                id,
                kind: ComponentKind::Not,
                position: Position { x: 0., y: 0. },
                label: "NOT".into(),
                constant_value: true,
            }],
            wires: vec![Wire {
                id: WireId(0),
                endpoints: [output, pin],
            }],
        };
        let mut session = Session {
            document: document.clone(),
            saved: document,
            ..Session::default()
        };
        session.start_simulation().unwrap();
        let error = session.settle_simulation(2_000_000).unwrap_err();
        assert!(
            matches!(error, SimulationError::Oscillation { diagnostic: OscillationDiagnostic { threshold: 1_000, steps, .. }, .. } if steps >= 1_000)
        );
        assert!(!session.simulation_active());
        let context = session.terminal_simulation_context().unwrap();
        assert_eq!(context.startup_seed, 0);
        assert_eq!(context.phase, StartupPhase::StartupSettling);
    }

    #[test]
    fn public_three_not_ring_startup_is_bounded_without_forced_state() {
        use crate::domain::{PinDirection, PinId};
        let pin = |id, direction| PinRef {
            component: ComponentId(id),
            pin: PinId {
                direction,
                index: 0,
            },
        };
        let document = Document {
            components: (0..3)
                .map(|id| Component {
                    id: ComponentId(id),
                    kind: ComponentKind::Not,
                    position: Position {
                        x: id as f32,
                        y: 0.,
                    },
                    label: format!("N{id}"),
                    constant_value: true,
                })
                .collect(),
            wires: (0..3)
                .map(|id| Wire {
                    id: WireId(id),
                    endpoints: [
                        pin(id, PinDirection::Output),
                        pin((id + 1) % 3, PinDirection::Input),
                    ],
                })
                .collect(),
        };
        let mut session = Session {
            document: document.clone(),
            saved: document,
            ..Session::default()
        };
        session
            .start_simulation_with_options(SimulationOptions {
                startup_seed: 9,
                oscillation_limit: 2,
            })
            .unwrap();
        assert!(matches!(
            session.settle_simulation(10_000),
            Err(SimulationError::Oscillation { .. })
        ));
        assert!(!session.simulation_active());
        let context = session.terminal_simulation_context().unwrap();
        assert_eq!(context.startup_seed, 9);
        assert_eq!(context.phase, StartupPhase::StartupSettling);
    }

    #[test]
    fn oscillation_limit_validation_does_not_start_runtime() {
        let mut s = Session::default();
        assert!(matches!(
            s.start_simulation_with_oscillation_limit(0),
            Err(SimulationError::InvalidOscillationLimit(0))
        ));
        assert!(!s.simulation_active());
    }

    #[test]
    fn feedback_start_is_locked_and_stop_restores_editing_and_history() {
        use crate::domain::{PinDirection, PinId};
        let mut s = Session::default();
        let a = s
            .place(ComponentKind::Not, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let b = s
            .place(ComponentKind::Not, Position { x: 1.0, y: 0.0 })
            .unwrap();
        let pin = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        s.connect([pin(a, PinDirection::Output), pin(b, PinDirection::Input)])
            .unwrap();
        s.connect([pin(b, PinDirection::Output), pin(a, PinDirection::Input)])
            .unwrap();
        let before = snapshot(&s);
        s.start_running_for_test().unwrap();
        assert!(s.simulation_active());
        assert_eq!(s.undo(), Err(EditError::SimulationActive));
        assert_eq!(s.redo(), Err(EditError::SimulationActive));
        assert_eq!(
            s.place(ComponentKind::Input, Position { x: 4.0, y: 4.0 }),
            Err(EditError::SimulationActive)
        );
        assert_unchanged(&s, &before);
        s.stop_simulation().unwrap();
        assert!(!s.simulation_active());
        s.undo().unwrap();
        assert_ne!(s.document, before.document);
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
        s.start_running_for_test().unwrap();
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
        s.start_running_for_test().unwrap(); // empty document compiles
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
        s.start_running_for_test().unwrap();
        assert_eq!(
            s.start_running_for_test(),
            Err(SimulationError::AlreadyActive)
        );
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
        s.start_running_for_test().unwrap();
        assert_eq!(
            s.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::Unavailable)
        );
    }
}
