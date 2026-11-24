//! Headless runtime for one-bit circuits compiled by [`crate::simulation`].
//!
//! Runtime values live here, never in the editable [`Document`]. Startup is a separately
//! seeded, randomized gatewise lifecycle; normal microsteps read every scheduled gate's inputs
//! before publishing outputs together. The seed policy is reproducible within this Rust
//! implementation, not Java-trace-compatible. `Unavailable` is retained for explicit private
//! Running-state fixtures and is not used as a guessed startup value. High-Z and buses are absent.
use crate::domain::*;
use crate::simulation::{CompileError, CompiledCircuit, compile};
use crate::trace::{TraceEvent, TraceLog};
use std::collections::{BTreeMap, HashMap};

pub const DEFAULT_OSCILLATION_LIMIT: usize = 1_000;
pub const OSCILLATION_COLLECTION_OFFSET: usize = 100;
/// Stable Rust startup randomization/shuffle contract; intentionally not Java-trace-compatible.
pub const STARTUP_RANDOM_POLICY: &str = "splitmix64-fy-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OscillatingComponent {
    pub id: ComponentId,
    pub kind: ComponentKind,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OscillationDiagnostic {
    pub threshold: usize,
    pub steps: usize,
    pub components: Vec<OscillatingComponent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OscillationDetector {
    limit: usize,
    steps: usize,
    components: BTreeMap<ComponentId, OscillatingComponent>,
}
impl OscillationDetector {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            steps: 0,
            components: BTreeMap::new(),
        }
    }
    fn begin_batch<'a>(&mut self, gates: impl IntoIterator<Item = &'a Gate>) {
        self.steps += 1;
        if self.steps > self.limit + 1 {
            for gate in gates {
                self.components
                    .entry(gate.id)
                    .or_insert_with(|| OscillatingComponent {
                        id: gate.id,
                        kind: gate.kind,
                        label: gate.label.clone(),
                    });
            }
        }
    }
}

/// A one-bit net value. `Unavailable` means no gate has computed it yet; it is not false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Unavailable,
    Low,
    High,
}

impl Level {
    fn from_bool(value: bool) -> Self {
        if value { Self::High } else { Self::Low }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// Inputs may only change while no gate work is pending.
    Pending,
    UnknownComponent(ComponentId),
    NotAnInput(ComponentId),
    InvalidOscillationLimit(usize),
    TraceSequenceExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputChange {
    Unchanged,
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// Nothing was pending; state is untouched.
    Idle,
    /// One microstep published the outputs of `evaluated` gates.
    Stepped {
        evaluated: usize,
    },
    /// A previous settle latched terminal oscillation; no more work is permitted.
    Oscillating {
        diagnostic: OscillationDiagnostic,
    },
    TraceSequenceExhausted,
}

/// Work is counted in gate evaluations plus one unit per microstep started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    Settled {
        work: usize,
    },
    /// The budget ran out; the runtime is intact and a later call resumes where this stopped.
    /// This says nothing about oscillation.
    BudgetExhausted {
        work: usize,
    },
    Oscillating {
        work: usize,
        diagnostic: OscillationDiagnostic,
    },
    TraceSequenceExhausted {
        work: usize,
    },
}

#[derive(Clone, Copy)]
enum Role {
    Input { net: usize },
    Output { net: usize },
    Const,
    Gate,
}

struct Gate {
    id: ComponentId,
    label: String,
    kind: ComponentKind,
    inputs: [usize; 2],
    output: usize,
}

impl Gate {
    fn evaluate(&self, values: &[Level]) -> Level {
        let a = values[self.inputs[0]];
        let b = values[self.inputs[1]];
        match (self.kind, a, b) {
            (ComponentKind::Input | ComponentKind::Output | ComponentKind::Const, _, _) => {
                Level::Unavailable
            }
            (ComponentKind::Not, Level::Unavailable, _) => Level::Unavailable,
            (ComponentKind::Not, a, _) => Level::from_bool(a == Level::Low),
            (_, Level::Unavailable, _) | (_, _, Level::Unavailable) => Level::Unavailable,
            (ComponentKind::And, a, b) => Level::from_bool(a == Level::High && b == Level::High),
            (ComponentKind::NAnd, a, b) => {
                Level::from_bool(!(a == Level::High && b == Level::High))
            }
            (ComponentKind::Or, a, b) => Level::from_bool(a == Level::High || b == Level::High),
            (ComponentKind::NOr, a, b) => Level::from_bool(!(a == Level::High || b == Level::High)),
            (ComponentKind::XOr, a, b) => {
                Level::from_bool((a == Level::High) != (b == Level::High))
            }
            (ComponentKind::XNOr, a, b) => {
                Level::from_bool((a == Level::High) == (b == Level::High))
            }
        }
    }
}

/// A microstep whose gates are being evaluated; nothing is published until all are done.
struct Batch {
    gates: Vec<usize>,
    staged: Vec<Level>,
}

// The polarity descriptor is production infrastructure; no supported public component currently
// constructs participants. Its variants are intentionally instantiated only by test seams for M2.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum ResetOutputPolarity {
    Normal,
    Inverted,
}
impl ResetOutputPolarity {
    fn asserted_level(self) -> Level {
        match self {
            Self::Normal => Level::High,
            Self::Inverted => Level::Low,
        }
    }
    fn released_level(self) -> Level {
        match self {
            Self::Normal => Level::Low,
            Self::Inverted => Level::High,
        }
    }
}

