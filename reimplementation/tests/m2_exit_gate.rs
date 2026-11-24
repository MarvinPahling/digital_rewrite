mod common;
use common::routed_wires;
use digital_rust::{
    domain::{
        Component, ComponentId, ComponentKind as K, Document, PinDirection as D, PinId, PinRef,
        Position,
    },
    engine::Session,
    runtime::{Level, Runtime, SettleOutcome, SimulationOptions, StepOutcome},
    signal::SignalValue,
    trace::{TraceEvent, TraceLog},
};

fn pin(id: u64, direction: D, index: u8) -> PinRef {
    PinRef {
        component: ComponentId(id),
        pin: PinId { direction, index },
    }
}
fn component(id: u64, kind: K, value: bool) -> Component {
    Component {
        id: ComponentId(id),
        kind,
        position: Position {
            x: id as f32 * 300.0,
            y: (id % 7) as f32 * 180.0,
        },
        label: String::new(),
        constant_value: value,
        wide_shape: false,
        rotation: 0,
        mirror: false,
    }
}
fn circuit(parts: &[(u64, K, bool)], wires: &[(u64, D, u8, u64, D, u8)]) -> Document {
    let components = parts
        .iter()
        .map(|&(id, kind, value)| component(id, kind, value))
        .collect::<Vec<_>>();
    let pairs = wires
        .iter()
        .map(|&(a, ad, ai, b, bd, bi)| (pin(a, ad, ai), pin(b, bd, bi)))
        .collect::<Vec<_>>();
    Document {
        wires: routed_wires(&components, &pairs),
        components,
    }
}

fn settle(runtime: &mut Runtime, budget: usize) {
    for _ in 0..100_000 {
        match runtime.settle(budget) {
            SettleOutcome::Settled { .. } => return,
            SettleOutcome::BudgetExhausted { .. } => {}
            other => panic!("settling failed: {other:?}"),
        }
    }
    panic!("settle polling exceeded cap");
}
type CanonicalTraceEntry = (u8, u64, u8, u8, u8);

fn level_code(level: Option<SignalValue>) -> u8 {
    match level {
        None => 0,
        Some(value) => 1 + value.bits() as u8,
    }
}
fn gate_oracle(kind: K, a: bool, b: bool) -> bool {
    match kind {
        K::And => a && b,
        K::NAnd => !(a && b),
        K::Or => a || b,
        K::NOr => !(a || b),
        K::XOr => a != b,
        K::XNOr => a == b,
        _ => unreachable!(),
    }
}

