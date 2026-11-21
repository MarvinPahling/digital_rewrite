//! Headless runtime for one-bit acyclic circuits compiled by [`crate::simulation`].
//!
//! Runtime values live here, never in the editable [`Document`]. A microstep reads every
//! scheduled gate's inputs before any output is published, so results do not depend on
//! evaluation order. Acyclic initialisation is deterministic and makes no claim about
//! randomised feedback startup. High-Z, buses and feedback are not modelled.
use crate::domain::*;
use crate::simulation::{CompileError, CompiledCircuit, compile};
use std::collections::HashMap;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputChange {
    Unchanged,
    Changed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// Nothing was pending; state is untouched.
    Idle,
    /// One microstep published the outputs of `evaluated` gates.
    Stepped { evaluated: usize },
}

/// Work is counted in gate evaluations plus one unit per microstep started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    Settled {
        work: usize,
    },
    /// The budget ran out; the runtime is intact and a later call resumes where this stopped.
    /// This says nothing about oscillation.
    BudgetExhausted {
        work: usize,
    },
}

#[derive(Clone, Copy)]
enum Role {
    Input { net: usize },
    Output { net: usize },
    Gate,
}

struct Gate {
    kind: ComponentKind,
    inputs: [usize; 2],
    output: usize,
}

impl Gate {
    fn evaluate(&self, values: &[Level]) -> Level {
        let a = values[self.inputs[0]];
        let b = values[self.inputs[1]];
        match (self.kind, a, b) {
            (ComponentKind::Not, Level::Unavailable, _) => Level::Unavailable,
            (ComponentKind::Not, a, _) => Level::from_bool(a == Level::Low),
            (_, Level::Unavailable, _) | (_, _, Level::Unavailable) => Level::Unavailable,
            (ComponentKind::And, a, b) => Level::from_bool(a == Level::High && b == Level::High),
            (_, a, b) => Level::from_bool(a == Level::High || b == Level::High),
        }
    }
}

/// A microstep whose gates are being evaluated; nothing is published until all are done.
struct Batch {
    gates: Vec<usize>,
    staged: Vec<Level>,
}

pub struct Runtime {
    roles: HashMap<ComponentId, Role>,
    pin_nets: HashMap<PinRef, usize>,
    gates: Vec<Gate>,
    consumers: Vec<Vec<usize>>,
    values: Vec<Level>,
    queue: Vec<usize>,
    queued: Vec<bool>,
    batch: Option<Batch>,
}

impl Runtime {
    /// Compiles `document` and schedules every gate. Inputs start false.
    pub fn start(document: &Document) -> Result<Self, CompileError> {
        Ok(Self::new(document, &compile(document)?))
    }

