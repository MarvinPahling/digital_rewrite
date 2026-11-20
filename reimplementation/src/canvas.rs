//! World-to-screen mapping of the editor canvas.
//!
//! World coordinates are the document's units (20 per grid unit); screen coordinates are egui
//! points. All canvas drawing and picking goes through [`CanvasTransform`] so that zoom and pan
//! can later change it without touching the callers.
use crate::symbols::Point;
use eframe::egui::{Pos2, Vec2};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasTransform {
    /// Screen position of the world origin.
    origin: Pos2,
    /// Screen points per world unit; strictly positive.
    zoom: f32,
}

impl CanvasTransform {
    /// Returns `None` unless `zoom` is a positive finite number and `origin` is finite.
    pub fn new(origin: Pos2, zoom: f32) -> Option<Self> {
        (zoom.is_finite() && zoom > 0.0 && origin.is_finite()).then_some(Self { origin, zoom })
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn to_screen(self, world: Point) -> Pos2 {
        self.origin + Vec2::new(world[0], world[1]) * self.zoom
    }

    pub fn to_world(self, screen: Pos2) -> Point {
        let d = (screen - self.origin) / self.zoom;
        [d.x, d.y]
    }

    /// Converts a screen-space distance into world units.
    pub fn length_to_world(&self, points: f32) -> f32 {
        points / self.zoom
    }

    /// Converts a screen-space displacement into a world displacement.
    pub fn delta_to_world(&self, delta: Vec2) -> Point {
        let d = delta / self.zoom;
        [d.x, d.y]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_and_screen_round_trip() {
        let t = CanvasTransform::new(Pos2::new(208., 8.), 2.5).unwrap();
        let screen = t.to_screen([10., 20.]);
        assert_eq!(screen, Pos2::new(233., 58.));
        assert_eq!(t.to_world(screen), [10., 20.]);
        assert_eq!(t.delta_to_world(Vec2::new(5., 10.)), [2., 4.]);
        assert_eq!(t.length_to_world(5.), 2.);
    }

    #[test]
    fn rejects_unusable_transforms() {
        for zoom in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(CanvasTransform::new(Pos2::ZERO, zoom).is_none());
        }
        assert!(CanvasTransform::new(Pos2::new(f32::NAN, 0.), 1.).is_none());
    }
}