/// Private startup reset driver bound to a compiled one-bit net.
struct ResetParticipant {
    net: usize,
    polarity: ResetOutputPolarity,
    released: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResetParticipantError {
    NetOutOfRange,
    DuplicateNet,
}
impl ResetParticipant {
    fn validated(
        net: usize,
        net_count: usize,
        polarity: ResetOutputPolarity,
    ) -> Result<Self, ResetParticipantError> {
        if net >= net_count {
            return Err(ResetParticipantError::NetOutOfRange);
        }
        Ok(Self {
            net,
            polarity,
            released: false,
        })
    }
    fn asserted_level(&self) -> Level {
        self.polarity.asserted_level()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInspection {
    pub awaiting_evaluation: Vec<ComponentId>,
    pub staged_unpublished: Vec<ComponentId>,
    pub next_scheduled: Vec<ComponentId>,
    pub unique_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupPhase {
    StartupSettling,
    ResetRelease,
    ResetSettling,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimulationOptions {
    pub startup_seed: u64,
    pub oscillation_limit: usize,
}
impl Default for SimulationOptions {
    fn default() -> Self {
        Self {
            startup_seed: 0,
            oscillation_limit: DEFAULT_OSCILLATION_LIMIT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SplitMix64(u64);
impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: usize) -> usize {
        let bound = bound as u64;
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let value = self.next();
            if value >= threshold {
                return (value % bound) as usize;
            }
        }
    }
    fn shuffle(&mut self, values: &mut [usize]) {
        for i in (1..values.len()).rev() {
            let j = self.below(i + 1);
            values.swap(i, j);
        }
    }
}

pub struct Runtime {
    roles: HashMap<ComponentId, Role>,
    pin_nets: HashMap<PinRef, usize>,
    gates: Vec<Gate>,
    consumers: Vec<Vec<usize>>,
    values: Vec<Level>,
    queue: Vec<usize>,
    queued: Vec<bool>,
    startup_frontier: Vec<usize>,
    startup_next: Vec<usize>,
    startup_queued: Vec<bool>,
    startup_cursor: usize,
    startup_batch_pending: bool,
    startup_frontier_shuffled: bool,
    startup_seed: u64,
    startup_phase: StartupPhase,
    rng: SplitMix64,
    batch: Option<Batch>,
    oscillation_limit: usize,
    detector: Option<OscillationDetector>,
    terminal: Option<OscillationDiagnostic>,
    trace: Option<TraceLog>,
    trace_stable: bool,
    microstep_active: bool,
    reset_participants: Vec<ResetParticipant>,
}

impl Runtime {
    /// Compiles `document` and schedules every gate. Inputs start false.
    pub fn start(document: &Document) -> Result<Self, CompileError> {
        Self::start_with_seed(document, 0)
    }
    pub fn start_with_seed(document: &Document, seed: u64) -> Result<Self, CompileError> {
        Ok(Self::new(document, &compile(document)?, seed))
    }

    /// `compiled` must come from compiling `document`.
    fn new(document: &Document, compiled: &CompiledCircuit, seed: u64) -> Self {
        let mut pin_nets = HashMap::new();
        let mut consumers = vec![Vec::new(); compiled.nets.len()];
        for (index, net) in compiled.nets.iter().enumerate() {
            for pin in net.driver.iter().chain(&net.receivers) {
                pin_nets.insert(*pin, index);
            }
        }
        let net_of = |component, direction, index| {
            pin_nets[&PinRef {
                component,
                pin: PinId { direction, index },
            }]
        };
        let mut roles = HashMap::new();
        let mut gates = Vec::new();
        let mut values = vec![Level::Low; compiled.nets.len()];
        for component in &document.components {
            let id = component.id;
            let role = match component.kind {
                ComponentKind::Input => {
                    let net = net_of(id, PinDirection::Output, 0);
                    values[net] = Level::Low;
                    Role::Input { net }
                }
                ComponentKind::Const => {
                    let net = net_of(id, PinDirection::Output, 0);
                    values[net] = Level::from_bool(component.constant_value);
                    Role::Const
                }
                ComponentKind::Output => Role::Output {
                    net: net_of(id, PinDirection::Input, 0),
                },
                kind => {
                    let first = net_of(id, PinDirection::Input, 0);
                    let second = if kind == ComponentKind::Not {
                        first
                    } else {
                        net_of(id, PinDirection::Input, 1)
                    };
                    let gate = gates.len();
                    // `gate` is newly allocated, so the only possible duplicate is the
                    // previous push when both inputs share a net.
                    consumers[first].push(gate);
                    if second != first {
                        consumers[second].push(gate);
                    }
                    gates.push(Gate {
                        id,
                        label: component.label.clone(),
                        kind,
                        inputs: [first, second],
                        output: net_of(id, PinDirection::Output, 0),
                    });
                    Role::Gate
                }
            };
            roles.insert(id, role);
        }
        let mut startup_frontier: Vec<_> = (0..gates.len()).collect();
        startup_frontier.sort_by_key(|&i| gates[i].id);
        let rng = SplitMix64(seed);
        Self {
            roles,
            pin_nets,
            queue: Vec::new(),
            queued: vec![false; gates.len()],
            startup_frontier,
            startup_next: Vec::new(),
            startup_queued: vec![false; gates.len()],
            startup_cursor: 0,
            startup_batch_pending: true,
            startup_frontier_shuffled: false,
            startup_seed: seed,
            startup_phase: StartupPhase::StartupSettling,
            rng,
            gates,
            consumers,
            values,
            batch: None,
            oscillation_limit: DEFAULT_OSCILLATION_LIMIT,
            detector: None,
            terminal: None,
            trace: None,
            trace_stable: false,
            microstep_active: false,
            reset_participants: Vec::new(),
        }
    }

    /// Starts with an already validated, bounded trace ring so capture includes startup.
    pub fn start_with_trace(
        document: &Document,
        seed: u64,
        trace: TraceLog,
    ) -> Result<Self, CompileError> {
        let mut runtime = Self::start_with_seed(document, seed)?;
        runtime.trace = Some(trace);
        runtime.trace_event(TraceEvent::Lifecycle {
            phase: StartupPhase::StartupSettling,
        });
        Ok(runtime)
    }

    fn trace_can_record(&self, count: usize) -> bool {
        self.trace
            .as_ref()
            .is_none_or(|trace| trace.can_record(count))
    }
    fn trace_stability_records(&self) -> usize {
        usize::from(
            !self.trace_stable && self.startup_phase == StartupPhase::Running && !self.is_pending(),
        )
    }
    fn trace_frontier_remaining(&self) -> usize {
        let frontier = match self.startup_phase {
            StartupPhase::StartupSettling => {
                let remaining = self
                    .startup_frontier
                    .len()
                    .saturating_sub(self.startup_cursor);
                // At a completed startup frontier, the next call may install and evaluate the
                // queued successor before returning. Include it in the preflight reservation.
                if remaining == 0 {
                    self.startup_next.len()
                } else {
                    remaining
                }
            }
            StartupPhase::ResetRelease => 0,
            StartupPhase::ResetSettling | StartupPhase::Running => {
                // Reset propagation uses the normal read/evaluate/publish scheduler, not the
                // randomized startup arrays. Reserve the entire active frontier, including
                // staged gates whose outputs publish together at completion.
                self.batch
                    .as_ref()
                    .map_or_else(|| self.queue.len(), |batch| batch.gates.len())
            }
        };
        // A single operation can cross the startup barrier and release every reset. Reserve
        // both release records and worst-case net publications regardless of its entry phase.
        let releases = self
            .reset_participants
            .iter()
            .filter(|p| !p.released)
            .count();
        frontier.saturating_add(if releases == 0 {
            0
        } else {
            releases.saturating_mul(2).saturating_add(2)
        })
    }
    pub fn trace(&self) -> Option<&TraceLog> {
        self.trace.as_ref()
    }
    pub(crate) fn take_trace(&mut self) -> Option<TraceLog> {
        self.trace.take()
    }
    #[cfg(test)]
    pub(crate) fn force_trace_sequence_for_test(&mut self, sequence: u64) {
        if let Some(trace) = self.trace.as_mut() {
            trace.force_sequence(sequence);
        }
    }
    #[cfg(test)]
    pub(crate) fn force_trace_exhausted_for_test(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            trace.force_sequence(u64::MAX);
            trace.record(TraceEvent::Stable);
        }
    }
    pub(crate) fn trace_stop(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            if trace.can_record(1) {
                trace.record(TraceEvent::Stop);
            } else {
                trace.mark_unrecorded_stop();
            }
        }
    }
    fn trace_event(&mut self, event: TraceEvent) {
        if let Some(trace) = self.trace.as_mut() {
            trace.record(event);
        }
    }
    fn trace_stability_if_reached(&mut self) {
        if self.startup_phase == StartupPhase::Running && !self.is_pending() {
            self.mark_stable();
        }
    }
    fn mark_stable(&mut self) {
        if !self.trace_stable {
            self.trace_event(TraceEvent::Stable);
            self.trace_stable = true;
        }
    }

    pub fn startup_phase(&self) -> StartupPhase {
        self.startup_phase
    }
    pub fn startup_seed(&self) -> u64 {
        self.startup_seed
    }
    pub fn is_pending(&self) -> bool {
        self.startup_phase != StartupPhase::Running || self.normal_pending()
    }

    fn normal_pending(&self) -> bool {
        self.batch.is_some() || !self.queue.is_empty()
    }

    fn reset_is_held(&self, net: usize) -> bool {
        self.reset_participants
            .iter()
            .any(|p| p.net == net && !p.released)
    }

    fn finish_reset_if_idle(&mut self) {
        if self.startup_phase == StartupPhase::ResetSettling && !self.normal_pending() {
            self.startup_phase = StartupPhase::Running;
            self.trace_event(TraceEvent::Lifecycle {
                phase: StartupPhase::Running,
            });
            self.detector = None;
        }
    }

    // Private production lifecycle mechanism, deliberately not exposed as a public component API.
    #[allow(dead_code)]
    fn install_reset_participant(
        &mut self,
        net: usize,
        polarity: ResetOutputPolarity,
    ) -> Result<(), ResetParticipantError> {
        if self.reset_participants.iter().any(|p| p.net == net) {
            return Err(ResetParticipantError::DuplicateNet);
        }
        let participant = ResetParticipant::validated(net, self.values.len(), polarity)?;
        // The typed production participant owns asserted-level initialization; this seam only
        // calls that mechanism and never duplicates its reset semantics.
        self.values[net] = participant.asserted_level();
        self.reset_participants.push(participant);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn register_reset_participant_for_test(
        &mut self,
        net: usize,
        polarity: ResetOutputPolarity,
    ) {
        self.install_reset_participant(net, polarity).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn register_reset_pin_for_test(
        &mut self,
        pin: PinRef,
        polarity: ResetOutputPolarity,
    ) {
        let net = self.pin_nets[&pin];
        self.register_reset_participant_for_test(net, polarity);
    }

    pub fn set_oscillation_limit(&mut self, limit: usize) -> Result<(), RuntimeError> {
        if limit == 0 || limit > crate::simulation::MAX_WORK {
            return Err(RuntimeError::InvalidOscillationLimit(limit));
        }
        self.oscillation_limit = limit;
        Ok(())
    }
    pub fn oscillation_limit(&self) -> usize {
        self.oscillation_limit
    }
    pub fn terminal_diagnostic(&self) -> Option<&OscillationDiagnostic> {
        self.terminal.as_ref()
    }
    #[cfg(test)]
    pub(crate) fn force_running_for_test(&mut self) {
        self.startup_phase = StartupPhase::Running;
        self.startup_frontier.clear();
        self.startup_next.clear();
        self.startup_cursor = 0;
        self.startup_batch_pending = false;
        self.startup_frontier_shuffled = true;
        self.startup_queued.fill(false);
        for gate in &self.gates {
            self.values[gate.output] = Level::Unavailable;
        }
        self.queue = (0..self.gates.len()).collect();
        self.queued.fill(true);
    }
    #[cfg(test)]
    pub(crate) fn seed_output_for_test(&mut self, id: ComponentId, value: Level) {
        if let Some(gate) = self.gates.iter().find(|gate| gate.id == id) {
            self.values[gate.output] = value;
        }
    }

    /// Immutable scheduler view. A gate can appear in multiple categories during a partially
    /// evaluated batch; `unique_count` counts distinct IDs. Ordering is stable by component ID.
    pub fn pending_inspection(&self) -> PendingInspection {
        let mut awaiting = Vec::new();
        let mut staged = Vec::new();
        let mut next = Vec::new();
        if matches!(
            self.startup_phase,
            StartupPhase::StartupSettling | StartupPhase::ResetRelease
        ) {
            awaiting.extend(
                self.startup_frontier
                    .iter()
                    .skip(self.startup_cursor)
                    .map(|&i| self.gates[i].id),
            );
            next.extend(self.startup_next.iter().map(|&i| self.gates[i].id));
        } else {
            next.extend(self.queue.iter().map(|&i| self.gates[i].id));
            if let Some(batch) = &self.batch {
                let evaluated = batch.staged.len();
                staged.extend(
                    batch
                        .gates
                        .iter()
                        .take(evaluated)
                        .map(|&i| self.gates[i].id),
                );
                awaiting.extend(
                    batch
                        .gates
                        .iter()
                        .skip(evaluated)
                        .map(|&i| self.gates[i].id),
                );
            }
        }
        for ids in [&mut awaiting, &mut staged, &mut next] {
            ids.sort_unstable();
            ids.dedup();
        }
        let mut all = awaiting
            .iter()
            .chain(&staged)
            .chain(&next)
            .copied()
            .collect::<Vec<_>>();
        all.sort_unstable();
        all.dedup();
        PendingInspection {
            awaiting_evaluation: awaiting,
            staged_unpublished: staged,
            next_scheduled: next,
            unique_count: all.len(),
        }
    }

    /// Legacy total scheduler count, retained for compatibility. It is not a unique count.
    pub fn pending_gate_count(&self) -> usize {
        if matches!(
            self.startup_phase,
            StartupPhase::StartupSettling | StartupPhase::ResetRelease
        ) {
            self.startup_frontier
                .len()
                .saturating_sub(self.startup_cursor)
                + self.startup_next.len()
        } else {
            self.queue.len() + self.batch.as_ref().map_or(0, |batch| batch.gates.len())
        }
    }

    pub fn net_count(&self) -> usize {
        self.values.len()
    }

    pub fn net_value(&self, net: usize) -> Option<Level> {
        self.values.get(net).copied()
    }

    /// Value of the net containing `pin`; `None` if the pin does not exist in the circuit.
    pub fn pin_value(&self, pin: PinRef) -> Option<Level> {
        self.pin_nets.get(&pin).map(|&net| self.values[net])
    }

    /// What an Output component currently sees; `None` if `component` is not an Output.
    pub fn output_value(&self, component: ComponentId) -> Option<Level> {
        match self.roles.get(&component) {
            Some(Role::Output { net }) => Some(self.values[*net]),
            _ => None,
        }
    }

    /// Accepted only while idle. Setting the current value schedules nothing.
    pub fn set_input(
        &mut self,
        component: ComponentId,
        value: bool,
    ) -> Result<InputChange, RuntimeError> {
        let net = match self.roles.get(&component) {
            None => return Err(RuntimeError::UnknownComponent(component)),
            Some(Role::Input { net }) => *net,
            Some(_) => return Err(RuntimeError::NotAnInput(component)),
        };
        if self.startup_phase != StartupPhase::Running || self.is_pending() {
            return Err(RuntimeError::Pending);
        }
        let level = Level::from_bool(value);
        if self.values[net] == level {
            return Ok(InputChange::Unchanged);
        }
        if !self.trace_can_record(1) {
            return Err(RuntimeError::TraceSequenceExhausted);
        }
        self.values[net] = level;
        self.trace_stable = false;
        self.trace_event(TraceEvent::InputChanged { component, value });
        self.schedule_consumers(net);
        Ok(InputChange::Changed)
    }

    pub fn toggle_input(&mut self, component: ComponentId) -> Result<InputChange, RuntimeError> {
        match self.roles.get(&component) {
            Some(Role::Input { net }) => {
                let high = self.values[*net] == Level::High;
                self.set_input(component, !high)
            }
            Some(_) => Err(RuntimeError::NotAnInput(component)),
            None => Err(RuntimeError::UnknownComponent(component)),
        }
    }

    /// Runs one bounded legacy work unit. During startup this is a gatewise startup unit;
    /// during normal operation it completes the entire active read/evaluate/publish frontier.
    /// Prefer [`micro_step`](Self::micro_step) for an explicit one-frontier operation.
    pub fn step(&mut self) -> StepOutcome {
        if let Some(diagnostic) = self.terminal.clone() {
            return StepOutcome::Oscillating { diagnostic };
        }
        let frontier = self.trace_frontier_remaining();
        let required = if self.is_pending() {
            frontier.saturating_mul(2).saturating_add(6)
        } else {
            self.trace_stability_records()
        };
        if !self.trace_can_record(required) {
            return StepOutcome::TraceSequenceExhausted;
        }
        self.microstep_active = false;
        if matches!(
            self.startup_phase,
            StartupPhase::StartupSettling | StartupPhase::ResetRelease
        ) {
            let outcome = match self.advance_startup_unit() {
                Ok(true) => StepOutcome::Stepped { evaluated: 1 },
                Ok(false) if self.startup_phase != StartupPhase::Running => {
                    StepOutcome::Stepped { evaluated: 0 }
                }
                Ok(false) => StepOutcome::Idle,
                Err(diagnostic) => StepOutcome::Oscillating { diagnostic },
            };
            self.trace_stability_if_reached();
            return outcome;
        }
        // Running-state explicit stepping is raw and undetected; reset settling remains
        // within the detector episode opened by reset release.
        if self.startup_phase != StartupPhase::ResetSettling {
            self.detector = None;
        }
        if !self.normal_pending() {
            self.finish_reset_if_idle();
            self.trace_stability_if_reached();
            return StepOutcome::Idle;
        }
        if self.batch.is_none() {
            if self.startup_phase == StartupPhase::ResetSettling {
                if let Err(diagnostic) = self.begin_detected_batch() {
                    return StepOutcome::Oscillating { diagnostic };
                }
            } else {
                self.begin_batch();
            }
        }
        loop {
            if let Some(evaluated) = self.evaluate_next() {
                self.finish_reset_if_idle();
                if self.startup_phase != StartupPhase::ResetSettling {
                    self.detector = None;
                }
                self.trace_stability_if_reached();
                return StepOutcome::Stepped { evaluated };
            }
        }
    }

    /// Completes exactly the currently active frontier (or starts and completes one startup
    /// frontier). It never consumes a successor frontier. Normal outputs publish together; the
    /// startup policy publishes each shuffled gate immediately, matching startup semantics.
    pub fn micro_step(&mut self) -> StepOutcome {
        if let Some(diagnostic) = self.terminal.clone() {
            return StepOutcome::Oscillating { diagnostic };
        }
        let frontier = self.trace_frontier_remaining();
        let required = if self.is_pending() {
            frontier.saturating_mul(2).saturating_add(6)
        } else {
            self.trace_stability_records()
        };
        if !self.trace_can_record(required) {
            return StepOutcome::TraceSequenceExhausted;
        }
        self.microstep_active = false;
        if matches!(
            self.startup_phase,
            StartupPhase::StartupSettling | StartupPhase::ResetRelease
        ) {
            if self.startup_cursor >= self.startup_frontier.len() {
                let _ = self.advance_startup_unit();
            }
            let initial_len = self.startup_frontier.len();
            let mut evaluated = 0;
            while self.startup_cursor < initial_len {
                match self.advance_startup_unit() {
                    Ok(true) => evaluated += 1,
                    Ok(false) => {}
                    Err(diagnostic) => return StepOutcome::Oscillating { diagnostic },
                }
            }
            // Empty startup frontiers advance to their lifecycle barrier, without fabricating
            // a gate evaluation.
            if initial_len == 0 && self.startup_phase == StartupPhase::StartupSettling {
                let _ = self.advance_startup_unit();
            }
            self.trace_stability_if_reached();
            return if evaluated > 0 || self.startup_phase != StartupPhase::Running {
                StepOutcome::Stepped { evaluated }
            } else {
                StepOutcome::Idle
            };
        }
        if self.startup_phase != StartupPhase::ResetSettling {
            self.detector = None;
        }
        if !self.normal_pending() {
            self.finish_reset_if_idle();
            self.trace_stability_if_reached();
            return StepOutcome::Idle;
        }
        if self.batch.is_none() {
            if self.startup_phase == StartupPhase::ResetSettling {
                if let Err(diagnostic) = self.begin_detected_batch() {
                    return StepOutcome::Oscillating { diagnostic };
                }
            } else {
                self.begin_batch();
            }
        }
        let mut evaluated = 0;
        while self.batch.is_some() {
            let remaining = self
                .batch
                .as_ref()
                .map_or(0, |batch| batch.gates.len() - batch.staged.len());
            if remaining == 0 {
                break;
            }
            self.evaluate_next();
            evaluated += 1;
        }
        self.finish_reset_if_idle();
        if self.startup_phase != StartupPhase::ResetSettling {
            self.detector = None;
        }
        self.trace_stability_if_reached();
        StepOutcome::Stepped { evaluated }
    }

    /// Continues one logical microstep within the caller's per-frame work budget.
    /// Repeated calls after `BudgetExhausted` resume the same frontier; a fresh request resets
    /// oscillation detection once. Publication of the final gate is atomic and preflighted.
    pub fn micro_step_bounded(&mut self, budget: usize) -> SettleOutcome {
        if let Some(diagnostic) = self.terminal.clone() {
            return SettleOutcome::Oscillating {
                work: 0,
                diagnostic,
            };
        }
        if budget == 0 {
            return SettleOutcome::BudgetExhausted { work: 0 };
        }
        let remaining_frontier = self.trace_frontier_remaining();
        let required = if self.is_pending() {
            remaining_frontier.saturating_mul(2).saturating_add(6)
        } else {
            self.trace_stability_records()
        };
        if !self.trace_can_record(required) {
            return SettleOutcome::TraceSequenceExhausted { work: 0 };
        }
        if !self.microstep_active {
            if self.startup_phase != StartupPhase::ResetSettling {
                self.detector = None;
            }
            self.microstep_active = true;
        }
        let mut work = 0;
        while matches!(
            self.startup_phase,
            StartupPhase::Running | StartupPhase::ResetSettling
        ) {
            if !self.normal_pending() {
                self.finish_reset_if_idle();
                self.microstep_active = false;
                self.trace_stability_if_reached();
                return SettleOutcome::Settled { work };
            }
            if work == budget {
                return SettleOutcome::BudgetExhausted { work };
            }
            if self.batch.is_none() {
                if self.startup_phase == StartupPhase::ResetSettling {
                    if let Err(diagnostic) = self.begin_detected_batch() {
                        self.microstep_active = false;
                        return SettleOutcome::Oscillating { work, diagnostic };
                    }
                } else {
                    self.begin_batch();
                }
                work += 1;
                if work == budget {
                    return SettleOutcome::BudgetExhausted { work };
                }
            }
            let count = self.batch.as_ref().map_or(0, |batch| batch.gates.len());
            let evaluated = self.batch.as_ref().map_or(0, |batch| batch.staged.len());
            if evaluated == count {
                self.finish_reset_if_idle();
                self.microstep_active = false;
                if self.startup_phase != StartupPhase::ResetSettling {
                    self.detector = None;
                }
                self.trace_stability_if_reached();
                return SettleOutcome::Settled { work };
            }
            self.evaluate_next();
            work += 1;
            if self.batch.is_none() {
                self.finish_reset_if_idle();
                self.microstep_active = false;
                if self.startup_phase != StartupPhase::ResetSettling {
                    self.detector = None;
                }
                self.trace_stability_if_reached();
                return SettleOutcome::Settled { work };
            }
        }
        // Startup policy publishes gatewise. One startup scheduler unit is one frame work unit;
        // unlike the legacy complete-frontier helper, this never loops over the entire frontier.
        while work < budget && self.startup_phase != StartupPhase::Running {
            match self.advance_startup_unit() {
                Ok(true) => work += 1,
                Ok(false) => work += 1,
                Err(diagnostic) => {
                    self.microstep_active = false;
                    return SettleOutcome::Oscillating { work, diagnostic };
                }
            }
            if self.startup_phase == StartupPhase::Running
                || self.startup_cursor >= self.startup_frontier.len()
            {
                self.microstep_active = false;
                self.trace_stability_if_reached();
                return SettleOutcome::Settled { work };
            }
        }
        SettleOutcome::BudgetExhausted { work }
    }

    /// Bounded full-settle episode; chunks resume one unchanged detection episode.
    pub fn settle(&mut self, budget: usize) -> SettleOutcome {
        // Completion can atomically publish every changed output in the current frontier.
        let frontier = self.trace_frontier_remaining();
        let trace_headroom = budget
            .saturating_mul(4)
            .saturating_add(frontier.saturating_mul(2))
            .saturating_add(12);
        if budget > 0 && !self.trace_can_record(trace_headroom) {
            return SettleOutcome::TraceSequenceExhausted { work: 0 };
        }
        if let Some(diagnostic) = self.terminal.clone() {
            return SettleOutcome::Oscillating {
                work: 0,
                diagnostic,
            };
        }
        if budget == 0 {
            return if self.is_pending() {
                SettleOutcome::BudgetExhausted { work: 0 }
            } else {
                SettleOutcome::Settled { work: 0 }
            };
        }
        self.microstep_active = false;
        let mut work = 0;
        while self.startup_phase != StartupPhase::Running {
            if self.startup_phase == StartupPhase::ResetSettling {
                if self.normal_pending() {
                    break;
                }
                if work == budget {
                    return SettleOutcome::BudgetExhausted { work };
                }
                let _ = self.advance_startup_unit();
                work += 1;
                continue;
            }
            if work == budget {
                return SettleOutcome::BudgetExhausted { work };
            }
            if self.startup_phase == StartupPhase::ResetRelease {
                if self.reset_participants.is_empty() {
                    // The transition into this empty barrier was already charged as a startup
                    // scheduler unit; do not invent a second work unit for an absent participant.
                    let _ = self.advance_startup_unit();
                    continue;
                }
                if work == budget {
                    return SettleOutcome::BudgetExhausted { work };
                }
                let _ = self.advance_startup_unit();
                work += 1;
                continue;
            }
            match self.advance_startup_unit() {
                Ok(true) => work += 1,
                Ok(false) => work += 1,
                Err(diagnostic) => return SettleOutcome::Oscillating { work, diagnostic },
            }
        }
        if self.detector.is_none() {
            self.detector = Some(OscillationDetector::new(self.oscillation_limit));
        }
        while self.normal_pending() {
            if work == budget {
                return SettleOutcome::BudgetExhausted { work };
            }
            if self.batch.is_none() {
                let scheduled: Vec<_> = self.queue.iter().map(|&gate| &self.gates[gate]).collect();
                let detector = self
                    .detector
                    .get_or_insert_with(|| OscillationDetector::new(self.oscillation_limit));
                if detector.steps >= detector.limit.saturating_add(OSCILLATION_COLLECTION_OFFSET) {
                    let components = detector.components.values().cloned().collect();
                    let diagnostic = OscillationDiagnostic {
                        threshold: detector.limit,
                        steps: detector.steps,
                        components,
                    };
                    self.terminal = Some(diagnostic.clone());
                    self.trace_event(TraceEvent::Terminal);
                    return SettleOutcome::Oscillating { work, diagnostic };
                }
                detector.begin_batch(scheduled);
                work += 1;
                self.begin_batch();
            } else {
                work += 1;
                self.evaluate_next();
            }
        }
        self.finish_reset_if_idle();
        self.detector = None;
        self.mark_stable();
        SettleOutcome::Settled { work }
    }

    /// Executes startup gatewise. Each frontier is shuffled once; its successor frontier is
    /// kept separate so a change never runs a later gate twice in the same frontier.
    fn advance_startup_unit(&mut self) -> Result<bool, OscillationDiagnostic> {
        if self.startup_phase == StartupPhase::StartupSettling {
            if self.startup_cursor >= self.startup_frontier.len() {
                if self.startup_next.is_empty() {
                    self.startup_phase = StartupPhase::ResetRelease;
                    self.trace_event(TraceEvent::Lifecycle {
                        phase: StartupPhase::ResetRelease,
                    });
                    return Ok(false);
                }
                let mut next = std::mem::take(&mut self.startup_next);
                for &gate in &next {
                    self.startup_queued[gate] = false;
                }
                next.sort_by_key(|&i| self.gates[i].id);
                self.startup_frontier = next;
                self.startup_cursor = 0;
                self.startup_batch_pending = true;
                self.startup_frontier_shuffled = false;
            }
            if self.startup_batch_pending && !self.startup_frontier.is_empty() {
                self.trace_event(TraceEvent::FrontierBegin {
                    startup: true,
                    phase: self.startup_phase,
                });
                if !self.startup_frontier_shuffled {
                    self.rng.shuffle(&mut self.startup_frontier);
                    self.startup_frontier_shuffled = true;
                }
                let detector = self
                    .detector
                    .get_or_insert_with(|| OscillationDetector::new(self.oscillation_limit));
                if detector.steps >= detector.limit.saturating_add(OSCILLATION_COLLECTION_OFFSET) {
                    let diagnostic = OscillationDiagnostic {
                        threshold: detector.limit,
                        steps: detector.steps,
                        components: detector.components.values().cloned().collect(),
                    };
                    self.terminal = Some(diagnostic.clone());
                    self.trace_event(TraceEvent::Terminal);
                    return Err(diagnostic);
                }
                let scheduled: Vec<_> = self
                    .startup_frontier
                    .iter()
                    .map(|&i| &self.gates[i])
                    .collect();
                detector.begin_batch(scheduled);
                self.startup_batch_pending = false;
                return Ok(false);
            }
            if let Some(&index) = self.startup_frontier.get(self.startup_cursor) {
                self.startup_cursor += 1;
                let (gate_id, net, level) = {
                    let gate = &self.gates[index];
                    (gate.id, gate.output, gate.evaluate(&self.values))
                };
                self.trace_event(TraceEvent::GateEvaluated {
                    component: gate_id,
                    immediate: true,
                    value: level,
                });
                if !self.reset_is_held(net) && self.values[net] != level {
                    let old = self.values[net];
                    self.values[net] = level;
                    self.trace_event(TraceEvent::NetPublished {
                        net,
                        driver: PinRef {
                            component: gate_id,
                            pin: PinId {
                                direction: PinDirection::Output,
                                index: 0,
                            },
                        },
                        old,
                        new: level,
                    });
                    for &consumer in &self.consumers[net] {
                        if !self.startup_queued[consumer] {
                            self.startup_queued[consumer] = true;
                            self.startup_next.push(consumer);
                        }
                    }
                }
                if self.startup_cursor == self.startup_frontier.len() {
                    self.trace_event(TraceEvent::FrontierComplete {
                        startup: true,
                        evaluated: self.startup_frontier.len(),
                    });
                }
                return Ok(true);
            }
        }
        match self.startup_phase {
            StartupPhase::ResetRelease => {
                self.startup_frontier.clear();
                self.startup_cursor = 0;
                for index in 0..self.reset_participants.len() {
                    if self.reset_participants[index].released {
                        continue;
                    }
                    let net = self.reset_participants[index].net;
                    self.reset_participants[index].released = true;
                    let old = self.values[net];
                    let released_level = self.reset_participants[index].polarity.released_level();
                    self.values[net] = released_level;
                    self.trace_event(TraceEvent::ResetReleased { net });
                    if old != released_level {
                        if let Some(driver) = self.pin_nets.iter().find_map(|(pin, &candidate)| {
                            (candidate == net && pin.pin.direction == PinDirection::Output)
                                .then_some(*pin)
                        }) {
                            self.trace_event(TraceEvent::NetPublished {
                                net,
                                driver,
                                old,
                                new: released_level,
                            });
                        }
                        self.schedule_consumers(net);
                    }
                }
                if self.reset_participants.is_empty() {
                    self.startup_phase = StartupPhase::Running;
                    self.trace_event(TraceEvent::Lifecycle {
                        phase: StartupPhase::Running,
                    });
                    self.detector = None;
                } else {
                    self.startup_phase = StartupPhase::ResetSettling;
                    self.trace_event(TraceEvent::Lifecycle {
                        phase: StartupPhase::ResetSettling,
                    });
                    self.detector = Some(OscillationDetector::new(self.oscillation_limit));
                }
                Ok(false)
            }
            StartupPhase::ResetSettling => {
                self.finish_reset_if_idle();
                Ok(false)
            }
            StartupPhase::Running => Ok(false),
            StartupPhase::StartupSettling => Ok(false),
        }
    }

    /// Starts one normal batch inside the persistent reset-settling detector episode.
    fn begin_detected_batch(&mut self) -> Result<(), OscillationDiagnostic> {
        let scheduled: Vec<_> = self.queue.iter().map(|&gate| &self.gates[gate]).collect();
        let detector = self
            .detector
            .get_or_insert_with(|| OscillationDetector::new(self.oscillation_limit));
        if detector.steps >= detector.limit.saturating_add(OSCILLATION_COLLECTION_OFFSET) {
            let diagnostic = OscillationDiagnostic {
                threshold: detector.limit,
                steps: detector.steps,
                components: detector.components.values().cloned().collect(),
            };
            self.terminal = Some(diagnostic.clone());
            self.trace_event(TraceEvent::Terminal);
            return Err(diagnostic);
        }
        detector.begin_batch(scheduled);
        self.begin_batch();
        Ok(())
    }

    /// Snapshots the scheduled gates as the next microstep.
    fn begin_batch(&mut self) {
        self.trace_event(TraceEvent::FrontierBegin {
            startup: false,
            phase: self.startup_phase,
        });
        let gates = std::mem::take(&mut self.queue);
        for &gate in &gates {
            self.queued[gate] = false;
        }
        self.batch = Some(Batch {
            staged: Vec::with_capacity(gates.len()),
            gates,
        });
    }

    /// Evaluates one gate of the current microstep against unpublished values. After the
    /// last gate, publishes all outputs together and returns the batch size.
    fn evaluate_next(&mut self) -> Option<usize> {
        let gate = {
            let batch = self.batch.as_ref()?;
            *batch.gates.get(batch.staged.len())?
        };
        let gate_id = self.gates[gate].id;
        let value = self.gates[gate].evaluate(&self.values);
        self.trace_event(TraceEvent::GateEvaluated {
            component: gate_id,
            immediate: false,
            value,
        });
        let batch = self.batch.as_mut()?;
        batch.staged.push(value);
        if batch.staged.len() < batch.gates.len() {
            return None;
        }
        let batch = self.batch.take()?;
        let mut changed = Vec::new();
        for (&gate, &level) in batch.gates.iter().zip(&batch.staged) {
            let net = self.gates[gate].output;
            if self.values[net] != level {
                let old = self.values[net];
                self.values[net] = level;
                self.trace_event(TraceEvent::NetPublished {
                    net,
                    driver: PinRef {
                        component: self.gates[gate].id,
                        pin: PinId {
                            direction: PinDirection::Output,
                            index: 0,
                        },
                    },
                    old,
                    new: level,
                });
                changed.push(net);
            }
        }
        for net in changed {
            self.schedule_consumers(net);
        }
        self.trace_event(TraceEvent::FrontierComplete {
            startup: false,
            evaluated: batch.gates.len(),
        });
        Some(batch.gates.len())
    }

    fn schedule_consumers(&mut self, net: usize) {
        for &gate in &self.consumers[net] {
            if !self.queued[gate] {
                self.queued[gate] = true;
                self.queue.push(gate);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Level::{High, Low, Unavailable};

    fn component(id: u64, kind: ComponentKind) -> Component {
        Component {
            id: ComponentId(id),
            kind,
            position: Position {
                x: id as f32 * 300.0,
                y: (id % 7) as f32 * 180.0,
            },
            label: String::new(),
            constant_value: true,
            wide_shape: false,
            rotation: 0,
            mirror: false,
        }
    }
    fn out(id: u64) -> PinRef {
        PinRef {
            component: ComponentId(id),
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        }
    }
    fn inp(id: u64, index: u8) -> PinRef {
        PinRef {
            component: ComponentId(id),
            pin: PinId {
                direction: PinDirection::Input,
                index,
            },
        }
    }
    fn document(kinds: &[(u64, ComponentKind)], wires: &[(PinRef, PinRef)]) -> Document {
        let components = kinds
            .iter()
            .map(|&(id, kind)| component(id, kind))
            .collect::<Vec<_>>();
        Document {
            wires: crate::test_support::routed_wires(&components, wires),
            components,
        }
    }
    /// Input 1 and 2 feed a two-input gate 3 that drives Output 4.
    fn binary(kind: ComponentKind) -> Document {
        document(
            &[
                (1, ComponentKind::Input),
                (2, ComponentKind::Input),
                (3, kind),
                (4, ComponentKind::Output),
            ],
            &[
                (out(1), inp(3, 0)),
                (out(2), inp(3, 1)),
                (out(3), inp(4, 0)),
            ],
        )
    }
    /// Input 1 feeds `length` NOT gates (ids 2..) ending at an Output with id `length + 2`.
    fn not_chain(length: u64) -> Document {
        let mut kinds = vec![(1, ComponentKind::Input)];
        let mut wires = Vec::new();
        for id in 2..length + 2 {
            kinds.push((id, ComponentKind::Not));
            wires.push((out(id - 1), inp(id, 0)));
        }
        kinds.push((length + 2, ComponentKind::Output));
        wires.push((out(length + 1), inp(length + 2, 0)));
        document(&kinds, &wires)
    }
    fn all_pins(document: &Document) -> Vec<PinRef> {
        let mut pins = Vec::new();
        for component in &document.components {
            for direction in [PinDirection::Input, PinDirection::Output] {
                for index in 0..2 {
                    let pin = PinRef {
                        component: component.id,
                        pin: PinId { direction, index },
                    };
                    if component.kind.has_pin(pin.pin) {
                        pins.push(pin);
                    }
                }
            }
        }
        pins.sort();
        pins
    }
    fn snapshot(runtime: &Runtime, document: &Document) -> Vec<Option<Level>> {
        all_pins(document)
            .into_iter()
            .map(|pin| runtime.pin_value(pin))
            .collect()
    }
    fn settled(runtime: &mut Runtime) {
        assert!(matches!(
            runtime.settle(1_000_000),
            SettleOutcome::Settled { .. }
        ));
        assert!(!runtime.is_pending());
    }
    fn level(value: bool) -> Level {
        if value { High } else { Low }
    }
    // Legacy scheduler regressions are explicitly placed in a private normal-running state;
    // Digital startup behavior is exercised separately through Runtime::start.
    fn running(document: &Document) -> Runtime {
        let mut runtime = Runtime::start(document).unwrap();
        runtime.startup_phase = StartupPhase::Running;
        runtime.startup_frontier.clear();
        runtime.startup_next.clear();
        runtime.startup_cursor = 0;
        runtime.values.fill(Level::Unavailable);
        for component in &document.components {
            if let Some(Role::Input { net }) = runtime.roles.get(&component.id) {
                runtime.values[*net] = Level::Low;
            }
            if let Some(Role::Const) = runtime.roles.get(&component.id) {
                let net = runtime.pin_nets[&out(component.id.0)];
                runtime.values[net] = Level::from_bool(component.constant_value);
            }
        }
        runtime.queue = (0..runtime.gates.len()).collect();
        runtime.queued.fill(true);
        runtime
    }

    #[test]
    fn startup_defaults_low_is_random_seeded_chunk_invariant_and_locks_inputs_until_ready() {
        let doc = document(
            &[(1, ComponentKind::Not), (2, ComponentKind::Not)],
            &[(out(1), inp(2, 0)), (out(2), inp(1, 0))],
        );
        for seed in 0..24 {
            let mut whole = Runtime::start_with_seed(&doc, seed).unwrap();
            assert_eq!(whole.pin_value(out(1)), Some(Low));
            assert_eq!(whole.pin_value(out(2)), Some(Low));
            assert_eq!(whole.startup_phase(), StartupPhase::StartupSettling);
            let mut input_runtime = Runtime::start(&not_chain(1)).unwrap();
            assert_eq!(
                input_runtime.set_input(ComponentId(1), true),
                Err(RuntimeError::Pending)
            );
            assert_eq!(whole.settle(0), SettleOutcome::BudgetExhausted { work: 0 });
            let mut chunked = Runtime::start_with_seed(&doc, seed).unwrap();
            let mut inspected = Runtime::start_with_seed(&doc, seed).unwrap();
            let before = snapshot(&inspected, &doc);
            for pin in all_pins(&doc) {
                let _ = inspected.pin_value(pin);
            }
            assert_eq!(snapshot(&inspected, &doc), before);
            settled(&mut inspected);
            settled(&mut whole);
            loop {
                match chunked.settle(1) {
                    SettleOutcome::Settled { .. } => break,
                    SettleOutcome::BudgetExhausted { .. } => {}
                    other => panic!("unexpected startup outcome: {other:?}"),
                }
            }
            let a = whole.pin_value(out(1)).unwrap();
            let b = whole.pin_value(out(2)).unwrap();
            assert_ne!(
                a, b,
                "cross-coupled NOT latch must reach a complementary state"
            );
            assert_eq!(snapshot(&whole, &doc), snapshot(&chunked, &doc));
            assert_eq!(snapshot(&whole, &doc), snapshot(&inspected, &doc));
            assert_eq!(whole.startup_phase(), StartupPhase::Running);
            let mut repeated = Runtime::start_with_seed(&doc, seed).unwrap();
            settled(&mut repeated);
            assert_eq!(snapshot(&whole, &doc), snapshot(&repeated, &doc));
            let mut reordered = doc.clone();
            reordered.components.reverse();
            reordered.wires.geometric_mut().unwrap().reverse();
            let mut canonical = Runtime::start_with_seed(&reordered, seed).unwrap();
            settled(&mut canonical);
            assert_eq!(snapshot(&whole, &doc), snapshot(&canonical, &reordered));
        }
    }

    #[test]
    fn empty_circuit_has_bounded_ordered_reset_release_barrier() {
        let mut runtime = Runtime::start(&Document::default()).unwrap();
        assert_eq!(runtime.startup_phase(), StartupPhase::StartupSettling);
        assert_eq!(
            runtime.settle(0),
            SettleOutcome::BudgetExhausted { work: 0 }
        );
        assert_eq!(runtime.startup_phase(), StartupPhase::StartupSettling);
        assert_eq!(
            runtime.settle(1),
            SettleOutcome::BudgetExhausted { work: 1 }
        );
        assert_eq!(runtime.startup_phase(), StartupPhase::ResetRelease);
        assert_eq!(runtime.settle(1), SettleOutcome::Settled { work: 0 });
        assert_eq!(runtime.startup_phase(), StartupPhase::Running);
    }

    #[test]
    fn nor_and_nand_latches_start_complementary_and_set_reset_hold_after_readiness() {
        for kind in [ComponentKind::NOr, ComponentKind::NAnd] {
            let doc = if kind == ComponentKind::NOr {
                document(
                    &[
                        (1, ComponentKind::Input),
                        (2, ComponentKind::Input),
                        (3, kind),
                        (4, kind),
                    ],
                    &[
                        (out(2), inp(3, 0)),
                        (out(4), inp(3, 1)),
                        (out(1), inp(4, 0)),
                        (out(3), inp(4, 1)),
                    ],
                )
            } else {
                document(
                    &[
                        (1, ComponentKind::Input),
                        (2, ComponentKind::Input),
                        (3, ComponentKind::Not),
                        (4, ComponentKind::Not),
                        (5, kind),
                        (6, kind),
                    ],
                    &[
                        (out(1), inp(3, 0)),
                        (out(2), inp(4, 0)),
                        (out(4), inp(5, 0)),
                        (out(6), inp(5, 1)),
                        (out(3), inp(6, 0)),
                        (out(5), inp(6, 1)),
                    ],
                )
            };
            for seed in 0..24 {
                let mut runtime = Runtime::start_with_seed(&doc, seed).unwrap();
                settled(&mut runtime);
                let (q, qbar) = if kind == ComponentKind::NOr {
                    (
                        runtime.pin_value(out(3)).unwrap(),
                        runtime.pin_value(out(4)).unwrap(),
                    )
                } else {
                    (
                        runtime.pin_value(out(5)).unwrap(),
                        runtime.pin_value(out(6)).unwrap(),
                    )
                };
                assert_ne!(q, qbar, "{kind:?} seed {seed}");
                // Set, hold, reset, then return to the inactive inputs and hold again.
                let (set_id, reset_id) = if kind == ComponentKind::NOr {
                    (1, 2)
                } else {
                    (2, 1)
                };
                runtime.set_input(ComponentId(set_id), true).unwrap();
                settled(&mut runtime);
                let q_set = if kind == ComponentKind::NOr {
                    out(3)
                } else {
                    out(5)
                };
                assert_eq!(
                    runtime.pin_value(q_set),
                    Some(High),
                    "{kind:?} set seed {seed}"
                );
                settled(&mut runtime);
                runtime.set_input(ComponentId(set_id), false).unwrap();
                settled(&mut runtime);
                runtime.set_input(ComponentId(reset_id), true).unwrap();
                settled(&mut runtime);
                assert_eq!(
                    runtime.pin_value(q_set),
                    Some(Low),
                    "{kind:?} reset seed {seed}"
                );
                runtime.set_input(ComponentId(reset_id), false).unwrap();
                settled(&mut runtime);
                assert_eq!(
                    runtime.pin_value(q_set),
                    Some(Low),
                    "{kind:?} hold seed {seed}"
                );
            }
        }
    }

    #[test]
    fn startup_oscillation_collection_tracks_feedback_and_downstream_but_not_stable_region() {
        let mut doc = document(
            &[
                (1, ComponentKind::Not),
                (2, ComponentKind::Not),
                (3, ComponentKind::Not),
                (4, ComponentKind::Const),
            ],
            &[
                (out(1), inp(1, 0)),
                (out(1), inp(2, 0)),
                (out(4), inp(3, 0)),
            ],
        );
        doc.components
            .iter_mut()
            .find(|component| component.id == ComponentId(4))
            .unwrap()
            .constant_value = false;
        let mut runtime = Runtime::start_with_seed(&doc, 5).unwrap();
        runtime.set_oscillation_limit(1).unwrap();
        let SettleOutcome::Oscillating { diagnostic, .. } = runtime.settle(100_000) else {
            panic!("expected bounded startup oscillation");
        };
        let ids: Vec<_> = diagnostic
            .components
            .iter()
            .map(|component| component.id)
            .collect();
        assert!(ids.contains(&ComponentId(1)));
        assert!(ids.contains(&ComponentId(2)));
        assert!(!ids.contains(&ComponentId(3)));
    }

    #[test]
    fn startup_detector_uses_exact_collection_window_and_excludes_rejected_frontier() {
        let doc = document(
            &[(1, ComponentKind::Not), (2, ComponentKind::Not)],
            &[(out(1), inp(1, 0)), (out(2), inp(2, 0))],
        );
        let runtime = Runtime::start(&doc).unwrap();
        let mut detector = OscillationDetector::new(3);
        for step in 1..=4 {
            detector.begin_batch(std::iter::once(&runtime.gates[0]));
            assert_eq!(detector.steps, step);
            assert!(detector.components.is_empty(), "T+1 is not collected");
        }
        for step in 5..=103 {
            detector.begin_batch(std::iter::once(&runtime.gates[0]));
            assert_eq!(detector.steps, step);
            assert_eq!(detector.components.len(), 1, "collect T+2 through T+100");
        }
        // The scheduler rejects before opening the next batch. In particular, its
        // scheduled gates are not added to the already-collected diagnostic.
        let rejected_frontier = &runtime.gates[1];
        assert!(detector.steps >= detector.limit + OSCILLATION_COLLECTION_OFFSET);
        assert!(!detector.components.contains_key(&rejected_frontier.id));
        assert_eq!(
            detector.components.keys().copied().collect::<Vec<_>>(),
            vec![ComponentId(1)]
        );
    }

    #[test]
    fn startup_converges_on_the_last_permitted_frontier() {
        let doc = document(
            &[(1, ComponentKind::Input), (2, ComponentKind::Not)],
            &[(out(1), inp(2, 0))],
        );
        let mut runtime = Runtime::start(&doc).unwrap();
        runtime.set_oscillation_limit(1).unwrap();
        let output_net = runtime.pin_nets[&out(2)];
        runtime.values[output_net] = High;
        runtime.detector = Some(OscillationDetector {
            limit: 1,
            steps: 100,
            components: BTreeMap::new(),
        });
        assert!(matches!(runtime.settle(10), SettleOutcome::Settled { .. }));
        assert_eq!(runtime.startup_phase(), StartupPhase::Running);
        assert_eq!(runtime.terminal_diagnostic(), None);
        assert!(runtime.detector.is_none());
    }

    #[test]
    fn startup_detector_matches_mixed_single_step_and_chunked_settle() {
        let doc = document(&[(1, ComponentKind::Not)], &[(out(1), inp(1, 0))]);
        let mut whole = Runtime::start_with_seed(&doc, 17).unwrap();
        let mut mixed = Runtime::start_with_seed(&doc, 17).unwrap();
        whole.set_oscillation_limit(2).unwrap();
        mixed.set_oscillation_limit(2).unwrap();
        let mut expected = None;
        for _ in 0..1_000 {
            if let StepOutcome::Oscillating { diagnostic } = whole.step() {
                expected = Some(diagnostic);
                break;
            }
        }
        let expected = expected.expect("startup self-ring is detected");
        let mut actual = None;
        for (step_once, budget) in [(true, 1), (false, 2), (true, 1), (false, 3)] {
            if step_once {
                if let StepOutcome::Oscillating { diagnostic } = mixed.step() {
                    actual = Some(diagnostic);
                    break;
                }
            } else if let SettleOutcome::Oscillating { diagnostic, .. } = mixed.settle(budget) {
                actual = Some(diagnostic);
                break;
            }
        }
        for _ in 0..1_000 {
            if actual.is_some() {
                break;
            }
            if let SettleOutcome::Oscillating { diagnostic, .. } = mixed.settle(1) {
                actual = Some(diagnostic);
            }
        }
        assert_eq!(actual, Some(expected));
    }

    #[test]
    fn startup_prng_and_shuffle_policy_has_versioned_golden_vectors() {
        assert_eq!(STARTUP_RANDOM_POLICY, "splitmix64-fy-v1");
        let mut rng = SplitMix64(0);
        assert_eq!(
            [rng.next(), rng.next(), rng.next()],
            [
                16294208416658607535,
                7960286522194355700,
                487617019471545679
            ]
        );
        let mut rng = SplitMix64(42);
        let mut order = (0..8).collect::<Vec<_>>();
        rng.shuffle(&mut order);
        assert_eq!(order, vec![3, 1, 6, 2, 4, 0, 7, 5]);
    }

    #[test]
    fn startup_step_observes_detector_while_running_step_remains_raw() {
        let self_loop = document(&[(1, ComponentKind::Not)], &[(out(1), inp(1, 0))]);
        let mut runtime = Runtime::start_with_seed(&self_loop, 3).unwrap();
        runtime.set_oscillation_limit(1).unwrap();
        let mut detected = None;
        for _ in 0..1_000 {
            if let StepOutcome::Oscillating { diagnostic } = runtime.step() {
                detected = Some(diagnostic);
                break;
            }
        }
        assert!(
            detected.is_some(),
            "startup stepping must not bypass bounded detection"
        );
        let mut normal = running(&self_loop);
        normal.values[normal.pin_nets[&out(1)]] = Low;
        normal.set_oscillation_limit(1).unwrap();
        for _ in 0..1_000 {
            assert!(matches!(normal.step(), StepOutcome::Stepped { .. }));
        }
        assert_eq!(
            normal.terminal_diagnostic(),
            None,
            "Running raw steps remain undetected"
        );
    }

    #[test]
    fn startup_microstep_is_immediate_gatewise_while_normal_remains_staged() {
        let doc = document(
            &[(1, ComponentKind::Not), (2, ComponentKind::Not)],
            &[(out(1), inp(2, 0)), (out(2), inp(1, 0))],
        );
        let mut startup = Runtime::start_with_seed(&doc, 4).unwrap();
        assert_eq!(startup.step(), StepOutcome::Stepped { evaluated: 0 }); // frontier begin
        assert_eq!(startup.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_ne!(startup.pin_value(out(1)), startup.pin_value(out(2)));
        let mut normal = running(&doc);
        normal.values[normal.pin_nets[&out(1)]] = Low;
        normal.values[normal.pin_nets[&out(2)]] = Low;
        assert_eq!(normal.step(), StepOutcome::Stepped { evaluated: 2 });
        assert_eq!(normal.pin_value(out(1)), Some(High));
        assert_eq!(normal.pin_value(out(2)), Some(High));
    }

    #[test]
    fn controlled_feedback_microsteps_are_staged_and_document_order_independent() {
        let make = |reverse: bool| {
            let mut doc = document(
                &[(1, ComponentKind::Not), (2, ComponentKind::Not)],
                &[(out(1), inp(2, 0)), (out(2), inp(1, 0))],
            );
            if reverse {
                doc.components.reverse();
                doc.wires.geometric_mut().unwrap().reverse();
            }
            doc
        };
        let mut traces = Vec::new();
        for doc in [make(false), make(true)] {
            let mut runtime = running(&doc);
            // Controlled known state for scheduler semantics only; no startup behavior asserted.
            runtime.values[runtime.pin_nets[&out(1)]] = Low;
            runtime.values[runtime.pin_nets[&out(2)]] = Low;
            assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 2 });
            assert_eq!(runtime.pin_value(out(1)), Some(High));
            assert_eq!(runtime.pin_value(out(2)), Some(High));
            let mut trace = vec![runtime.pin_value(out(1)), runtime.pin_value(out(2))];
            assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 2 });
            trace.extend([runtime.pin_value(out(1)), runtime.pin_value(out(2))]);
            traces.push(trace);
        }
        assert_eq!(traces[0], traces[1]);
        assert_eq!(traces[0], [Some(High), Some(High), Some(Low), Some(Low)]);
    }

    #[test]
    fn shared_input_net_deduplicates_a_gates_two_consumers() {
        let doc = document(
            &[(1, ComponentKind::Input), (2, ComponentKind::And)],
            &[(out(1), inp(2, 0)), (out(1), inp(2, 1))],
        );
        let mut runtime = running(&doc);
        while runtime.step() != StepOutcome::Idle {}
        runtime.toggle_input(ComponentId(1)).unwrap();
        assert_eq!(runtime.pending_gate_count(), 1);
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
    }

    #[test]
    fn self_loop_reschedules_and_constant_feedback_fixed_point_idles() {
        let self_loop = document(&[(1, ComponentKind::Not)], &[(out(1), inp(1, 0))]);
        let mut runtime = running(&self_loop);
        runtime.values[runtime.pin_nets[&out(1)]] = Low;
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(runtime.pin_value(out(1)), Some(High));
        assert_eq!(runtime.pending_gate_count(), 1);
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(runtime.pin_value(out(1)), Some(Low));
        assert_eq!(runtime.pending_gate_count(), 1);

        let mut fixed = document(
            &[(1, ComponentKind::Or), (2, ComponentKind::Const)],
            &[(out(1), inp(1, 0)), (out(2), inp(1, 1))],
        );
        fixed.components[1].constant_value = false;
        let mut runtime = running(&fixed);
        runtime.values[runtime.pin_nets[&out(1)]] = High;
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(runtime.pin_value(out(1)), Some(High));
        assert_eq!(runtime.step(), StepOutcome::Idle);
        assert!(!runtime.is_pending());
    }

    #[test]
    fn default_limit_is_one_thousand_and_convergence_at_last_allowed_step_succeeds() {
        let ordinary = running(&not_chain(1));
        assert_eq!(ordinary.oscillation_limit(), 1_000);
        let long = not_chain(101);
        let mut runtime = running(&long);
        runtime.set_oscillation_limit(1).unwrap();
        assert!(matches!(
            runtime.settle(10_000),
            SettleOutcome::Settled { .. }
        ));
        assert!(!runtime.is_pending());
        assert_eq!(runtime.output_value(ComponentId(103)), Some(High));
    }

    #[test]
    fn oscillation_boundary_collects_only_late_scheduled_gate_union() {
        let doc = document(
            &[
                (1, ComponentKind::Not),
                (2, ComponentKind::Not),
                (3, ComponentKind::Not),
            ],
            &[
                (out(1), inp(2, 0)),
                (out(2), inp(1, 0)),
                (out(2), inp(3, 0)),
            ],
        );
        let mut runtime = running(&doc);
        runtime.set_oscillation_limit(1).unwrap();
        for id in [1, 2, 3] {
            runtime.values[runtime.pin_nets[&out(id)]] = Low;
        }
        // Two normal microsteps, then 99 collection batches. The forbidden next batch
        // is rejected before its frontier is executed or included.
        let outcome = runtime.settle(10_000);
        let SettleOutcome::Oscillating { work, diagnostic } = outcome else {
            panic!("expected oscillation");
        };
        assert_eq!(diagnostic.threshold, 1);
        assert_eq!(diagnostic.steps, 101);
        assert_eq!(work, 404);
        assert_eq!(
            diagnostic
                .components
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![ComponentId(1), ComponentId(2), ComponentId(3)]
        );
        assert_eq!(
            runtime.settle(10),
            SettleOutcome::Oscillating {
                work: 0,
                diagnostic: diagnostic.clone()
            }
        );
        assert_eq!(runtime.step(), StepOutcome::Oscillating { diagnostic });
    }

    #[test]
    fn zero_budget_and_partial_batch_preserve_oscillation_detector_partition() {
        let doc = document(
            &[(1, ComponentKind::Not), (2, ComponentKind::Not)],
            &[(out(1), inp(2, 0)), (out(2), inp(1, 0))],
        );
        let init = |runtime: &mut Runtime| {
            runtime.set_oscillation_limit(1).unwrap();
            runtime.values[runtime.pin_nets[&out(1)]] = Low;
            runtime.values[runtime.pin_nets[&out(2)]] = Low;
        };
        let mut whole = running(&doc);
        init(&mut whole);
        let mut chunked = running(&doc);
        init(&mut chunked);
        assert_eq!(
            chunked.settle(0),
            SettleOutcome::BudgetExhausted { work: 0 }
        );
        assert_eq!(
            chunked.settle(2),
            SettleOutcome::BudgetExhausted { work: 2 }
        );
        let expected = whole.settle(10_000);
        let resumed = chunked.settle(10_000);
        let SettleOutcome::Oscillating {
            work: expected_work,
            diagnostic: expected_diagnostic,
        } = expected
        else {
            panic!("expected oscillation");
        };
        let SettleOutcome::Oscillating { work, diagnostic } = resumed else {
            panic!("expected oscillation");
        };
        assert_eq!(diagnostic, expected_diagnostic);
        assert_eq!(diagnostic.steps, 101);
        assert_eq!(expected_work, work + 2);
        assert_eq!(work, 301);
    }

    #[test]
    fn explicit_raw_step_restarts_detection_episode() {
        let doc = document(&[(1, ComponentKind::Not)], &[(out(1), inp(1, 0))]);
        let mut runtime = running(&doc);
        runtime.set_oscillation_limit(1).unwrap();
        runtime.values[runtime.pin_nets[&out(1)]] = Low;
        assert!(matches!(
            runtime.settle(1),
            SettleOutcome::BudgetExhausted { .. }
        ));
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert!(matches!(
            runtime.settle(1),
            SettleOutcome::BudgetExhausted { .. }
        ));
        assert_eq!(runtime.detector.as_ref().map(|d| d.steps), Some(1));
    }

    #[test]
    fn toggling_feedback_is_bounded_and_resumable() {
        // Both gates are in each frontier. Different call partitions must preserve the
        // partially evaluated frontier as well as the successive published oscillation.
        let doc = document(
            &[(1, ComponentKind::Not), (2, ComponentKind::Not)],
            &[(out(1), inp(2, 0)), (out(2), inp(1, 0))],
        );
        let mut first = running(&doc);
        let mut second = running(&doc);
        for runtime in [&mut first, &mut second] {
            runtime.values[runtime.pin_nets[&out(1)]] = Low;
            runtime.values[runtime.pin_nets[&out(2)]] = Low;
        }

        let checkpoints = [2, 3, 7, 8, 12];
        let run_partition = |runtime: &mut Runtime, parts: &[usize]| {
            let mut consumed = 0;
            let mut states = Vec::new();
            for &budget in parts {
                assert_ne!(budget, 0);
                assert_eq!(
                    runtime.settle(budget),
                    SettleOutcome::BudgetExhausted { work: budget }
                );
                consumed += budget;
                assert!(runtime.is_pending());
                if checkpoints.contains(&consumed) {
                    states.push((
                        consumed,
                        [runtime.pin_value(out(1)), runtime.pin_value(out(2))],
                    ));
                }
            }
            states
        };

        let first_states = run_partition(&mut first, &[2, 1, 4, 1, 4]);
        let second_states = run_partition(&mut second, &[1, 1, 1, 2, 2, 1, 4]);
        assert_eq!(first_states, second_states);
        assert_eq!(
            first_states
                .iter()
                .map(|(work, _)| *work)
                .collect::<Vec<_>>(),
            checkpoints
        );
        // Work 1 starts the batch; work 2 has staged just one of two gates, so
        // outputs remain unpublished until work 3 completes the frontier.
        assert_eq!(first_states[0].1, [Some(Low), Some(Low)]);
        assert_eq!(first_states[1].1, [Some(High), Some(High)]);
        assert_eq!(first_states[4].1, [Some(Low), Some(Low)]);
    }

    #[test]
    fn constants_drive_before_gates_and_do_not_become_interactive_inputs() {
        let mut high = component(9, ComponentKind::Const);
        high.constant_value = true;
        let mut low = component(2, ComponentKind::Const);
        low.constant_value = false;
        let doc = document(
            &[
                (9, ComponentKind::Const),
                (2, ComponentKind::Const),
                (3, ComponentKind::And),
                (4, ComponentKind::Output),
                (5, ComponentKind::Output),
            ],
            &[
                (out(9), inp(3, 0)),
                (out(2), inp(3, 1)),
                (out(3), inp(4, 0)),
                (out(9), inp(5, 0)),
            ],
        );
        // Component data uses the configured bits even if visual order is reversed.
        let mut doc = doc;
        doc.components[0] = high;
        doc.components[1] = low;
        let mut runtime = running(&doc);
        assert_eq!(runtime.pin_value(out(9)), Some(High));
        assert_eq!(runtime.pin_value(out(2)), Some(Low));
        assert_eq!(runtime.output_value(ComponentId(5)), Some(High));
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Unavailable));
        settled(&mut runtime);
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Low));
        let mut reversed = doc.clone();
        reversed.components.reverse();
        reversed.wires.geometric_mut().unwrap().reverse();
        let mut reordered = running(&reversed);
        settled(&mut reordered);
        assert_eq!(reordered.output_value(ComponentId(4)), Some(Low));
        assert_eq!(
            runtime.set_input(ComponentId(9), false),
            Err(RuntimeError::NotAnInput(ComponentId(9)))
        );
        assert_eq!(
            runtime.toggle_input(ComponentId(2)),
            Err(RuntimeError::NotAnInput(ComponentId(2)))
        );
        let isolated = document(&[(7, ComponentKind::Const)], &[]);
        let mut standalone = running(&isolated);
        assert_eq!(standalone.settle(0), SettleOutcome::Settled { work: 0 });
        assert_eq!(standalone.pin_value(out(7)), Some(High));
    }

