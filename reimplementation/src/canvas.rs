//! Bounded canvas camera and world-to-screen mapping, independent of the circuit session.
//!
//! Camera state is transient view state: it is never part of the document or engine history.
//! The camera is anchored at a world point and a canvas-relative screen point, which keeps fitting
//! large finite document coordinates stable without sending unbounded coordinates to egui.
use crate::symbols::Point;
use eframe::egui::{Pos2, Rect, Vec2};

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 16.0;
const MAX_PROJECTED: f64 = 1_000_000.0;
const MAX_VIEW_TRANSLATION: f64 = 1_000_000.0;
const MAX_WORLD_DELTA: f64 = 1_000_000.0;
const MAX_WORLD_COORD: f64 = f32::MAX as f64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasTransform {
    screen_anchor: Pos2,
    world_anchor: [f64; 2],
    zoom: f32,
}
impl CanvasTransform {
    pub fn new(origin: Pos2, zoom: f32) -> Option<Self> {
        (zoom.is_finite() && (MIN_ZOOM..=MAX_ZOOM).contains(&zoom) && origin.is_finite()).then_some(
            Self {
                screen_anchor: origin,
                world_anchor: [0.0, 0.0],
                zoom,
            },
        )
    }
    fn anchored(screen_anchor: Pos2, world_anchor: [f64; 2], zoom: f32) -> Option<Self> {
        (screen_anchor.is_finite()
            && world_anchor.into_iter().all(f64::is_finite)
            && zoom.is_finite()
            && (MIN_ZOOM..=MAX_ZOOM).contains(&zoom))
        .then_some(Self {
            screen_anchor,
            world_anchor,
            zoom,
        })
    }
    pub fn zoom(&self) -> f32 {
        self.zoom
    }
    pub fn to_screen(self, world: Point) -> Pos2 {
        self.to_screen_exact(f64::from(world[0]), f64::from(world[1]))
    }
    /// Projects canonical integer wire coordinates without first rounding them through f32.
    pub fn to_screen_i32(self, x: i32, y: i32) -> Pos2 {
        self.to_screen_exact(f64::from(x), f64::from(y))
    }
    fn to_screen_exact(self, x: f64, y: f64) -> Pos2 {
        Pos2::new(
            project(self.screen_anchor.x as f64 + (x - self.world_anchor[0]) * self.zoom as f64),
            project(self.screen_anchor.y as f64 + (y - self.world_anchor[1]) * self.zoom as f64),
        )
    }
    pub fn to_world(self, screen: Pos2) -> Point {
        let p = if screen.is_finite() {
            screen
        } else {
            self.screen_anchor
        };
        [
            world_coord(
                self.world_anchor[0]
                    + (p.x as f64 - self.screen_anchor.x as f64) / self.zoom as f64,
            ) as f32,
            world_coord(
                self.world_anchor[1]
                    + (p.y as f64 - self.screen_anchor.y as f64) / self.zoom as f64,
            ) as f32,
        ]
    }
    pub fn length_to_world(&self, points: f32) -> f32 {
        if points.is_finite() {
            (points as f64 / self.zoom as f64).min(MAX_WORLD_DELTA) as f32
        } else {
            0.0
        }
    }
    pub fn delta_to_world(&self, delta: Vec2) -> Point {
        let x = if delta.x.is_finite() {
            delta.x as f64 / self.zoom as f64
        } else {
            0.0
        };
        let y = if delta.y.is_finite() {
            delta.y as f64 / self.zoom as f64
        } else {
            0.0
        };
        [world_delta(x) as f32, world_delta(y) as f32]
    }
}
fn project(value: f64) -> f32 {
    if !value.is_finite() {
        0.0
    } else {
        value.clamp(-MAX_PROJECTED, MAX_PROJECTED) as f32
    }
}
fn world_coord(value: f64) -> f64 {
    if !value.is_finite() {
        0.0
    } else {
        value.clamp(-MAX_WORLD_COORD, MAX_WORLD_COORD)
    }
}
fn world_delta(value: f64) -> f64 {
    if !value.is_finite() {
        0.0
    } else {
        value.clamp(-MAX_WORLD_DELTA, MAX_WORLD_DELTA)
    }
}
fn valid_view(view: Rect) -> bool {
    view.is_finite()
        && view.width().is_finite()
        && view.height().is_finite()
        && view.min.x.abs() <= MAX_PROJECTED as f32
        && view.min.y.abs() <= MAX_PROJECTED as f32
        && view.max.x.abs() <= MAX_PROJECTED as f32
        && view.max.y.abs() <= MAX_PROJECTED as f32
        && view.width() > 0.0
        && view.height() > 0.0
        && view.width() as f64 <= MAX_VIEW_TRANSLATION
        && view.height() as f64 <= MAX_VIEW_TRANSLATION
}

