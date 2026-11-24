//! Validated document mutations and snapshot history.
#![allow(dead_code)] // Wire editing is exposed to the UI in a later stage.
use crate::domain::{
    Component, ComponentId, ComponentKind, Document, LabelError, PinRef, Position,
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
use crate::trace::{MAX_TRACE_CAPACITY, TraceConfigError, TraceLog};
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
    next_segment_id: u64,
    /// Transient simulation state. Never part of the document or its history.
    runtime: Option<Runtime>,
    oscillation_diagnostic: Option<OscillationDiagnostic>,
    terminal_simulation_context: Option<TerminalSimulationContext>,
    trace: Option<TraceLog>,
    simulation_generation: u64,
    path: Option<PathBuf>,
}
/// A wire-routing endpoint chosen by the user. Interior targets name the exact segment to split,
/// so a click at a crossing never implicitly joins both wires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RouteTarget {
    Pin(PinRef),
    ExistingEndpoint(crate::geometric_wiring::Point),
    SegmentInterior {
        segment_id: u64,
        point: crate::geometric_wiring::Point,
    },
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
    DuplicateConnection,
    Geometry(crate::geometric_wiring::GeometryError),
    InvalidRouteTarget,
    InvalidSelection,
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
            Self::DuplicateConnection => "Pins are already connected",
            Self::Geometry(error) => return write!(f, "Geometric wire edit rejected: {error}"),
            Self::InvalidRouteTarget => "Wire route target is stale or invalid",
            Self::InvalidSelection => "Selection contains a stale or invalid element",
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
    TraceConfig(TraceConfigError),
    TraceSequenceExhausted,
    SessionGenerationExhausted,
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
            Self::TraceConfig(error) => error.fmt(f),
            Self::TraceSequenceExhausted => {
                f.write_str("Trace sequence exhausted; operation was not advanced")
            }
            Self::SessionGenerationExhausted => {
                f.write_str("Simulation session generation exhausted")
            }
            Self::Oscillation { diagnostic, .. } => write!(
                f,
                "Circuit appears to oscillate after {} microsteps (limit {})",
                diagnostic.steps, diagnostic.threshold
            ),
        }
    }
}
impl std::error::Error for SimulationError {}
fn normalize_document_segments(
    document: &Document,
    segments: &[crate::geometric_wiring::Segment],
    high_water: u64,
) -> Result<Vec<crate::geometric_wiring::Segment>, EditError> {
    let mut pins = Vec::new();
    for component in &document.components {
        let resolved = crate::pin_geometry::all_pin_points(component).map_err(|_| {
            EditError::Geometry(crate::geometric_wiring::GeometryError::InvalidGeometry(
                "component pin is not exactly representable",
            ))
        })?;
        pins.extend(resolved.into_iter().map(|(_, point)| point));
    }
    crate::geometric_wiring::normalize_with_high_water_preserving(segments, high_water, &pins)
        .map_err(EditError::Geometry)
}

fn validate_component_pin_geometry(component: &Component) -> Result<(), EditError> {
    crate::pin_geometry::all_pin_points(component)
        .map(|_| ())
        .map_err(|_| {
            EditError::Geometry(crate::geometric_wiring::GeometryError::InvalidGeometry(
                "component pin is not exactly representable",
            ))
        })
}

fn valid_geometric_position(position: Position) -> bool {
    position.valid()
        && [position.x, position.y].into_iter().all(|coordinate| {
            coordinate.fract() == 0.0
                && (coordinate as f64) >= i32::MIN as f64
                && (coordinate as f64) <= i32::MAX as f64
        })
}

