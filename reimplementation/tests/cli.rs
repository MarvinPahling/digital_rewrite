mod common;
use common::routed_wires;
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
fn trace_cli_uses_actual_bounded_microstep_work_and_rejects_numeric_duplicates() {
    let dir = TempDir::new();
    fs::write(dir.0.join("idle.dig"), fixture("idle")).unwrap();
    let affordable = cli(
        &dir,
        &["trace", "idle.dig", "--budget", "1", "--op", "micro"],
    );
    assert!(
        affordable.status.success(),
        "{}",
        String::from_utf8_lossy(&affordable.stderr)
    );
    for args in [
        vec!["trace", "missing.dig", "--budget", "1", "--budget", "2"],
        vec![
            "trace",
            "missing.dig",
            "--oscillation-limit",
            "1",
            "--oscillation-limit",
            "2",
        ],
        vec![
            "trace",
            "missing.dig",
            "--trace-capacity",
            "1",
            "--trace-capacity",
            "2",
        ],
        vec!["trace", "missing.dig", "--budget", "+1"],
        vec!["trace", "missing.dig", "--oscillation-limit", "+1"],
        vec!["trace", "missing.dig", "--trace-capacity", "+1"],
    ] {
        let result = cli(&dir, &args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "args={args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&result.stderr).contains("cannot open document"));
    }
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
        ComponentId as C, ComponentKind as K, Document, PinDirection as D, PinId, PinRef,
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
        d.wires = routed_wires(
            &d.components,
            &[
                (p(0, D::Output, 0), p(1, D::Input, 0)),
                (p(1, D::Output, 0), p(2, D::Input, 0)),
            ],
        )
    } else {
        set_component_at(&mut d, 0, K::Input, (0, 0), (0, 0), "a");
        set_component_at(&mut d, 1, K::Input, (0, 40), (0, 0), "b");
        set_component_at(&mut d, 2, kind, (100, 0), (0, 0), "gate");
        set_component_at(&mut d, 3, K::Output, (220, 20), (0, 0), "result");
        d.wires = routed_wires(
            &d.components,
            &[
                (p(0, D::Output, 0), p(2, D::Input, 0)),
                (p(1, D::Output, 0), p(2, D::Input, 1)),
                (p(2, D::Output, 0), p(3, D::Input, 0)),
            ],
        )
    }
    digital_rust::formats::serialize_v2(&digital_rust::formats::from_domain(&d).unwrap()).unwrap()
}
#[test]
fn cli_reports_real_feedback_startup_oscillation_without_success_rows() {
    use digital_rust::domain::{
        Component, ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position,
    };
    let dir = TempDir::new();
    let id = ComponentId(0);
    let input = PinRef {
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
        label: "ring".into(),
        constant_value: true,
    }];
    let document = Document {
        wires: routed_wires(&components, &[(output, input)]),
        components,
    };
    let encoded = digital_rust::formats::serialize_v2(
        &digital_rust::formats::from_domain(&document).unwrap(),
    )
    .unwrap();
    fs::write(dir.0.join("ring.dig"), &encoded).unwrap();
    let output = cli(
        &dir,
        &[
            "simulate",
            "ring.dig",
            "--oscillation-limit",
            "1",
            "--budget",
            "80000",
            "--seed",
            "91",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "no success output may precede failure"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("appears to oscillate")
            && stderr.contains("startup seed 91")
            && stderr.contains("StartupSettling"),
        "{stderr}"
    );
    assert_eq!(fs::read(dir.0.join("ring.dig")).unwrap(), encoded);
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

    // Startup costs one frontier-begin unit, one gate evaluation and one empty reset-release
    // barrier. The later input propagation costs a normal begin and evaluate unit.
    let exact = cli(
        &dir,
        &["simulate", "not.dig", "--input", "0=1", "--budget", "5"],
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
        &["simulate", "not.dig", "--input", "0=1", "--budget", "4"],
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

#[test]
fn geometric_cli_open_preserves_records_and_validate_reports_floating_net_without_writing() {
    let dir = TempDir::new();
    let open_xml = b"<circuit><version>2</version><attributes/><visualElements/><wires><wire><p1 x=\"9\" y=\"8\"/><p2 x=\"2\" y=\"3\"/></wire><wire><p1 x=\"9\" y=\"8\"/><p2 x=\"2\" y=\"3\"/></wire><wire><p1 x=\"-2147483648\" y=\"2147483647\"/><p2 x=\"2147483647\" y=\"-2147483648\"/></wire></wires></circuit>";
    fs::write(dir.0.join("geometry.dig"), open_xml).unwrap();
    let opened = cli(&dir, &["open", "geometry.dig"]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let output = String::from_utf8_lossy(&opened.stdout);
    assert!(output.contains("wiring=geometric-segments"));
    assert!(output.contains("segment id=0 (9,8)--(2,3)"));
    assert!(output.contains("segment id=1 (9,8)--(2,3)"));
    assert_eq!(fs::read(dir.0.join("geometry.dig")).unwrap(), open_xml);
    let valid = cli(&dir, &["validate", "geometry.dig"]);
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );

    let floating = b"<circuit><version>2</version><attributes/><visualElements><visualElement><elementName>Out</elementName><elementAttributes/><pos x=\"100\" y=\"100\"/></visualElement></visualElements><wires><wire><p1 x=\"100\" y=\"100\"/><p2 x=\"110\" y=\"100\"/></wire></wires></circuit>";
    fs::write(dir.0.join("floating-geometry.dig"), floating).unwrap();
    let invalid = cli(&dir, &["validate", "floating-geometry.dig"]);
    assert_eq!(invalid.status.code(), Some(1));
    let diagnostic = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        diagnostic.contains("Receiver net has no driver"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("Output"),
        "diagnostic should identify the receiver component: {diagnostic}"
    );
    assert!(
        diagnostic.contains("segments #0"),
        "diagnostic should locate the geometric wire: {diagnostic}"
    );
    assert_eq!(
        fs::read(dir.0.join("floating-geometry.dig")).unwrap(),
        floating
    );
}

/// Self-authored Boolean oracle for adder circuits: Digital source d118dce079b6b8867024d40f9b14dfa667e52106,
/// `core/basic/And.java` and `XOr.java` define the primitive operations used here. No fixture is copied.
#[test]
fn half_and_full_adder_formulas_match_exhaustive_binary_addition_tables() {
    for a in [false, true] {
        for b in [false, true] {
            let sum = a ^ b;
            let carry = a & b;
            let expected = u8::from(a) + u8::from(b);
            assert_eq!(u8::from(sum) | (u8::from(carry) << 1), expected);
            for cin in [false, true] {
                let propagate = a ^ b;
                let full_sum = propagate ^ cin;
                let full_carry = (a & b) | (propagate & cin);
                let expected = u8::from(a) + u8::from(b) + u8::from(cin);
                assert_eq!(u8::from(full_sum) | (u8::from(full_carry) << 1), expected);
            }
        }
    }
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
fn validate_keeps_driver_errors_but_accepts_feedback_without_claiming_convergence() {
    use digital_rust::{
        domain::{ComponentKind as K, PinDirection as D, PinId},
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
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("disconnected"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut multiple = formats::to_domain(&formats::parse_v2(&base).unwrap()).unwrap();
    set_component_at(&mut multiple, 4, K::Input, (50, 0), (0, 0), "second driver");
    let mut segments = multiple.wires.geometric().unwrap().to_vec();
    segments.push(digital_rust::geometric_wiring::Segment {
        id: segments.iter().map(|segment| segment.id).max().unwrap_or(0) + 1,
        a: digital_rust::geometric_wiring::Point::new(0, 0),
        b: digital_rust::geometric_wiring::Point::new(50, 0),
    });
    multiple.wires = digital_rust::domain::Wiring::GeometricSegments(segments);
    let bytes = formats::serialize_v2(&formats::from_domain(&multiple).unwrap()).unwrap();
    fs::write(dir.0.join("drivers.dig"), bytes).unwrap();
    let out = cli(&dir, &["validate", "drivers.dig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Several outputs"));

    let mut cycle = digital_rust::domain::Document::default();
    set_component_at(&mut cycle, 0, K::Not, (0, 0), (0, 0), "loop");
    let input = digital_rust::pin_geometry::pin_point(
        &cycle.components[0],
        PinId {
            direction: D::Input,
            index: 0,
        },
    )
    .unwrap();
    let output = digital_rust::pin_geometry::pin_point(
        &cycle.components[0],
        PinId {
            direction: D::Output,
            index: 0,
        },
    )
    .unwrap();
    cycle.wires = digital_rust::domain::Wiring::GeometricSegments(vec![
        digital_rust::geometric_wiring::Segment {
            id: 0,
            a: input,
            b: output,
        },
    ]);
    let bytes = formats::serialize_v2(&formats::from_domain(&cycle).unwrap()).unwrap();
    fs::write(dir.0.join("cycle.dig"), bytes).unwrap();
    let out = cli(&dir, &["validate", "cycle.dig"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("feedback convergence is not assessed"),
        "{stdout}"
    );
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

#[test]
fn startup_seed_cli_is_ascii_u64_deterministic_and_validated_before_file_access() {
    let dir = TempDir::new();
    let doc = gate_document(digital_rust::domain::ComponentKind::Not);
    fs::write(dir.0.join("seed.dig"), &doc).unwrap();
    let default = cli(&dir, &["simulate", "seed.dig", "--input", "0=1"]);
    let zero = cli(
        &dir,
        &["simulate", "seed.dig", "--seed", "0", "--input", "0=1"],
    );
    assert!(
        default.status.success(),
        "{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        zero.status.success(),
        "{}",
        String::from_utf8_lossy(&zero.stderr)
    );
    assert_eq!(default.stdout, zero.stdout);
    let max = cli(
        &dir,
        &["simulate", "seed.dig", "--seed", "18446744073709551615"],
    );
    assert!(
        max.status.success(),
        "{}",
        String::from_utf8_lossy(&max.stderr)
    );
    for args in [
        vec!["simulate", "absent.dig", "--seed", ""],
        vec!["simulate", "absent.dig", "--seed", "-1"],
        vec!["simulate", "absent.dig", "--seed", "+1"],
        vec!["simulate", "absent.dig", "--seed", "18446744073709551616"],
        vec!["simulate", "absent.dig", "--seed"],
        vec!["simulate", "absent.dig", "--seed", "1", "--seed", "2"],
    ] {
        let output = cli(&dir, &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("seed"));
    }
    assert_eq!(fs::read(dir.0.join("seed.dig")).unwrap(), doc);
}

#[test]
fn feedback_startup_exhaustion_is_an_error_with_replay_seed_and_no_rows() {
    use digital_rust::domain::{
        Component, ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position,
    };
    let dir = TempDir::new();
    let id = ComponentId(0);
    let input = PinRef {
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
        label: "loop".into(),
        constant_value: true,
    }];
    let doc = Document {
        wires: routed_wires(&components, &[(output, input)]),
        components,
    };
    let dig = digital_rust::formats::from_domain(&doc).unwrap();
    let bytes = digital_rust::formats::serialize_v2(&dig).unwrap();
    fs::write(dir.0.join("ring.dig"), &bytes).unwrap();
    let output = cli(&dir, &["simulate", "ring.dig", "--seed", "77"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("startup seed 77"), "{message}");
    assert!(message.contains("StartupSettling"), "{message}");
    assert_eq!(fs::read(dir.0.join("ring.dig")).unwrap(), bytes);
}

#[test]
fn bounded_trace_command_runs_ops_reports_ring_eviction_and_never_changes_file() {
    let dir = TempDir::new();
    let path = dir.0.join("trace.dig");
    let bytes = fixture("");
    fs::write(&path, &bytes).unwrap();
    let result = cli(
        &dir,
        &[
            "trace",
            "trace.dig",
            "--seed",
            "5",
            "--budget",
            "20",
            "--trace-capacity",
            "1",
            "--op",
            "full",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(output.contains("op 1=full pending=0"), "{output}");
    assert!(
        output.contains("capacity=1 retained=1 dropped="),
        "{output}"
    );
    assert!(output.contains("event=Stop"), "{output}");
    assert_eq!(fs::read(path).unwrap(), bytes);
    let invalid = cli(&dir, &["trace", "trace.dig", "--trace-capacity", "0"]);
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
fn trace_terminal_failure_reports_diagnostic_rows_without_settled_success() {
    use digital_rust::domain::{
        Component, ComponentId, ComponentKind as K, Document, PinDirection as D, PinId, PinRef,
        Position,
    };
    let dir = TempDir::new();
    let pin = |id, direction| PinRef {
        component: ComponentId(id),
        pin: PinId {
            direction,
            index: 0,
        },
    };
    let components = vec![
        Component {
            id: ComponentId(1),
            kind: K::Not,
            position: Position { x: 0., y: 0. },
            label: String::new(),
            constant_value: true,
        },
        Component {
            id: ComponentId(2),
            kind: K::Output,
            position: Position { x: 100., y: 0. },
            label: String::new(),
            constant_value: true,
        },
    ];
    let document = Document {
        wires: routed_wires(
            &components,
            &[
                (pin(1, D::Output), pin(1, D::Input)),
                (pin(1, D::Output), pin(2, D::Input)),
            ],
        ),
        components,
    };
    let bytes = digital_rust::formats::serialize_v2(
        &digital_rust::formats::from_domain(&document).unwrap(),
    )
    .unwrap();
    fs::write(dir.0.join("ring.dig"), &bytes).unwrap();
    let output = cli(
        &dir,
        &[
            "trace",
            "ring.dig",
            "--oscillation-limit",
            "1",
            "--budget",
            "80000",
            "--op",
            "full",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("trace status=diagnostic"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("event=Terminal"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("oscillat"));
    assert_eq!(fs::read(dir.0.join("ring.dig")).unwrap(), bytes);
}

// CLI geometry fixtures are serialized from explicit v2 endpoint coordinates, not routed_wires.
type EndpointPair = ((i32, i32), (i32, i32));
fn endpoint_fixture(
    elements: &[(digital_rust::domain::ComponentKind, (i32, i32), &str)],
    wires: &[EndpointPair],
) -> Vec<u8> {
    use digital_rust::formats::{DigElement, DigPoint, DigV2, DigWire};
    let elements = elements
        .iter()
        .map(|(kind, (x, y), label)| DigElement {
            name: kind.digital_name().into(),
            x: *x,
            y: *y,
            label: Some((*label).into()),
            value: None,
        })
        .collect();
    let wires = wires
        .iter()
        .map(|(a, b)| DigWire {
            p1: DigPoint { x: a.0, y: a.1 },
            p2: DigPoint { x: b.0, y: b.1 },
        })
        .collect();
    digital_rust::formats::serialize_v2(&DigV2 { elements, wires }).unwrap()
}

#[test]
fn subprocess_geometric_fanout_crossings_junctions_and_pin_interior_contract() {
    use digital_rust::domain::ComponentKind as K;
    let dir = TempDir::new();
    // A degree-four endpoint junction distributes one source to three receivers.
    let fanout = endpoint_fixture(
        &[
            (K::Input, (0, 0), "source"),
            (K::Output, (100, 0), "east"),
            (K::Output, (50, 50), "south"),
            (K::Output, (50, -50), "north"),
        ],
        &[
            ((0, 0), (50, 0)),
            ((50, 0), (100, 0)),
            ((50, 0), (50, 50)),
            ((50, 0), (50, -50)),
        ],
    );
    fs::write(dir.0.join("fanout.dig"), &fanout).unwrap();
    let opened = cli(&dir, &["open", "fanout.dig"]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let valid = cli(&dir, &["validate", "fanout.dig"]);
    assert!(
        valid.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&valid.stderr),
        String::from_utf8_lossy(&opened.stdout)
    );
    for level in [0, 1] {
        let simulated = cli(
            &dir,
            &["simulate", "fanout.dig", "--input", &format!("0={level}")],
        );
        assert!(
            simulated.status.success(),
            "{}",
            String::from_utf8_lossy(&simulated.stderr)
        );
        let rows = String::from_utf8(simulated.stdout).unwrap();
        for id in [1, 2, 3] {
            assert!(
                rows.lines().any(|line| {
                    line.contains(&format!("output id={id} label="))
                        && line.ends_with(&format!("value={level}"))
                }),
                "expected output {id}={level} in {rows}"
            );
        }
    }
    assert_eq!(fs::read(dir.0.join("fanout.dig")).unwrap(), fanout);

    // Two diagonal nets cross geometrically but have no common endpoint: input assignments
    // reach their corresponding outputs independently.
    let crossing = endpoint_fixture(
        &[
            (K::Input, (0, 0), "a"),
            (K::Input, (0, 100), "b"),
            (K::Output, (100, 100), "oa"),
            (K::Output, (100, 0), "ob"),
        ],
        &[((0, 0), (100, 100)), ((0, 100), (100, 0))],
    );
    fs::write(dir.0.join("crossing.dig"), &crossing).unwrap();
    let valid = cli(&dir, &["validate", "crossing.dig"]);
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let run = cli(
        &dir,
        &[
            "simulate",
            "crossing.dig",
            "--input",
            "0=0",
            "--input",
            "1=1",
        ],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let rows = String::from_utf8(run.stdout).unwrap();
    assert!(rows.contains("output id=2 label=\"oa\" value=0"), "{rows}");
    assert!(rows.contains("output id=3 label=\"ob\" value=1"), "{rows}");
    assert_eq!(fs::read(dir.0.join("crossing.dig")).unwrap(), crossing);

    // Splitting BOTH crossing lines at their intersection explicitly creates a shared
    // multi-driver net. It must fail validation with location and never print success rows.
    let junction = endpoint_fixture(
        &[
            (K::Input, (0, 0), "a"),
            (K::Input, (0, 100), "b"),
            (K::Output, (100, 100), "oa"),
            (K::Output, (100, 0), "ob"),
        ],
        &[
            ((0, 0), (50, 50)),
            ((50, 50), (100, 100)),
            ((0, 100), (50, 50)),
            ((50, 50), (100, 0)),
        ],
    );
    fs::write(dir.0.join("junction.dig"), &junction).unwrap();
    let failed = cli(&dir, &["validate", "junction.dig"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&failed.stderr);
    assert!(
        diagnostic.contains("Several outputs") && diagnostic.contains("segments #"),
        "{diagnostic}"
    );
    let failed = cli(&dir, &["simulate", "junction.dig"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    assert_eq!(fs::read(dir.0.join("junction.dig")).unwrap(), junction);

    // Raw T contact and receiver-pin-on-interior are not implicit connections. Splitting
    // the stored segment at that exact pin coordinate makes it an explicit endpoint.
    let raw_t = endpoint_fixture(
        &[
            (K::Input, (0, 0), "driver"),
            (K::Output, (50, 0), "receiver"),
        ],
        &[((0, 0), (100, 0))],
    );
    fs::write(dir.0.join("raw-t.dig"), &raw_t).unwrap();
    let opened = cli(&dir, &["open", "raw-t.dig"]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let invalid = cli(&dir, &["validate", "raw-t.dig"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        diagnostic.contains("disconnected") || diagnostic.contains("no driver"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("Output")
            && diagnostic.contains("#1")
            && diagnostic.contains("input pin"),
        "{diagnostic}"
    );
    let failed = cli(&dir, &["simulate", "raw-t.dig"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("input pin"));
    let disconnected = endpoint_fixture(&[(K::Output, (300, 0), "isolated")], &[]);
    fs::write(dir.0.join("disconnected.dig"), &disconnected).unwrap();
    assert!(cli(&dir, &["open", "disconnected.dig"]).status.success());
    for command in ["validate", "simulate"] {
        let failed = cli(&dir, &[command, "disconnected.dig"]);
        assert_eq!(failed.status.code(), Some(1));
        assert!(failed.stdout.is_empty());
        let diagnostic = String::from_utf8_lossy(&failed.stderr);
        assert!(
            diagnostic.contains("Output")
                && diagnostic.contains("#0")
                && diagnostic.contains("input pin"),
            "{diagnostic}"
        );
    }
    assert_eq!(
        fs::read(dir.0.join("disconnected.dig")).unwrap(),
        disconnected
    );
    let split = endpoint_fixture(
        &[
            (K::Input, (0, 0), "driver"),
            (K::Output, (50, 0), "receiver"),
        ],
        &[((0, 0), (50, 0)), ((50, 0), (100, 0))],
    );
    fs::write(dir.0.join("split-pin.dig"), &split).unwrap();
    let run = cli(&dir, &["simulate", "split-pin.dig", "--input", "0=1"]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("output id=1 label=\"receiver\" value=1")
    );
    assert_eq!(fs::read(dir.0.join("raw-t.dig")).unwrap(), raw_t);
    assert_eq!(fs::read(dir.0.join("split-pin.dig")).unwrap(), split);
}

fn cli_adder_fixture(full: bool) -> Vec<u8> {
    use digital_rust::domain::ComponentKind as K;
    let mut parts: Vec<(K, (i32, i32), &str)> =
        vec![(K::Input, (-100, 0), "a"), (K::Input, (-100, 40), "b")];
    if full {
        parts.push((K::Input, (-100, 80), "cin"));
    }
    let (xor1, and1) = if full { (3, 4) } else { (2, 3) };
    parts.push((K::XOr, (0, 0), "xor1"));
    parts.push((K::And, (0, 100), "and1"));
    if full {
        parts.extend([
            (K::XOr, (200, 40), "xor2"),
            (K::And, (200, 140), "and2"),
            (K::Or, (400, 100), "or"),
        ]);
    }
    let sum_id = parts.len();
    parts.push((K::Output, if full { (300, 60) } else { (300, 20) }, "sum"));
    let carry_id = parts.len();
    parts.push((
        K::Output,
        if full { (500, 120) } else { (300, 120) },
        "carry",
    ));
    let pin = |id: usize, input: Option<u8>| {
        let (kind, (x, y), _) = parts[id];
        let (px, py) = match (kind, input) {
            (K::Input, _) | (K::Output, _) => (0, 0),
            (K::And | K::XOr | K::Or, Some(1)) => (0, 40),
            (K::And | K::XOr | K::Or, Some(_)) => (0, 0),
            (K::And | K::XOr | K::Or, None) => (60, 20),
            _ => unreachable!(),
        };
        (x + px, y + py)
    };
    let mut connections = vec![
        (pin(0, None), pin(xor1, Some(0))),
        (pin(0, None), pin(and1, Some(0))),
        (pin(1, None), pin(xor1, Some(1))),
        (pin(1, None), pin(and1, Some(1))),
    ];
    if full {
        let xor2 = 5;
        let and2 = 6;
        let or = 7;
        let cin = 2;
        connections.extend([
            (pin(xor1, None), pin(xor2, Some(0))),
            (pin(xor1, None), pin(and2, Some(0))),
            (pin(cin, None), pin(xor2, Some(1))),
            (pin(cin, None), pin(and2, Some(1))),
            (pin(and1, None), pin(or, Some(0))),
            (pin(and2, None), pin(or, Some(1))),
            (pin(xor2, None), pin(sum_id, Some(0))),
            (pin(or, None), pin(carry_id, Some(0))),
        ]);
    } else {
        connections.extend([
            (pin(xor1, None), pin(sum_id, Some(0))),
            (pin(and1, None), pin(carry_id, Some(0))),
        ]);
    }
    let wires = connections;
    endpoint_fixture(&parts, &wires)
}

#[test]
fn subprocess_half_and_full_adder_truth_tables_open_validate_and_preserve_bytes() {
    for full in [false, true] {
        let dir = TempDir::new();
        let path = if full {
            "full-adder.dig"
        } else {
            "half-adder.dig"
        };
        let bytes = cli_adder_fixture(full);
        fs::write(dir.0.join(path), &bytes).unwrap();
        let opened = cli(&dir, &["open", path]);
        assert!(
            opened.status.success(),
            "{}",
            String::from_utf8_lossy(&opened.stderr)
        );
        let valid = cli(&dir, &["validate", path]);
        assert!(
            valid.status.success(),
            "{}",
            String::from_utf8_lossy(&valid.stderr)
        );
        let input_count = if full { 3 } else { 2 };
        for row in 0..(1 << input_count) {
            let mut args = vec!["simulate".to_owned(), path.to_owned()];
            for input in 0..input_count {
                args.push("--input".into());
                args.push(format!("{input}={}", (row >> input) & 1));
            }
            let output = Command::new(env!("CARGO_BIN_EXE_digital-rust-cli"))
                .args(&args)
                .current_dir(&dir.0)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "row={row}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            let a = row & 1 != 0;
            let b = row & 2 != 0;
            let cin = full && row & 4 != 0;
            let sum = a ^ b ^ cin;
            let carry = (a && b) || (a && cin) || (b && cin);
            assert!(
                stdout.contains(&format!(
                    "output id={} label=\"sum\" value={}",
                    if full { 8 } else { 4 },
                    u8::from(sum)
                )),
                "{stdout}"
            );
            assert!(
                stdout.contains(&format!(
                    "output id={} label=\"carry\" value={}",
                    if full { 9 } else { 5 },
                    u8::from(carry)
                )),
                "{stdout}"
            );
        }
        assert_eq!(fs::read(dir.0.join(path)).unwrap(), bytes);
    }
}