/// Pure transient camera. The screen anchor is relative to the current view rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasViewState {
    zoom: f32,
    world_anchor: [f64; 2],
    screen_offset: Vec2,
}
impl Default for CanvasViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            world_anchor: [0.0, 0.0],
            screen_offset: Vec2::ZERO,
        }
    }
}
impl CanvasViewState {
    pub fn zoom(self) -> f32 {
        self.zoom
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn transform(self, view: Rect) -> Option<CanvasTransform> {
        if !valid_view(view) || !self.screen_offset.is_finite() {
            return None;
        }
        let anchor = Pos2::new(
            project(view.min.x as f64 + self.screen_offset.x as f64),
            project(view.min.y as f64 + self.screen_offset.y as f64),
        );
        if self.world_anchor == [0.0, 0.0] {
            CanvasTransform::new(anchor, self.zoom)
        } else {
            CanvasTransform::anchored(anchor, self.world_anchor, self.zoom)
        }
    }
    pub fn pan(&mut self, delta: Vec2) -> bool {
        if !delta.is_finite() || !self.zoom.is_finite() {
            return false;
        }
        self.screen_offset = Vec2::new(
            (self.screen_offset.x as f64 + delta.x as f64)
                .clamp(-MAX_VIEW_TRANSLATION, MAX_VIEW_TRANSLATION) as f32,
            (self.screen_offset.y as f64 + delta.y as f64)
                .clamp(-MAX_VIEW_TRANSLATION, MAX_VIEW_TRANSLATION) as f32,
        );
        true
    }
    /// Zoom anchored at the pointer; clamping is included in the same invariant-preserving update.
    pub fn zoom_at(&mut self, view: Rect, pointer: Pos2, factor: f32) -> bool {
        if !valid_view(view)
            || !pointer.is_finite()
            || !factor.is_finite()
            || factor <= 0.0
            || !self.zoom.is_finite()
            || !self.screen_offset.is_finite()
        {
            return false;
        }
        let Some(before) = self.transform(view).map(|t| t.to_world(pointer)) else {
            return false;
        };
        let target = (self.zoom as f64 * factor as f64).clamp(MIN_ZOOM as f64, MAX_ZOOM as f64);
        let x = pointer.x as f64 - view.min.x as f64;
        let y = pointer.y as f64 - view.min.y as f64;
        if !x.is_finite()
            || !y.is_finite()
            || x.abs() > MAX_VIEW_TRANSLATION
            || y.abs() > MAX_VIEW_TRANSLATION
        {
            return false;
        }
        self.zoom = target as f32;
        self.world_anchor = [before[0] as f64, before[1] as f64];
        self.screen_offset = Vec2::new(x as f32, y as f32);
        true
    }
    /// Whether the extent can only be fitted by clamping to minimum zoom.
    pub fn fit_limited(view: Rect, min: Point, max: Point, margin: f32) -> bool {
        if !valid_view(view)
            || !min.into_iter().chain(max).all(f32::is_finite)
            || min[0] > max[0]
            || min[1] > max[1]
            || !margin.is_finite()
            || margin < 0.0
        {
            return false;
        }
        let w = (view.width() as f64 - 2.0 * margin as f64).max(1.0);
        let h = (view.height() as f64 - 2.0 * margin as f64).max(1.0);
        let ew = (max[0] as f64 - min[0] as f64).max(1.0);
        let eh = (max[1] as f64 - min[1] as f64).max(1.0);
        w / ew < MIN_ZOOM as f64
            || h / eh < MIN_ZOOM as f64
            || 2.0 * margin as f64 >= view.width() as f64
            || 2.0 * margin as f64 >= view.height() as f64
    }
    /// Fit finite world bounds with screen margin. Returns false without mutation for invalid input.
    pub fn fit(&mut self, view: Rect, min: Point, max: Point, margin: f32) -> bool {
        if !valid_view(view)
            || !min.into_iter().chain(max).all(f32::is_finite)
            || min[0] > max[0]
            || min[1] > max[1]
            || !margin.is_finite()
            || margin < 0.0
        {
            return false;
        }
        let available_w = (view.width() as f64 - 2.0 * margin as f64).max(1.0);
        let available_h = (view.height() as f64 - 2.0 * margin as f64).max(1.0);
        let width = (max[0] as f64 - min[0] as f64).max(1.0);
        let height = (max[1] as f64 - min[1] as f64).max(1.0);
        let zoom = (available_w / width)
            .min(available_h / height)
            .clamp(MIN_ZOOM as f64, MAX_ZOOM as f64);
        let cx = min[0] as f64 + (max[0] as f64 - min[0] as f64) * 0.5;
        let cy = min[1] as f64 + (max[1] as f64 - min[1] as f64) * 0.5;
        if ![zoom, cx, cy].into_iter().all(f64::is_finite) {
            return false;
        }
        self.zoom = zoom as f32;
        self.world_anchor = [cx, cy];
        self.screen_offset = view.size() * 0.5;
        true
    }
}

/// Bounded adaptive lattice. It never iterates on floating-point progress or emits more than
/// 512 lines, including for invalid/extreme viewport extents. Grid points stay >=8 screen points apart.
pub fn grid_lines(view: Rect, transform: CanvasTransform, grid_unit: f32) -> Vec<(bool, f32)> {
    const MAX_LINES: i64 = 512;
    if !valid_view(view) || !grid_unit.is_finite() || grid_unit <= 0.0 {
        return Vec::new();
    }
    let zoom = transform.zoom as f64;
    let required = (8.0 / (grid_unit as f64 * zoom)).ceil();
    const MAX_STRIDE: u64 = 1_u64 << 50;
    if !required.is_finite() || required > MAX_STRIDE as f64 {
        return Vec::new();
    }
    let stride = (required.max(1.0) as u64)
        .checked_next_power_of_two()
        .unwrap_or(MAX_STRIDE)
        .min(MAX_STRIDE) as i64;
    let [left, top] = transform.to_world(view.min);
    let [right, bottom] = transform.to_world(view.max);
    if ![left, top, right, bottom].into_iter().all(f32::is_finite) {
        return Vec::new();
    }
    let unit = grid_unit as f64 * stride as f64;
    let (x0, x1) = (
        (left.min(right) as f64 / unit).ceil() as i64,
        (left.max(right) as f64 / unit).floor() as i64,
    );
    let (y0, y1) = (
        (top.min(bottom) as f64 / unit).ceil() as i64,
        (top.max(bottom) as f64 / unit).floor() as i64,
    );
    let mut lines = Vec::with_capacity(MAX_LINES as usize);
    for i in bounded_indices(x0, x1, MAX_LINES / 2) {
        let world = i as f64 * unit;
        if world.abs() <= MAX_WORLD_COORD {
            lines.push((true, transform.to_screen([world as f32, 0.]).x));
        }
    }
    for i in bounded_indices(y0, y1, MAX_LINES - lines.len() as i64) {
        let world = i as f64 * unit;
        if world.abs() <= MAX_WORLD_COORD {
            lines.push((false, transform.to_screen([0., world as f32]).y));
        }
    }
    lines
        .into_iter()
        .filter(|(_, p)| p.is_finite() && *p >= -MAX_PROJECTED as f32 && *p <= MAX_PROJECTED as f32)
        .collect()
}
fn bounded_indices(first: i64, last: i64, limit: i64) -> Vec<i64> {
    if first > last || limit <= 0 {
        return Vec::new();
    }
    let count = (last as i128 - first as i128 + 1).min(limit as i128) as i64;
    let skip = ((last as i128 - first as i128 + 1 - count as i128).max(0) / 2) as i64;
    (0..count)
        .map(|n| first.saturating_add(skip).saturating_add(n))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> Rect {
        Rect::from_min_max(Pos2::new(30., 20.), Pos2::new(630., 420.))
    }
    #[test]
    fn canonical_large_integer_wire_coordinates_are_not_rounded_through_f32() {
        let transform =
            CanvasTransform::anchored(Pos2::ZERO, [16_777_216.0, 16_777_216.0], 1.0).unwrap();
        assert_eq!(
            transform.to_screen_i32(16_777_217, 16_777_219),
            Pos2::new(1.0, 3.0)
        );
        assert_eq!(
            transform.to_screen([16_777_217.0, 16_777_219.0]),
            Pos2::new(0.0, 4.0)
        );
    }

    #[test]
    fn transform_round_trip_and_relative_origin() {
        let t = CanvasTransform::new(Pos2::new(208., 8.), 2.5).unwrap();
        let screen = t.to_screen([10., 20.]);
        assert_eq!(screen, Pos2::new(233., 58.));
        assert_eq!(t.to_world(screen), [10., 20.]);
        assert_eq!(t.delta_to_world(Vec2::new(5., 10.)), [2., 4.]);
        let t = CanvasViewState::default().transform(view()).unwrap();
        assert_eq!(t.to_screen([0., 0.]), view().min);
    }
    #[test]
    fn anchored_zoom_clamps_and_preserves_pointer_world_position() {
        let mut camera = CanvasViewState::default();
        let pointer = Pos2::new(230., 170.);
        let before = camera.transform(view()).unwrap().to_world(pointer);
        assert!(camera.zoom_at(view(), pointer, 3.));
        let after = camera.transform(view()).unwrap().to_world(pointer);
        assert!((before[0] - after[0]).abs() < 0.001 && (before[1] - after[1]).abs() < 0.001);
        assert!(camera.zoom_at(view(), pointer, f32::MAX));
        assert_eq!(camera.zoom(), MAX_ZOOM);
        let snapshot = camera;
        for factor in [f32::NAN, f32::INFINITY, 0., -1.] {
            assert!(!camera.zoom_at(view(), pointer, factor));
            assert_eq!(camera, snapshot);
        }
    }
    #[test]
    fn invalid_pan_viewport_and_fit_leave_camera_unchanged() {
        let mut camera = CanvasViewState::default();
        let original = camera;
        assert!(!camera.pan(Vec2::new(f32::NAN, 0.)));
        assert!(!camera.zoom_at(Rect::NOTHING, Pos2::ZERO, 2.));
        assert!(!camera.fit(view(), [f32::NAN, 0.], [1., 1.], 10.));
        assert_eq!(camera, original);
        for zoom in [f32::NAN, 0., -1., f32::INFINITY] {
            assert!(CanvasTransform::new(Pos2::ZERO, zoom).is_none());
        }
    }
    #[test]
    fn extreme_fit_centers_at_bounded_camera_and_reports_minimum_zoom_limit() {
        let mut camera = CanvasViewState::default();
        let huge = [f32::MAX, -f32::MAX];
        assert!(CanvasViewState::fit_limited(
            view(),
            [-f32::MAX, -f32::MAX],
            huge,
            12.
        ));
        assert!(camera.fit(view(), [-f32::MAX, -f32::MAX], huge, 12.));
        let t = camera.transform(view()).unwrap();
        let center = [0., 0.];
        assert!(t.to_screen(center).is_finite());
        assert_eq!(t.zoom(), MIN_ZOOM);
        assert!(camera.fit(view(), [1e30, 1e30], [1e30, 1e30], 12.));
        let t = camera.transform(view()).unwrap();
        assert!(t.to_screen([1e30, 1e30]).distance(view().center()) < 0.01);
    }
    #[test]
    fn grid_generation_is_bounded_finite_and_adaptive() {
        let t = CanvasTransform::new(Pos2::ZERO, MIN_ZOOM).unwrap();
        let lines = grid_lines(
            Rect::from_min_max(Pos2::new(-1e30, -1e30), Pos2::new(1e30, 1e30)),
            t,
            20.,
        );
        assert!(lines.len() <= 512 && lines.iter().all(|(_, p)| p.is_finite()));
        assert!(grid_lines(Rect::NOTHING, t, 20.).is_empty());
    }
}