    #[test]
    fn equal_constant_drivers_are_still_a_compile_error() {
        let mut a = component(1, ComponentKind::Const);
        let b = component(2, ComponentKind::Const);
        a.constant_value = b.constant_value;
        let components = vec![a, b];
        let doc = Document {
            wires: crate::test_support::routed_wires(&components, &[(out(1), out(2))]),
            components,
        };
        assert!(matches!(
            Runtime::start(&doc),
            Err(CompileError::GeometricMultipleDrivers { .. })
        ));
    }

    #[test]
    fn and_or_truth_tables_with_toggles() {
        for (kind, truth) in [
            (
                ComponentKind::And,
                (|a, b| a && b) as fn(bool, bool) -> bool,
            ),
            (ComponentKind::Or, |a, b| a || b),
        ] {
            let mut runtime = running(&binary(kind));
            settled(&mut runtime);
            let (mut a, mut b) = (false, false);
            assert_eq!(
                runtime.output_value(ComponentId(4)),
                Some(level(truth(a, b)))
            );
            // Gray-code walk covers every combination using single toggles.
            for toggle_first in [true, false, true] {
                let id = ComponentId(if toggle_first { 1 } else { 2 });
                assert_eq!(runtime.toggle_input(id), Ok(InputChange::Changed));
                if toggle_first {
                    a = !a;
                } else {
                    b = !b;
                }
                settled(&mut runtime);
                assert_eq!(
                    runtime.output_value(ComponentId(4)),
                    Some(level(truth(a, b)))
                );
            }
            for (a, b) in [(false, false), (true, false), (false, true), (true, true)] {
                runtime.set_input(ComponentId(1), a).unwrap();
                settled(&mut runtime);
                runtime.set_input(ComponentId(2), b).unwrap();
                settled(&mut runtime);
                assert_eq!(
                    runtime.output_value(ComponentId(4)),
                    Some(level(truth(a, b)))
                );
            }
        }
    }

