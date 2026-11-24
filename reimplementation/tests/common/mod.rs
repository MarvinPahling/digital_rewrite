use digital_rust::domain::{Component, PinRef, Wiring};

pub fn routed_wires(components: &[Component], pairs: &[(PinRef, PinRef)]) -> Wiring {
    let segments = pairs
        .iter()
        .enumerate()
        .map(|(id, &(a, b))| {
            let point = |pin: PinRef| {
                let component = components
                    .iter()
                    .find(|c| c.id == pin.component)
                    .expect("fixture component exists");
                digital_rust::pin_geometry::pin_point(component, pin.pin)
                    .expect("fixture pin geometry exists")
            };
            digital_rust::geometric_wiring::Segment {
                id: id as u64 + 1,
                a: point(a),
                b: point(b),
            }
        })
        .collect();
    Wiring::GeometricSegments(segments)
}