#[test]
fn public_seeded_startup_exhaustive_gate_and_source_oracle() {
    let binary = [K::And, K::NAnd, K::Or, K::NOr, K::XOr, K::XNOr];
    let mut rows = 0;
    for kind in binary {
        for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
            let d = circuit(
                &[
                    (1, K::Input, true),
                    (2, K::Input, true),
                    (3, kind, true),
                    (4, K::Output, true),
                ],
                &[
                    (1, D::Output, 0, 3, D::Input, 0),
                    (2, D::Output, 0, 3, D::Input, 1),
                    (3, D::Output, 0, 4, D::Input, 0),
                ],
            );
            let mut runtime = Runtime::start_with_seed(&d, 0x5eed + rows).unwrap();
            settle(&mut runtime, 2);
            runtime.set_input(ComponentId(1), a).unwrap();
            settle(&mut runtime, 1);
            runtime.set_input(ComponentId(2), b).unwrap();
            settle(&mut runtime, 1);
            assert_eq!(
                runtime.output_value(ComponentId(4)),
                Some(if gate_oracle(kind, a, b) {
                    Level::High
                } else {
                    Level::Low
                }),
                "{kind:?} {a} {b}"
            );
            rows += 1;
        }
    }
    assert_eq!(rows, 24);

    // NOT, Input→Output, and configured constants each have an independent public startup oracle.
    for input in [false, true] {
        let not_doc = circuit(
            &[(1, K::Input, true), (2, K::Not, true), (3, K::Output, true)],
            &[
                (1, D::Output, 0, 2, D::Input, 0),
                (2, D::Output, 0, 3, D::Input, 0),
            ],
        );
        let mut runtime = Runtime::start_with_seed(&not_doc, 91).unwrap();
        settle(&mut runtime, 1);
        runtime.set_input(ComponentId(1), input).unwrap();
        settle(&mut runtime, 2);
        assert_eq!(
            runtime.output_value(ComponentId(3)),
            Some(if !input { Level::High } else { Level::Low })
        );

        let pass_doc = circuit(
            &[(1, K::Input, true), (2, K::Output, true)],
            &[(1, D::Output, 0, 2, D::Input, 0)],
        );
        let mut runtime = Runtime::start_with_seed(&pass_doc, 91).unwrap();
        settle(&mut runtime, 2);
        runtime.set_input(ComponentId(1), input).unwrap();
        assert_eq!(
            runtime.output_value(ComponentId(2)),
            Some(if input { Level::High } else { Level::Low })
        );

        let const_doc = circuit(
            &[(1, K::Const, input), (2, K::Output, true)],
            &[(1, D::Output, 0, 2, D::Input, 0)],
        );
        let mut runtime = Runtime::start_with_seed(&const_doc, 91).unwrap();
        settle(&mut runtime, 2);
        assert_eq!(
            runtime.output_value(ComponentId(2)),
            Some(if input { Level::High } else { Level::Low })
        );
    }
}