    #[test]
    fn nand_nor_xor_xnor_truth_tables_are_exhaustive() {
        for (kind, expected) in [
            (ComponentKind::NAnd, [true, true, true, false]),
            (ComponentKind::NOr, [true, false, false, false]),
            (ComponentKind::XOr, [false, true, true, false]),
            (ComponentKind::XNOr, [true, false, false, true]),
        ] {
            let document = binary(kind);
            let mut runtime = running(&document);
            settled(&mut runtime);
            for (index, (a, b)) in [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .enumerate()
            {
                runtime.set_input(ComponentId(1), a).unwrap();
                settled(&mut runtime);
                runtime.set_input(ComponentId(2), b).unwrap();
                settled(&mut runtime);
                assert_eq!(
                    runtime.output_value(ComponentId(4)),
                    Some(level(expected[index])),
                    "{kind:?} {a}{b}"
                );
            }
        }
    }

    #[test]
    fn mixed_new_gate_cascade_has_order_independent_microsteps() {
        let kinds = [
            (1, ComponentKind::Input),
            (2, ComponentKind::Input),
            (3, ComponentKind::NAnd),
            (4, ComponentKind::XOr),
            (5, ComponentKind::NOr),
            (6, ComponentKind::Output),
        ];
        let wires = [
            (out(1), inp(3, 0)),
            (out(2), inp(3, 1)),
            (out(3), inp(4, 0)),
            (out(2), inp(4, 1)),
            (out(4), inp(5, 0)),
            (out(1), inp(5, 1)),
            (out(5), inp(6, 0)),
        ];
        let mut reversed_kinds = kinds;
        reversed_kinds.reverse();
        let mut reversed_wires = wires;
        reversed_wires.reverse();
        let docs = [
            document(&kinds, &wires),
            document(&reversed_kinds, &reversed_wires),
        ];
        let mut traces = Vec::new();
        for doc in &docs {
            let mut runtime = running(doc);
            let mut trace = Vec::new();
            while runtime.step() != StepOutcome::Idle {
                trace.push(snapshot(&runtime, doc));
            }
            runtime.set_input(ComponentId(1), true).unwrap();
            while runtime.step() != StepOutcome::Idle {
                trace.push(snapshot(&runtime, doc));
            }
            assert_eq!(runtime.output_value(ComponentId(6)), Some(Low));
            traces.push(trace);
        }
        assert_eq!(traces[0], traces[1]);
    }

    #[test]
    fn new_gate_frontier_budget_keeps_every_output_unpublished_until_resume() {
        let kinds = [
            (1, ComponentKind::Input),
            (2, ComponentKind::Input),
            (3, ComponentKind::Input),
            (4, ComponentKind::Input),
            (5, ComponentKind::NAnd),
            (6, ComponentKind::XNOr),
            (7, ComponentKind::Output),
            (8, ComponentKind::Output),
        ];
        let wires = [
            (out(1), inp(5, 0)),
            (out(2), inp(5, 1)),
            (out(3), inp(6, 0)),
            (out(4), inp(6, 1)),
            (out(5), inp(7, 0)),
            (out(6), inp(8, 0)),
        ];
        let document = document(&kinds, &wires);
        let mut whole = running(&document);
        settled(&mut whole);
        let mut resumed = running(&document);
        assert_eq!(
            resumed.settle(2),
            SettleOutcome::BudgetExhausted { work: 2 }
        );
        for pin in [out(5), out(6), inp(7, 0), inp(8, 0)] {
            assert_eq!(resumed.pin_value(pin), Some(Unavailable));
        }
        assert_eq!(resumed.settle(1), SettleOutcome::Settled { work: 1 });
        assert_eq!(snapshot(&resumed, &document), snapshot(&whole, &document));
        assert_eq!(resumed.output_value(ComponentId(7)), Some(High));
        assert_eq!(resumed.output_value(ComponentId(8)), Some(High));
    }

    #[test]
    fn binary_gates_are_strictly_unavailable_with_unavailable_operand() {
        use Level::*;
        for kind in [ComponentKind::And, ComponentKind::Or] {
            let gate = Gate {
                id: ComponentId(1),
                label: String::new(),
                kind,
                inputs: [0, 1],
                output: 2,
            };
            for known in [Low, High, Unavailable] {
                assert_eq!(
                    gate.evaluate(&[Unavailable, known, Unavailable]),
                    Unavailable
                );
                assert_eq!(
                    gate.evaluate(&[known, Unavailable, Unavailable]),
                    Unavailable
                );
            }
        }
    }

    /// Input 1 -> NOT 2 -> NOT 3. Gate 4 reads NOT 2 and NOT 3, so its second microstep reads
    /// a known High beside the still unavailable NOT 3, and output 5 shows the result.
    fn known_beside_unavailable(kind: ComponentKind, swapped: bool) -> Document {
        let (first, second) = if swapped { (3, 2) } else { (2, 3) };
        document(
            &[
                (1, ComponentKind::Input),
                (2, ComponentKind::Not),
                (3, ComponentKind::Not),
                (4, kind),
                (5, ComponentKind::Output),
            ],
            &[
                (out(1), inp(2, 0)),
                (out(2), inp(3, 0)),
                (out(first), inp(4, 0)),
                (out(second), inp(4, 1)),
                (out(4), inp(5, 0)),
            ],
        )
    }

    #[test]
    fn controlling_value_does_not_short_circuit_unavailable() {
        for (kind, final_value) in [(ComponentKind::And, Low), (ComponentKind::Or, High)] {
            for swapped in [false, true] {
                let mut runtime = running(&known_beside_unavailable(kind, swapped));
                assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 3 });
                assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 2 });
                // Gate 4 read High beside Unavailable (NOT 3 publishes only now), so
                // OR(High, U) must not have become High.
                assert_eq!(runtime.pin_value(out(2)), Some(High));
                assert_eq!(runtime.pin_value(out(3)), Some(Low));
                assert_eq!(runtime.output_value(ComponentId(5)), Some(Unavailable));
                settled(&mut runtime);
                assert_eq!(runtime.output_value(ComponentId(5)), Some(final_value));
            }
        }
    }

    #[test]
    fn low_beside_unavailable_stays_unavailable_for_and_or() {
        // Input 1 (Low) feeds gate 3 beside NOT 2, which is unavailable at start.
        for kind in [ComponentKind::And, ComponentKind::Or] {
            for swapped in [false, true] {
                let (first, second) = if swapped { (2, 1) } else { (1, 2) };
                let document = document(
                    &[
                        (1, ComponentKind::Input),
                        (2, ComponentKind::Not),
                        (3, kind),
                        (4, ComponentKind::Output),
                    ],
                    &[
                        (out(1), inp(2, 0)),
                        (out(first), inp(3, 0)),
                        (out(second), inp(3, 1)),
                        (out(3), inp(4, 0)),
                    ],
                );
                let mut runtime = running(&document);
                assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 2 });
                assert_eq!(runtime.pin_value(out(2)), Some(High));
                assert_eq!(runtime.output_value(ComponentId(4)), Some(Unavailable));
                settled(&mut runtime);
                let expected = if kind == ComponentKind::And {
                    Low
                } else {
                    High
                };
                assert_eq!(runtime.output_value(ComponentId(4)), Some(expected));
            }
        }
    }

    #[test]
    fn wide_fanout_registers_consumers_in_linear_time() {
        const FANOUT: u64 = 3_000;
        let mut kinds = vec![(1, ComponentKind::Input)];
        let mut wires = Vec::new();
        for id in 2..FANOUT + 2 {
            kinds.push((id, ComponentKind::Not));
            wires.push((out(1), inp(id, 0)));
        }
        let document = document(&kinds, &wires);
        let started = std::time::Instant::now();
        let mut runtime = running(&document);
        settled(&mut runtime);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(runtime.pin_value(out(2)), Some(High));
        assert_eq!(runtime.pin_value(out(FANOUT + 1)), Some(High));
        runtime.toggle_input(ComponentId(1)).unwrap();
        settled(&mut runtime);
        assert_eq!(runtime.pin_value(out(2)), Some(Low));
        assert_eq!(runtime.pin_value(out(FANOUT + 1)), Some(Low));
        // Each NOT is registered exactly once on the Input net, so one microstep evaluates all.
        runtime.toggle_input(ComponentId(1)).unwrap();
        assert_eq!(runtime.pending_gate_count(), FANOUT as usize);
    }

    #[test]
    fn not_truth_table() {
        let mut runtime = running(&not_chain(1));
        settled(&mut runtime);
        assert_eq!(runtime.output_value(ComponentId(3)), Some(High));
        assert_eq!(
            runtime.toggle_input(ComponentId(1)),
            Ok(InputChange::Changed)
        );
        settled(&mut runtime);
        assert_eq!(runtime.output_value(ComponentId(3)), Some(Low));
        runtime.toggle_input(ComponentId(1)).unwrap();
        settled(&mut runtime);
        assert_eq!(runtime.output_value(ComponentId(3)), Some(High));
    }

    #[test]
    fn start_state_is_unavailable_except_inputs() {
        let mut runtime = Runtime::start(&not_chain(2)).unwrap();
        assert!(runtime.is_pending());
        assert_eq!(runtime.pending_gate_count(), 2);
        assert_eq!(runtime.pin_value(out(1)), Some(Low));
        assert_eq!(runtime.pin_value(out(2)), Some(Low));
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Low));
        assert_eq!(runtime.output_value(ComponentId(1)), None);
        assert_eq!(runtime.output_value(ComponentId(99)), None);
        assert_eq!(runtime.pin_value(inp(99, 0)), None);
        assert_eq!(runtime.net_value(runtime.net_count()), None);
        assert_eq!(runtime.startup_phase(), StartupPhase::StartupSettling);
        assert_eq!(
            runtime.settle(0),
            SettleOutcome::BudgetExhausted { work: 0 }
        );
        assert_eq!(runtime.pin_value(out(2)), Some(Low));
        assert!(runtime.is_pending());
    }

    #[test]
    fn deterministic_startup_settles_not_chain() {
        for length in 1..=6u64 {
            let mut runtime = Runtime::start(&not_chain(length)).unwrap();
            settled(&mut runtime);
            assert_eq!(
                runtime.output_value(ComponentId(length + 2)),
                Some(level(length % 2 == 1)),
                "chain of {length}"
            );
        }
    }

    #[test]
    fn one_microstep_does_not_cross_two_levels() {
        let mut runtime = running(&not_chain(2));
        settled(&mut runtime);
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Low));
        assert_eq!(
            runtime.toggle_input(ComponentId(1)),
            Ok(InputChange::Changed)
        );
        assert!(runtime.is_pending());
        // Rejected while work is pending, and nothing changes.
        assert_eq!(
            runtime.set_input(ComponentId(1), false),
            Err(RuntimeError::Pending)
        );
        assert_eq!(
            runtime.toggle_input(ComponentId(1)),
            Err(RuntimeError::Pending)
        );
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(runtime.pin_value(out(2)), Some(Low));
        assert_eq!(
            runtime.output_value(ComponentId(4)),
            Some(Low),
            "second level not reached"
        );
        assert!(runtime.is_pending());
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(runtime.output_value(ComponentId(4)), Some(High));
        assert!(!runtime.is_pending());
    }

    #[test]
    fn reconvergent_read_then_publish_is_order_independent() {
        // Input 1 drives NOT 2 and AND 3 directly; AND 3 also reads NOT 2.
        let wires = [
            (out(1), inp(2, 0)),
            (out(1), inp(3, 0)),
            (out(2), inp(3, 1)),
            (out(3), inp(4, 0)),
        ];
        let kinds = [
            (1, ComponentKind::Input),
            (2, ComponentKind::Not),
            (3, ComponentKind::And),
            (4, ComponentKind::Output),
        ];
        let mut reversed_kinds = kinds;
        reversed_kinds.reverse();
        let mut reversed_wires = wires;
        reversed_wires.reverse();
        let documents = [
            document(&kinds, &wires),
            document(&reversed_kinds, &reversed_wires),
        ];
        let mut traces = Vec::new();
        for document in &documents {
            let mut runtime = Runtime::start(document).unwrap();
            settled(&mut runtime);
            assert_eq!(runtime.output_value(ComponentId(4)), Some(Low));
            runtime.toggle_input(ComponentId(1)).unwrap();
            let mut trace = vec![snapshot(&runtime, document)];
            while runtime.step() != StepOutcome::Idle {
                trace.push(snapshot(&runtime, document));
            }
            // AND read the old NOT output while NOT read the new input: a one-step glitch.
            assert_eq!(trace.len(), 3);
            let glitch = all_pins(document)
                .iter()
                .position(|pin| *pin == inp(4, 0))
                .unwrap();
            assert_eq!(trace[1][glitch], Some(High));
            assert_eq!(trace[2][glitch], Some(Low));
            traces.push(trace);
        }
        assert_eq!(traces[0], traces[1]);
    }

    #[test]
    fn tiny_budget_matches_uninterrupted_settle() {
        // A wide first frontier (four NOTs on one input) followed by one deeper gate.
        let document = document(
            &[
                (1, ComponentKind::Input),
                (2, ComponentKind::Not),
                (3, ComponentKind::Not),
                (4, ComponentKind::Not),
                (5, ComponentKind::Not),
                (6, ComponentKind::Output),
                (7, ComponentKind::Output),
                (8, ComponentKind::Output),
            ],
            &[
                (out(1), inp(2, 0)),
                (out(1), inp(3, 0)),
                (out(1), inp(4, 0)),
                (out(4), inp(5, 0)),
                (out(2), inp(6, 0)),
                (out(3), inp(7, 0)),
                (out(5), inp(8, 0)),
            ],
        );
        let mut whole = running(&document);
        let SettleOutcome::Settled { work: total } = whole.settle(1_000) else {
            panic!("generous budget must settle");
        };
        // Step 1: 1 + 4 gate evaluations. NOT 5 reads an unavailable net, so step 2 is 1 + 1.
        assert_eq!(total, 5 + 2);

        let mut chunked = running(&document);
        let mut spent = 0;
        let mut calls = 0;
        loop {
            calls += 1;
            let outcome = chunked.settle(1);
            if spent < 4 {
                // Mid-microstep nothing is published yet.
                assert_eq!(chunked.pin_value(out(2)), Some(Unavailable));
            }
            match outcome {
                SettleOutcome::BudgetExhausted { work } => {
                    assert_eq!(work, 1);
                    assert!(chunked.is_pending());
                    spent += work;
                }
                SettleOutcome::Settled { work } => {
                    spent += work;
                    break;
                }
                SettleOutcome::Oscillating { .. } => panic!("no detector configured"),
                SettleOutcome::TraceSequenceExhausted { .. } => panic!("trace capture is disabled"),
            }
            assert!(calls < 1_000, "bounded calls must make progress");
        }
        assert_eq!(spent, total);
        assert!(calls > 1);
        assert_eq!(snapshot(&chunked, &document), snapshot(&whole, &document));
        assert_eq!(chunked.output_value(ComponentId(8)), Some(Low));
    }

    #[test]
    fn zero_budget_makes_no_progress_while_pending() {
        let mut runtime = running(&not_chain(1));
        assert_eq!(
            runtime.settle(0),
            SettleOutcome::BudgetExhausted { work: 0 }
        );
        assert!(runtime.is_pending());
        assert_eq!(runtime.pin_value(out(2)), Some(Unavailable));
    }

    #[test]
    fn idle_step_and_same_value_toggle_are_no_ops() {
        let document = binary(ComponentKind::And);
        let mut runtime = running(&document);
        settled(&mut runtime);
        let before = snapshot(&runtime, &document);
        assert_eq!(runtime.step(), StepOutcome::Idle);
        assert_eq!(runtime.settle(0), SettleOutcome::Settled { work: 0 });
        assert_eq!(
            runtime.set_input(ComponentId(1), false),
            Ok(InputChange::Unchanged)
        );
        assert!(!runtime.is_pending());
        assert_eq!(runtime.pending_gate_count(), 0);
        assert_eq!(snapshot(&runtime, &document), before);
    }

    #[test]
    fn input_errors_are_typed() {
        let mut runtime = running(&binary(ComponentKind::Or));
        settled(&mut runtime);
        assert_eq!(
            runtime.set_input(ComponentId(99), true),
            Err(RuntimeError::UnknownComponent(ComponentId(99)))
        );
        for id in [3, 4] {
            assert_eq!(
                runtime.set_input(ComponentId(id), true),
                Err(RuntimeError::NotAnInput(ComponentId(id)))
            );
            assert_eq!(
                runtime.toggle_input(ComponentId(id)),
                Err(RuntimeError::NotAnInput(ComponentId(id)))
            );
        }
        assert_eq!(
            runtime.toggle_input(ComponentId(99)),
            Err(RuntimeError::UnknownComponent(ComponentId(99)))
        );
    }

    #[test]
    fn invalid_documents_do_not_start() {
        let document = document(&[(1, ComponentKind::Not)], &[]);
        assert!(matches!(
            Runtime::start(&document),
            Err(CompileError::DisconnectedReceiver { .. })
        ));
    }

    #[test]
    fn settle_preflights_atomic_publication_for_a_wide_partially_staged_frontier() {
        let mut kinds = vec![(1, ComponentKind::Input)];
        let mut wires = Vec::new();
        for i in 0..300_u64 {
            let gate = 2 + i * 2;
            let output = gate + 1;
            kinds.push((gate, ComponentKind::Not));
            kinds.push((output, ComponentKind::Output));
            wires.push((out(1), inp(gate, 0)));
            wires.push((out(gate), inp(output, 0)));
        }
        let doc = document(&kinds, &wires);
        let mut runtime =
            Runtime::start_with_trace(&doc, 7, crate::trace::TraceLog::new(1024, 7, 0).unwrap())
                .unwrap();
        runtime.force_running_for_test();
        // Stage all but the final gate. Outputs still publish together at frontier completion.
        assert_eq!(
            runtime.settle(300),
            SettleOutcome::BudgetExhausted { work: 300 }
        );
        assert_eq!(runtime.batch.as_ref().unwrap().staged.len(), 299);
        runtime.force_trace_sequence_for_test(u64::MAX - 100);
        let before = runtime.pending_inspection();
        assert_eq!(
            runtime.settle(1),
            SettleOutcome::TraceSequenceExhausted { work: 0 }
        );
        assert_eq!(runtime.pending_inspection(), before);
        assert!(
            runtime
                .gates
                .iter()
                .all(|gate| runtime.values[gate.output] == Level::Unavailable)
        );
    }

    #[test]
    fn idle_step_preflights_stable_event_after_final_input_sequence() {
        let doc = document(
            &[(1, ComponentKind::Input), (2, ComponentKind::Output)],
            &[(out(1), inp(2, 0))],
        );
        let mut runtime =
            Runtime::start_with_trace(&doc, 4, crate::trace::TraceLog::new(16, 4, 0).unwrap())
                .unwrap();
        runtime.force_running_for_test();
        runtime.force_trace_sequence_for_test(u64::MAX);
        assert_eq!(
            runtime.set_input(ComponentId(1), true),
            Ok(InputChange::Changed)
        );
        let before = snapshot(&runtime, &doc);
        assert_eq!(runtime.step(), StepOutcome::TraceSequenceExhausted);
        assert_eq!(snapshot(&runtime, &doc), before);
    }

    fn completed_startup_frontier_with_successor() -> (Document, Runtime) {
        let mut kinds = Vec::new();
        let mut wires = Vec::new();
        for id in 1..=8 {
            kinds.push((id, ComponentKind::Not));
            wires.push((out(id), inp(id, 0)));
        }
        let doc = document(&kinds, &wires);
        let mut runtime =
            Runtime::start_with_trace(&doc, 17, crate::trace::TraceLog::new(128, 17, 0).unwrap())
                .unwrap();
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 0 });
        for _ in 0..8 {
            assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        }
        assert_eq!(runtime.startup_cursor, runtime.startup_frontier.len());
        assert_eq!(runtime.startup_next.len(), 8);
        (doc, runtime)
    }

    #[test]
    fn startup_successor_frontier_trace_headroom_is_preflighted() {
        for bounded in [false, true] {
            let (doc, mut runtime) = completed_startup_frontier_with_successor();
            runtime.force_trace_sequence_for_test(u64::MAX - 5);
            let before_values = snapshot(&runtime, &doc);
            let before_pending = runtime.pending_inspection();
            let before_frontier = runtime.startup_frontier.clone();
            let before_next = runtime.startup_next.clone();
            let before_cursor = runtime.startup_cursor;
            let before_queued = runtime.startup_queued.clone();
            let before_batch_pending = runtime.startup_batch_pending;
            let before_shuffled = runtime.startup_frontier_shuffled;
            let before_phase = runtime.startup_phase;
            let before_detector_active = runtime.detector.is_some();
            let before_microstep_active = runtime.microstep_active;
            let before_rng = runtime.rng.0;
            let before_trace = runtime
                .trace
                .as_ref()
                .unwrap()
                .records()
                .collect::<Vec<_>>();

            if bounded {
                assert_eq!(
                    runtime.micro_step_bounded(100),
                    SettleOutcome::TraceSequenceExhausted { work: 0 }
                );
            } else {
                assert_eq!(runtime.micro_step(), StepOutcome::TraceSequenceExhausted);
            }
            assert_eq!(snapshot(&runtime, &doc), before_values);
            assert_eq!(runtime.pending_inspection(), before_pending);
            assert_eq!(runtime.startup_frontier, before_frontier);
            assert_eq!(runtime.startup_next, before_next);
            assert_eq!(runtime.startup_cursor, before_cursor);
            assert_eq!(runtime.startup_queued, before_queued);
            assert_eq!(runtime.startup_batch_pending, before_batch_pending);
            assert_eq!(runtime.startup_frontier_shuffled, before_shuffled);
            assert_eq!(runtime.startup_phase, before_phase);
            assert_eq!(runtime.detector.is_some(), before_detector_active);
            assert_eq!(runtime.microstep_active, before_microstep_active);
            assert_eq!(runtime.rng.0, before_rng);
            assert_eq!(
                runtime
                    .trace
                    .as_ref()
                    .unwrap()
                    .records()
                    .collect::<Vec<_>>(),
                before_trace
            );

            runtime.force_trace_sequence_for_test(u64::MAX - 100);
            if bounded {
                assert!(matches!(
                    runtime.micro_step_bounded(100),
                    SettleOutcome::Settled { work: 9 }
                ));
            } else {
                assert_eq!(runtime.micro_step(), StepOutcome::Stepped { evaluated: 8 });
            }
        }
    }

    #[test]
    fn trace_sequence_exhaustion_refusal_preserves_every_observable_scheduler_state() {
        let doc = not_chain(2);
        for api in 0..8 {
            let mut runtime =
                Runtime::start_with_trace(&doc, 5, crate::trace::TraceLog::new(32, 5, 0).unwrap())
                    .unwrap();
            if api >= 4 {
                let first = runtime.pin_nets[&out(2)];
                let second = runtime.pin_nets[&out(3)];
                runtime.register_reset_participant_for_test(first, ResetOutputPolarity::Inverted);
                runtime.register_reset_participant_for_test(second, ResetOutputPolarity::Inverted);
                while runtime.startup_phase == StartupPhase::StartupSettling {
                    let _ = runtime.step();
                }
                assert_eq!(runtime.startup_phase, StartupPhase::ResetRelease);
            }
            runtime.force_trace_sequence_for_test(u64::MAX);
            let before_values = snapshot(&runtime, &doc);
            let before_pending = runtime.pending_inspection();
            let before_frontier = runtime.startup_frontier.clone();
            let before_next = runtime.startup_next.clone();
            let before_cursor = runtime.startup_cursor;
            let before_queued = runtime.startup_queued.clone();
            let before_batch_pending = runtime.startup_batch_pending;
            let before_shuffled = runtime.startup_frontier_shuffled;
            let before_phase = runtime.startup_phase;
            let before_detector = runtime.detector.clone();
            let before_terminal = runtime.terminal.clone();
            let before_rng = runtime.rng.0;
            let before_microstep = runtime.microstep_active;
            let before_reset_released = runtime
                .reset_participants
                .iter()
                .map(|p| p.released)
                .collect::<Vec<_>>();
            let before_trace = runtime
                .trace
                .as_ref()
                .unwrap()
                .records()
                .collect::<Vec<_>>();
            let trace_dropped = runtime.trace.as_ref().unwrap().dropped();
            let trace_first = runtime.trace.as_ref().unwrap().first_sequence();
            let trace_capacity = runtime.trace.as_ref().unwrap().capacity();
            let trace_sequence = runtime.trace.as_ref().unwrap().sequence_state_for_test();
            let refused = match api % 4 {
                0 => runtime.step() == StepOutcome::TraceSequenceExhausted,
                1 => runtime.micro_step() == StepOutcome::TraceSequenceExhausted,
                2 => matches!(
                    runtime.micro_step_bounded(2),
                    SettleOutcome::TraceSequenceExhausted { work: 0 }
                ),
                _ => matches!(
                    runtime.settle(2),
                    SettleOutcome::TraceSequenceExhausted { work: 0 }
                ),
            };
            assert!(refused, "execution API {api}");
            assert_eq!(snapshot(&runtime, &doc), before_values);
            assert_eq!(runtime.pending_inspection(), before_pending);
            assert_eq!(runtime.startup_frontier, before_frontier);
            assert_eq!(runtime.startup_next, before_next);
            assert_eq!(runtime.startup_cursor, before_cursor);
            assert_eq!(runtime.startup_queued, before_queued);
            assert_eq!(runtime.startup_batch_pending, before_batch_pending);
            assert_eq!(runtime.startup_frontier_shuffled, before_shuffled);
            assert_eq!(runtime.startup_phase, before_phase);
            assert_eq!(runtime.detector, before_detector);
            assert_eq!(runtime.terminal, before_terminal);
            assert_eq!(runtime.rng.0, before_rng);
            assert_eq!(runtime.microstep_active, before_microstep);
            assert_eq!(
                runtime
                    .reset_participants
                    .iter()
                    .map(|p| p.released)
                    .collect::<Vec<_>>(),
                before_reset_released
            );
            let trace = runtime.trace.as_ref().unwrap();
            assert_eq!(trace.records().collect::<Vec<_>>(), before_trace);
            assert_eq!(trace.dropped(), trace_dropped);
            assert_eq!(trace.first_sequence(), trace_first);
            assert_eq!(trace.capacity(), trace_capacity);
            assert_eq!(trace.sequence_state_for_test(), trace_sequence);
        }
    }

    #[test]
    fn production_reset_registration_validates_before_typed_asserted_initialization() {
        let doc = not_chain(1);
        let mut runtime = Runtime::start_with_seed(&doc, 13).unwrap();
        let net = runtime.pin_nets[&out(2)];
        let before = runtime.values.clone();
        assert_eq!(
            runtime.install_reset_participant(usize::MAX, ResetOutputPolarity::Normal),
            Err(ResetParticipantError::NetOutOfRange)
        );
        assert_eq!(runtime.values, before);
        assert!(runtime.reset_participants.is_empty());
        runtime
            .install_reset_participant(net, ResetOutputPolarity::Normal)
            .unwrap();
        assert_eq!(runtime.values[net], Level::High);
        let after_install = runtime.values.clone();
        assert_eq!(
            runtime.install_reset_participant(net, ResetOutputPolarity::Inverted),
            Err(ResetParticipantError::DuplicateNet)
        );
        assert_eq!(runtime.values, after_install);
        assert_eq!(runtime.reset_participants.len(), 1);
    }

    #[test]
    fn reset_participants_hold_during_startup_release_once_and_use_normal_scheduler() {
        use crate::trace::TraceEvent;
        let doc = not_chain(2);
        let mut runtime =
            Runtime::start_with_trace(&doc, 13, crate::trace::TraceLog::new(2048, 13, 0).unwrap())
                .unwrap();
        let net_a = runtime.pin_nets[&out(2)];
        let net_b = runtime.pin_nets[&out(3)];
        runtime.register_reset_participant_for_test(net_a, ResetOutputPolarity::Inverted);
        runtime.register_reset_participant_for_test(net_b, ResetOutputPolarity::Inverted);
        loop {
            match runtime.settle(2) {
                SettleOutcome::BudgetExhausted { .. } => {}
                SettleOutcome::Settled { .. } => break,
                other => panic!("unexpected reset startup: {other:?}"),
            }
            if runtime.startup_phase == StartupPhase::ResetRelease {
                break;
            }
        }
        assert_eq!(runtime.startup_phase(), StartupPhase::ResetRelease);
        assert_eq!(runtime.pin_value(out(2)), Some(Level::Low));
        assert_eq!(runtime.pin_value(out(3)), Some(Level::Low));
        assert_eq!(
            runtime.settle(0),
            SettleOutcome::BudgetExhausted { work: 0 }
        );
        assert_eq!(runtime.startup_phase(), StartupPhase::ResetRelease);
        assert!(matches!(
            runtime.settle(1),
            SettleOutcome::BudgetExhausted { .. }
        ));
        assert_eq!(runtime.startup_phase(), StartupPhase::ResetSettling);
        assert_eq!(runtime.pin_value(out(2)), Some(Level::High));
        assert_eq!(runtime.pin_value(out(3)), Some(Level::High));
        assert_eq!(
            runtime.pending_inspection().next_scheduled,
            vec![ComponentId(3)]
        );
        assert!(matches!(
            runtime.settle(1),
            SettleOutcome::BudgetExhausted { .. }
        ));
        assert_eq!(runtime.startup_phase(), StartupPhase::ResetSettling);
        assert_eq!(
            runtime.pin_value(out(3)),
            Some(Level::High),
            "staged normal output is not published early"
        );
        assert!(matches!(runtime.settle(1), SettleOutcome::Settled { .. }));
        assert_eq!(runtime.startup_phase(), StartupPhase::Running);
        assert_eq!(runtime.pin_value(out(3)), Some(Level::Low));
        let rows = runtime
            .trace()
            .unwrap()
            .records()
            .map(|r| r.event)
            .collect::<Vec<_>>();
        assert_eq!(
            rows.iter()
                .filter(|event| **event == TraceEvent::ResetReleased { net: net_a })
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|event| **event == TraceEvent::ResetReleased { net: net_b })
                .count(),
            1
        );
        assert!(rows.iter().any(|event| matches!(
            event,
            TraceEvent::FrontierBegin {
                startup: false,
                phase: StartupPhase::ResetSettling
            }
        )));
        assert!(rows.iter().any(|event| matches!(event, TraceEvent::NetPublished { net, new: Level::Low, .. } if *net == net_b)));
        let release = rows
            .iter()
            .rposition(|event| matches!(event, TraceEvent::ResetReleased { .. }))
            .unwrap();
        let normal_frontier = rows
            .iter()
            .position(|event| {
                matches!(
                    event,
                    TraceEvent::FrontierBegin {
                        startup: false,
                        phase: StartupPhase::ResetSettling
                    }
                )
            })
            .unwrap();
        let running = rows
            .iter()
            .position(|event| {
                *event
                    == TraceEvent::Lifecycle {
                        phase: StartupPhase::Running,
                    }
            })
            .unwrap();
        let stable = rows
            .iter()
            .position(|event| *event == TraceEvent::Stable)
            .unwrap();
        assert!(release < normal_frontier && normal_frontier < running && running < stable);

        let variants = [1, 2, 64].map(|budget| {
            let mut runtime = Runtime::start_with_trace(
                &doc,
                13,
                crate::trace::TraceLog::new(2048, 13, 0).unwrap(),
            )
            .unwrap();
            let first = runtime.pin_nets[&out(2)];
            let second = runtime.pin_nets[&out(3)];
            runtime.register_reset_participant_for_test(first, ResetOutputPolarity::Inverted);
            runtime.register_reset_participant_for_test(second, ResetOutputPolarity::Inverted);
            loop {
                match runtime.settle(budget) {
                    SettleOutcome::Settled { .. } => break,
                    SettleOutcome::BudgetExhausted { .. } => {}
                    other => panic!("reset budget partition failed: {other:?}"),
                }
            }
            (
                snapshot(&runtime, &doc),
                runtime
                    .trace()
                    .unwrap()
                    .records()
                    .map(|record| record.event)
                    .collect::<Vec<_>>(),
            )
        });
        assert_eq!(variants[0], variants[1]);
        assert_eq!(variants[0], variants[2]);

        let mut stepped = Runtime::start_with_seed(&doc, 13).unwrap();
        stepped.register_reset_participant_for_test(
            stepped.pin_nets[&out(2)],
            ResetOutputPolarity::Inverted,
        );
        stepped.register_reset_participant_for_test(
            stepped.pin_nets[&out(3)],
            ResetOutputPolarity::Inverted,
        );
        while stepped.startup_phase() == StartupPhase::StartupSettling {
            let _ = stepped.step();
        }
        assert_eq!(stepped.step(), StepOutcome::Stepped { evaluated: 0 });
        assert_eq!(stepped.startup_phase(), StartupPhase::ResetSettling);
        assert_eq!(stepped.micro_step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(stepped.startup_phase(), StartupPhase::Running);
        assert_eq!(stepped.pin_value(out(3)), Some(Level::Low));
    }

    #[test]
    fn reset_settling_preflight_reserves_normal_fanout_frontier_for_every_api() {
        for staged_publication in [false, true] {
            for api in 0..4 {
                let kinds = std::iter::once((1, ComponentKind::Input))
                    .chain((2..=21).map(|id| (id, ComponentKind::Not)))
                    .collect::<Vec<_>>();
                let wires = (2..=21).map(|id| (out(1), inp(id, 0))).collect::<Vec<_>>();
                let doc = document(&kinds, &wires);
                let mut runtime = Runtime::start_with_trace(
                    &doc,
                    37,
                    crate::trace::TraceLog::new(4096, 37, 0).unwrap(),
                )
                .unwrap();
                runtime.register_reset_pin_for_test(out(1), ResetOutputPolarity::Inverted);
                while runtime.startup_phase == StartupPhase::StartupSettling {
                    let _ = runtime.advance_startup_unit();
                }
                assert_eq!(runtime.startup_phase, StartupPhase::ResetRelease);
                assert!(runtime.advance_startup_unit().is_ok());
                assert_eq!(runtime.startup_phase, StartupPhase::ResetSettling);
                assert_eq!(runtime.queue.len(), 20);
                assert!(runtime.batch.is_none());
                if staged_publication {
                    assert_eq!(
                        runtime.micro_step_bounded(20),
                        SettleOutcome::BudgetExhausted { work: 20 }
                    );
                    assert_eq!(runtime.batch.as_ref().unwrap().staged.len(), 19);
                }

                // step/microstep/bounded need at least the full 20-gate publication
                // frontier, while settle also reserves its bounded-episode records.
                runtime.force_trace_sequence_for_test(u64::MAX - 20);
                let values = runtime.values.clone();
                let queue = runtime.queue.clone();
                let queued = runtime.queued.clone();
                let batch = runtime
                    .batch
                    .as_ref()
                    .map(|batch| (batch.gates.clone(), batch.staged.clone()));
                let phase = runtime.startup_phase;
                let detector = runtime.detector.clone();
                let terminal = runtime.terminal.clone();
                let rng = runtime.rng.0;
                let microstep = runtime.microstep_active;
                let startup_frontier = runtime.startup_frontier.clone();
                let startup_next = runtime.startup_next.clone();
                let startup_cursor = runtime.startup_cursor;
                let startup_queued = runtime.startup_queued.clone();
                let reset_released = runtime
                    .reset_participants
                    .iter()
                    .map(|participant| participant.released)
                    .collect::<Vec<_>>();
                let trace = runtime.trace.as_ref().unwrap();
                let records = trace.records().collect::<Vec<_>>();
                let dropped = trace.dropped();
                let first = trace.first_sequence();
                let sequence = trace.sequence_state_for_test();
                let capacity = trace.capacity();

                let refused = match api {
                    0 => runtime.step() == StepOutcome::TraceSequenceExhausted,
                    1 => runtime.micro_step() == StepOutcome::TraceSequenceExhausted,
                    2 => matches!(
                        runtime.micro_step_bounded(2),
                        SettleOutcome::TraceSequenceExhausted { work: 0 }
                    ),
                    _ => matches!(
                        runtime.settle(1),
                        SettleOutcome::TraceSequenceExhausted { work: 0 }
                    ),
                };
                assert!(refused, "execution API {api}");
                assert_eq!(runtime.values, values, "net values, API {api}");
                assert_eq!(runtime.queue, queue, "queue, API {api}");
                assert_eq!(runtime.queued, queued, "queued flags, API {api}");
                assert_eq!(
                    runtime
                        .batch
                        .as_ref()
                        .map(|batch| (batch.gates.clone(), batch.staged.clone())),
                    batch,
                    "batch/staged outputs, API {api}"
                );
                assert_eq!(runtime.startup_phase, phase, "phase, API {api}");
                assert_eq!(runtime.detector, detector, "detector, API {api}");
                assert_eq!(runtime.terminal, terminal, "terminal state, API {api}");
                assert_eq!(runtime.rng.0, rng, "RNG, API {api}");
                assert_eq!(runtime.microstep_active, microstep, "microstep, API {api}");
                assert_eq!(
                    runtime.startup_frontier, startup_frontier,
                    "frontier, API {api}"
                );
                assert_eq!(runtime.startup_next, startup_next, "next, API {api}");
                assert_eq!(runtime.startup_cursor, startup_cursor, "cursor, API {api}");
                assert_eq!(
                    runtime.startup_queued, startup_queued,
                    "startup flags, API {api}"
                );
                assert_eq!(
                    runtime
                        .reset_participants
                        .iter()
                        .map(|participant| participant.released)
                        .collect::<Vec<_>>(),
                    reset_released,
                    "reset release flags, API {api}"
                );
                let trace = runtime.trace.as_ref().unwrap();
                assert_eq!(
                    trace.records().collect::<Vec<_>>(),
                    records,
                    "records, API {api}"
                );
                assert_eq!(trace.dropped(), dropped, "dropped, API {api}");
                assert_eq!(trace.first_sequence(), first, "first sequence, API {api}");
                assert_eq!(
                    trace.sequence_state_for_test(),
                    sequence,
                    "sequence, API {api}"
                );
                assert_eq!(trace.capacity(), capacity, "capacity, API {api}");

                runtime.force_trace_sequence_for_test(u64::MAX - 1_000);
                let progressed = match api {
                    0 => runtime.step() != StepOutcome::TraceSequenceExhausted,
                    1 => runtime.micro_step() != StepOutcome::TraceSequenceExhausted,
                    2 => !matches!(
                        runtime.micro_step_bounded(2),
                        SettleOutcome::TraceSequenceExhausted { .. }
                    ),
                    _ => !matches!(
                        runtime.settle(1),
                        SettleOutcome::TraceSequenceExhausted { .. }
                    ),
                };
                assert!(progressed, "retry with headroom, API {api}");
            }
        }
    }

    #[test]
    fn reset_release_trace_preflight_reserves_all_releases_from_startup_phase() {
        let kinds = (1..=20)
            .map(|id| (id, ComponentKind::Not))
            .collect::<Vec<_>>();
        let wires = (1..=20).map(|id| (out(id), inp(id, 0))).collect::<Vec<_>>();
        let doc = document(&kinds, &wires);
        let mut runtime =
            Runtime::start_with_trace(&doc, 31, crate::trace::TraceLog::new(128, 31, 0).unwrap())
                .unwrap();
        for id in 1..=20 {
            let net = runtime.pin_nets[&out(id)];
            runtime.register_reset_participant_for_test(net, ResetOutputPolarity::Inverted);
        }
        // Model the empty startup frontier immediately before the barrier. The upcoming
        // settle operation crosses startup, release, and reset-settling in one call.
        runtime.startup_frontier.clear();
        runtime.startup_cursor = 0;
        runtime.startup_batch_pending = false;
        runtime.startup_frontier_shuffled = true;
        runtime.force_trace_sequence_for_test(u64::MAX - 21);
        let before_values = runtime.values.clone();
        let before_queue = runtime.queue.clone();
        let before_queued = runtime.queued.clone();
        let before_phase = runtime.startup_phase;
        let before_detector = runtime.detector.clone();
        let before_rng = runtime.rng.0;
        let before_frontier = runtime.startup_frontier.clone();
        let before_next = runtime.startup_next.clone();
        let before_cursor = runtime.startup_cursor;
        let before_released = runtime
            .reset_participants
            .iter()
            .map(|p| p.released)
            .collect::<Vec<_>>();
        let before_trace = runtime
            .trace
            .as_ref()
            .unwrap()
            .records()
            .collect::<Vec<_>>();
        let before_dropped = runtime.trace.as_ref().unwrap().dropped();
        let before_first = runtime.trace.as_ref().unwrap().first_sequence();
        let before_sequence = runtime.trace.as_ref().unwrap().sequence_state_for_test();

        assert_eq!(
            runtime.settle(2),
            SettleOutcome::TraceSequenceExhausted { work: 0 }
        );
        assert_eq!(runtime.values, before_values);
        assert_eq!(runtime.queue, before_queue);
        assert_eq!(runtime.queued, before_queued);
        assert_eq!(runtime.startup_phase, before_phase);
        assert_eq!(runtime.detector, before_detector);
        assert_eq!(runtime.rng.0, before_rng);
        assert_eq!(runtime.startup_frontier, before_frontier);
        assert_eq!(runtime.startup_next, before_next);
        assert_eq!(runtime.startup_cursor, before_cursor);
        assert_eq!(
            runtime
                .reset_participants
                .iter()
                .map(|p| p.released)
                .collect::<Vec<_>>(),
            before_released
        );
        let trace = runtime.trace.as_ref().unwrap();
        assert_eq!(trace.records().collect::<Vec<_>>(), before_trace);
        assert_eq!(trace.dropped(), before_dropped);
        assert_eq!(trace.first_sequence(), before_first);
        assert_eq!(trace.sequence_state_for_test(), before_sequence);
    }

    fn self_not_after_held_reset_release() -> (Document, Runtime) {
        let doc = document(&[(1, ComponentKind::Not)], &[(out(1), inp(1, 0))]);
        let mut runtime = Runtime::start_with_seed(&doc, 7).unwrap();
        runtime.register_reset_participant_for_test(
            runtime.pin_nets[&out(1)],
            ResetOutputPolarity::Inverted,
        );
        runtime.set_oscillation_limit(1).unwrap();
        while runtime.startup_phase() != StartupPhase::ResetSettling {
            let outcome = runtime.step();
            assert!(!matches!(outcome, StepOutcome::Oscillating { .. }));
        }
        (doc, runtime)
    }

    #[test]
    fn reset_settling_oscillation_survives_all_stepping_apis_and_chunks() {
        for api in 0..4 {
            let (_doc, mut runtime) = self_not_after_held_reset_release();
            let mut polls = 0;
            loop {
                polls += 1;
                assert!(
                    polls < 2_000,
                    "reset oscillation must be detected in finite work"
                );
                let outcome = match api {
                    0 => match runtime.step() {
                        StepOutcome::Oscillating { diagnostic } => SettleOutcome::Oscillating {
                            work: 0,
                            diagnostic,
                        },
                        _ => SettleOutcome::BudgetExhausted { work: 0 },
                    },
                    1 => match runtime.micro_step() {
                        StepOutcome::Oscillating { diagnostic } => SettleOutcome::Oscillating {
                            work: 0,
                            diagnostic,
                        },
                        _ => SettleOutcome::BudgetExhausted { work: 0 },
                    },
                    2 => runtime.micro_step_bounded(1),
                    _ => runtime.settle(1),
                };
                if let SettleOutcome::Oscillating { diagnostic, .. } = outcome {
                    assert_eq!(diagnostic.threshold, 1);
                    assert_eq!(diagnostic.steps, 1 + OSCILLATION_COLLECTION_OFFSET);
                    assert_eq!(runtime.terminal_diagnostic(), Some(&diagnostic));
                    assert!(
                        diagnostic
                            .components
                            .iter()
                            .any(|component| component.id == ComponentId(1))
                    );
                    break;
                }
            }
        }
    }

    #[test]
    fn released_reset_feedback_uses_resumable_bounded_detection() {
        let doc = document(&[(1, ComponentKind::Not)], &[(out(1), inp(1, 0))]);
        let mut runtime = Runtime::start_with_seed(&doc, 7).unwrap();
        runtime.register_reset_participant_for_test(
            runtime.pin_nets[&out(1)],
            ResetOutputPolarity::Inverted,
        );
        runtime.set_oscillation_limit(1).unwrap();
        while runtime.startup_phase() == StartupPhase::StartupSettling {
            let _ = runtime.step();
        }
        assert_eq!(runtime.startup_phase(), StartupPhase::ResetRelease);
        let mut polls = 0;
        loop {
            polls += 1;
            assert!(polls < 10_000, "reset settling must stay bounded");
            match runtime.settle(2) {
                SettleOutcome::BudgetExhausted { .. } => {}
                SettleOutcome::Oscillating { diagnostic, .. } => {
                    assert_eq!(diagnostic.threshold, 1);
                    assert!(!diagnostic.components.is_empty());
                    assert_eq!(runtime.terminal_diagnostic(), Some(&diagnostic));
                    break;
                }
                other => panic!("expected reset-release feedback oscillation, got {other:?}"),
            }
        }
    }
}
