//! Exact integer placement of Digital pins from editor artwork-center coordinates.
use crate::{
    domain::{Component, ComponentKind, PinId, PinRef},
    geometric_wiring::Point,
    symbols::{self, SymbolStyle},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinGeometryError {
    Fractional(ComponentKind),
    OutOfRange(ComponentKind),
    Unsupported(ComponentKind),
}

/// Convert a stored f32 coordinate without rounding. f32 -> f64 is exact; range is checked
/// before casting, so neither fractional positions nor saturating casts can alter topology.
fn integral(value: f32, kind: ComponentKind) -> Result<i32, PinGeometryError> {
    if !value.is_finite() {
        return Err(PinGeometryError::OutOfRange(kind));
    }
    let wide = f64::from(value);
    if wide.fract() != 0.0 {
        return Err(PinGeometryError::Fractional(kind));
    }
    if wide < i32::MIN as f64 || wide > i32::MAX as f64 {
        return Err(PinGeometryError::OutOfRange(kind));
    }
    Ok(wide as i32)
}

/// Resolve a pin using the integral bounds-center offset and Digital's origin-relative pin offsets.
pub fn pin_point(component: &Component, pin: PinId) -> Result<Point, PinGeometryError> {
    if !component.kind.has_pin(pin) {
        return Err(PinGeometryError::Unsupported(component.kind));
    }
    let cx = integral(component.position.x, component.kind)?;
    let cy = integral(component.position.y, component.kind)?;
    let center_offset = symbols::center_from_origin_offset(SymbolStyle::Rect, component.kind)
        .ok_or(PinGeometryError::Unsupported(component.kind))?;
    let ox = integral(center_offset[0], component.kind)?;
    let oy = integral(center_offset[1], component.kind)?;
    let (px, py) = match (component.kind, pin.direction, pin.index) {
        (ComponentKind::Input | ComponentKind::Output | ComponentKind::Const, _, 0) => (0, 0),
        (ComponentKind::Not, crate::domain::PinDirection::Input, 0) => (0, 0),
        (ComponentKind::Not, crate::domain::PinDirection::Output, 0) => (40, 0),
        (
            ComponentKind::And | ComponentKind::Or | ComponentKind::XOr,
            crate::domain::PinDirection::Input,
            0,
        ) => (0, 0),
        (
            ComponentKind::And | ComponentKind::Or | ComponentKind::XOr,
            crate::domain::PinDirection::Input,
            1,
        ) => (0, 40),
        (
            ComponentKind::And | ComponentKind::Or | ComponentKind::XOr,
            crate::domain::PinDirection::Output,
            0,
        ) => (60, 20),
        (
            ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr,
            crate::domain::PinDirection::Input,
            0,
        ) => (0, 0),
        (
            ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr,
            crate::domain::PinDirection::Input,
            1,
        ) => (0, 40),
        (
            ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr,
            crate::domain::PinDirection::Output,
            0,
        ) => (80, 20),
        _ => return Err(PinGeometryError::Unsupported(component.kind)),
    };
    let x = cx
        .checked_sub(ox)
        .and_then(|n| n.checked_add(px))
        .ok_or(PinGeometryError::OutOfRange(component.kind))?;
    let y = cy
        .checked_sub(oy)
        .and_then(|n| n.checked_add(py))
        .ok_or(PinGeometryError::OutOfRange(component.kind))?;
    Ok(Point::new(x, y))
}

pub fn all_pin_points(component: &Component) -> Result<Vec<(PinRef, Point)>, PinGeometryError> {
    let mut result = Vec::new();
    for direction in [
        crate::domain::PinDirection::Input,
        crate::domain::PinDirection::Output,
    ] {
        for index in 0..2 {
            let pin = PinId { direction, index };
            if component.kind.has_pin(pin) {
                result.push((
                    PinRef {
                        component: component.id,
                        pin,
                    },
                    pin_point(component, pin)?,
                ));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::PinDirection::{Input, Output};
    fn c(kind: ComponentKind, x: f32, y: f32) -> Component {
        Component {
            id: crate::domain::ComponentId(1),
            kind,
            position: crate::domain::Position { x, y },
            label: String::new(),
            constant_value: true,
        }
    }
    fn p(dir: crate::domain::PinDirection, index: u8) -> PinId {
        PinId {
            direction: dir,
            index,
        }
    }
    #[test]
    fn digital_pin_offsets_are_integral_and_cover_supported_kinds() {
        for kind in [
            ComponentKind::Input,
            ComponentKind::Output,
            ComponentKind::Const,
        ] {
            let expected_x = 100
                - symbols::center_from_origin_offset(SymbolStyle::Rect, kind).unwrap()[0] as i32;
            let expected_y = 200
                - symbols::center_from_origin_offset(SymbolStyle::Rect, kind).unwrap()[1] as i32;
            let direction = if kind == ComponentKind::Output {
                Input
            } else {
                Output
            };
            assert_eq!(
                pin_point(&c(kind, 100., 200.), p(direction, 0)).unwrap(),
                Point::new(expected_x, expected_y)
            );
        }
        for kind in [ComponentKind::And, ComponentKind::Or, ComponentKind::XOr] {
            let offset = symbols::center_from_origin_offset(SymbolStyle::Rect, kind).unwrap();
            let origin = Point::new(100 - offset[0] as i32, 200 - offset[1] as i32);
            assert_eq!(
                pin_point(&c(kind, 100., 200.), p(Input, 0)).unwrap(),
                origin
            );
            assert_eq!(
                pin_point(&c(kind, 100., 200.), p(Input, 1)).unwrap(),
                Point::new(origin.x, origin.y + 40)
            );
            assert_eq!(
                pin_point(&c(kind, 100., 200.), p(Output, 0)).unwrap(),
                Point::new(origin.x + 60, origin.y + 20)
            );
        }
        for kind in [ComponentKind::NAnd, ComponentKind::NOr, ComponentKind::XNOr] {
            let offset = symbols::center_from_origin_offset(SymbolStyle::Rect, kind).unwrap();
            assert_eq!(
                pin_point(&c(kind, 100., 200.), p(Output, 0)).unwrap(),
                Point::new(100 - offset[0] as i32 + 80, 200 - offset[1] as i32 + 20)
            );
        }
        assert_eq!(
            pin_point(&c(ComponentKind::Not, 100., 200.), p(Input, 0)).unwrap(),
            Point::new(80, 200)
        );
        assert_eq!(
            pin_point(&c(ComponentKind::Not, 100., 200.), p(Output, 0)).unwrap(),
            Point::new(120, 200)
        );
    }
    #[test]
    fn invalid_fractional_and_overflow_placements_fail_without_rounding() {
        assert_eq!(
            pin_point(&c(ComponentKind::Input, 0.5, 0.), p(Output, 0)),
            Err(PinGeometryError::Fractional(ComponentKind::Input))
        );
        assert!(matches!(
            pin_point(&c(ComponentKind::Input, f32::MAX, 0.), p(Output, 0)),
            Err(PinGeometryError::OutOfRange(_))
        ));
        assert!(matches!(
            pin_point(&c(ComponentKind::And, i32::MAX as f32, 0.), p(Output, 0)),
            Err(PinGeometryError::OutOfRange(_))
        ));
        // The conversion never routes integer coordinates back through f32.
        assert_eq!(
            pin_point(&c(ComponentKind::Input, 16_777_216., 0.), p(Output, 0))
                .unwrap()
                .x,
            16_777_234
        );
    }
}
