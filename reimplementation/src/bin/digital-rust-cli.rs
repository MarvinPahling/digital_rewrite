#![forbid(unsafe_code)]
//! Small, bounded headless entry point for the documented v2 editing subset.
use digital_rust::{
    domain::{ComponentId, ComponentKind},
    engine::{Session, SimulationError},
    file_session::{DiscardIntent, FileIo, LocalFileIo},
    runtime::{Level, SettleOutcome, SimulationOptions},
    sim_text,
    simulation::{self, MAX_COMPONENTS, MAX_WORK},
};
use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

const DEFAULT_BUDGET: usize = 20_000;
const MAX_ASSIGNMENTS: usize = 10_000;
const MAX_BUDGET: usize = MAX_WORK;

fn usage() -> &'static str {
    "Usage:\n  digital-rust-cli open PATH\n  digital-rust-cli validate PATH\n  digital-rust-cli simulate PATH [--input ID=0|1]... [--budget N] [--oscillation-limit N] [--seed N]\n  digital-rust-cli trace PATH [--seed N] [--oscillation-limit N] [--budget N] [--trace-capacity N] [--op micro|full|input:ID=0|1]...\n\nExit codes: 0 success, 1 document/operation/simulation failure, 2 usage error.\nOnly the strict editable .dig v2 subset and one-bit simulator are supported; feedback is accepted but settling is bounded and convergence is not guaranteed. Startup seed defaults to 0; seeded order uses splitmix64-fy-v1 and is not Java trace-compatible. This is not Digital CLI compatibility."
}
fn fail(message: impl std::fmt::Display, code: u8) -> ExitCode {
    eprintln!("digital-rust-cli: {message}");
    ExitCode::from(code)
}
fn escaped(text: &str) -> String {
    text.chars().flat_map(char::escape_default).collect()
}
fn kind_name(kind: ComponentKind) -> &'static str {
    kind.name()
}
fn parse_number(text: &str, what: &str) -> Result<usize, String> {
    text.parse::<usize>()
        .map_err(|_| format!("invalid {what}: {text:?}"))
}
fn run(args: Vec<OsString>) -> Result<(), (String, u8)> {
    let base =
        env::current_dir().map_err(|e| (format!("cannot capture startup directory: {e}"), 1))?;
    if !base.is_absolute() {
        return Err(("startup directory is not absolute".into(), 1));
    }
    let mut io = LocalFileIo::new(base);
    run_with_io(args, &mut io)
}
fn run_with_io(args: Vec<OsString>, io: &mut dyn FileIo) -> Result<(), (String, u8)> {
    run_with_io_output(args, io, &mut std::io::stdout().lock())
}
fn run_with_io_output(
    args: Vec<OsString>,
    io: &mut dyn FileIo,
    output: &mut dyn std::io::Write,
) -> Result<(), (String, u8)> {
    macro_rules! outln {
        ($($arg:tt)*) => {
            writeln!(output, $($arg)*)
                .map_err(|error| (format!("cannot write CLI output: {error}"), 1))?
        };
    }
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err((usage().into(), 2));
    };
    let command = command
        .to_str()
        .ok_or_else(|| ("command must be UTF-8".into(), 2))?;
    if command == "--help" || command == "-h" {
        outln!("{}", usage());
        return Ok(());
    }
    if !matches!(command, "open" | "validate" | "simulate" | "trace") {
        return Err((format!("unknown command {command:?}\n{}", usage()), 2));
    }
    let path = args
        .next()
        .ok_or_else(|| (format!("missing PATH\n{}", usage()), 2))?;
    let (mut assignments, mut budget, mut oscillation_limit, mut startup_seed, mut seed_seen) = (
        Vec::<(ComponentId, bool)>::new(),
        DEFAULT_BUDGET,
        digital_rust::runtime::DEFAULT_OSCILLATION_LIMIT,
        0_u64,
        false,
    );
    let mut trace_capacity = 1024usize;
    let mut trace_ops = Vec::<String>::new();
    if !matches!(command, "simulate" | "trace") && args.next().is_some() {
        return Err((format!("unexpected argument\n{}", usage()), 2));
    }
    if command == "simulate" || command == "trace" {
        let rest: Vec<_> = args.collect();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].to_str() {
                Some("--input") if command == "simulate" => {
                    i += 1;
                    let text = rest
                        .get(i)
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| ("--input requires UTF-8 ID=0|1".into(), 2))?;
                    let (id, value) = text.split_once('=').ok_or_else(|| {
                        (
                            format!("invalid input assignment {text:?}; expected ID=0|1"),
                            2,
                        )
                    })?;
                    let id = id
                        .parse::<u64>()
                        .map_err(|_| (format!("invalid component ID {id:?}"), 2))?;
                    let value = match value {
                        "0" => false,
                        "1" => true,
                        _ => {
                            return Err((
                                format!("invalid input value {value:?}; expected 0 or 1"),
                                2,
                            ));
                        }
                    };
                    assignments.push((ComponentId(id), value));
                    if assignments.len() > MAX_ASSIGNMENTS {
                        return Err((format!("too many assignments (limit {MAX_ASSIGNMENTS})"), 2));
                    }
                }
                Some("--op") if command == "trace" => {
                    i += 1;
                    let op = rest
                        .get(i)
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| ("--op requires a UTF-8 operation".into(), 2))?;
                    if !(op == "micro" || op == "full" || op.starts_with("input:")) {
                        return Err((format!("invalid trace operation {op:?}"), 2));
                    }
                    if let Some(assignment) = op.strip_prefix("input:") {
                        let (id, value) = assignment.split_once('=').ok_or_else(|| {
                            (
                                format!(
                                    "invalid trace input operation {op:?}; expected input:ID=0|1"
                                ),
                                2,
                            )
                        })?;
                        id.parse::<u64>()
                            .map_err(|_| (format!("invalid component ID {id:?}"), 2))?;
                        if !matches!(value, "0" | "1") {
                            return Err((
                                format!("invalid input value {value:?}; expected 0 or 1"),
                                2,
                            ));
                        }
                    }
                    trace_ops.push(op.to_owned());
                    if trace_ops.len() > 10_000 {
                        return Err(("too many trace operations (limit 10000)".into(), 2));
                    }
                }
                Some("--trace-capacity") if command == "trace" => {
                    i += 1;
                    let text = rest
                        .get(i)
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| ("--trace-capacity requires an integer".into(), 2))?;
                    trace_capacity = parse_number(text, "trace capacity").map_err(|e| (e, 2))?;
                    if !(1..=digital_rust::trace::MAX_TRACE_CAPACITY).contains(&trace_capacity) {
                        return Err((
                            format!(
                                "trace capacity must be 1..={}",
                                digital_rust::trace::MAX_TRACE_CAPACITY
                            ),
                            2,
                        ));
                    }
                }
                Some("--seed") => {
                    if seed_seen {
                        return Err(("duplicate --seed".into(), 2));
                    }
                    seed_seen = true;
                    i += 1;
                    let text = rest
                        .get(i)
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| ("--seed requires unsigned decimal u64".into(), 2))?;
                    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                        return Err((
                            format!("invalid startup seed {text:?}; expected unsigned decimal u64"),
                            2,
                        ));
                    }
                    startup_seed = text
                        .parse::<u64>()
                        .map_err(|_| (format!("startup seed out of range: {text:?}"), 2))?;
                }
                Some("--oscillation-limit") => {
                    i += 1;
                    let text = rest
                        .get(i)
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| ("--oscillation-limit requires an integer".into(), 2))?;
                    oscillation_limit =
                        parse_number(text, "oscillation limit").map_err(|e| (e, 2))?;
                    if oscillation_limit == 0 || oscillation_limit > MAX_WORK {
                        return Err((
                            format!("oscillation limit must be between 1 and {MAX_WORK}"),
                            2,
                        ));
                    }
                }
                Some("--budget") => {
                    i += 1;
                    let text = rest
                        .get(i)
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| ("--budget requires an integer".into(), 2))?;
                    budget = parse_number(text, "work budget").map_err(|e| (e, 2))?;
                    if budget == 0 || budget > MAX_BUDGET {
                        return Err((format!("budget must be between 1 and {MAX_BUDGET}"), 2));
                    }
                }
                Some(flag) => return Err((format!("unknown option {flag:?}\n{}", usage()), 2)),
                None => return Err(("options must be UTF-8".into(), 2)),
            }
            i += 1;
        }
    }
    let mut session = Session::default();
    session
        .open_path(io, &PathBuf::from(path), DiscardIntent::DiscardDirty)
        .map_err(|e| (format!("cannot open document: {e}"), 1))?;
    match command {
        "open" => {
            outln!("Opened supported editable .dig v2 subset; NOT simulator-validated.");
            for c in &session.document().components {
                outln!(
                    "component id={} kind={} label=\"{}\"{}",
                    c.id.0,
                    kind_name(c.kind),
                    escaped(&c.label),
                    if c.kind == ComponentKind::Const {
                        format!(" configured={}", if c.constant_value { 1 } else { 0 })
                    } else {
                        String::new()
                    }
                );
            }
            outln!("wires={}", session.document().wires.len());
            for wire in &session.document().wires {
                let endpoint = |pin: &digital_rust::domain::PinRef| {
                    let direction = match pin.pin.direction {
                        digital_rust::domain::PinDirection::Input => "in",
                        digital_rust::domain::PinDirection::Output => "out",
                    };
                    format!(
                        "{}:{direction}{}",
                        pin.component.0,
                        u16::from(pin.pin.index) + 1
                    )
                };
                outln!(
                    "wire id={} {}--{}",
                    wire.id.0,
                    endpoint(&wire.endpoints[0]),
                    endpoint(&wire.endpoints[1])
                );
            }
        }
        "validate" => {
            simulation::compile(session.document())
                .map_err(|e| (sim_text::describe_compile_error(session.document(), &e), 1))?;
            outln!(
                "valid: supported one-bit circuit structure (not full Digital validation; feedback convergence is not assessed)"
            );
        }
        "trace" => {
            for op in &trace_ops {
                if let Some(assignment) = op.strip_prefix("input:") {
                    let (id, _) = assignment
                        .split_once('=')
                        .ok_or_else(|| ("invalid trace input operation".into(), 2))?;
                    let id = ComponentId(
                        id.parse::<u64>()
                            .map_err(|_| (format!("invalid component ID {id:?}"), 2))?,
                    );
                    let Some(component) = session
                        .document()
                        .components
                        .iter()
                        .find(|component| component.id == id)
                    else {
                        return Err((format!("unknown input component ID {}", id.0), 1));
                    };
                    if component.kind != ComponentKind::Input {
                        return Err((format!("component ID {} is not an input", id.0), 1));
                    }
                }
            }
            if session.document().components.len() > MAX_COMPONENTS {
                return Err(("component limit exceeded".into(), 1));
            }
            session
                .start_simulation_with_trace(
                    SimulationOptions {
                        startup_seed,
                        oscillation_limit,
                    },
                    trace_capacity,
                )
                .map_err(|e| {
                    (
                        sim_text::describe_simulation_error(session.document(), &e),
                        1,
                    )
                })?;
            let execution = (|| -> Result<(), (String, u8)> {
                let mut remaining = budget;
                for (number, op) in trace_ops.iter().enumerate() {
                    if op == "micro" {
                        let max_micro = 1 + session
                            .document()
                            .components
                            .iter()
                            .filter(|c| {
                                matches!(
                                    c.kind,
                                    ComponentKind::And
                                        | ComponentKind::NAnd
                                        | ComponentKind::Or
                                        | ComponentKind::NOr
                                        | ComponentKind::XOr
                                        | ComponentKind::XNOr
                                        | ComponentKind::Not
                                )
                            })
                            .count();
                        if remaining < max_micro {
                            return Err((
                                format!(
                                    "trace work budget exhausted before operation {} (needs up to {max_micro} work units)",
                                    number + 1
                                ),
                                1,
                            ));
                        }
                        match session.micro_step_simulation().map_err(|e| {
                            (
                                sim_text::describe_simulation_error(session.document(), &e),
                                1,
                            )
                        })? {
                            digital_rust::runtime::StepOutcome::Stepped { evaluated } => {
                                remaining = remaining.saturating_sub(evaluated.saturating_add(1));
                            }
                            digital_rust::runtime::StepOutcome::Idle => {}
                            other => {
                                return Err((format!("microstep did not complete: {other:?}"), 1));
                            }
                        }
                    } else if op == "full" {
                        let before = remaining;
                        settle(&mut session, &mut remaining, budget)?;
                        if before == remaining
                            && session
                                .inspect()
                                .is_some_and(|runtime| runtime.is_pending())
                        {
                            return Err(("full settle made no progress".into(), 1));
                        }
                    } else {
                        let assignment = op.strip_prefix("input:").unwrap_or_default();
                        let (id, value) = assignment.split_once('=').ok_or_else(|| {
                            (
                                format!(
                                    "invalid trace input operation {op:?}; expected input:ID=0|1"
                                ),
                                2,
                            )
                        })?;
                        let id = ComponentId(
                            id.parse::<u64>()
                                .map_err(|_| (format!("invalid component ID {id:?}"), 2))?,
                        );
                        let value = match value {
                            "0" => false,
                            "1" => true,
                            _ => return Err((format!("invalid input value {value:?}"), 2)),
                        };
                        if remaining == 0 {
                            return Err((
                                format!(
                                    "trace work budget exhausted before operation {}",
                                    number + 1
                                ),
                                1,
                            ));
                        }
                        session.set_input(id, value).map_err(|e| {
                            (
                                sim_text::describe_simulation_error(session.document(), &e),
                                1,
                            )
                        })?;
                        remaining -= 1;
                    }
                    let pending = session.inspect().map(|r| r.pending_inspection());
                    if let Some(pending) = pending {
                        outln!(
                            "op {}={} pending={} awaiting={:?} staged={:?} next={:?}",
                            number + 1,
                            op,
                            pending.unique_count,
                            pending
                                .awaiting_evaluation
                                .iter()
                                .map(|id| id.0)
                                .collect::<Vec<_>>(),
                            pending
                                .staged_unpublished
                                .iter()
                                .map(|id| id.0)
                                .collect::<Vec<_>>(),
                            pending
                                .next_scheduled
                                .iter()
                                .map(|id| id.0)
                                .collect::<Vec<_>>()
                        );
                    } else {
                        outln!("op {}={} status=terminal", number + 1, op);
                    }
                }
                Ok(())
            })();
            if session.simulation_active() {
                session.stop_simulation().map_err(|e| (e.to_string(), 1))?;
            }
            let trace = session
                .trace()
                .ok_or_else(|| ("trace capture unavailable".into(), 1))?;
            if let Err((message, _)) = &execution {
                outln!("trace status=diagnostic error=\"{}\"", escaped(message));
            }
            outln!(
                "trace seed={} policy={} capacity={} retained={} dropped={} first_sequence={:?}",
                trace.header.startup_seed,
                trace.header.random_policy,
                trace.capacity(),
                trace.len(),
                trace.dropped(),
                trace.first_sequence()
            );
            for record in trace.records() {
                outln!("trace seq={} event={:?}", record.sequence, record.event);
            }
            execution?;
        }
        "simulate" => {
            // Check every user-supplied assignment before compilation/startup can tick runtime.
            let mut seen = std::collections::HashSet::new();
            for (id, _) in &assignments {
                let component = session
                    .document()
                    .components
                    .iter()
                    .find(|c| c.id == *id)
                    .ok_or_else(|| (format!("unknown input component ID {}", id.0), 1))?;
                if component.kind != ComponentKind::Input {
                    return Err((format!("component ID {} is not an input", id.0), 1));
                }
                if !seen.insert(*id) {
                    return Err((
                        format!("duplicate input assignment for component ID {}", id.0),
                        2,
                    ));
                }
            }
            if session.document().components.len() > MAX_COMPONENTS {
                return Err(("component limit exceeded".into(), 1));
            }
            session
                .start_simulation_with_options(SimulationOptions {
                    startup_seed,
                    oscillation_limit,
                })
                .map_err(|e| {
                    (
                        sim_text::describe_simulation_error(session.document(), &e),
                        1,
                    )
                })?;
            let mut remaining = budget;
            settle(&mut session, &mut remaining, budget)?;
            for (id, value) in assignments {
                session.set_input(id, value).map_err(|e| {
                    (
                        sim_text::describe_simulation_error(session.document(), &e),
                        1,
                    )
                })?;
                settle(&mut session, &mut remaining, budget)?;
            }
            let rows = session
                .document()
                .components
                .iter()
                .filter(|c| c.kind == ComponentKind::Output)
                .map(|c| {
                    let v = session
                        .inspect()
                        .and_then(|r| r.output_value(c.id))
                        .unwrap_or(Level::Unavailable);
                    (c.id, escaped(&c.label), v)
                })
                .collect::<Vec<_>>();
            for (id, label, value) in rows {
                let value = match value {
                    Level::Low => "0",
                    Level::High => "1",
                    Level::Unavailable => "?",
                };
                outln!("output id={} label=\"{}\" value={value}", id.0, label);
            }
        }
        _ => return Err((format!("unknown command {command:?}"), 2)),
    }
    Ok(())
}
fn settle(session: &mut Session, remaining: &mut usize, total: usize) -> Result<(), (String, u8)> {
    let result = session.settle_simulation(*remaining);
    let outcome = result.map_err(|e: SimulationError| {
        let mut message = sim_text::describe_simulation_error(session.document(), &e);
        if let Some(context) = session.terminal_simulation_context() {
            message.push_str(&format!(
                " (startup seed {}, phase {:?})",
                context.startup_seed, context.phase
            ));
        }
        (message, 1)
    })?;
    match outcome {
        SettleOutcome::Settled { work } => {
            *remaining -= work;
            Ok(())
        }
        SettleOutcome::BudgetExhausted { .. } => Err((
            format!("settling work budget exhausted (limit {total}); no settled outputs reported"),
            1,
        )),
        SettleOutcome::TraceSequenceExhausted { .. } => Err((
            "trace sequence exhausted; operation was not advanced".into(),
            1,
        )),
        SettleOutcome::Oscillating { work, diagnostic } => Err((
            sim_text::describe_oscillation(session.document(), &diagnostic, work),
            1,
        )),
    }
}
fn main() -> ExitCode {
    match run(env::args_os().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err((message, code)) => fail(message, code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use digital_rust::file_session::{SaveAsIntent, StagedFile};
    use std::{cell::Cell, io, path::Path};

    struct FakeIo {
        base: PathBuf,
        bytes: Vec<u8>,
        writes: Cell<bool>,
        reads: Cell<usize>,
    }
    impl FileIo for FakeIo {
        fn base_dir(&self) -> &Path {
            &self.base
        }
        fn read_bounded_regular(&mut self, _: &Path, limit: usize) -> io::Result<Vec<u8>> {
            self.reads.set(self.reads.get() + 1);
            if self.bytes.len() > limit {
                return Err(io::Error::other("fixture exceeds read bound"));
            }
            Ok(self.bytes.clone())
        }
        fn validate_target(&mut self, _: &Path, _: SaveAsIntent) -> io::Result<()> {
            self.writes.set(true);
            Err(io::Error::other("unexpected target validation"))
        }
        fn create_temp(&mut self, _: &Path) -> io::Result<Box<dyn StagedFile>> {
            self.writes.set(true);
            Err(io::Error::other("unexpected stage"))
        }
        fn publish(&mut self, _: &Path, _: &Path, _: SaveAsIntent) -> io::Result<()> {
            self.writes.set(true);
            Err(io::Error::other("unexpected publish"))
        }
        fn sync_parent(&mut self, _: &Path) -> io::Result<()> {
            self.writes.set(true);
            Err(io::Error::other("unexpected sync"))
        }
        fn remove_temp(&mut self, _: &Path) -> io::Result<()> {
            self.writes.set(true);
            Err(io::Error::other("unexpected remove"))
        }
    }

    fn feedback_with_output_dig() -> Vec<u8> {
        use digital_rust::domain::{
            Component, ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position,
            Wire, WireId,
        };
        let not = Component {
            id: ComponentId(1),
            kind: ComponentKind::Not,
            position: Position { x: 0., y: 0. },
            label: String::new(),
            constant_value: true,
        };
        let output = Component {
            id: ComponentId(2),
            kind: ComponentKind::Output,
            position: Position { x: 100., y: 0. },
            label: "oscillating output".into(),
            constant_value: true,
        };
        let pin = |component, direction| PinRef {
            component: ComponentId(component),
            pin: PinId {
                direction,
                index: 0,
            },
        };
        let document = Document {
            components: vec![not, output],
            wires: vec![
                Wire {
                    id: WireId(1),
                    endpoints: [pin(1, PinDirection::Output), pin(1, PinDirection::Input)],
                },
                Wire {
                    id: WireId(2),
                    endpoints: [pin(1, PinDirection::Output), pin(2, PinDirection::Input)],
                },
            ],
        };
        let dig = digital_rust::formats::from_domain(&document).unwrap();
        digital_rust::formats::serialize_v2(&dig).unwrap()
    }

    #[test]
    fn open_validate_reject_seed_and_simulate_seed_parser_rejects_non_ascii_whitespace_before_io() {
        for args in [
            vec!["open", "input.dig", "--seed", "0"],
            vec!["validate", "input.dig", "--seed", "0"],
            vec!["simulate", "input.dig", "--seed", " 1"],
            vec!["simulate", "input.dig", "--seed", "1 "],
            vec!["simulate", "input.dig", "--seed", "１"],
            vec!["simulate", "input.dig", "--seed", " 1"],
        ] {
            let mut io = FakeIo {
                base: PathBuf::from("/virtual"),
                bytes: vec![],
                writes: Cell::new(false),
                reads: Cell::new(0),
            };
            let result = run_with_io(args.iter().map(OsString::from).collect(), &mut io);
            assert!(result.is_err(), "accepted arguments {args:?}");
            assert_eq!(result.unwrap_err().1, 2);
            assert_eq!(
                io.reads.get(),
                0,
                "invalid seed/option must fail before file access"
            );
        }
    }

    #[test]
    fn feedback_ring_with_output_fails_without_returning_success_rows() {
        let mut io = FakeIo {
            base: PathBuf::from("/virtual"),
            bytes: feedback_with_output_dig(),
            writes: Cell::new(false),
            reads: Cell::new(0),
        };
        let mut stdout = Vec::new();
        let result = run_with_io_output(
            vec![OsString::from("simulate"), OsString::from("ring.dig")],
            &mut io,
            &mut stdout,
        );
        let (message, code) = result.unwrap_err();
        assert_eq!(code, 1);
        assert!(message.contains("oscillat"), "{message}");
        assert!(
            stdout.is_empty(),
            "failure printed output rows: {}",
            String::from_utf8_lossy(&stdout)
        );
        assert!(
            !message.contains("output id="),
            "failure cannot report success rows"
        );
        assert_eq!(io.reads.get(), 1);
        assert!(!io.writes.get());
    }

    #[test]
    fn every_command_uses_injected_io_read_only() {
        let bytes = b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement></visualElements><wires/></circuit>".to_vec();
        for command in ["open", "validate", "simulate"] {
            let mut io = FakeIo {
                base: PathBuf::from("/virtual"),
                bytes: bytes.clone(),
                writes: Cell::new(false),
                reads: Cell::new(0),
            };
            let mut args = vec![OsString::from(command), OsString::from("input.dig")];
            if command == "simulate" {
                args.extend([OsString::from("--input"), OsString::from("0=1")]);
            }
            assert!(run_with_io(args, &mut io).is_ok());
            assert!(!io.writes.get(), "{command} attempted filesystem output");
        }
    }
}
