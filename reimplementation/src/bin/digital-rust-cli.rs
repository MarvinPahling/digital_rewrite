#![forbid(unsafe_code)]
//! Small, bounded headless entry point for the documented v2 editing subset.
use digital_rust::{
    domain::{ComponentId, ComponentKind},
    engine::{Session, SimulationError},
    file_session::{DiscardIntent, FileIo, LocalFileIo},
    runtime::{Level, SettleOutcome},
    sim_text,
    simulation::{self, MAX_COMPONENTS, MAX_WORK},
};
use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

const DEFAULT_BUDGET: usize = 20_000;
const MAX_ASSIGNMENTS: usize = 10_000;
const MAX_BUDGET: usize = MAX_WORK;

fn usage() -> &'static str {
    "Usage:\n  digital-rust-cli open PATH\n  digital-rust-cli validate PATH\n  digital-rust-cli simulate PATH [--input ID=0|1]... [--budget N]\n\nExit codes: 0 success, 1 document/operation/simulation failure, 2 usage error.\nOnly the strict editable .dig v2 subset and one-bit simulator are supported; feedback is accepted but settling is bounded and convergence is not guaranteed. This is not Digital CLI compatibility."
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
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err((usage().into(), 2));
    };
    let command = command
        .to_str()
        .ok_or_else(|| ("command must be UTF-8".into(), 2))?;
    if command == "--help" || command == "-h" {
        println!("{}", usage());
        return Ok(());
    }
    if !matches!(command, "open" | "validate" | "simulate") {
        return Err((format!("unknown command {command:?}\n{}", usage()), 2));
    }
    let path = args
        .next()
        .ok_or_else(|| (format!("missing PATH\n{}", usage()), 2))?;
    let (mut assignments, mut budget) = (Vec::<(ComponentId, bool)>::new(), DEFAULT_BUDGET);
    if command != "simulate" && args.next().is_some() {
        return Err((format!("unexpected argument\n{}", usage()), 2));
    }
    if command == "simulate" {
        let rest: Vec<_> = args.collect();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].to_str() {
                Some("--input") => {
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
            println!("Opened supported editable .dig v2 subset; NOT simulator-validated.");
            for c in &session.document().components {
                println!(
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
            println!("wires={}", session.document().wires.len());
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
                println!(
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
            println!(
                "valid: supported one-bit circuit structure (not full Digital validation; feedback convergence is not assessed)"
            );
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
            session.start_simulation().map_err(|e| {
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
                println!("output id={} label=\"{}\" value={value}", id.0, label);
            }
        }
        _ => return Err((format!("unknown command {command:?}"), 2)),
    }
    Ok(())
}
fn settle(session: &mut Session, remaining: &mut usize, total: usize) -> Result<(), (String, u8)> {
    match session
        .settle_simulation(*remaining)
        .map_err(|e: SimulationError| (e.to_string(), 1))?
    {
        SettleOutcome::Settled { work } => {
            *remaining -= work;
            Ok(())
        }
        SettleOutcome::BudgetExhausted { .. } => Err((
            format!("settling work budget exhausted (limit {total}); no settled outputs reported"),
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
    }
    impl FileIo for FakeIo {
        fn base_dir(&self) -> &Path {
            &self.base
        }
        fn read_bounded_regular(&mut self, _: &Path, limit: usize) -> io::Result<Vec<u8>> {
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

    #[test]
    fn every_command_uses_injected_io_read_only() {
        let bytes = b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement></visualElements><wires/></circuit>".to_vec();
        for command in ["open", "validate", "simulate"] {
            let mut io = FakeIo {
                base: PathBuf::from("/virtual"),
                bytes: bytes.clone(),
                writes: Cell::new(false),
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
