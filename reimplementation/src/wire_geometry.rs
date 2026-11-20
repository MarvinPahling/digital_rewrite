//! Deterministic geometry for explicit pin-to-pin wires; crossings do not join wires.
#![allow(dead_code)] // The UI adapter will consume this seam.
use crate::{domain::PinRef, symbols::Point};

/// Canonicalized path, including a single point for coincident endpoints.
pub fn path(a: (PinRef, Point), b: (PinRef, Point)) -> Vec<Point> {
    let (a, b) = if a.0 <= b.0 { (a.1, b.1) } else { (b.1, a.1) };
    let corner = [b[0], a[1]];
    let mut points = vec![a];
    if corner != a {
        points.push(corner);
    }
    if b != corner {
        points.push(b);
    }
    points
}

/// Tests distance to the path, including its marker when it has no segments.
pub fn hit(points: &[Point], point: Point, tolerance: f32) -> bool {
    if tolerance < 0.0 || points.is_empty() {
        return false;
    }
    let distance_sq = |a: Point, b: Point| {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let length = dx * dx + dy * dy;
        let t = if length == 0.0 {
            0.0
        } else {
            (((point[0] - a[0]) * dx + (point[1] - a[1]) * dy) / length).clamp(0.0, 1.0)
        };
        (point[0] - a[0] - t * dx).powi(2) + (point[1] - a[1] - t * dy).powi(2)
    };
    if points.len() == 1 {
        return distance_sq(points[0], points[0]) <= tolerance * tolerance;
    }
    points
        .windows(2)
        .any(|p| distance_sq(p[0], p[1]) <= tolerance * tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ComponentId, PinDirection, PinId};
    fn pin(id: u64) -> PinRef {
        PinRef {
            component: ComponentId(id),
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        }
    }
    #[test]
    fn canonical_paths_and_hits() {
        let a = (pin(1), [1., 2.]);
        let b = (pin(2), [4., 6.]);
        assert_eq!(path(a, b), vec![[1., 2.], [4., 2.], [4., 6.]]);
        assert_eq!(path(a, b), path(b, a));
        assert_eq!(path(a, (pin(2), [1., 6.])), vec![[1., 2.], [1., 6.]]);
        assert_eq!(path(a, (pin(2), [4., 2.])), vec![[1., 2.], [4., 2.]]);
        let point = path(a, (pin(2), a.1));
        assert_eq!(point, vec![a.1]);
        assert!(hit(&point, [2., 2.], 1.));
        assert!(!hit(&point, [2.1, 2.], 1.));
        assert!(hit(&path(a, b), [3., 3.], 1.));
    }
}

#[cfg(test)]
mod anchor_tests {
    use crate::{
        domain::{ComponentKind, PinDirection, PinId},
        symbol_render::SymbolLayout,
        symbols::SymbolStyle,
    };
    #[test]
    fn typed_pins_match_all_symbol_anchors() {
        for style in [SymbolStyle::Rect, SymbolStyle::Ieee] {
            for kind in [
                ComponentKind::Input,
                ComponentKind::Output,
                ComponentKind::And,
                ComponentKind::Or,
                ComponentKind::Not,
            ] {
                for center in [[0., 0.], [13.5, -27.25]] {
                    let layout = SymbolLayout::centered_on(style, kind, center).unwrap();
                    let mut count = 0;
                    for direction in [PinDirection::Input, PinDirection::Output] {
                        for index in 0..3 {
                            let pin = PinId { direction, index };
                            assert_eq!(layout.pin_position(pin).is_some(), kind.has_pin(pin));
                            count += usize::from(kind.has_pin(pin));
                        }
                    }
                    assert_eq!(count, layout.anchors().count());
                }
            }
        }
    }
}