    /// `compiled` must come from compiling `document`.
    fn new(document: &Document, compiled: &CompiledCircuit) -> Self {
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
        let mut values = vec![Level::Unavailable; compiled.nets.len()];
        for component in &document.components {
            let id = component.id;
            let role = match component.kind {
                ComponentKind::Input => {
                    let net = net_of(id, PinDirection::Output, 0);
                    values[net] = Level::Low;
                    Role::Input { net }
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
                        kind,
                        inputs: [first, second],
                        output: net_of(id, PinDirection::Output, 0),
                    });
                    Role::Gate
                }
            };
            roles.insert(id, role);
        }
        Self {
            roles,
            pin_nets,
            queue: (0..gates.len()).collect(),
            queued: vec![true; gates.len()],
            gates,
            consumers,
            values,
            batch: None,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.batch.is_some() || !self.queue.is_empty()
    }

    /// Gates scheduled or part-way through the current microstep.
    pub fn pending_gate_count(&self) -> usize {
        self.queue.len() + self.batch.as_ref().map_or(0, |batch| batch.gates.len())
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
        if self.is_pending() {
            return Err(RuntimeError::Pending);
        }
        let level = Level::from_bool(value);
        if self.values[net] == level {
            return Ok(InputChange::Unchanged);
        }
        self.values[net] = level;
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

    /// Runs one complete microstep (finishing one already begun by a bounded settle).
    /// Its cost is bounded by the compiled gate count.
    pub fn step(&mut self) -> StepOutcome {
        if !self.is_pending() {
            return StepOutcome::Idle;
        }
        if self.batch.is_none() {
            self.begin_batch();
        }
        loop {
            if let Some(evaluated) = self.evaluate_next() {
                return StepOutcome::Stepped { evaluated };
            }
        }
    }

    /// Microsteps until idle or `budget` work units are spent. A microstep in progress is
    /// resumed, so a large frontier is evaluated across calls without partial publication.
    pub fn settle(&mut self, budget: usize) -> SettleOutcome {
        let mut work = 0;
        while self.is_pending() {
            if work == budget {
                return SettleOutcome::BudgetExhausted { work };
            }
            work += 1;
            if self.batch.is_none() {
                self.begin_batch();
            } else {
                self.evaluate_next();
            }
        }
        SettleOutcome::Settled { work }
    }

    /// Snapshots the scheduled gates as the next microstep.
    fn begin_batch(&mut self) {
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
        let batch = self.batch.as_mut()?;
        let gate = batch.gates[batch.staged.len()];
        batch.staged.push(self.gates[gate].evaluate(&self.values));
        if batch.staged.len() < batch.gates.len() {
            return None;
        }
        let batch = self.batch.take()?;
        let mut changed = Vec::new();
        for (&gate, &level) in batch.gates.iter().zip(&batch.staged) {
            let net = self.gates[gate].output;
            if self.values[net] != level {
                self.values[net] = level;
                changed.push(net);
            }
        }
        for net in changed {
            self.schedule_consumers(net);
        }
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
            position: Position { x: 0.0, y: 0.0 },
            label: String::new(),
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
        Document {
            components: kinds
                .iter()
                .map(|&(id, kind)| component(id, kind))
                .collect(),
            wires: wires
                .iter()
                .enumerate()
                .map(|(i, &(a, b))| Wire {
                    id: WireId(i as u64),
                    endpoints: [a, b],
                })
                .collect(),
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

    #[test]
    fn and_or_truth_tables_with_toggles() {
        for (kind, truth) in [
            (
                ComponentKind::And,
                (|a, b| a && b) as fn(bool, bool) -> bool,
            ),
            (ComponentKind::Or, |a, b| a || b),
        ] {
            let mut runtime = Runtime::start(&binary(kind)).unwrap();
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
    fn binary_gates_are_strictly_unavailable_with_unavailable_operand() {
        use Level::*;
        for kind in [ComponentKind::And, ComponentKind::Or] {
            let gate = Gate {
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
                let mut runtime = Runtime::start(&known_beside_unavailable(kind, swapped)).unwrap();
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
                let mut runtime = Runtime::start(&document).unwrap();
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
        let mut runtime = Runtime::start(&document).unwrap();
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
        let mut runtime = Runtime::start(&not_chain(1)).unwrap();
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
        assert_eq!(runtime.pin_value(out(2)), Some(Unavailable));
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Unavailable));
        assert_eq!(runtime.output_value(ComponentId(1)), None);
        assert_eq!(runtime.output_value(ComponentId(99)), None);
        assert_eq!(runtime.pin_value(inp(99, 0)), None);
        assert_eq!(runtime.net_value(runtime.net_count()), None);
        // Unavailable inputs yield unavailable outputs rather than a guessed value.
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 2 });
        assert_eq!(runtime.pin_value(out(2)), Some(High));
        assert_eq!(runtime.pin_value(out(3)), Some(Unavailable));
        assert!(runtime.is_pending());
        assert_eq!(runtime.step(), StepOutcome::Stepped { evaluated: 1 });
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Low));
        assert!(!runtime.is_pending());
    }

    #[test]
    fn startup_settles_acyclic_not_chain() {
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
        let mut runtime = Runtime::start(&not_chain(2)).unwrap();
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
        let mut whole = Runtime::start(&document).unwrap();
        let SettleOutcome::Settled { work: total } = whole.settle(1_000) else {
            panic!("generous budget must settle");
        };
        // Step 1: 1 + 4 gate evaluations. NOT 5 reads an unavailable net, so step 2 is 1 + 1.
        assert_eq!(total, 5 + 2);

        let mut chunked = Runtime::start(&document).unwrap();
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
        let mut runtime = Runtime::start(&not_chain(1)).unwrap();
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
        let mut runtime = Runtime::start(&document).unwrap();
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
        let mut runtime = Runtime::start(&binary(ComponentKind::Or)).unwrap();
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
            Err(CompileError::MissingDriver { .. })
        ));
    }
}
