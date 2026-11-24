//! Self-authored test fixture helpers that encode connectivity as real geometric segments.
use crate::{
    domain::{Component, PinRef, Wiring},
    geometric_wiring::Segment,
};

pub(crate) fn routed_wires(components: &[Component], connections: &[(PinRef, PinRef)]) -> Wiring {
    let segments = connections
        .iter()
        .enumerate()
        .map(|(index, &(a, b))| {
            let component = |id| {
                components
                    .iter()
                    .find(|component| component.id == id)
                    .expect("fixture component exists")
            };
            let a = crate::pin_geometry::pin_point(component(a.component), a.pin)
                .expect("fixture pin geometry is valid");
            let b = crate::pin_geometry::pin_point(component(b.component), b.pin)
                .expect("fixture pin geometry is valid");
            Segment {
                id: index as u64 + 1,
                a,
                b,
            }
        })
        .collect();
    Wiring::GeometricSegments(segments)
}