fn integer_point_on_segment(
    p: crate::geometric_wiring::Point,
    a: crate::geometric_wiring::Point,
    b: crate::geometric_wiring::Point,
) -> bool {
    let dx = i128::from(b.x) - i128::from(a.x);
    let dy = i128::from(b.y) - i128::from(a.y);
    let px = i128::from(p.x) - i128::from(a.x);
    let py = i128::from(p.y) - i128::from(a.y);
    px * dy == py * dx
        && p.x >= a.x.min(b.x)
        && p.x <= a.x.max(b.x)
        && p.y >= a.y.min(b.y)
        && p.y <= a.y.max(b.y)
}

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
        self.trace = None;
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
        self.saved = Document::default();
        self.path = None;
        self.undo.clear();
        self.redo.clear();
        self.next_id = 0;
        self.next_segment_id = 0;
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
        self.install_opened_document(document, None)
    }
    fn install_opened_document(
        &mut self,
        document: Document,
        path: Option<PathBuf>,
    ) -> Result<(), SessionDigError> {
        let next_id = u64::try_from(document.components.len()).map_err(|_| {
            SessionDigError::Domain(formats::DomainCodecError::Invalid(
                "component ID counter overflow".into(),
            ))
        })?;
        let next_segment_id = match document
            .wires
            .geometric()
            .ok()
            .and_then(|segments| segments.iter().map(|s| s.id).max())
        {
            Some(max) => max.checked_add(1).ok_or_else(|| {
                SessionDigError::Domain(formats::DomainCodecError::Invalid(
                    "segment ID counter overflow".into(),
                ))
            })?,
            None => 0,
        };
        self.document = document.clone();
        self.trace = None;
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
        self.saved = document;
        self.path = path;
        self.undo.clear();
        self.redo.clear();
        self.next_id = next_id;
        self.next_segment_id = next_segment_id;
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
        self.install_opened_document(document, path)
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
        self.start_with_capture(options, None)
    }
    /// Starts with bounded trace capture. Capacity and allocation are validated before compile
    /// or session mutation; successful starts replace the previously retained trace.
    pub fn start_simulation_with_trace(
        &mut self,
        options: SimulationOptions,
        capacity: usize,
    ) -> Result<(), SimulationError> {
        if !(1..=MAX_TRACE_CAPACITY).contains(&capacity) {
            return Err(SimulationError::TraceConfig(
                TraceConfigError::CapacityOutOfRange(capacity),
            ));
        }
        let generation = self
            .simulation_generation
            .checked_add(1)
            .ok_or(SimulationError::SessionGenerationExhausted)?;
        let capture = TraceLog::new(capacity, options.startup_seed, generation)
            .map_err(SimulationError::TraceConfig)?;
        self.start_with_capture(options, Some(capture))
    }
    fn start_with_capture(
        &mut self,
        options: SimulationOptions,
        capture: Option<TraceLog>,
    ) -> Result<(), SimulationError> {
        if self.runtime.is_some() {
            return Err(SimulationError::AlreadyActive);
        }
        let next_generation = self
            .simulation_generation
            .checked_add(1)
            .ok_or(SimulationError::SessionGenerationExhausted)?;
        if options.oscillation_limit == 0 || options.oscillation_limit > crate::simulation::MAX_WORK
        {
            return Err(SimulationError::InvalidOscillationLimit(
                options.oscillation_limit,
            ));
        }
        let mut runtime = match capture {
            Some(trace) => Runtime::start_with_trace(&self.document, options.startup_seed, trace),
            None => Runtime::start_with_seed(&self.document, options.startup_seed),
        }
        .map_err(SimulationError::Compile)?;
        runtime
            .set_oscillation_limit(options.oscillation_limit)
            .map_err(|_| SimulationError::InvalidOscillationLimit(options.oscillation_limit))?;
        self.runtime = Some(runtime);
        self.simulation_generation = next_generation;
        self.trace = None;
        self.oscillation_diagnostic = None;
        self.terminal_simulation_context = None;
        Ok(())
    }
    pub fn oscillation_diagnostic(&self) -> Option<&OscillationDiagnostic> {
        self.oscillation_diagnostic.as_ref()
    }
    pub fn trace(&self) -> Option<&TraceLog> {
        self.runtime
            .as_ref()
            .and_then(Runtime::trace)
            .or(self.trace.as_ref())
    }
    pub fn terminal_simulation_context(&self) -> Option<&TerminalSimulationContext> {
        self.terminal_simulation_context.as_ref()
    }
    /// Discards the runtime; the document, history, dirty state and ids were never touched.
    pub fn stop_simulation(&mut self) -> Result<(), SimulationError> {
        let Some(mut runtime) = self.runtime.take() else {
            return Err(SimulationError::NotActive);
        };
        // Stop is a lifecycle safety operation: it always releases the edit lock. If the trace
        // sequence has no room, preserve the trace and expose its missing-stop metadata instead.
        runtime.trace_stop();
        self.trace = runtime.take_trace();
        drop(runtime);
        Ok(())
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
        if outcome == StepOutcome::TraceSequenceExhausted {
            return Err(SimulationError::TraceSequenceExhausted);
        }
        if let StepOutcome::Oscillating { diagnostic } = outcome {
            let mut runtime = self.runtime.take().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.trace = runtime.take_trace();
            drop(runtime);
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

    /// Start/continue one logical microstep, bounded to `budget` scheduler work units.
    /// Call again when `BudgetExhausted`; stop/pause between calls leaves staged state intact.
    pub fn micro_step_simulation_bounded(
        &mut self,
        budget: usize,
    ) -> Result<SettleOutcome, SimulationError> {
        if let Some(diagnostic) = self.oscillation_diagnostic.clone() {
            return Err(SimulationError::Oscillation {
                diagnostic,
                work: 0,
            });
        }
        let outcome = self.runtime_mut()?.micro_step_bounded(budget);
        if let SettleOutcome::Oscillating { work, diagnostic } = outcome {
            let mut runtime = self.runtime.take().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.trace = runtime.take_trace();
            drop(runtime);
            self.oscillation_diagnostic = Some(diagnostic.clone());
            self.terminal_simulation_context = Some(context);
            return Err(SimulationError::Oscillation { diagnostic, work });
        }
        if matches!(outcome, SettleOutcome::TraceSequenceExhausted { .. }) {
            return Err(SimulationError::TraceSequenceExhausted);
        }
        Ok(outcome)
    }

    fn retain_terminal(&mut self, outcome: StepOutcome) -> Result<StepOutcome, SimulationError> {
        if outcome == StepOutcome::TraceSequenceExhausted {
            return Err(SimulationError::TraceSequenceExhausted);
        }
        if let StepOutcome::Oscillating { diagnostic } = outcome {
            let mut runtime = self.runtime.take().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.trace = runtime.take_trace();
            drop(runtime);
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
        if matches!(&outcome, SettleOutcome::TraceSequenceExhausted { .. }) {
            return Err(SimulationError::TraceSequenceExhausted);
        }
        if let SettleOutcome::Oscillating { work, diagnostic } = outcome {
            let mut runtime = self.runtime.take().ok_or(SimulationError::NotActive)?;
            let context = TerminalSimulationContext {
                startup_seed: runtime.startup_seed(),
                phase: runtime.startup_phase(),
            };
            self.trace = runtime.take_trace();
            drop(runtime);
            self.oscillation_diagnostic = Some(diagnostic.clone());
            self.terminal_simulation_context = Some(context);
            return Err(SimulationError::Oscillation { diagnostic, work });
        }
        Ok(outcome)
    }
    pub fn toggle_input(&mut self, id: ComponentId) -> Result<InputChange, SimulationError> {
        self.runtime_mut()?.toggle_input(id).map_err(|error| {
            if error == RuntimeError::TraceSequenceExhausted {
                SimulationError::TraceSequenceExhausted
            } else {
                SimulationError::Runtime(error)
            }
        })
    }
    /// Set a one-bit input without changing the editable document or history.
    pub fn set_input(
        &mut self,
        id: ComponentId,
        value: bool,
    ) -> Result<InputChange, SimulationError> {
        self.runtime_mut()?.set_input(id, value).map_err(|error| {
            if error == RuntimeError::TraceSequenceExhausted {
                SimulationError::TraceSequenceExhausted
            } else {
                SimulationError::Runtime(error)
            }
        })
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
        if !valid_geometric_position(position) {
            return Err(EditError::InvalidPosition);
        }
        let id = ComponentId(self.next_id);
        let next_id = self.next_id.checked_add(1).ok_or(EditError::IdExhausted)?;
        let mut next = self.document.clone();
        let component = Component {
            id,
            kind,
            position,
            label: kind.name().into(),
            constant_value: true,
        };
        validate_component_pin_geometry(&component)?;
        next.components.push(component);
        self.edit(next);
        self.next_id = next_id;
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
        if !valid_geometric_position(position) {
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
            validate_component_pin_geometry(component)?;
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
        self.edit(next);
        Ok(())
    }
    /// Atomically translate a mixed transient selection by an integral world-space delta.
    /// Segment endpoints are independent; unselected endpoint coordinates are preserved.
    pub fn move_selection(
        &mut self,
        selection: &crate::selection::Selection,
        dx: i32,
        dy: i32,
    ) -> Result<bool, EditError> {
        self.ensure_editable()?;
        if selection.is_empty() {
            return Ok(false);
        }
        let mut next = self.document.clone();
        for id in &selection.components {
            let component = next
                .components
                .iter_mut()
                .find(|c| c.id == *id)
                .ok_or(EditError::UnknownComponent)?;
            let x = component.position.x as f64 + f64::from(dx);
            let y = component.position.y as f64 + f64::from(dy);
            let position = Position {
                x: x as f32,
                y: y as f32,
            };
            if !valid_geometric_position(position)
                || f64::from(position.x) != x
                || f64::from(position.y) != y
            {
                return Err(EditError::InvalidPosition);
            }
            component.position = position;
            validate_component_pin_geometry(component)?;
        }
        let segments = next.wires.geometric_mut().map_err(EditError::Geometry)?;
        let mut known = std::collections::BTreeSet::new();
        for segment in segments.iter_mut() {
            if let Some(mask) = selection.wires.get(&segment.id) {
                known.insert(segment.id);
                let shift = |p: crate::geometric_wiring::Point| -> Result<crate::geometric_wiring::Point, EditError> {
                    Ok(crate::geometric_wiring::Point::new(
                        if mask.contains_a() { p.x.checked_add(dx).ok_or(EditError::InvalidPosition)? } else { p.x },
                        if mask.contains_a() { p.y.checked_add(dy).ok_or(EditError::InvalidPosition)? } else { p.y },
                    ))
                };
                if mask.is_empty() {
                    return Err(EditError::InvalidSelection);
                }
                if mask.contains_a() {
                    segment.a = shift(segment.a)?;
                }
                if mask.contains_b() {
                    segment.b = crate::geometric_wiring::Point::new(
                        segment
                            .b
                            .x
                            .checked_add(dx)
                            .ok_or(EditError::InvalidPosition)?,
                        segment
                            .b
                            .y
                            .checked_add(dy)
                            .ok_or(EditError::InvalidPosition)?,
                    );
                }
            }
        }
        if known.len() != selection.wires.len() {
            return Err(EditError::InvalidSelection);
        }
        // Only selected/moved segments can collapse; remove those deterministically, retaining all
        // unrelated imported geometry exactly as stored.
        segments.retain(|s| !selection.wires.contains_key(&s.id) || s.a != s.b);
        if next == self.document {
            return Ok(false);
        }
        self.edit(next);
        Ok(true)
    }
    /// Delete all selected components and selected wire segments in a single history step.
    /// Selecting either wire endpoint deletes that stored segment; component deletion does not
    /// infer or remove attached wires.
    pub fn delete_selection(
        &mut self,
        selection: &crate::selection::Selection,
    ) -> Result<bool, EditError> {
        self.ensure_editable()?;
        if selection.is_empty() {
            return Ok(false);
        }
        let mut next = self.document.clone();
        let before = next.components.len();
        next.components
            .retain(|c| !selection.components.contains(&c.id));
        if before - next.components.len() != selection.components.len() {
            return Err(EditError::InvalidSelection);
        }
        let segments = next.wires.geometric_mut().map_err(EditError::Geometry)?;
        let existing: std::collections::BTreeSet<_> = segments.iter().map(|s| s.id).collect();
        if selection.wires.keys().any(|id| !existing.contains(id))
            || selection.wires.values().any(|m| m.is_empty())
        {
            return Err(EditError::InvalidSelection);
        }
        segments.retain(|s| !selection.wires.contains_key(&s.id));
        if next == self.document {
            return Ok(false);
        }
        self.edit(next);
        Ok(true)
    }
    /// Route between valid pins through the shared geometric editing path.
    pub fn connect(&mut self, endpoints: [PinRef; 2]) -> Result<(), EditError> {
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
        if crate::simulation::geometric_pin_partitions(&self.document).is_ok_and(|nets| {
            nets.iter()
                .any(|net| endpoints.iter().all(|pin| net.contains(pin)))
        }) {
            return Err(EditError::DuplicateConnection);
        }
        if !self.route_geometric(
            RouteTarget::Pin(endpoints[0]),
            RouteTarget::Pin(endpoints[1]),
            crate::geometric_wiring::RouteOrientation::HorizontalFirst,
        )? {
            return Err(EditError::DuplicateConnection);
        }
        Ok(())
    }
    /// Publish geometry and its monotonic ID high-water mark as one transaction.
    fn commit_geometric(
        &mut self,
        segments: Vec<crate::geometric_wiring::Segment>,
    ) -> Result<bool, EditError> {
        let mut ids = std::collections::BTreeSet::new();
        for segment in &segments {
            if !ids.insert(segment.id) {
                return Err(EditError::Geometry(
                    crate::geometric_wiring::GeometryError::DuplicateId(segment.id),
                ));
            }
        }
        let next_segment_id = match segments.iter().map(|segment| segment.id).max() {
            Some(max_id) => {
                self.next_segment_id
                    .max(max_id.checked_add(1).ok_or(EditError::Geometry(
                        crate::geometric_wiring::GeometryError::IdExhausted,
                    ))?)
            }
            None => self.next_segment_id,
        };
        let mut next = self.document.clone();
        next.wires = crate::domain::Wiring::GeometricSegments(segments);
        if next == self.document {
            return Ok(false);
        }
        self.edit(next);
        self.next_segment_id = next_segment_id;
        Ok(true)
    }
    /// Insert a checked pin/endpoint-to-pin/endpoint orthogonal route in one history snapshot.
    /// Existing crossing wires are not joined; an interior target must identify the exact segment.
    pub fn route_geometric(
        &mut self,
        start: RouteTarget,
        end: RouteTarget,
        orientation: crate::geometric_wiring::RouteOrientation,
    ) -> Result<bool, EditError> {
        self.ensure_editable()?;
        let existing = self
            .document
            .wires
            .geometric()
            .map_err(EditError::Geometry)?;
        let resolve = |target: RouteTarget,
                       segments: &[crate::geometric_wiring::Segment],
                       document: &Document|
         -> Result<crate::geometric_wiring::Point, EditError> {
            match target {
                RouteTarget::Pin(pin) => {
                    let component = document
                        .components
                        .iter()
                        .find(|c| c.id == pin.component)
                        .ok_or(EditError::InvalidRouteTarget)?;
                    crate::pin_geometry::pin_point(component, pin.pin)
                        .map_err(|_| EditError::InvalidRouteTarget)
                }
                RouteTarget::ExistingEndpoint(point)
                    if segments.iter().any(|s| s.a == point || s.b == point) =>
                {
                    Ok(point)
                }
                RouteTarget::SegmentInterior { segment_id, point } => {
                    let segment = segments
                        .iter()
                        .find(|s| s.id == segment_id)
                        .ok_or(EditError::InvalidRouteTarget)?;
                    if point == segment.a
                        || point == segment.b
                        || !integer_point_on_segment(point, segment.a, segment.b)
                    {
                        Err(EditError::InvalidRouteTarget)
                    } else {
                        Ok(point)
                    }
                }
                RouteTarget::ExistingEndpoint(_) => Err(EditError::InvalidRouteTarget),
            }
        };
        let a = resolve(start, existing, &self.document)?;
        let b = resolve(end, existing, &self.document)?;
        if a == b {
            return Ok(false);
        }
        let points = crate::geometric_wiring::plan_orthogonal_route(a, b, orientation);
        let splits = [start, end]
            .into_iter()
            .filter_map(|target| match target {
                RouteTarget::SegmentInterior { segment_id, point } => Some((segment_id, point)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if existing
            .len()
            .saturating_add(points.len().saturating_sub(1))
            .saturating_add(splits.len())
            > crate::geometric_wiring::MAX_SEGMENTS
        {
            return Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::SegmentLimit,
            ));
        }
        let mut split_points = std::collections::BTreeMap::<
            u64,
            std::collections::BTreeSet<crate::geometric_wiring::Point>,
        >::new();
        for (segment_id, point) in splits {
            split_points.entry(segment_id).or_default().insert(point);
        }
        let mut segments = Vec::with_capacity(existing.len() + points.len());
        let mut next_id = self.next_segment_id;
        for old in existing {
            let Some(cuts) = split_points.remove(&old.id) else {
                segments.push(*old);
                continue;
            };
            let dx = i128::from(old.b.x) - i128::from(old.a.x);
            let dy = i128::from(old.b.y) - i128::from(old.a.y);
            let mut ordered = vec![old.a];
            ordered.extend(cuts);
            ordered.push(old.b);
            ordered.sort_by_key(|point| {
                (i128::from(point.x) - i128::from(old.a.x)) * dx
                    + (i128::from(point.y) - i128::from(old.a.y)) * dy
            });
            ordered.dedup();
            for (index, pair) in ordered.windows(2).enumerate() {
                let id = if index == 0 {
                    old.id
                } else {
                    let id = next_id;
                    next_id = next_id.checked_add(1).ok_or(EditError::Geometry(
                        crate::geometric_wiring::GeometryError::IdExhausted,
                    ))?;
                    id
                };
                segments.push(crate::geometric_wiring::Segment {
                    id,
                    a: pair[0],
                    b: pair[1],
                });
            }
        }
        for pair in points.windows(2) {
            if pair[0] == pair[1] {
                continue;
            }
            segments.push(crate::geometric_wiring::Segment {
                id: next_id,
                a: pair[0],
                b: pair[1],
            });
            next_id = next_id.checked_add(1).ok_or(EditError::Geometry(
                crate::geometric_wiring::GeometryError::IdExhausted,
            ))?;
        }
        if segments.len() > crate::geometric_wiring::MAX_SEGMENTS {
            return Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::SegmentLimit,
            ));
        }
        self.commit_geometric(segments)
    }
    pub fn add_geometric_segments(
        &mut self,
        segments: &[(
            crate::geometric_wiring::Point,
            crate::geometric_wiring::Point,
        )],
    ) -> Result<bool, EditError> {
        self.ensure_editable()?;
        let existing = self
            .document
            .wires
            .geometric()
            .map_err(EditError::Geometry)?;
        if segments.is_empty() {
            return Ok(false);
        }
        let mut all = existing.to_vec();
        let mut next_id = self.next_segment_id;
        for &(a, b) in segments {
            if a == b {
                return Err(EditError::Geometry(
                    crate::geometric_wiring::GeometryError::ZeroLength,
                ));
            }
            all.push(crate::geometric_wiring::Segment { id: next_id, a, b });
            next_id = next_id.checked_add(1).ok_or(EditError::Geometry(
                crate::geometric_wiring::GeometryError::IdExhausted,
            ))?;
        }
        let normalized = normalize_document_segments(&self.document, &all, self.next_segment_id)?;
        if normalized == existing {
            return Ok(false);
        }
        self.commit_geometric(normalized)
    }
    pub fn delete_geometric_segment(&mut self, id: u64) -> Result<bool, EditError> {
        self.ensure_editable()?;
        let mut segments = self
            .document
            .wires
            .geometric()
            .map_err(EditError::Geometry)?
            .to_vec();
        let count = segments.len();
        segments.retain(|segment| segment.id != id);
        if segments.len() == count {
            return Ok(false);
        }
        self.commit_geometric(segments)
    }
    pub fn move_geometric_segment(&mut self, id: u64, dx: i32, dy: i32) -> Result<bool, EditError> {
        self.ensure_editable()?;
        let mut segments = self
            .document
            .wires
            .geometric()
            .map_err(EditError::Geometry)?
            .to_vec();
        let Some(segment) = segments.iter_mut().find(|segment| segment.id == id) else {
            return Ok(false);
        };
        if dx == 0 && dy == 0 {
            return Ok(false);
        }
        let shift = |point: crate::geometric_wiring::Point| {
            Some(crate::geometric_wiring::Point::new(
                point.x.checked_add(dx)?,
                point.y.checked_add(dy)?,
            ))
        };
        segment.a = shift(segment.a).ok_or(EditError::Geometry(
            crate::geometric_wiring::GeometryError::InvalidGeometry("coordinate overflow"),
        ))?;
        segment.b = shift(segment.b).ok_or(EditError::Geometry(
            crate::geometric_wiring::GeometryError::InvalidGeometry("coordinate overflow"),
        ))?;
        let normalized =
            normalize_document_segments(&self.document, &segments, self.next_segment_id)?;
        self.commit_geometric(normalized)
    }
    pub fn move_geometric_endpoint(
        &mut self,
        id: u64,
        endpoint: usize,
        point: crate::geometric_wiring::Point,
    ) -> Result<bool, EditError> {
        self.ensure_editable()?;
        if endpoint > 1 {
            return Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::InvalidGeometry("endpoint index"),
            ));
        }
        let mut segments = self
            .document
            .wires
            .geometric()
            .map_err(EditError::Geometry)?
            .to_vec();
        let Some(segment) = segments.iter_mut().find(|segment| segment.id == id) else {
            return Ok(false);
        };
        if endpoint == 0 {
            if segment.a == point {
                return Ok(false);
            }
            segment.a = point;
        } else {
            if segment.b == point {
                return Ok(false);
            }
            segment.b = point;
        }
        let normalized =
            normalize_document_segments(&self.document, &segments, self.next_segment_id)?;
        self.commit_geometric(normalized)
    }
    pub fn split_geometric_segment(
        &mut self,
        id: u64,
        point: crate::geometric_wiring::Point,
    ) -> Result<bool, EditError> {
        self.ensure_editable()?;
        let mut segments = self
            .document
            .wires
            .geometric()
            .map_err(EditError::Geometry)?
            .to_vec();
        let Some(index) = segments.iter().position(|segment| segment.id == id) else {
            return Ok(false);
        };
        let original = segments[index];
        if point == original.a || point == original.b {
            return Ok(false);
        }
        if !integer_point_on_segment(point, original.a, original.b) {
            return Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::InvalidGeometry(
                    "split point is not on segment",
                ),
            ));
        }
        if segments.len() >= crate::geometric_wiring::MAX_SEGMENTS {
            return Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::SegmentLimit,
            ));
        }
        let segment_id = self.next_segment_id;
        segments[index].b = point;
        segments.push(crate::geometric_wiring::Segment {
            id: segment_id,
            a: point,
            b: original.b,
        });
        self.commit_geometric(segments)
    }
    pub fn merge_geometric_segments(&mut self) -> Result<bool, EditError> {
        self.ensure_editable()?;
        let normalized = normalize_document_segments(
            &self.document,
            self.document
                .wires
                .geometric()
                .map_err(EditError::Geometry)?,
            self.next_segment_id,
        )?;
        self.commit_geometric(normalized)
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
    use crate::geometric_wiring::Point;
    fn simple_geometric_session() -> Session {
        let mut session = Session::default();
        let input = session
            .place(ComponentKind::Input, Position { x: 18.0, y: 0.0 })
            .unwrap();
        let output = session
            .place(ComponentKind::Output, Position { x: 78.0, y: 0.0 })
            .unwrap();
        session
            .connect([
                PinRef {
                    component: input,
                    pin: crate::domain::PinId {
                        direction: crate::domain::PinDirection::Output,
                        index: 0,
                    },
                },
                PinRef {
                    component: output,
                    pin: crate::domain::PinId {
                        direction: crate::domain::PinDirection::Input,
                        index: 0,
                    },
                },
            ])
            .unwrap();
        session
    }
    #[test]
    fn component_commands_require_exact_integer_artwork_centers_transactionally() {
        let mut session = Session::default();
        assert_eq!(
            session.place(ComponentKind::Input, Position { x: 0.5, y: 0.0 }),
            Err(EditError::InvalidPosition)
        );
        let id = session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let document = session.document().clone();
        let undo = session.undo.len();
        assert_eq!(
            session.move_component(id, Position { x: 2.5, y: 0.0 }),
            Err(EditError::InvalidPosition)
        );
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo.len(), undo);
    }

    #[test]
    fn default_document_routes_fanout_simulates_roundtrips_and_undoes_geometry() {
        let mut session = Session::default();
        assert!(matches!(
            session.document().wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        let input = session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let out_a = session
            .place(ComponentKind::Output, Position { x: 120.0, y: 40.0 })
            .unwrap();
        let out_b = session
            .place(ComponentKind::Output, Position { x: 120.0, y: -40.0 })
            .unwrap();
        let pin = |component, direction| PinRef {
            component,
            pin: crate::domain::PinId {
                direction,
                index: 0,
            },
        };
        session
            .connect([
                pin(input, crate::domain::PinDirection::Output),
                pin(out_a, crate::domain::PinDirection::Input),
            ])
            .unwrap();
        session
            .connect([
                pin(input, crate::domain::PinDirection::Output),
                pin(out_b, crate::domain::PinDirection::Input),
            ])
            .unwrap();
        assert!(session.document().wires.len() >= 2);
        crate::simulation::compile(session.document()).unwrap();
        session.start_running_for_test().unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(out_a),
            Some(crate::runtime::Level::Low)
        );
        assert_eq!(
            session.inspect().unwrap().output_value(out_b),
            Some(crate::runtime::Level::Low)
        );
        session.stop_simulation().unwrap();

        let full = session.document().clone();
        session.undo().unwrap();
        assert_ne!(session.document(), &full);
        session.redo().unwrap();
        assert_eq!(session.document(), &full);
        use crate::file_session::{DiscardIntent, LocalFileIo, SaveAsIntent};
        let dir = std::env::temp_dir().join(format!(
            "digital-rust-geometric-default-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut io = LocalFileIo::new(&dir);
        session
            .save_as(&mut io, Path::new("fanout.dig"), SaveAsIntent::CreateOnly)
            .unwrap();
        let mut reopened = Session::default();
        reopened
            .open_path(&mut io, Path::new("fanout.dig"), DiscardIntent::RefuseDirty)
            .unwrap();
        assert_eq!(reopened.document(), session.document());
        crate::simulation::compile(reopened.document()).unwrap();
    }

    #[test]
    fn geometric_session_keeps_segment_ids_unique_and_high_water_monotonic_across_undo() {
        let mut session = Session::default();
        session
            .add_geometric_segments(&[(Point::new(0, 0), Point::new(10, 0))])
            .unwrap();
        let issued = session.document().wires.geometric().unwrap()[0].id;
        session.undo().unwrap();
        session
            .add_geometric_segments(&[(Point::new(20, 0), Point::new(30, 0))])
            .unwrap();
        let reissued = session.document().wires.geometric().unwrap()[0].id;
        assert!(reissued > issued);
        session
            .add_geometric_segments(&[(Point::new(40, 0), Point::new(50, 0))])
            .unwrap();
        let ids = session
            .document()
            .wires
            .geometric()
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), session.document().wires.len());
        assert!(session.next_segment_id > *ids.iter().max().unwrap());
    }
    #[test]
    fn split_rejects_duplicate_ids_transactionally() {
        let mut session = Session::default();
        session
            .add_geometric_segments(&[(Point::new(0, 0), Point::new(10, 0))])
            .unwrap();
        let id = session.document().wires.geometric().unwrap()[0].id;
        session.next_segment_id = id;
        let document = session.document().clone();
        let undo = session.undo.clone();
        let redo = session.redo.clone();
        assert!(matches!(
            session.split_geometric_segment(id, Point::new(5, 0)),
            Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::DuplicateId(duplicate)
            )) if duplicate == id
        ));
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo, undo);
        assert_eq!(session.redo, redo);
        assert_eq!(session.next_segment_id, id);
    }

    #[test]
    fn geometric_session_noops_preserve_split_geometry_history_and_ids() {
        let mut session = Session::default();
        session
            .start_simulation_with_trace(SimulationOptions::default(), 8)
            .unwrap();
        session.settle_simulation(10).unwrap();
        session.stop_simulation().unwrap();
        session
            .add_geometric_segments(&[(Point::new(0, 0), Point::new(10, 0))])
            .unwrap();
        let id = session.document().wires.geometric().unwrap()[0].id;
        session
            .split_geometric_segment(id, Point::new(5, 0))
            .unwrap();
        session
            .add_geometric_segments(&[(Point::new(20, 0), Point::new(30, 0))])
            .unwrap();
        session.undo().unwrap();
        assert_eq!(session.redo.len(), 1);
        let document = session.document().clone();
        let undo = session.undo.clone();
        let redo = session.redo.clone();
        let next_id = session.next_segment_id;
        let trace_state = |trace: &TraceLog| {
            (
                trace.header,
                trace.records().collect::<Vec<_>>(),
                trace.dropped(),
                trace.has_unrecorded_stop(),
            )
        };
        let trace = session.trace.as_ref().map(trace_state);
        assert!(!session.add_geometric_segments(&[]).unwrap());
        assert!(!session.move_geometric_segment(id, 0, 0).unwrap());
        let endpoint = session.document().wires.geometric().unwrap()[0].a;
        assert!(!session.move_geometric_endpoint(id, 0, endpoint).unwrap());
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo, undo);
        assert_eq!(session.redo, redo);
        assert_eq!(session.next_segment_id, next_id);
        assert_eq!(session.trace.as_ref().map(trace_state), trace);
        assert_eq!(session.document().wires.geometric().unwrap().len(), 2);
    }

    #[test]
    fn session_owns_geometric_batches_history_noops_failures_and_lock() {
        let mut session = Session::default();
        assert!(
            session
                .add_geometric_segments(&[(Point::new(0, 0), Point::new(10, 0))])
                .unwrap()
        );
        assert!(matches!(
            session.document().wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        assert!(!session.add_geometric_segments(&[]).unwrap());
        let before = session.document().clone();
        let undo = session.undo.len();
        assert!(
            session
                .add_geometric_segments(&[(Point::new(4, 4), Point::new(4, 4))])
                .is_err()
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.undo.len(), undo);
        let first_segment = session.document().wires.geometric().unwrap()[0].id;
        assert!(
            session
                .split_geometric_segment(first_segment, Point::new(5, 0))
                .unwrap()
        );
        assert!(
            session
                .add_geometric_segments(&[(Point::new(5, 0), Point::new(5, 5))])
                .unwrap()
        );
        let branch = session.document().clone();
        assert!(session.undo().is_ok());
        assert!(session.redo().is_ok());
        assert_eq!(session.document(), &branch);
        let segment_id = session.document().wires.geometric().unwrap()[0].id;
        assert!(session.move_geometric_segment(segment_id, 1, 0).unwrap());
        assert!(session.undo().is_ok());
        assert!(matches!(
            session.move_geometric_endpoint(segment_id, 2, Point::new(0, 0)),
            Err(EditError::Geometry(_))
        ));
        assert!(session.delete_geometric_segment(segment_id).unwrap());
        assert!(session.undo().is_ok());
        assert!(!session.merge_geometric_segments().unwrap());
        let mut locked_session = Session::default();
        locked_session.start_running_for_test().unwrap();
        locked_session.document.wires =
            crate::domain::Wiring::GeometricSegments(vec![crate::geometric_wiring::Segment {
                id: 0,
                a: Point::new(0, 0),
                b: Point::new(10, 0),
            }]);
        let locked = locked_session.document().clone();
        assert!(matches!(
            locked_session.add_geometric_segments(&[]),
            Err(EditError::SimulationActive)
        ));
        assert!(matches!(
            locked_session.undo(),
            Err(EditError::SimulationActive)
        ));
        assert_eq!(locked_session.document(), &locked);
        locked_session.stop_simulation().unwrap();
    }
    #[test]
    fn geometric_pin_contacts_survive_collinear_normalization() {
        use crate::domain::{PinDirection, PinId};
        let center_for_pin = |kind| {
            let offset =
                crate::symbols::center_from_origin_offset(crate::symbols::SymbolStyle::Rect, kind)
                    .unwrap();
            Position {
                x: 40.0 + offset[0],
                y: offset[1],
            }
        };
        let mut session = Session::default();
        let left = session
            .place(
                ComponentKind::Input,
                Position {
                    x: crate::symbols::center_from_origin_offset(
                        crate::symbols::SymbolStyle::Rect,
                        ComponentKind::Input,
                    )
                    .unwrap()[0],
                    y: 0.0,
                },
            )
            .unwrap();
        let middle = session
            .place(ComponentKind::Output, center_for_pin(ComponentKind::Output))
            .unwrap();
        let right = session
            .place(
                ComponentKind::Input,
                Position {
                    x: 80.0
                        + crate::symbols::center_from_origin_offset(
                            crate::symbols::SymbolStyle::Rect,
                            ComponentKind::Input,
                        )
                        .unwrap()[0],
                    y: 0.0,
                },
            )
            .unwrap();
        let output = |component| PinRef {
            component,
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        };
        let receiver = |component| PinRef {
            component,
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        session.connect([output(left), receiver(middle)]).unwrap();
        session.connect([output(right), receiver(middle)]).unwrap();
        assert!(matches!(
            crate::simulation::compile(session.document()),
            Err(CompileError::GeometricMultipleDrivers { .. })
        ));
        let compiled = crate::simulation::compile(session.document());
        assert!(
            matches!(compiled, Err(CompileError::GeometricMultipleDrivers { .. })),
            "{compiled:?}"
        );
    }

    #[test]
    fn geometric_document_runs_through_the_same_runtime() {
        let mut session = simple_geometric_session();
        let output = ComponentId(1);
        let input = ComponentId(0);
        session.start_running_for_test().unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::Low)
        );
        session.toggle_input(input).unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(output),
            Some(crate::runtime::Level::High)
        );
        session.stop_simulation().unwrap();
    }

    #[test]
    fn component_move_and_delete_preserve_independent_geometric_segments() {
        let mut session = simple_geometric_session();
        let before = session.document().wires.clone();
        let component = session.document().components[0].id;
        session
            .move_component(component, Position { x: 100.0, y: 200.0 })
            .unwrap();
        assert_eq!(session.document().wires, before);
        session.delete(component).unwrap();
        assert_eq!(session.document().wires, before);
    }
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
        let noncanonical = b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement></visualElements><wires><wire><p1 x=\"0\" y=\"0\"/><p2 x=\"0\" y=\"0\"/></wire></wires></circuit>";
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
    fn imported_geometric_segments_seed_ids_for_next_edit() {
        let input = b"<circuit><version>2</version><attributes/><visualElements/><wires><wire><p1 x=\"10\" y=\"0\"/><p2 x=\"0\" y=\"0\"/></wire><wire><p1 x=\"0\" y=\"0\"/><p2 x=\"10\" y=\"0\"/></wire></wires></circuit>";
        let mut session = Session::default();
        session.open_dig_bytes(input).unwrap();
        let segments = session.document().wires.geometric().unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|s| (s.id, s.a.x, s.b.x))
                .collect::<Vec<_>>(),
            vec![(0, 10, 0), (1, 0, 10)]
        );
        assert_eq!(session.next_segment_id, 2);
        session
            .add_geometric_segments(&[(Point::new(30, 0), Point::new(40, 0))])
            .unwrap();
        assert!(
            session
                .document()
                .wires
                .geometric()
                .unwrap()
                .iter()
                .any(|s| s.id >= 2)
        );
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
        let id = session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        session
            .document
            .components
            .iter_mut()
            .find(|c| c.id == id)
            .unwrap()
            .position
            .x = f32::MAX;
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
    fn unrepresentable_origin_move_is_rejected_before_session_mutation() {
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::XOr, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let document = session.document().clone();
        let undo = session.undo.clone();
        let next_id = session.next_id;
        assert!(matches!(
            session.move_component(
                id,
                Position {
                    x: i32::MIN as f32,
                    y: 0.0,
                },
            ),
            Err(EditError::Geometry(_))
        ));
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo, undo);
        assert_eq!(session.next_id, next_id);
        assert!(session.dirty());
        assert!(session.path().is_none());
    }

    #[test]
    fn place_and_move_reject_unrepresentable_pin_origins_transactionally() {
        let mut session = Session::default();
        let input = session
            .place(ComponentKind::Input, Position { x: 18., y: 0. })
            .unwrap();
        let id = session
            .place(ComponentKind::Not, Position { x: 40., y: 0. })
            .unwrap();
        let output = session
            .place(ComponentKind::Output, Position { x: 98., y: 0. })
            .unwrap();
        let pin = |component, direction, index| PinRef {
            component,
            pin: crate::domain::PinId { direction, index },
        };
        session
            .connect([
                pin(input, crate::domain::PinDirection::Output, 0),
                pin(id, crate::domain::PinDirection::Input, 0),
            ])
            .unwrap();
        session
            .connect([
                pin(id, crate::domain::PinDirection::Output, 0),
                pin(output, crate::domain::PinDirection::Input, 0),
            ])
            .unwrap();
        session
            .place(ComponentKind::Const, Position { x: 300., y: 300. })
            .unwrap();
        session.undo().unwrap();
        session
            .start_simulation_with_trace(SimulationOptions::default(), 32)
            .unwrap();
        session.settle_simulation(100).unwrap();
        session.stop_simulation().unwrap();
        session.oscillation_diagnostic = Some(OscillationDiagnostic {
            threshold: 4,
            steps: 5,
            components: Vec::new(),
        });
        let trace_state = |trace: &TraceLog| {
            (
                trace.header,
                trace.records().collect::<Vec<_>>(),
                trace.dropped(),
                trace.has_unrecorded_stop(),
            )
        };
        let trace = session.trace.as_ref().map(trace_state);
        let diagnostic = session.oscillation_diagnostic.clone();
        let document = session.document().clone();
        let undo = session.undo.clone();
        let redo = session.redo.clone();
        let next_id = session.next_id;

        assert!(matches!(
            session.place(
                ComponentKind::Not,
                Position {
                    x: i32::MIN as f32,
                    y: 0.
                }
            ),
            Err(EditError::Geometry(_))
        ));
        assert!(matches!(
            session.move_component(
                id,
                Position {
                    x: i32::MIN as f32,
                    y: 0.
                }
            ),
            Err(EditError::Geometry(_))
        ));
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo, undo);
        assert_eq!(session.redo, redo);
        assert_eq!(session.next_id, next_id);
        assert_eq!(session.oscillation_diagnostic, diagnostic);
        assert_eq!(session.trace.as_ref().map(trace_state), trace);

        // Large exactly representable coordinates remain valid when all derived pins fit i32.
        let large = Position {
            x: 16_777_216.,
            y: 1_000_000_000.,
        };
        session.move_component(id, large).unwrap();
        assert_eq!(
            session
                .document()
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap()
                .position,
            large
        );
    }

    #[test]
    fn save_reopens_component_placement_at_f32_precision_boundary() {
        use crate::file_session::{DiscardIntent, LocalFileIo, SaveAsIntent};
        let dir = std::env::temp_dir().join(format!(
            "digital-rust-precision-save-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut io = LocalFileIo::new(&dir);
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::XOr, Position { x: 0.0, y: 20.0 })
            .unwrap();
        session
            .move_component(
                id,
                Position {
                    x: 67_108_864.0,
                    y: 20.0,
                },
            )
            .unwrap();
        let expected = session.document().clone();
        session
            .save_as(
                &mut io,
                Path::new("precision.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();
        assert!(!session.dirty());
        let mut reopened = Session::default();
        reopened
            .open_path(
                &mut io,
                Path::new("precision.dig"),
                DiscardIntent::RefuseDirty,
            )
            .unwrap();
        assert_eq!(reopened.document().components, expected.components);
        assert!(!reopened.dirty());
        std::fs::remove_dir_all(dir).unwrap();
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
        assert_eq!(
            reopened.document().components,
            session.document().components
        );
        assert!(matches!(
            reopened.document().wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        assert!(reopened.path().is_some());
        assert_eq!(id, crate::domain::ComponentId(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn self_authored_adder_fixture(full: bool) -> Vec<u8> {
        use crate::formats::{DigElement, DigPoint, DigV2, DigWire};
        let mut elements = Vec::new();
        let mut pins_to_connect = Vec::new();
        let mut add = |name: &str, origin: (i32, i32)| -> usize {
            let kind = crate::components::from_digital_name(name).unwrap();
            let offset =
                crate::symbols::center_from_origin_offset(crate::symbols::DEFAULT_STYLE, kind)
                    .unwrap();
            let id = elements.len();
            elements.push(DigElement {
                name: name.into(),
                x: origin.0 + offset[0] as i32,
                y: origin.1 + offset[1] as i32,
                label: None,
                value: None,
            });
            id
        };
        let a = add("In", (-100, 0));
        let b = add("In", (-100, 40));
        let cin = full.then(|| add("In", (-100, 80)));
        let xor1 = add("XOr", (0, 0));
        let and1 = add("And", (0, 100));
        let xor2 = full.then(|| add("XOr", (200, 40)));
        let and2 = full.then(|| add("And", (200, 140)));
        let or = full.then(|| add("Or", (400, 100)));
        let sum = add("Out", if full { (300, 60) } else { (300, 20) });
        let carry = add("Out", if full { (500, 120) } else { (300, 120) });
        let output = |id: usize| PinRef {
            component: ComponentId(id as u64),
            pin: crate::domain::PinId {
                direction: crate::domain::PinDirection::Output,
                index: 0,
            },
        };
        let input = |id: usize, n: u8| PinRef {
            component: ComponentId(id as u64),
            pin: crate::domain::PinId {
                direction: crate::domain::PinDirection::Input,
                index: n,
            },
        };
        let connect =
            |pairs: &mut Vec<(PinRef, PinRef)>, from: PinRef, to: PinRef| pairs.push((from, to));
        connect(&mut pins_to_connect, output(a), input(xor1, 0));
        connect(&mut pins_to_connect, output(a), input(and1, 0));
        connect(&mut pins_to_connect, output(b), input(xor1, 1));
        connect(&mut pins_to_connect, output(b), input(and1, 1));
        if let (Some(cin), Some(xor2), Some(and2), Some(or)) = (cin, xor2, and2, or) {
            connect(&mut pins_to_connect, output(xor1), input(xor2, 0));
            connect(&mut pins_to_connect, output(xor1), input(and2, 0));
            connect(&mut pins_to_connect, output(cin), input(xor2, 1));
            connect(&mut pins_to_connect, output(cin), input(and2, 1));
            connect(&mut pins_to_connect, output(and1), input(or, 0));
            connect(&mut pins_to_connect, output(and2), input(or, 1));
            connect(&mut pins_to_connect, output(xor2), input(sum, 0));
            connect(&mut pins_to_connect, output(or), input(carry, 0));
        } else {
            connect(&mut pins_to_connect, output(xor1), input(sum, 0));
            connect(&mut pins_to_connect, output(and1), input(carry, 0));
        }
        let base = crate::formats::to_domain(&DigV2 {
            elements: elements.clone(),
            wires: Vec::new(),
        })
        .unwrap();
        let wires = pins_to_connect
            .into_iter()
            .map(|(a, b)| {
                let a =
                    crate::pin_geometry::pin_point(&base.components[a.component.0 as usize], a.pin)
                        .unwrap();
                let b =
                    crate::pin_geometry::pin_point(&base.components[b.component.0 as usize], b.pin)
                        .unwrap();
                assert_ne!(a, b);
                DigWire {
                    p1: DigPoint { x: a.x, y: a.y },
                    p2: DigPoint { x: b.x, y: b.y },
                }
            })
            .collect();
        crate::formats::serialize_v2(&DigV2 { elements, wires }).unwrap()
    }

    #[test]
    fn self_authored_half_and_full_adder_dig_truth_tables_survive_record_roundtrip() {
        for full in [false, true] {
            let source = self_authored_adder_fixture(full);
            let semantic =
                crate::formats::to_domain(&crate::formats::parse_v2(&source).unwrap()).unwrap();
            let roundtrip =
                crate::formats::serialize_v2(&crate::formats::from_domain(&semantic).unwrap())
                    .unwrap();
            for bytes in [&source, &roundtrip] {
                let mut session = Session::default();
                session.open_dig_bytes(bytes).unwrap();
                let inputs = if full { 3 } else { 2 };
                let outputs = [
                    ComponentId((if full { 8 } else { 4 }) as u64),
                    ComponentId((if full { 9 } else { 5 }) as u64),
                ];
                session.start_simulation().unwrap();
                session.settle_simulation(1000).unwrap();
                for row in 0..(1 << inputs) {
                    for index in 0..inputs {
                        session
                            .set_input(ComponentId(index as u64), row & (1 << index) != 0)
                            .unwrap();
                        session.settle_simulation(1000).unwrap();
                    }
                    let a = row & 1 != 0;
                    let b = row & 2 != 0;
                    let cin = full && row & 4 != 0;
                    let sum = a ^ b ^ cin;
                    let carry = (a && b) || (a && cin) || (b && cin);
                    assert_eq!(
                        session.inspect().unwrap().output_value(outputs[0]),
                        Some(if sum {
                            crate::runtime::Level::High
                        } else {
                            crate::runtime::Level::Low
                        }),
                        "full={full}, row={row}"
                    );
                    assert_eq!(
                        session.inspect().unwrap().output_value(outputs[1]),
                        Some(if carry {
                            crate::runtime::Level::High
                        } else {
                            crate::runtime::Level::Low
                        }),
                        "full={full}, row={row}"
                    );
                }
            }
        }
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
    fn geometric_inverter_truth_table_uses_production_engine_runtime() {
        let xml=b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement><visualElement><elementName>Not</elementName><elementAttributes/><pos x=\"100\" y=\"0\"/></visualElement><visualElement><elementName>Out</elementName><elementAttributes/><pos x=\"200\" y=\"0\"/></visualElement></visualElements><wires><wire><p1 x=\"0\" y=\"0\"/><p2 x=\"100\" y=\"0\"/></wire><wire><p1 x=\"140\" y=\"0\"/><p2 x=\"200\" y=\"0\"/></wire></wires></circuit>";
        let mut session = Session::default();
        session.open_dig_bytes(xml).unwrap();
        assert!(matches!(
            session.document().wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        session.start_running_for_test().unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(ComponentId(2)),
            Some(crate::runtime::Level::High)
        );
        session.toggle_input(ComponentId(0)).unwrap();
        session.settle_simulation(100).unwrap();
        assert_eq!(
            session.inspect().unwrap().output_value(ComponentId(2)),
            Some(crate::runtime::Level::Low)
        );
        session.stop_simulation().unwrap();
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
        s.connect([x, y]).unwrap();
        let first = s.document.wires.geometric().unwrap()[0].id;
        s.connect([x, z]).unwrap();
        let snapshot = s.document.clone();
        assert_eq!(s.connect([y, x]), Err(EditError::DuplicateConnection));
        assert_eq!(s.connect([x, x]), Err(EditError::SameEndpoint));
        assert_eq!(
            s.connect([x, pin(b, PinDirection::Input, 2)]),
            Err(EditError::InvalidPin)
        );
        assert_eq!(
            s.connect([x, pin(ComponentId(999), PinDirection::Input, 0)]),
            Err(EditError::UnknownComponent)
        );
        assert!(!s.delete_geometric_segment(999).unwrap());
        assert_eq!(s.document, snapshot);
        s.delete(b).unwrap();
        // Digital leaves geometric records stationary when a single component is deleted.
        assert_eq!(s.document.wires, snapshot.wires);
        s.undo().unwrap();
        assert_eq!(s.document, snapshot);
        s.redo().unwrap();
        s.undo().unwrap();
        s.delete_geometric_segment(first).unwrap();
        s.undo().unwrap();
        s.undo().unwrap();
        s.next_segment_id = u64::MAX;
        assert!(matches!(
            s.connect([x, z]),
            Err(EditError::Geometry(
                crate::geometric_wiring::GeometryError::IdExhausted
            ))
        ));
        s.redo().unwrap();
        assert_eq!(s.document, snapshot);
        s.undo().unwrap();
        s.next_segment_id = 2;
        let before = s.document.clone();
        s.connect([y, z]).unwrap();
        assert_ne!(s.document, before);
        s.redo().unwrap();
        assert_eq!(s.document.wires.len(), 3);
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
    fn stop_always_unlocks_when_trace_sequence_is_exhausted() {
        let mut session = Session::default();
        session
            .start_simulation_with_trace(SimulationOptions::default(), 8)
            .unwrap();
        session
            .runtime
            .as_mut()
            .unwrap()
            .force_trace_exhausted_for_test();
        session.stop_simulation().unwrap();
        assert!(!session.simulation_active());
        assert!(session.trace().unwrap().has_unrecorded_stop());
        assert!(
            session
                .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
                .is_ok()
        );
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
        let components = vec![Component {
            id,
            kind: ComponentKind::Not,
            position: Position { x: 0., y: 0. },
            label: "NOT".into(),
            constant_value: true,
        }];
        let document = Document {
            wires: crate::test_support::routed_wires(&components, &[(output, pin)]),
            components,
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
        let components = (0..3)
            .map(|id| Component {
                id: ComponentId(id),
                kind: ComponentKind::Not,
                position: Position {
                    x: id as f32 * 200.0,
                    y: 0.,
                },
                label: format!("N{id}"),
                constant_value: true,
            })
            .collect::<Vec<_>>();
        let pairs = (0..3)
            .map(|id| {
                (
                    pin(id, PinDirection::Output),
                    pin((id + 1) % 3, PinDirection::Input),
                )
            })
            .collect::<Vec<_>>();
        let document = Document {
            wires: crate::test_support::routed_wires(&components, &pairs),
            components,
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
            Err(SimulationError::Compile(
                CompileError::DisconnectedReceiver { .. }
            ))
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
        let wire = s.document.wires.geometric().unwrap()[0].id;
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
        assert_eq!(
            s.delete_geometric_segment(wire),
            Err(EditError::SimulationActive)
        );
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

    #[test]
    fn stop_during_reset_settling_retains_document_history_and_release_trace() {
        use crate::runtime::{SimulationOptions, StartupPhase};
        use crate::trace::TraceEvent;
        let mut session = Session::default();
        let (input, gate, _output) = wired_not(&mut session);
        let document = session.document().clone();
        let undo_len = session.undo.len();
        session
            .start_simulation_with_trace(SimulationOptions::default(), 1024)
            .unwrap();
        session
            .runtime
            .as_mut()
            .unwrap()
            .register_reset_pin_for_test(
                crate::domain::PinRef {
                    component: gate,
                    pin: crate::domain::PinId {
                        direction: crate::domain::PinDirection::Output,
                        index: 0,
                    },
                },
                crate::runtime::ResetOutputPolarity::Normal,
            );
        for _ in 0..100 {
            if session.startup_phase() == Some(StartupPhase::ResetRelease) {
                break;
            }
            session.step_simulation().unwrap();
        }
        assert_eq!(session.startup_phase(), Some(StartupPhase::ResetRelease));
        let gate_output = crate::domain::PinRef {
            component: gate,
            pin: crate::domain::PinId {
                direction: crate::domain::PinDirection::Output,
                index: 0,
            },
        };
        assert_eq!(
            session.inspect().unwrap().pin_value(gate_output),
            Some(crate::runtime::Level::High)
        );
        session.step_simulation().unwrap();
        assert_eq!(session.startup_phase(), Some(StartupPhase::ResetSettling));
        assert_eq!(
            session.inspect().unwrap().pin_value(gate_output),
            Some(crate::runtime::Level::Low)
        );
        assert_eq!(
            session.toggle_input(input),
            Err(SimulationError::Runtime(RuntimeError::Pending))
        );
        assert!(matches!(session.undo(), Err(EditError::SimulationActive)));
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo.len(), undo_len);
        session.stop_simulation().unwrap();
        assert_eq!(session.document(), &document);
        assert_eq!(session.undo.len(), undo_len);
        let events = session
            .trace()
            .unwrap()
            .records()
            .map(|record| record.event)
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, TraceEvent::ResetReleased { .. }))
                .count(),
            1
        );
        assert!(events.contains(&TraceEvent::Stop));
        let before_undo = session.document().clone();
        session.undo().unwrap();
        assert_ne!(session.document(), &before_undo);
    }

    #[test]
    fn geometric_route_splits_only_named_segment_and_is_one_undoable_edit() {
        use crate::geometric_wiring::{Point as P, RouteOrientation, Segment};
        let mut session = Session::default();
        session.document.wires = crate::domain::Wiring::GeometricSegments(vec![
            Segment {
                id: 4,
                a: P::new(0, 0),
                b: P::new(20, 0),
            },
            Segment {
                id: 5,
                a: P::new(10, -10),
                b: P::new(10, 10),
            },
        ]);
        session.next_segment_id = 6;
        assert!(
            session
                .route_geometric(
                    RouteTarget::ExistingEndpoint(P::new(0, 0)),
                    RouteTarget::SegmentInterior {
                        segment_id: 4,
                        point: P::new(10, 0)
                    },
                    RouteOrientation::VerticalFirst,
                )
                .unwrap()
        );
        let segments = session.document.wires.geometric().unwrap();
        assert!(
            segments
                .iter()
                .any(|s| s.id == 5 && s.a == P::new(10, -10) && s.b == P::new(10, 10))
        );
        assert!(
            segments
                .iter()
                .any(|s| s.a == P::new(0, 0) && s.b == P::new(10, 0))
        );
        assert!(
            segments
                .iter()
                .any(|s| s.a == P::new(10, 0) && s.b == P::new(20, 0))
        );
        assert_eq!(session.undo.len(), 1);
        session.undo().unwrap();
        assert_eq!(session.document.wires.geometric().unwrap().len(), 2);
    }

    #[test]
    fn geometric_route_rejects_stale_targets_and_same_point_is_history_noop() {
        use crate::geometric_wiring::{Point as P, RouteOrientation, Segment};
        let mut session = Session::default();
        session.document.wires = crate::domain::Wiring::GeometricSegments(vec![Segment {
            id: 2,
            a: P::new(0, 0),
            b: P::new(10, 0),
        }]);
        session.next_segment_id = 3;
        assert!(
            !session
                .route_geometric(
                    RouteTarget::ExistingEndpoint(P::new(0, 0)),
                    RouteTarget::ExistingEndpoint(P::new(0, 0)),
                    RouteOrientation::HorizontalFirst,
                )
                .unwrap()
        );
        assert!(session.undo.is_empty());
        assert_eq!(
            session.route_geometric(
                RouteTarget::ExistingEndpoint(P::new(0, 0)),
                RouteTarget::ExistingEndpoint(P::new(5, 0)),
                RouteOrientation::HorizontalFirst,
            ),
            Err(EditError::InvalidRouteTarget)
        );
        assert_eq!(session.document.wires.geometric().unwrap().len(), 1);
    }

    #[test]
    fn mixed_group_edit_is_atomic_undoable_and_preserves_unselected_wire_geometry() {
        use crate::geometric_wiring::{Point as P, Segment};
        let mut session = Session::default();
        let a = session
            .place(ComponentKind::Input, Position { x: 20., y: 20. })
            .unwrap();
        let b = session
            .place(ComponentKind::Output, Position { x: 120., y: 20. })
            .unwrap();
        session.document.wires = crate::domain::Wiring::GeometricSegments(vec![
            Segment {
                id: 7,
                a: P::new(0, 0),
                b: P::new(30, 0),
            },
            Segment {
                id: 8,
                a: P::new(50, 50),
                b: P::new(70, 50),
            },
        ]);
        session.next_segment_id = 9;
        let before = session.document().clone();
        let history_before = session.undo.len();
        let mut selected = crate::selection::Selection::default();
        selected.components.insert(a);
        selected.components.insert(b);
        selected.wires.insert(7, crate::selection::EndpointMask::A);
        assert!(session.move_selection(&selected, 20, 0).unwrap());
        assert_eq!(
            session.document.components[0].position,
            Position { x: 40., y: 20. }
        );
        let segments = session.document.wires.geometric().unwrap();
        assert_eq!(
            (segments[0].a, segments[0].b),
            (P::new(20, 0), P::new(30, 0))
        );
        assert_eq!(segments[1], before.wires.geometric().unwrap()[1]);
        let encoded =
            crate::formats::serialize_v2(&crate::formats::from_domain(session.document()).unwrap())
                .unwrap();
        let reopened =
            crate::formats::to_domain(&crate::formats::parse_v2(&encoded).unwrap()).unwrap();
        assert_eq!(reopened.components, session.document.components);
        assert_eq!(
            reopened
                .wires
                .geometric()
                .unwrap()
                .iter()
                .map(|s| (s.a, s.b))
                .collect::<Vec<_>>(),
            segments.iter().map(|s| (s.a, s.b)).collect::<Vec<_>>()
        );
        assert_eq!(session.undo.len(), history_before + 1);
        session.undo().unwrap();
        assert_eq!(session.document(), &before);
        session.redo().unwrap();
        let dirty = session.dirty();
        assert!(!session.move_selection(&selected, 0, 0).unwrap());
        assert_eq!(session.dirty(), dirty);
    }

    #[test]
    fn selection_rejections_preserve_state_and_delete_keeps_stationary_unselected_wires() {
        use crate::geometric_wiring::{Point as P, Segment};
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::Input, Position { x: 20., y: 20. })
            .unwrap();
        session.document.wires = crate::domain::Wiring::GeometricSegments(vec![
            Segment {
                id: 1,
                a: P::new(0, 0),
                b: P::new(20, 0),
            },
            Segment {
                id: 2,
                a: P::new(10, 10),
                b: P::new(20, 10),
            },
        ]);
        let before = session.document().clone();
        let hist = session.undo.len();
        let mut stale = crate::selection::Selection::default();
        stale.wires.insert(99, crate::selection::EndpointMask::A);
        assert_eq!(
            session.move_selection(&stale, 20, 0),
            Err(EditError::InvalidSelection)
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.undo.len(), hist);
        let mut delete = crate::selection::Selection::default();
        delete.components.insert(id);
        delete.wires.insert(1, crate::selection::EndpointMask::A);
        assert!(session.delete_selection(&delete).unwrap());
        assert!(session.document.components.is_empty());
        assert_eq!(
            session.document.wires.geometric().unwrap(),
            &[Segment {
                id: 2,
                a: P::new(10, 10),
                b: P::new(20, 10)
            }]
        );
        assert_eq!(session.undo.len(), hist + 1);
        session.undo().unwrap();
        assert_eq!(session.document(), &before);
    }

    #[test]
    fn selection_edit_obeys_simulation_lock_before_validation() {
        let mut session = Session::default();
        let _id = session
            .place(ComponentKind::Input, Position { x: 20., y: 20. })
            .unwrap();
        session.start_simulation().unwrap();
        let mut selection = crate::selection::Selection::default();
        selection.components.insert(ComponentId(999));
        assert_eq!(
            session.move_selection(&selection, 1, 0),
            Err(EditError::SimulationActive)
        );
        assert_eq!(
            session.delete_selection(&selection),
            Err(EditError::SimulationActive)
        );
    }

    #[test]
    fn overflowing_group_move_preserves_document_redo_and_id_state() {
        let mut session = Session::default();
        let id = session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let mut selected = crate::selection::Selection::default();
        selected.components.insert(id);
        session.move_selection(&selected, 20, 0).unwrap();
        session.undo().unwrap();
        let before = session.document().clone();
        let redo_count = session.redo.len();
        assert_eq!(
            session.move_selection(&selected, i32::MAX, 0),
            Err(EditError::InvalidPosition)
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.redo.len(), redo_count);
        session.redo().unwrap();
        assert_eq!(session.document().components[0].position.x, 20.);
    }
}
