use digital_rust::{
    domain::{
        Component, ComponentId, ComponentKind as K, Document, PinDirection as D, PinId, PinRef,
        Position, Wire, WireId,
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
    Document {
        components: [(1, K::Input), (2, K::Not), (3, K::Not), (4, K::Output)]
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
            })
            .collect(),
        wires: [
            (1, 2, D::Output, D::Input),
            (2, 3, D::Output, D::Input),
            (3, 4, D::Output, D::Input),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (a, b, ad, bd))| Wire {
            id: WireId(i as u64),
            endpoints: [pin(a, ad), pin(b, bd)],
        })
        .collect(),
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
fn pending_inspection_is_pure_sorted_and_distinguishes_partial_batch() {
    let d = doc();
    let mut runtime = digital_rust::runtime::Runtime::start(&d).unwrap();
    let before = runtime.pending_inspection();
    assert_eq!(before.unique_count, 2);
    let _ = runtime.pin_value(pin(2, D::Output));
    assert_eq!(runtime.pending_inspection(), before);
    assert!(matches!(
        runtime.settle(1),
        SettleOutcome::BudgetExhausted { .. }
    ));
    let pending = runtime.pending_inspection();
    assert_eq!(pending.unique_count, 2);
    assert_eq!(
        pending.awaiting_evaluation.len() + pending.staged_unpublished.len(),
        2
    );
}
