mod common;
use common::routed_wires;
use digital_rust::{
    domain::{
        Component, ComponentId, ComponentKind as K, Document, PinDirection as D, PinId, PinRef,
        Position,
    },
    engine::Session,
    runtime::{Level, SettleOutcome},
};
fn pin(id: u64, direction: D) -> PinRef {
    PinRef {
        component: ComponentId(id),
        pin: PinId {
            direction,
            index: 0,
        },
    }
}
fn doc() -> Document {
    let components = [(1, K::Input), (2, K::Not), (3, K::Not), (4, K::Output)]
        .into_iter()
        .map(|(id, kind)| Component {
            id: ComponentId(id),
            kind,
            position: Position {
                x: id as f32 * 100.,
                y: 0.,
            },
            label: String::new(),
            constant_value: true,
            wide_shape: false,
            rotation: 0,
            mirror: false,
        })
        .collect::<Vec<_>>();
    let pairs = [
        (1, 2, D::Output, D::Input),
        (2, 3, D::Output, D::Input),
        (3, 4, D::Output, D::Input),
    ]
    .map(|(a, b, ad, bd)| (pin(a, ad), pin(b, bd)));
    Document {
        wires: routed_wires(&components, &pairs),
        components,
    }
}
#[test]
fn microstep_stops_at_successor_frontier_and_full_settle_reaches_output() {
    let mut session = Session::default();
    let a = session.place(K::Input, Position { x: 0., y: 0. }).unwrap();
    let b = session.place(K::Not, Position { x: 1., y: 0. }).unwrap();
    let c = session.place(K::Not, Position { x: 2., y: 0. }).unwrap();
    let out = session.place(K::Output, Position { x: 3., y: 0. }).unwrap();
    session
        .connect([pin(a.0, D::Output), pin(b.0, D::Input)])
        .unwrap();
    session
        .connect([pin(b.0, D::Output), pin(c.0, D::Input)])
        .unwrap();
    session
        .connect([pin(c.0, D::Output), pin(out.0, D::Input)])
        .unwrap();
    session.start_simulation().unwrap();
    assert!(matches!(
        session.settle_simulation(100),
        Ok(SettleOutcome::Settled { .. })
    ));
    session.set_input(a, true).unwrap();
    assert!(matches!(
        session.micro_step_simulation(),
        Ok(digital_rust::runtime::StepOutcome::Stepped { evaluated: 1 })
    ));
    assert_eq!(
        session.inspect().unwrap().output_value(out),
        Some(Level::Low)
    );
    assert_eq!(
        session
            .inspect()
            .unwrap()
            .pending_inspection()
            .next_scheduled,
        vec![c]
    );
    assert!(matches!(
        session.micro_step_simulation(),
        Ok(digital_rust::runtime::StepOutcome::Stepped { evaluated: 1 })
    ));
    assert_eq!(
        session.inspect().unwrap().output_value(out),
        Some(Level::High)
    );
    session.stop_simulation().unwrap();
    let d = doc();
    let mut micro = digital_rust::runtime::Runtime::start(&d).unwrap();
    assert!(matches!(micro.settle(100), SettleOutcome::Settled { .. }));
    micro.set_input(ComponentId(1), true).unwrap();
    assert_eq!(
        micro.micro_step(),
        digital_rust::runtime::StepOutcome::Stepped { evaluated: 1 }
    );
    assert_eq!(micro.output_value(ComponentId(4)), Some(Level::Low));
    assert_eq!(
        micro.pending_inspection().next_scheduled,
        vec![ComponentId(3)]
    );
    assert_eq!(
        micro.micro_step(),
        digital_rust::runtime::StepOutcome::Stepped { evaluated: 1 }
    );
    assert_eq!(micro.output_value(ComponentId(4)), Some(Level::High));
    let mut full = digital_rust::runtime::Runtime::start(&d).unwrap();
    assert!(matches!(full.settle(100), SettleOutcome::Settled { .. }));
    full.set_input(ComponentId(1), true).unwrap();
    assert!(matches!(full.settle(100), SettleOutcome::Settled { .. }));
    assert_eq!(full.output_value(ComponentId(4)), Some(Level::High));
}
#[test]
fn trace_is_partition_invariant_capture_observational_and_retained_after_stop() {
    let d = doc();
    let mut whole = digital_rust::runtime::Runtime::start_with_trace(
        &d,
        19,
        digital_rust::trace::TraceLog::new(1024, 19, 3).unwrap(),
    )
    .unwrap();
    let mut chunked = digital_rust::runtime::Runtime::start_with_trace(
        &d,
        19,
        digital_rust::trace::TraceLog::new(1024, 19, 3).unwrap(),
    )
    .unwrap();
    let whole_outcome = whole.settle(10_000);
    assert!(matches!(whole_outcome, SettleOutcome::Settled { .. }));
    loop {
        match chunked.settle(1) {
            SettleOutcome::Settled { .. } => break,
            SettleOutcome::BudgetExhausted { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    let a = whole.trace().unwrap().records().collect::<Vec<_>>();
    let b = chunked.trace().unwrap().records().collect::<Vec<_>>();
    assert_eq!(a, b);
    assert!(a.iter().any(|e| matches!(
        e.event,
        digital_rust::trace::TraceEvent::Lifecycle {
            phase: digital_rust::runtime::StartupPhase::ResetRelease
        }
    )));
    assert!(
        a.iter()
            .any(|e| matches!(e.event, digital_rust::trace::TraceEvent::Stable))
    );
    let mut plain = digital_rust::runtime::Runtime::start_with_seed(&d, 19).unwrap();
    assert_eq!(plain.settle(10_000), whole_outcome);
    assert_eq!(
        plain.output_value(ComponentId(4)),
        whole.output_value(ComponentId(4))
    );
    whole.set_input(ComponentId(1), true).unwrap();
    plain.set_input(ComponentId(1), true).unwrap();
    assert!(matches!(whole.settle(100), SettleOutcome::Settled { .. }));
    assert!(matches!(plain.settle(100), SettleOutcome::Settled { .. }));
    assert_eq!(
        plain.output_value(ComponentId(4)),
        whole.output_value(ComponentId(4))
    );
    assert!(whole.trace().unwrap().records().any(|r| r.event
        == digital_rust::trace::TraceEvent::InputChanged {
            component: ComponentId(1),
            value: digital_rust::signal::SignalValue::known(
                digital_rust::signal::SignalWidth::ONE,
                1,
            )
        }));
    assert!(whole.trace().unwrap().records().any(|r| matches!(
        r.event,
        digital_rust::trace::TraceEvent::GateEvaluated {
            immediate: false,
            ..
        }
    )));
    for record in whole.trace().unwrap().records() {
        match record.event {
            digital_rust::trace::TraceEvent::GateEvaluated {
                value: Some(value), ..
            }
            | digital_rust::trace::TraceEvent::InputChanged { value, .. } => {
                assert_eq!(value.width().get(), 1);
                assert_eq!(value.high_z(), 0);
            }
            digital_rust::trace::TraceEvent::NetPublished { old, new, .. } => {
                for value in [old, new].into_iter().flatten() {
                    assert_eq!(value.width().get(), 1);
                    assert_eq!(value.high_z(), 0);
                }
            }
            _ => {}
        }
    }

    let mut session = Session::default();
    let id = session.place(K::Input, Position { x: 0., y: 0. }).unwrap();
    session
        .start_simulation_with_trace(digital_rust::runtime::SimulationOptions::default(), 8)
        .unwrap();
    assert!(matches!(
        session.settle_simulation(100),
        Ok(SettleOutcome::Settled { .. })
    ));
    session.stop_simulation().unwrap();
    let saved = session.trace().unwrap();
    assert_eq!(
        saved.records().last().unwrap().event,
        digital_rust::trace::TraceEvent::Stop
    );
    let retained = saved.records().collect::<Vec<_>>();
    assert!(matches!(
        session.start_simulation_with_trace(digital_rust::runtime::SimulationOptions::default(), 0),
        Err(digital_rust::engine::SimulationError::TraceConfig(_))
    ));
    assert_eq!(
        session.trace().unwrap().records().collect::<Vec<_>>(),
        retained
    );
    session
        .start_simulation_with_trace(digital_rust::runtime::SimulationOptions::default(), 8)
        .unwrap();
    assert!(session.trace().unwrap().records().next().is_some());
    session.stop_simulation().unwrap();
    let _ = id;
}

#[test]
fn bounded_microstep_resumes_partial_frontier_without_publishing_or_crossing_successor() {
    let mut session = Session::default();
    let input = session.place(K::Input, Position { x: 0., y: 0. }).unwrap();
    let mut outputs = Vec::new();
    for index in 0..8 {
        let gate = session
            .place(
                K::Not,
                Position {
                    x: 1.,
                    y: index as f32,
                },
            )
            .unwrap();
        let output = session
            .place(
                K::Output,
                Position {
                    x: 2.,
                    y: index as f32,
                },
            )
            .unwrap();
        session
            .connect([pin(input.0, D::Output), pin(gate.0, D::Input)])
            .unwrap();
        session
            .connect([pin(gate.0, D::Output), pin(output.0, D::Input)])
            .unwrap();
        outputs.push(output);
    }
    session.start_simulation().unwrap();
    loop {
        if matches!(
            session.settle_simulation(100),
            Ok(SettleOutcome::Settled { .. })
        ) {
            break;
        }
    }
    session.set_input(input, true).unwrap();
    let mut frames = 0;
    loop {
        frames += 1;
        let result = session.micro_step_simulation_bounded(2).unwrap();
        if matches!(result, SettleOutcome::Settled { .. }) {
            break;
        }
        assert!(matches!(result, SettleOutcome::BudgetExhausted { .. }));
        assert!(
            outputs
                .iter()
                .all(|&id| session.inspect().unwrap().output_value(id) == Some(Level::High))
        );
        assert!(frames < 10);
    }
    assert!(frames > 1);
    assert!(
        outputs
            .iter()
            .all(|&id| session.inspect().unwrap().output_value(id) == Some(Level::Low))
    );
}

#[test]
fn stopping_a_partially_staged_bounded_microstep_unlocks_without_losing_document() {
    let mut session = Session::default();
    let input = session.place(K::Input, Position { x: 0., y: 0. }).unwrap();
    let mut gates = Vec::new();
    for index in 0..4 {
        let gate = session
            .place(
                K::Not,
                Position {
                    x: 1.,
                    y: index as f32,
                },
            )
            .unwrap();
        session
            .connect([pin(input.0, D::Output), pin(gate.0, D::Input)])
            .unwrap();
        gates.push(gate);
    }
    session.start_simulation().unwrap();
    assert!(matches!(
        session.settle_simulation(100),
        Ok(SettleOutcome::Settled { .. })
    ));
    session.set_input(input, true).unwrap();
    assert!(matches!(
        session.micro_step_simulation_bounded(2),
        Ok(SettleOutcome::BudgetExhausted { .. })
    ));
    let inspection = session.inspect().unwrap().pending_inspection();
    assert!(!inspection.staged_unpublished.is_empty());
    let saved_document = session.document().clone();
    session.stop_simulation().unwrap();
    assert!(!session.simulation_active());
    assert_eq!(session.document(), &saved_document);
    assert!(session.place(K::Output, Position { x: 5., y: 0. }).is_ok());
    assert_eq!(gates.len(), 4);
}

#[test]
fn terminal_startup_keeps_trace_after_runtime_removal() {
    let mut session = Session::default();
    let gate = session.place(K::Not, Position { x: 0., y: 0. }).unwrap();
    session
        .connect([pin(gate.0, D::Output), pin(gate.0, D::Input)])
        .unwrap();
    session
        .start_simulation_with_trace(
            digital_rust::runtime::SimulationOptions {
                startup_seed: 8,
                oscillation_limit: 1,
            },
            2048,
        )
        .unwrap();
    let failure = session.settle_simulation(80_000).unwrap_err();
    assert!(matches!(
        failure,
        digital_rust::engine::SimulationError::Oscillation { .. }
    ));
    assert!(!session.simulation_active());
    let trace = session.trace().unwrap();
    assert!(
        trace
            .records()
            .any(|r| r.event == digital_rust::trace::TraceEvent::Terminal)
    );
}

#[test]
fn pending_inspection_is_pure_sorted_and_distinguishes_partial_batch() {
    let mut runtime = digital_rust::runtime::Runtime::start(&doc()).unwrap();
    let before = runtime.pending_inspection();
    assert_eq!(before.unique_count, 2);
    let _ = runtime.pin_value(pin(2, D::Output));
    assert_eq!(runtime.pending_inspection(), before);
    assert!(matches!(
        runtime.settle(1),
        SettleOutcome::BudgetExhausted { .. }
    ));
    assert_eq!(runtime.pending_inspection(), before);

    let mut components = Vec::new();
    for (id, kind) in [
        (1, K::Input),
        (2, K::Not),
        (3, K::Not),
        (4, K::Output),
        (5, K::Output),
    ] {
        components.push(Component {
            id: ComponentId(id),
            kind,
            position: Position {
                x: id as f32,
                y: 0.,
            },
            label: String::new(),
            constant_value: true,
            wide_shape: false,
            rotation: 0,
            mirror: false,
        });
    }
    let wires = [
        (1, 2, D::Output, D::Input),
        (1, 3, D::Output, D::Input),
        (2, 4, D::Output, D::Input),
        (3, 5, D::Output, D::Input),
    ];
    let pairs = wires
        .into_iter()
        .map(|(a, b, ad, bd)| (pin(a, ad), pin(b, bd)))
        .collect::<Vec<_>>();
    let parallel = Document {
        wires: routed_wires(&components, &pairs),
        components,
    };
    let mut runtime = digital_rust::runtime::Runtime::start(&parallel).unwrap();
    assert!(matches!(runtime.settle(100), SettleOutcome::Settled { .. }));
    runtime.set_input(ComponentId(1), true).unwrap();
    assert!(matches!(
        runtime.settle(2),
        SettleOutcome::BudgetExhausted { work: 2 }
    ));
    let pending = runtime.pending_inspection();
    assert_eq!(pending.staged_unpublished.len(), 1);
    assert_eq!(pending.awaiting_evaluation.len(), 1);
    assert_eq!(pending.unique_count, 2);
}