#[test]
fn fanout_same_value_noop_and_seeded_startup_work_partitions() {
    let d = circuit(
        &[
            (1, K::Input, true),
            (2, K::Not, true),
            (3, K::Output, true),
            (4, K::Output, true),
        ],
        &[
            (1, D::Output, 0, 2, D::Input, 0),
            (2, D::Output, 0, 3, D::Input, 0),
            (2, D::Output, 0, 4, D::Input, 0),
        ],
    );
    let mut snapshots = Vec::new();
    for budget in [1, 2, 4096] {
        let mut runtime = Runtime::start_with_seed(&d, 317).unwrap();
        assert_eq!(
            runtime.settle(0),
            SettleOutcome::BudgetExhausted { work: 0 }
        );
        settle(&mut runtime, budget);
        let before = runtime.output_value(ComponentId(3));
        assert_eq!(
            runtime.set_input(ComponentId(1), false).unwrap(),
            digital_rust::runtime::InputChange::Unchanged
        );
        assert_eq!(runtime.output_value(ComponentId(3)), before);
        runtime.set_input(ComponentId(1), true).unwrap();
        settle(&mut runtime, budget);
        assert_eq!(runtime.output_value(ComponentId(3)), Some(Level::Low));
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Level::Low));
        snapshots.push((
            runtime.output_value(ComponentId(3)),
            runtime.output_value(ComponentId(4)),
        ));
    }
    assert!(snapshots.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn public_startup_execution_apis_and_budgets_are_bounded_and_convergent() {
    let d = circuit(
        &[
            (1, K::Input, true),
            (2, K::Not, true),
            (3, K::Not, true),
            (4, K::Output, true),
        ],
        &[
            (1, D::Output, 0, 2, D::Input, 0),
            (2, D::Output, 0, 3, D::Input, 0),
            (3, D::Output, 0, 4, D::Input, 0),
        ],
    );
    let mut stepped = Runtime::start_with_seed(&d, 24).unwrap();
    for _ in 0..100 {
        if stepped.startup_phase() == digital_rust::runtime::StartupPhase::Running {
            break;
        }
        let outcome = stepped.step();
        assert!(matches!(
            outcome,
            StepOutcome::Stepped { .. } | StepOutcome::Idle
        ));
    }
    assert_eq!(
        stepped.startup_phase(),
        digital_rust::runtime::StartupPhase::Running
    );

    let mut micro = Runtime::start_with_seed(&d, 24).unwrap();
    for _ in 0..100 {
        if micro.startup_phase() == digital_rust::runtime::StartupPhase::Running {
            break;
        }
        assert!(matches!(
            micro.micro_step(),
            StepOutcome::Stepped { .. } | StepOutcome::Idle
        ));
    }
    assert_eq!(
        micro.startup_phase(),
        digital_rust::runtime::StartupPhase::Running
    );

    let mut bounded = Runtime::start_with_seed(&d, 24).unwrap();
    assert_eq!(
        bounded.micro_step_bounded(0),
        SettleOutcome::BudgetExhausted { work: 0 }
    );
    let mut polls = 0;
    loop {
        polls += 1;
        assert!(polls <= 100);
        match bounded.micro_step_bounded(2) {
            SettleOutcome::Settled { .. } | SettleOutcome::BudgetExhausted { .. } => {}
            other => panic!("bounded microstep failed: {other:?}"),
        }
        if bounded.startup_phase() == digital_rust::runtime::StartupPhase::Running {
            break;
        }
    }
    assert_eq!(
        bounded.startup_phase(),
        digital_rust::runtime::StartupPhase::Running
    );
    for budget in [1, 2, 4096] {
        let mut runtime = Runtime::start_with_seed(&d, 24).unwrap();
        settle(&mut runtime, budget);
        assert_eq!(runtime.output_value(ComponentId(4)), Some(Level::Low));
    }
}

#[test]
fn feedback_permitted_outcomes_and_normal_trace_ignore_net_index_order() {
    let latch = circuit(
        &[(1, K::Not, true), (2, K::Not, true)],
        &[
            (1, D::Output, 0, 2, D::Input, 0),
            (2, D::Output, 0, 1, D::Input, 0),
        ],
    );
    for seed in 0..24 {
        let mut runtime = Runtime::start_with_seed(&latch, seed).unwrap();
        settle(&mut runtime, 2);
        assert_ne!(
            runtime.pin_value(pin(1, D::Output, 0)),
            runtime.pin_value(pin(2, D::Output, 0)),
            "permitted latch state seed {seed}"
        );
    }
    let self_ring = circuit(&[(1, K::Not, true)], &[(1, D::Output, 0, 1, D::Input, 0)]);
    assert!(Runtime::start_with_seed(&self_ring, 1).is_ok());

    let reconvergent = circuit(
        &[
            (1, K::Input, true),
            (2, K::Not, true),
            (3, K::Not, true),
            (4, K::And, true),
            (5, K::Output, true),
        ],
        &[
            (1, D::Output, 0, 2, D::Input, 0),
            (1, D::Output, 0, 3, D::Input, 0),
            (2, D::Output, 0, 4, D::Input, 0),
            (3, D::Output, 0, 4, D::Input, 1),
            (4, D::Output, 0, 5, D::Input, 0),
        ],
    );
    let mut reordered = reconvergent.clone();
    reordered.components.reverse();
    reordered.wires.geometric_mut().unwrap().reverse();
    let traces = [reconvergent, reordered].map(|doc| {
        let mut runtime =
            Runtime::start_with_trace(&doc, 77, TraceLog::new(4096, 77, 1).unwrap()).unwrap();
        settle(&mut runtime, 1);
        runtime.set_input(ComponentId(1), true).unwrap();
        settle(&mut runtime, 2);
        let mut frontiers: Vec<Vec<CanonicalTraceEntry>> = Vec::new();
        for record in runtime.trace().unwrap().records() {
            match record.event {
                TraceEvent::FrontierBegin { startup, .. } => {
                    frontiers.push(vec![(0, u64::from(startup), 0, 0, 0)])
                }
                TraceEvent::GateEvaluated {
                    component,
                    immediate,
                    value,
                } => {
                    frontiers.last_mut().unwrap().push((
                        1,
                        component.0,
                        u8::from(immediate),
                        level_code(value),
                        0,
                    ));
                }
                TraceEvent::NetPublished {
                    driver, old, new, ..
                } => {
                    frontiers.last_mut().unwrap().push((
                        2,
                        driver.component.0,
                        driver.pin.index,
                        level_code(old),
                        level_code(new),
                    ));
                }
                TraceEvent::FrontierComplete { .. } => {
                    frontiers.last_mut().unwrap().sort_unstable()
                }
                _ => {}
            }
        }
        frontiers
    });
    assert_eq!(
        traces[0], traces[1],
        "compare component/pin identities and frontier sequence, never compiler net indices"
    );
}

#[test]
fn session_stop_during_startup_keeps_editable_document_history() {
    let mut session = Session::default();
    let id = session
        .place(K::Input, Position { x: 0.0, y: 0.0 })
        .unwrap();
    let before = session.document().clone();
    session
        .start_simulation_with_options(SimulationOptions {
            startup_seed: 9,
            oscillation_limit: 1000,
        })
        .unwrap();
    assert_eq!(
        session.settle_simulation(1).unwrap(),
        SettleOutcome::BudgetExhausted { work: 1 }
    );
    assert!(session.undo().is_err());
    session.stop_simulation().unwrap();
    assert_eq!(session.document(), &before);
    assert!(session.undo().is_ok());
    let next = session
        .place(K::Output, Position { x: 1.0, y: 0.0 })
        .unwrap();
    assert!(next.0 > id.0);
}

#[test]
fn public_geometric_connectivity_membership_diagnostics_and_resource_bounds() {
    use digital_rust::{
        domain::Wiring,
        geometric_wiring::{Point, Segment},
        simulation::{CompileError, MAX_COMPONENTS, MAX_WIRES, compile},
    };
    let at = |id, kind, x: i32, y: i32| {
        let offset = digital_rust::symbols::center_from_origin_offset(
            digital_rust::symbols::DEFAULT_STYLE,
            kind,
        )
        .unwrap();
        Component {
            id: ComponentId(id),
            kind,
            position: Position {
                x: (x + offset[0] as i32) as f32,
                y: (y + offset[1] as i32) as f32,
            },
            label: format!("c{id}"),
            constant_value: true,
            wide_shape: false,
            rotation: 0,
            mirror: false,
        }
    };
    let segment = |id, a: (i32, i32), b: (i32, i32)| Segment {
        id,
        a: Point::new(a.0, a.1),
        b: Point::new(b.0, b.1),
    };
    let by_origins = |parts: Vec<Component>, wires: Vec<Segment>| Document {
        components: parts,
        wires: Wiring::GeometricSegments(wires),
    };

    // Distinct endpoint-connected diagonal nets remain distinct at their proper crossing.
    let crossing = by_origins(
        vec![
            at(0, K::Input, 0, 0),
            at(1, K::Input, 0, 100),
            at(2, K::Output, 100, 100),
            at(3, K::Output, 100, 0),
        ],
        vec![
            segment(10, (0, 0), (100, 100)),
            segment(11, (0, 100), (100, 0)),
        ],
    );
    let compiled = compile(&crossing).unwrap();
    let net_for = |pin: PinRef| {
        compiled
            .nets
            .iter()
            .position(|net| net.driver == Some(pin) || net.receivers.contains(&pin))
            .unwrap()
    };
    assert_ne!(net_for(pin(0, D::Output, 0)), net_for(pin(1, D::Output, 0)));
    assert_eq!(net_for(pin(0, D::Output, 0)), net_for(pin(2, D::Input, 0)));
    assert_eq!(net_for(pin(1, D::Output, 0)), net_for(pin(3, D::Input, 0)));

    // A split four-way endpoint joins source and every receiver; a raw T and pin-on-interior do not.
    let junction = by_origins(
        vec![
            at(0, K::Input, 0, 0),
            at(1, K::Output, 100, 0),
            at(2, K::Output, 50, 50),
            at(3, K::Output, 50, -50),
        ],
        vec![
            segment(1, (0, 0), (50, 0)),
            segment(2, (50, 0), (100, 0)),
            segment(3, (50, 0), (50, 50)),
            segment(4, (50, 0), (50, -50)),
        ],
    );
    let compiled = compile(&junction).unwrap();
    let junction_net = compiled
        .nets
        .iter()
        .find(|net| net.driver == Some(pin(0, D::Output, 0)))
        .unwrap();
    assert_eq!(junction_net.receivers.len(), 3);
    assert_eq!(
        junction_net
            .receivers
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            pin(1, D::Input, 0),
            pin(2, D::Input, 0),
            pin(3, D::Input, 0)
        ]
        .into_iter()
        .collect()
    );
    let raw_t = by_origins(
        vec![at(0, K::Input, 0, 0), at(1, K::Output, 50, 0)],
        vec![segment(9, (0, 0), (100, 0))],
    );
    assert!(
        matches!(compile(&raw_t),Err(CompileError::DisconnectedReceiver{pin:p}) if p==pin(1,D::Input,0))
    );
    let split_pin = by_origins(
        vec![at(0, K::Input, 0, 0), at(1, K::Output, 50, 0)],
        vec![segment(9, (0, 0), (50, 0)), segment(10, (50, 0), (100, 0))],
    );
    assert!(compile(&split_pin).is_ok());

    let floating = by_origins(
        vec![at(2, K::Output, 100, 0)],
        vec![segment(31, (100, 0), (110, 0))],
    );
    assert!(
        matches!(compile(&floating),Err(CompileError::FloatingReceiverNet{pins,segment_ids}) if pins==vec![pin(2,D::Input,0)] && segment_ids==vec![31])
    );
    let multi = by_origins(
        vec![
            at(0, K::Input, 0, 0),
            at(1, K::Input, 0, 100),
            at(2, K::Output, 100, 50),
        ],
        vec![
            segment(40, (0, 0), (50, 50)),
            segment(41, (0, 100), (50, 50)),
            segment(42, (50, 50), (100, 50)),
        ],
    );
    assert!(
        matches!(compile(&multi),Err(CompileError::GeometricMultipleDrivers{pins,segment_ids}) if pins==vec![pin(0,D::Output,0),pin(1,D::Output,0),pin(2,D::Input,0)] && segment_ids==vec![40,41,42])
    );

    let too_many_components = Document {
        components: (0..=MAX_COMPONENTS as u64)
            .map(|id| at(id, K::Input, id as i32 * 10, 0))
            .collect(),
        wires: Wiring::GeometricSegments(Vec::new()),
    };
    assert_eq!(
        compile(&too_many_components),
        Err(CompileError::ComponentLimit)
    );
    let too_many_segments = Document {
        components: Vec::new(),
        wires: Wiring::GeometricSegments(
            (0..=MAX_WIRES as u64)
                .map(|id| segment(id, (id as i32 * 2, 0), (id as i32 * 2 + 1, 0)))
                .collect(),
        ),
    };
    assert_eq!(compile(&too_many_segments), Err(CompileError::WireLimit));
    let work_limited = Document {
        components: (0..10_000u64)
            .map(|id| at(id, K::And, id as i32 * 100, 0))
            .collect(),
        wires: Wiring::GeometricSegments(
            (0..15_001u64)
                .map(|id| segment(id, (id as i32 * 2, 0), (id as i32 * 2 + 1, 0)))
                .collect(),
        ),
    };
    assert_eq!(compile(&work_limited), Err(CompileError::WorkLimit));
    let invalid_segment = by_origins(vec![], vec![segment(777, (1, 1), (1, 1))]);
    assert_eq!(
        compile(&invalid_segment),
        Err(CompileError::ZeroLengthSegment { segment: 777 })
    );
}
