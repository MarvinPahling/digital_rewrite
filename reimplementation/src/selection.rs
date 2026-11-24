//! Transient mixed circuit selection and rectangle selection geometry.
use crate::{domain::ComponentId, geometric_wiring::Segment};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointMask(u8);
impl EndpointMask {
    pub const NONE: Self = Self(0);
    pub const A: Self = Self(1);
    pub const B: Self = Self(2);
    pub const BOTH: Self = Self(3);
    pub const fn contains_a(self) -> bool {
        self.0 & 1 != 0
    }
    pub const fn contains_b(self) -> bool {
        self.0 & 2 != 0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn toggled(self, other: Self) -> Self {
        Self(self.0 ^ other.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub components: BTreeSet<ComponentId>,
    /// Per-segment endpoint selection; A/B endpoints are independently movable.
    pub wires: std::collections::BTreeMap<u64, EndpointMask>,
}
impl Selection {
    pub fn clear(&mut self) {
        self.components.clear();
        self.wires.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.components.is_empty() && self.wires.is_empty()
    }
    pub fn select_component(&mut self, id: ComponentId, shift: bool, toggle: bool) {
        if !shift && !toggle {
            self.clear();
        }
        if toggle {
            if !self.components.remove(&id) {
                self.components.insert(id);
            }
        } else if shift {
            self.components.insert(id);
        } else {
            self.components.clear();
            self.components.insert(id);
        }
    }
    pub fn select_wire_endpoint(
        &mut self,
        id: u64,
        endpoint: EndpointMask,
        shift: bool,
        toggle: bool,
    ) {
        if !shift && !toggle {
            self.wires.clear();
        }
        let mask = self.wires.entry(id).or_default();
        *mask = if toggle {
            mask.toggled(endpoint)
        } else {
            mask.union(endpoint)
        };
        if mask.is_empty() {
            self.wires.remove(&id);
        }
    }
    pub fn apply_marquee(&mut self, found: Self, shift: bool, toggle: bool) {
        if !shift && !toggle {
            *self = found;
            return;
        }
        if toggle {
            for id in found.components {
                if !self.components.remove(&id) {
                    self.components.insert(id);
                }
            }
            for (id, mask) in found.wires {
                let old = self
                    .wires
                    .get(&id)
                    .copied()
                    .unwrap_or_default()
                    .toggled(mask);
                if old.is_empty() {
                    self.wires.remove(&id);
                } else {
                    self.wires.insert(id, old);
                }
            }
        } else {
            self.components.extend(found.components);
            for (id, mask) in found.wires {
                self.wires
                    .entry(id)
                    .and_modify(|m| *m = m.union(mask))
                    .or_insert(mask);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}
impl Rect {
    pub fn normalized(a: [f32; 2], b: [f32; 2]) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1])],
            max: [a[0].max(b[0]), a[1].max(b[1])],
        }
    }
    pub fn contains(self, p: [f32; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }
    pub fn contains_rect(self, min: [f32; 2], max: [f32; 2]) -> bool {
        min[0] >= self.min[0]
            && max[0] <= self.max[0]
            && min[1] >= self.min[1]
            && max[1] <= self.max[1]
    }
}
pub fn marquee(
    components: impl IntoIterator<Item = (ComponentId, [f32; 2], [f32; 2])>,
    wires: &[Segment],
    rect: Rect,
) -> Selection {
    let mut found = Selection::default();
    for (id, min, max) in components {
        if rect.contains_rect(min, max) {
            found.components.insert(id);
        }
    }
    for wire in wires {
        let mut mask = EndpointMask::NONE;
        if rect.contains([wire.a.x as f32, wire.a.y as f32]) {
            mask = mask.union(EndpointMask::A);
        }
        if rect.contains([wire.b.x as f32, wire.b.y as f32]) {
            mask = mask.union(EndpointMask::B);
        }
        if !mask.is_empty() {
            found.wires.insert(wire.id, mask);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modifiers_and_marquee_select_endpoint_masks_without_crossing_hits() {
        let a = ComponentId(1);
        let mut s = Selection::default();
        s.select_component(a, false, false);
        s.select_component(ComponentId(2), true, false);
        s.select_component(a, false, true);
        assert_eq!(s.components, BTreeSet::from([ComponentId(2)]));
        let wires = vec![
            Segment {
                id: 1,
                a: crate::geometric_wiring::Point::new(0, 0),
                b: crate::geometric_wiring::Point::new(20, 0),
            },
            Segment {
                id: 2,
                a: crate::geometric_wiring::Point::new(10, -10),
                b: crate::geometric_wiring::Point::new(10, 10),
            },
        ];
        let r = Rect::normalized([0., 0.], [10., 0.]);
        let found = marquee([], &wires, r);
        assert_eq!(found.wires.get(&1), Some(&EndpointMask::A));
        assert!(!found.wires.contains_key(&2));
        let whole = marquee(
            [
                (ComponentId(3), [0., 0.], [10., 10.]),
                (ComponentId(4), [9., 9.], [12., 12.]),
            ],
            &wires,
            Rect::normalized([0., 0.], [10., 10.]),
        );
        assert!(whole.components.contains(&ComponentId(3)));
        assert!(!whole.components.contains(&ComponentId(4)));
        let reverse = marquee([], &wires, Rect::normalized([10., 0.], [0., 0.]));
        assert_eq!(found, reverse);
        let mut mixed = Selection::default();
        mixed.select_component(a, false, false);
        mixed.select_wire_endpoint(1, EndpointMask::A, true, false);
        assert!(mixed.components.contains(&a));
        assert_eq!(mixed.wires.get(&1), Some(&EndpointMask::A));
        mixed.select_wire_endpoint(1, EndpointMask::A, false, true);
        assert!(mixed.components.contains(&a));
        assert!(!mixed.wires.contains_key(&1));
    }
}
