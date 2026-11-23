use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "digital rust cli {} é {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn cli(dir: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_digital-rust-cli"))
        .args(args)
        .current_dir(&dir.0)
        .output()
        .unwrap()
}
fn fixture(label: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>In</elementName><elementAttributes><entry><string>Label</string><string>{label}</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement></visualElements><wires/></circuit>").into_bytes()
}
#[test]
fn open_validate_simulate_are_headless_read_only_and_use_relative_unicode_paths() {
    let dir = TempDir::new();
    let path = dir.0.join("circuit ü space.dig");
    let original = fixture("quote &quot; newline&#10;雪");
    fs::write(&path, &original).unwrap();
    let opened = cli(&dir, &["open", "circuit ü space.dig"]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let text = String::from_utf8(opened.stdout).unwrap();
    assert!(text.contains("NOT simulator-validated"));
    assert!(text.contains("component id=0 kind=Input"));
    assert!(text.contains("newline\\n"));
    assert!(text.contains("quote \\\""));
    let absolute = Command::new(env!("CARGO_BIN_EXE_digital-rust-cli"))
        .arg("open")
        .arg(path.as_os_str())
        .output()
        .unwrap();
    assert!(absolute.status.success());
    let valid = cli(&dir, &["validate", "circuit ü space.dig"]);
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let simulated = cli(&dir, &["simulate", "circuit ü space.dig", "--input", "0=1"]);
    assert!(
        simulated.status.success(),
        "{}",
        String::from_utf8_lossy(&simulated.stderr)
    );
    assert!(simulated.stdout.is_empty());
    assert_eq!(fs::read(path).unwrap(), original);
}
#[test]
fn const_cli_open_reports_both_configured_values_and_assignment_is_rejected_read_only() {
    let dir = TempDir::new();
    for value in ["0", "1"] {
        let name = format!("const-{value}.dig");
        let xml = format!(
            "<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>Const</elementName><elementAttributes><entry><string>Value</string><long>{value}</long></entry></elementAttributes><pos x=\"-100\" y=\"-20\"/></visualElement></visualElements><wires/></circuit>"
        );
        fs::write(dir.0.join(&name), xml.as_bytes()).unwrap();
        let opened = cli(&dir, &["open", &name]);
        assert!(
            opened.status.success(),
            "{}",
            String::from_utf8_lossy(&opened.stderr)
        );
        let text = String::from_utf8(opened.stdout).unwrap();
        assert!(text.contains(&format!("configured={value}")));
        assert!(text.contains("kind=Const"));
        let validated = cli(&dir, &["validate", &name]);
        assert!(
            validated.status.success(),
            "{}",
            String::from_utf8_lossy(&validated.stderr)
        );
        let simulated = cli(&dir, &["simulate", &name]);
        assert!(
            simulated.status.success(),
            "{}",
            String::from_utf8_lossy(&simulated.stderr)
        );
        let rejected = cli(&dir, &["simulate", &name, "--input", "0=1"]);
        assert_eq!(rejected.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("not an input"));
        assert_eq!(fs::read(dir.0.join(name)).unwrap(), xml.as_bytes());
    }
}

#[test]
fn malformed_missing_unsupported_and_usage_errors_have_stable_status_and_stderr() {
    let dir = TempDir::new();
    let missing = cli(&dir, &["open", "absent.dig"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert!(!missing.stderr.is_empty());
    fs::write(
        dir.0.join("bad.dig"),
        b"<circuit><version>9</version></circuit>",
    )
    .unwrap();
    for command in ["open", "validate", "simulate"] {
        let out = cli(&dir, &[command, "bad.dig"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
    fs::write(dir.0.join("broken.dig"), b"<not xml").unwrap();
    assert_eq!(
        cli(&dir, &["validate", "broken.dig"]).status.code(),
        Some(1)
    );
    assert_eq!(
        cli(&dir, &["simulate", "bad.dig", "--bogus"]).status.code(),
        Some(2)
    );
    assert_eq!(cli(&dir, &["open", "."]).status.code(), Some(1));
}
#[test]
fn assignments_are_validated_before_start_and_overflow_is_usage_error() {
    let dir = TempDir::new();
    fs::write(
        dir.0.join("input.dig"),
        gate_document(digital_rust::domain::ComponentKind::Not),
    )
    .unwrap();
    let invalid = cli(
        &dir,
        &["simulate", "input.dig", "--budget", "1", "--input", "99=1"],
    );
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("unknown input component ID 99"));
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("budget exhausted"));
    for args in [
        vec!["simulate", "input.dig", "--input", "0=2"],
        vec!["simulate", "input.dig", "--input", "0=1", "--input", "0=0"],
        vec![
            "simulate",
            "input.dig",
            "--budget",
            "99999999999999999999999999",
        ],
    ] {
        let out = cli(&dir, &args);
        assert_ne!(out.status.code(), Some(0));
        assert!(out.stdout.is_empty());
    }
}
fn set_component_at(
    doc: &mut digital_rust::domain::Document,
    id: u64,
    kind: digital_rust::domain::ComponentKind,
    target: (i32, i32),
    pin_offset: (i32, i32),
    label: &str,
) {
    let center = digital_rust::symbols::center_from_origin_offset(
        digital_rust::symbols::DEFAULT_STYLE,
        kind,
    )
    .unwrap();
    doc.components.push(digital_rust::domain::Component {
        id: digital_rust::domain::ComponentId(id),
        kind,
        position: digital_rust::domain::Position {
            x: center[0] + (target.0 - pin_offset.0) as f32,
            y: center[1] + (target.1 - pin_offset.1) as f32,
        },
        label: label.into(),
        constant_value: true,
    });
}
fn gate_document(kind: digital_rust::domain::ComponentKind) -> Vec<u8> {
    use digital_rust::domain::{
        ComponentId as C, ComponentKind as K, Document, PinDirection as D, PinId, PinRef, Wire,
        WireId,
    };
    let p = |id, direction, index| PinRef {
        component: C(id),
        pin: PinId { direction, index },
    };
    let mut d = Document::default();
    if kind == K::Not {
        set_component_at(&mut d, 0, K::Input, (0, 0), (0, 0), "input");
        set_component_at(&mut d, 1, K::Not, (100, 0), (0, 0), "inverter");
        set_component_at(&mut d, 2, K::Output, (200, 0), (0, 0), "result");
        d.wires = vec![
            Wire {
                id: WireId(0),
                endpoints: [p(0, D::Output, 0), p(1, D::Input, 0)],
            },
            Wire {
                id: WireId(1),
                endpoints: [p(1, D::Output, 0), p(2, D::Input, 0)],
            },
        ];
    } else {
        set_component_at(&mut d, 0, K::Input, (0, 0), (0, 0), "a");
        set_component_at(&mut d, 1, K::Input, (0, 40), (0, 0), "b");
        set_component_at(&mut d, 2, kind, (100, 0), (0, 0), "gate");
        set_component_at(&mut d, 3, K::Output, (220, 20), (0, 0), "result");
        d.wires = vec![
            Wire {
                id: WireId(0),
                endpoints: [p(0, D::Output, 0), p(2, D::Input, 0)],
            },
            Wire {
                id: WireId(1),
                endpoints: [p(1, D::Output, 0), p(2, D::Input, 1)],
            },
            Wire {
                id: WireId(2),
                endpoints: [p(2, D::Output, 0), p(3, D::Input, 0)],
            },
        ];
    }
    digital_rust::formats::serialize_v2(&digital_rust::formats::from_domain(&d).unwrap()).unwrap()
}
#[test]
fn inverter_and_two_input_gates_run_exhaustive_truth_tables() {
    use digital_rust::domain::ComponentKind;
    let dir = TempDir::new();
    fs::write(dir.0.join("not.dig"), gate_document(ComponentKind::Not)).unwrap();
    for input in [0, 1] {
        let out = cli(
            &dir,
            &["simulate", "not.dig", "--input", &format!("0={input}")],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .contains(&format!("output id=2 label=\"result\" value={}", 1 - input))
        );
    }
    for kind in [
        ComponentKind::And,
        ComponentKind::NAnd,
        ComponentKind::Or,
        ComponentKind::NOr,
        ComponentKind::XOr,
        ComponentKind::XNOr,
    ] {
        let file = format!("{}.dig", kind.name().to_ascii_lowercase());
        fs::write(dir.0.join(&file), gate_document(kind)).unwrap();
        for a in [0, 1] {
            for b in [0, 1] {
                let out = cli(
                    &dir,
                    &[
                        "simulate",
                        &file,
                        "--input",
                        &format!("0={a}"),
                        "--input",
                        &format!("1={b}"),
                    ],
                );
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let expected = match kind {
                    ComponentKind::And => a & b,
                    ComponentKind::NAnd => 1 - (a & b),
                    ComponentKind::Or => a | b,
                    ComponentKind::NOr => 1 - (a | b),
                    ComponentKind::XOr => a ^ b,
                    ComponentKind::XNOr => 1 - (a ^ b),
                    _ => unreachable!(),
                };
                assert!(
                    String::from_utf8_lossy(&out.stdout).contains(&format!("value={expected}")),
                    "{}",
                    String::from_utf8_lossy(&out.stdout)
                );
            }
        }
    }
}
#[test]
fn startup_and_input_assignment_share_one_budget_at_exact_boundary() {
    let dir = TempDir::new();
    fs::write(
        dir.0.join("not.dig"),
        gate_document(digital_rust::domain::ComponentKind::Not),
    )
    .unwrap();

    // One gate takes two settle work units (begin batch, evaluate); setting the input queues
    // that gate again, taking two more. This exact cumulative total must be accepted.
    let exact = cli(
        &dir,
        &["simulate", "not.dig", "--input", "0=1", "--budget", "4"],
    );
    assert!(
        exact.status.success(),
        "{}",
        String::from_utf8_lossy(&exact.stderr)
    );
    assert!(String::from_utf8_lossy(&exact.stdout).contains("value=0"));

    // One less than the cumulative startup-plus-assignment cost must fail without output.
    let short = cli(
        &dir,
        &["simulate", "not.dig", "--input", "0=1", "--budget", "3"],
    );
    assert_eq!(short.status.code(), Some(1));
    assert!(short.stdout.is_empty());
    assert!(String::from_utf8_lossy(&short.stderr).contains("budget exhausted"));
}

#[test]
fn settle_exhaustion_has_no_settled_output() {
    let dir = TempDir::new();
    fs::write(
        dir.0.join("gate.dig"),
        gate_document(digital_rust::domain::ComponentKind::And),
    )
    .unwrap();
    let out = cli(&dir, &["simulate", "gate.dig", "--budget", "1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("budget exhausted"));
}

#[test]
fn oversized_unsupported_component_and_bad_geometry_fail_without_success_output() {
    let dir = TempDir::new();
    fs::write(dir.0.join("large.dig"), vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    let large = cli(&dir, &["open", "large.dig"]);
    assert_eq!(large.status.code(), Some(1));
    assert!(large.stdout.is_empty());
    let unsupported = fixture("x");
    let unsupported = String::from_utf8(unsupported).unwrap().replace(
        "<elementName>In</elementName>",
        "<elementName>Clock</elementName>",
    );
    fs::write(dir.0.join("unsupported.dig"), unsupported).unwrap();
    let out = cli(&dir, &["open", "unsupported.dig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let geometry = fixture("x");
    let geometry = String::from_utf8(geometry).unwrap().replace("</visualElements>", "<visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement></visualElements>");
    fs::write(dir.0.join("geometry.dig"), geometry).unwrap();
    let out = cli(&dir, &["validate", "geometry.dig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn non_utf8_filename_is_passed_as_an_os_path() {
    use std::os::unix::ffi::OsStringExt;
    let dir = TempDir::new();
    let name = std::ffi::OsString::from_vec(b"circuit-\xff.dig".to_vec());
    match fs::write(dir.0.join(&name), fixture("byte path")) {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::InvalidInput
                || error.raw_os_error() == Some(92) =>
        {
            return;
        }
        Err(error) => panic!("cannot create non-UTF-8 test path: {error}"),
    }
    let out = Command::new(env!("CARGO_BIN_EXE_digital-rust-cli"))
        .arg("open")
        .arg(name)
        .current_dir(&dir.0)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn validate_reports_floating_multiple_driver_and_feedback_errors() {
    use digital_rust::{
        domain::{
            ComponentId as C, ComponentKind as K, PinDirection as D, PinId, PinRef, Wire, WireId,
        },
        formats,
    };
    let dir = TempDir::new();
    let base = gate_document(K::And);
    let mut floating = formats::parse_v2(&base).unwrap();
    floating.wires.remove(0);
    fs::write(
        dir.0.join("floating.dig"),
        formats::serialize_v2(&floating).unwrap(),
    )
    .unwrap();
    let out = cli(&dir, &["validate", "floating.dig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Nothing drives"));

    let mut multiple = formats::to_domain(&formats::parse_v2(&base).unwrap()).unwrap();
    multiple.wires.push(Wire {
        id: WireId(99),
        endpoints: [
            PinRef {
                component: C(0),
                pin: PinId {
                    direction: D::Output,
                    index: 0,
                },
            },
            PinRef {
                component: C(1),
                pin: PinId {
                    direction: D::Output,
                    index: 0,
                },
            },
        ],
    });
    let bytes = formats::serialize_v2(&formats::from_domain(&multiple).unwrap()).unwrap();
    fs::write(dir.0.join("drivers.dig"), bytes).unwrap();
    let out = cli(&dir, &["validate", "drivers.dig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Several outputs"));

    let mut cycle = digital_rust::domain::Document::default();
    set_component_at(&mut cycle, 0, K::Not, (0, 0), (0, 0), "loop");
    cycle.wires.push(Wire {
        id: WireId(0),
        endpoints: [
            PinRef {
                component: C(0),
                pin: PinId {
                    direction: D::Input,
                    index: 0,
                },
            },
            PinRef {
                component: C(0),
                pin: PinId {
                    direction: D::Output,
                    index: 0,
                },
            },
        ],
    });
    let bytes = formats::serialize_v2(&formats::from_domain(&cycle).unwrap()).unwrap();
    fs::write(dir.0.join("cycle.dig"), bytes).unwrap();
    let out = cli(&dir, &["validate", "cycle.dig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Feedback loop"));
}

#[test]
fn assignment_count_and_zero_budget_are_bounded_usage_errors() {
    let dir = TempDir::new();
    fs::write(dir.0.join("input.dig"), fixture("limited")).unwrap();
    let zero = cli(&dir, &["simulate", "input.dig", "--budget", "0"]);
    assert_eq!(zero.status.code(), Some(2));
    let mut command = Command::new(env!("CARGO_BIN_EXE_digital-rust-cli"));
    command.arg("simulate").arg("input.dig");
    for _ in 0..10_001 {
        command.arg("--input").arg("0=1");
    }
    let too_many = command.current_dir(&dir.0).output().unwrap();
    assert_eq!(too_many.status.code(), Some(2));
    assert!(too_many.stdout.is_empty());
}
