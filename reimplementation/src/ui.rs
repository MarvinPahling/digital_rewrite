//! Native adapter for component editing.
//!
//! Symbol drawing and picking live behind `symbol_render`; the canvas maps world units to the
//! screen through [`CanvasTransform`]. The shape style is transient UI state and never reaches
//! the engine or the document.
use crate::{
    canvas::{CanvasTransform, CanvasViewState, grid_lines},
    components::KINDS,
    domain::{ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position},
    engine::{EditError, SaveError, Session, SimulationError},
    file_session::{DiscardIntent, FileIo, LocalFileIo, PublishOutcome, SaveAsIntent},
    runtime::{InputChange, Level, SettleOutcome, SimulationOptions},
    sim_text::{describe_latched_oscillation, describe_simulation_error, level_badge, level_text},
    symbol_render::{PaintState, Palette, SymbolLayout, SymbolRenderer},
    symbols::{DEFAULT_STYLE, GRID_UNIT, STROKE_WIDTH, SymbolStyle},
};
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, Vec2};

/// Extra reach of a click around a symbol's bounds, in screen points.
const PICK_TOLERANCE: f32 = 2.0;

/// Positions are artwork-bounds centers, but Digital places on the logical origin grid. Convert
/// through each symbol's shared bounds offset before snapping so asymmetric artwork stays aligned.
fn snap_component_center(style: SymbolStyle, kind: ComponentKind, x: f32, y: f32) -> Position {
    snap_component_center_oriented(style, kind, crate::domain::Orientation::default(), x, y)
}
fn snap_component_center_oriented(
    _style: SymbolStyle,
    kind: ComponentKind,
    orientation: crate::domain::Orientation,
    x: f32,
    y: f32,
) -> Position {
    let offset = crate::symbols::center_from_origin_offset(crate::symbols::DEFAULT_STYLE, kind)
        .and_then(|d| orientation.transform_f32(d[0], d[1]))
        .map(|p| [p.0, p.1])
        .unwrap_or([0.0, 0.0]);
    let snap = |coordinate: f32, center_offset: f32| {
        let origin = coordinate - center_offset;
        (origin / GRID_UNIT).round() * GRID_UNIT + center_offset
    };
    Position {
        x: snap(x, offset[0]),
        y: snap(y, offset[1]),
    }
}
/// Gap between a symbol's bounds and its selection frame, in world units.
const SELECTION_MARGIN: f32 = 2.0;
/// Gap between a symbol's bounds and its label, in screen points.
const LABEL_GAP: f32 = 4.0;
/// Work units (gate evaluations plus microsteps) settled per frame. Small enough that a frame
/// stays short and Stop is handled between chunks, however large the circuit.
const SETTLE_BUDGET_PER_FRAME: usize = 2_048;
/// Shown wherever editing is disabled.
const EDIT_LOCK_NOTE: &str =
    "Editing is locked while the simulation is running. Stop it (Shift+F5) to edit.";
fn unique_route_targets(
    targets: impl IntoIterator<Item = crate::engine::RouteTarget>,
) -> Vec<crate::engine::RouteTarget> {
    let mut seen = std::collections::HashSet::new();
    targets
        .into_iter()
        .filter(|target| seen.insert(*target))
        .collect()
}

fn route_target_description(target: crate::engine::RouteTarget) -> String {
    match target {
        crate::engine::RouteTarget::Pin(pin) => format!(
            "pin on component {} ({:?} {})",
            pin.component.0, pin.pin.direction, pin.pin.index
        ),
        crate::engine::RouteTarget::ExistingEndpoint(point) => {
            format!("wire endpoint ({}, {})", point.x, point.y)
        }
        crate::engine::RouteTarget::SegmentInterior { segment_id, point } => {
            format!("segment {segment_id} at ({}, {})", point.x, point.y)
        }
    }
}

/// A simulation command from a button, shortcut or canvas click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimAction {
    Start,
    StartPaused,
    Stop,
    Step,
    Settle,
    Pause,
    Toggle(ComponentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewAction {
    ZoomIn,
    ZoomOut,
    Fit,
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimState {
    Idle,
    /// The last start failed and the document has not changed since.
    Error,
    /// A terminal oscillation was detected and editing is available again.
    Oscillating,
    Pending {
        settling: bool,
    },
    Settled,
}

pub trait TextClipboard {
    fn read_text(&mut self) -> Result<String, String>;
    fn write_text(&mut self, text: &str) -> Result<(), String>;
}

struct UnavailableClipboard;
impl TextClipboard for UnavailableClipboard {
    fn read_text(&mut self) -> Result<String, String> {
        Err("System clipboard is unavailable".into())
    }
    fn write_text(&mut self, _text: &str) -> Result<(), String> {
        Err("System clipboard is unavailable".into())
    }
}

struct NativeClipboard {
    inner: Option<arboard::Clipboard>,
    init_error: Option<String>,
}
impl NativeClipboard {
    fn new() -> Self {
        match arboard::Clipboard::new() {
            Ok(inner) => Self {
                inner: Some(inner),
                init_error: None,
            },
            Err(error) => Self {
                inner: None,
                init_error: Some(format!("Cannot initialize system clipboard: {error}")),
            },
        }
    }
}
impl TextClipboard for NativeClipboard {
    fn read_text(&mut self) -> Result<String, String> {
        self.inner
            .as_mut()
            .ok_or_else(|| {
                self.init_error
                    .clone()
                    .unwrap_or_else(|| "System clipboard is unavailable".into())
            })?
            .get_text()
            .map_err(|error| format!("Cannot read system clipboard: {error}"))
    }
    fn write_text(&mut self, text: &str) -> Result<(), String> {
        self.inner
            .as_mut()
            .ok_or_else(|| {
                self.init_error
                    .clone()
                    .unwrap_or_else(|| "System clipboard is unavailable".into())
            })?
            .set_text(text.to_owned())
            .map_err(|error| format!("Cannot write system clipboard: {error}"))
    }
}

#[derive(Clone, Debug)]
struct FragmentDraft {
    fragment: digital_rust::formats::DigV2,
    anchor: [f64; 2],
    delta: [i32; 2],
    center_fallback: bool,
    keyboard_adjusted: bool,
}

/// Materialize exactly the transformed, offset document consumed by the placement preview.
fn fragment_preview_document(draft: &FragmentDraft) -> Result<Document, String> {
    let mut document =
        digital_rust::formats::to_domain(&draft.fragment).map_err(|error| error.to_string())?;
    for component in &mut document.components {
        let x = f64::from(component.position.x) + f64::from(draft.delta[0]);
        let y = f64::from(component.position.y) + f64::from(draft.delta[1]);
        if !x.is_finite()
            || !y.is_finite()
            || x.abs() > f32::MAX as f64
            || y.abs() > f32::MAX as f64
        {
            return Err("Placement offset exceeds supported component coordinates".into());
        }
        component.position = Position {
            x: x as f32,
            y: y as f32,
        };
    }
    for segment in document
        .wires
        .geometric_mut()
        .map_err(|error| error.to_string())?
    {
        segment.a.x = segment
            .a
            .x
            .checked_add(draft.delta[0])
            .ok_or_else(|| "Placement offset exceeds supported wire coordinates".to_owned())?;
        segment.a.y = segment
            .a
            .y
            .checked_add(draft.delta[1])
            .ok_or_else(|| "Placement offset exceeds supported wire coordinates".to_owned())?;
        segment.b.x = segment
            .b
            .x
            .checked_add(draft.delta[0])
            .ok_or_else(|| "Placement offset exceeds supported wire coordinates".to_owned())?;
        segment.b.y = segment
            .b
            .y
            .checked_add(draft.delta[1])
            .ok_or_else(|| "Placement offset exceeds supported wire coordinates".to_owned())?;
    }
    Ok(document)
}

#[derive(Clone, Debug)]
enum ClipboardAction {
    Copy,
    Cut,
    Paste(Option<String>),
    Duplicate,
}

pub struct DigitalApp {
    session: Session,
    clipboard: Box<dyn TextClipboard>,
    fragment_draft: Option<FragmentDraft>,
    clipboard_error: Option<String>,
    selected: Option<ComponentId>,
    selected_segment: Option<u64>,
    selection: crate::selection::Selection,
    marquee: Option<(Pos2, egui::Modifiers)>,
    wiring: bool,
    geometric_route_start: Option<crate::engine::RouteTarget>,
    route_orientation: digital_rust::geometric_wiring::RouteOrientation,
    focused_pin: Option<PinRef>,
    focused_route_target: Option<crate::engine::RouteTarget>,
    route_pick_cycle: usize,
    placing: Option<ComponentKind>,
    drag: Option<(ComponentId, Position, Pos2)>,
    error: Option<String>,
    /// Work units settled per frame while `settling`.
    settle_budget: usize,
    startup_seed_text: String,
    seed_field_focused: bool,
    seed_field_rect: Option<Rect>,
    seed_field_id: Option<egui::Id>,
    canvas_id: Option<egui::Id>,
    seed_focus_requested: bool,
    /// Whether frames keep settling in bounded chunks. Step pauses it; Settle, Start and an
    /// input toggle resume it.
    settling: bool,
    /// One logical F10 frontier is continued in bounded frame work units.
    microstepping: bool,
    /// The document whose compilation last failed; the error state ends when it changes.
    failed_start: Option<Document>,
    style: SymbolStyle,
    renderer: SymbolRenderer,
    file_io: Option<Box<dyn FileIo>>,
    file_io_error: Option<String>,
    file_path_entry: String,
    pending_action: Option<PendingAction>,
    file_modal: Option<FileModal>,
    approved_close: bool,
    label_modal: Option<LabelDraft>,
    restore_canvas_focus: bool,
    fragment_focus_claimed: bool,
    fragment_focus_check_pending: bool,
    fragment_canvas_had_focus: bool,
    view: CanvasViewState,
    pan_owner: Option<egui::PointerButton>,
    fit_view_pending: bool,
    view_action: Option<ViewAction>,
    canvas_rect: Option<Rect>,
}
#[derive(Clone, Debug)]
struct LabelDraft {
    id: ComponentId,
    text: String,
    error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingAction {
    New,
    Open(std::path::PathBuf),
    Close,
}
#[derive(Clone, Debug)]
enum FileModal {
    OpenPath,
    SavePath {
        continuation: bool,
    },
    Unsaved,
    RunningClose,
    Overwrite {
        path: std::path::PathBuf,
        continuation: bool,
    },
    PublishedWarning {
        diagnostics: Vec<String>,
        continuation: bool,
    },
}
impl Default for DigitalApp {
    fn default() -> Self {
        let cwd = std::env::current_dir();
        let (file_io, file_io_error) = match cwd {
            Ok(path) if path.is_absolute() => (
                Some(Box::new(LocalFileIo::new(path)) as Box<dyn FileIo>),
                None,
            ),
            Ok(_) => (
                None,
                Some("The current directory is not absolute; file actions are unavailable".into()),
            ),
            Err(error) => (
                None,
                Some(format!("Cannot initialize local file access: {error}")),
            ),
        };
        Self::with_file_io(file_io, file_io_error)
    }
}
impl DigitalApp {
    pub fn with_file_io(file_io: Option<Box<dyn FileIo>>, file_io_error: Option<String>) -> Self {
        Self {
            session: Session::default(),
            clipboard: Box::new(UnavailableClipboard),
            fragment_draft: None,
            clipboard_error: None,
            selected: None,
            selected_segment: None,
            selection: crate::selection::Selection::default(),
            marquee: None,
            wiring: false,
            geometric_route_start: None,
            route_orientation: digital_rust::geometric_wiring::RouteOrientation::HorizontalFirst,
            focused_pin: None,
            focused_route_target: None,
            route_pick_cycle: 0,
            placing: None,
            drag: None,
            error: None,
            settle_budget: SETTLE_BUDGET_PER_FRAME,
            startup_seed_text: "0".into(),
            seed_field_focused: false,
            seed_field_rect: None,
            seed_field_id: None,
            canvas_id: None,
            seed_focus_requested: false,
            settling: false,
            microstepping: false,
            failed_start: None,
            style: DEFAULT_STYLE,
            renderer: SymbolRenderer::default(),
            file_io,
            file_io_error,
            file_path_entry: String::new(),
            pending_action: None,
            file_modal: None,
            approved_close: false,
            label_modal: None,
            restore_canvas_focus: false,
            fragment_focus_claimed: false,
            fragment_focus_check_pending: false,
            fragment_canvas_had_focus: false,
            view: CanvasViewState::default(),
            pan_owner: None,
            fit_view_pending: false,
            view_action: None,
            canvas_rect: None,
        }
    }
    pub fn with_clipboard(mut self, clipboard: Box<dyn TextClipboard>) -> Self {
        self.clipboard = clipboard;
        self
    }
    pub fn native() -> Self {
        let mut app = Self::default();
        let clipboard = NativeClipboard::new();
        app.clipboard_error = clipboard.init_error.clone();
        app.with_clipboard(Box::new(clipboard))
    }

    fn begin_fragment_draft(
        &mut self,
        fragment: digital_rust::formats::DigV2,
        initial_delta: [i32; 2],
        center_fallback: bool,
    ) -> Result<(), String> {
        let document =
            digital_rust::formats::to_domain(&fragment).map_err(|error| error.to_string())?;
        let mut max = [f64::NEG_INFINITY; 2];
        for component in &document.components {
            if [component.position.x, component.position.y]
                .into_iter()
                .any(|value| value.abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER)
            {
                return Err(format!(
                    "Clipboard component center exceeds exact symbol-rendering range (±{} world units)",
                    crate::symbol_render::MAX_EXACT_SYMBOL_CENTER
                ));
            }
            let layout = SymbolLayout::oriented_on(
                self.style,
                component.kind,
                [component.position.x, component.position.y],
                crate::domain::Orientation {
                    rotation: component.rotation,
                    mirror: component.mirror,
                },
            )
            .ok_or_else(|| format!("No geometry for {}", component.kind.name()))?;
            let bounds = layout.bounds();
            max[0] = max[0].max(f64::from(bounds.max[0]));
            max[1] = max[1].max(f64::from(bounds.max[1]));
        }
        for wire in &fragment.wires {
            max[0] = max[0].max(f64::from(wire.p1.x.max(wire.p2.x)));
            max[1] = max[1].max(f64::from(wire.p1.y.max(wire.p2.y)));
        }
        if !max[0].is_finite()
            || !max[1].is_finite()
            || max
                .iter()
                .any(|value| value.abs() > (i32::MAX as f64 * 2.0))
        {
            return Err("Clipboard fragment has an unrepresentable placement anchor".into());
        }
        self.placing = None;
        self.wiring = false;
        self.drag = None;
        self.marquee = None;
        self.geometric_route_start = None;
        self.pan_owner = None;
        let anchor = [
            (max[0] / f64::from(GRID_UNIT)).round() * f64::from(GRID_UNIT),
            (max[1] / f64::from(GRID_UNIT)).round() * f64::from(GRID_UNIT),
        ];
        self.fragment_draft = Some(FragmentDraft {
            fragment,
            anchor,
            delta: initial_delta,
            center_fallback,
            keyboard_adjusted: false,
        });
        self.restore_canvas_focus = true;
        self.fragment_focus_claimed = false;
        self.fragment_focus_check_pending = false;
        self.fragment_canvas_had_focus = false;
        Ok(())
    }

    fn run_clipboard_action(&mut self, action: ClipboardAction) {
        if self.modality_active() || self.fragment_draft.is_some() {
            return;
        }
        if self.session.simulation_active() {
            self.error = Some(EditError::SimulationActive.to_string());
            return;
        }
        match action {
            ClipboardAction::Copy => match self.session.copy_selection_text(&self.selection) {
                Ok(Some(text)) => match self.clipboard.write_text(&text) {
                    Ok(()) => {
                        self.cancel_edit_gestures();
                        self.error = None;
                        self.clipboard_error = None;
                    }
                    Err(error) => self.clipboard_error = Some(error),
                },
                Ok(None) => {}
                Err(error) => self.clipboard_error = Some(error.to_string()),
            },
            ClipboardAction::Cut => match self.session.prepare_cut(&self.selection) {
                Ok(Some(plan)) => match self.clipboard.write_text(&plan.text) {
                    Err(error) => self.clipboard_error = Some(error),
                    Ok(()) => {
                        let omitted = plan.omitted_boundary_wires;
                        match self.session.commit_cut(plan) {
                            Ok(_) => {
                                self.cancel_edit_gestures();
                                self.selection.clear();
                                self.selected = None;
                                self.selected_segment = None;
                                self.clipboard_error = None;
                                self.error = (omitted > 0).then(|| format!("Cut copied components and complete wires; left {omitted} partial boundary wire(s) in the document (safe-cut deviation from Digital)."));
                            }
                            Err(error) => self.clipboard_error = Some(error.to_string()),
                        }
                    }
                },
                Ok(None)
                    if self.selection.components.is_empty() && !self.selection.wires.is_empty() =>
                {
                    self.clipboard_error = Some("Cut not performed: a partial boundary wire cannot be copied or cut by itself.".into());
                }
                Ok(None) => {}
                Err(error) => self.clipboard_error = Some(error.to_string()),
            },
            ClipboardAction::Paste(text) => {
                let text = match text {
                    Some(text) => Ok(text),
                    None => self.clipboard.read_text(),
                };
                match text.and_then(|text| {
                    digital_rust::formats::parse_clipboard_list(text.as_bytes())
                        .map_err(|error| error.to_string())
                }) {
                    Ok(fragment) if fragment.elements.is_empty() && fragment.wires.is_empty() => {}
                    Ok(fragment) => {
                        self.cancel_edit_gestures();
                        let result = self.begin_fragment_draft(fragment, [0, 0], true);
                        match result {
                            Ok(()) => self.clipboard_error = None,
                            Err(error) => self.clipboard_error = Some(error),
                        }
                    }
                    Err(error) => self.clipboard_error = Some(error),
                }
            }
            ClipboardAction::Duplicate => match self.session.selected_fragment(&self.selection) {
                Ok(Some(fragment)) => {
                    self.cancel_edit_gestures();
                    match self.begin_fragment_draft(fragment, [20, 20], false) {
                        Ok(()) => self.clipboard_error = None,
                        Err(error) => self.clipboard_error = Some(error),
                    }
                }
                Ok(None) => {}
                Err(error) => self.clipboard_error = Some(error.to_string()),
            },
        }
    }

    fn commit_fragment_draft(&mut self) {
        let Some(draft) = self.fragment_draft.clone() else {
            return;
        };
        let previous_segments: std::collections::BTreeSet<_> = self
            .session
            .document()
            .wires
            .geometric()
            .map(|segments| segments.iter().map(|segment| segment.id).collect())
            .unwrap_or_default();
        match self.session.insert_clipboard_fragment(
            &draft.fragment,
            draft.delta[0],
            draft.delta[1],
        ) {
            Ok(ids) => {
                self.selection.clear();
                self.selection.components.extend(ids.iter().copied());
                if let Ok(segments) = self.session.document().wires.geometric() {
                    self.selection.wires.extend(
                        segments
                            .iter()
                            .filter(|segment| !previous_segments.contains(&segment.id))
                            .map(|segment| (segment.id, crate::selection::EndpointMask::BOTH)),
                    );
                }
                self.selected = (self.selection.components.len() == 1
                    && self.selection.wires.is_empty())
                .then(|| self.selection.components.iter().next().copied())
                .flatten();
                self.selected_segment = None;
                self.fragment_draft = None;
                self.clipboard_error = None;
                self.error = None;
            }
            Err(error) => self.clipboard_error = Some(error.to_string()),
        }
    }

    fn rotate_fragment_draft(&mut self) {
        let Some(draft) = &mut self.fragment_draft else {
            return;
        };
        let pivot = [draft.anchor[0] as i64, draft.anchor[1] as i64];
        let mut candidate = draft.fragment.clone();
        let rotate = |x: i32, y: i32| -> Option<(i32, i32)> {
            let dx = i64::from(x).checked_sub(pivot[0])?;
            let dy = i64::from(y).checked_sub(pivot[1])?;
            let nx = pivot[0].checked_add(dy)?;
            let ny = pivot[1].checked_sub(dx)?;
            Some((i32::try_from(nx).ok()?, i32::try_from(ny).ok()?))
        };
        for element in &mut candidate.elements {
            let Some((x, y)) = rotate(element.x, element.y) else {
                self.clipboard_error =
                    Some("Clipboard rotation exceeds supported coordinates".into());
                return;
            };
            element.x = x;
            element.y = y;
            element.rotation = (element.rotation + 1) % 4;
        }
        for wire in &mut candidate.wires {
            let Some((x1, y1)) = rotate(wire.p1.x, wire.p1.y) else {
                self.clipboard_error =
                    Some("Clipboard rotation exceeds supported coordinates".into());
                return;
            };
            let Some((x2, y2)) = rotate(wire.p2.x, wire.p2.y) else {
                self.clipboard_error =
                    Some("Clipboard rotation exceeds supported coordinates".into());
                return;
            };
            wire.p1.x = x1;
            wire.p1.y = y1;
            wire.p2.x = x2;
            wire.p2.y = y2;
        }
        if digital_rust::formats::to_domain(&candidate).is_err() {
            self.clipboard_error =
                Some("Clipboard rotation produces invalid component geometry".into());
            return;
        }
        draft.fragment = candidate;
        draft.keyboard_adjusted = true;
        self.clipboard_error = None;
    }

    fn adjust_fragment_draft(&mut self, dx: i32, dy: i32) {
        let Some(draft) = &mut self.fragment_draft else {
            return;
        };
        if let (Some(x), Some(y)) = (
            draft.delta[0].checked_add(dx),
            draft.delta[1].checked_add(dy),
        ) {
            draft.delta = [x, y];
            draft.keyboard_adjusted = true;
        } else {
            self.clipboard_error =
                Some("Placement offset exceeds supported integer geometry".into());
        }
    }

    fn cancel_edit_gestures(&mut self) {
        self.placing = None;
        self.wiring = false;
        self.drag = None;
        self.marquee = None;
        self.pan_owner = None;
        self.geometric_route_start = None;
        self.focused_route_target = None;
        self.focused_pin = None;
    }
}
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
}
/// Conservative content extent for fit: symbol strokes/bounds, canonical routed wire paths and
/// measured multiline labels. Text and runtime badges are screen-sized, so reserve their measured
/// galley width/height as screen margins rather than scaling the fonts with the camera.
type ViewBounds = ([f32; 2], [f32; 2], f32);
fn document_view_bounds(
    doc: &Document,
    style: SymbolStyle,
    painter: &egui::Painter,
) -> Result<Option<ViewBounds>, String> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    let mut have_geometry = false;
    let mut label_width = 0.0_f32;
    let mut label_height = 0.0_f32;
    let mut anchors = std::collections::HashMap::new();
    let include = |p: [f32; 2], min: &mut [f32; 2], max: &mut [f32; 2]| -> bool {
        if !p.into_iter().all(f32::is_finite) {
            return false;
        }
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
        true
    };
    for c in &doc.components {
        if [c.position.x, c.position.y]
            .into_iter()
            .any(|value| value.abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER)
        {
            return Err(format!(
                "Cannot fit: component #{} exceeds exact symbol-rendering range (±{} world units)",
                c.id.0,
                crate::symbol_render::MAX_EXACT_SYMBOL_CENTER
            ));
        }
        let Some(layout) = SymbolLayout::oriented_on(
            style,
            c.kind,
            [c.position.x, c.position.y],
            crate::domain::Orientation {
                rotation: c.rotation,
                mirror: c.mirror,
            },
        ) else {
            return Err(format!(
                "Cannot fit: missing symbol geometry for {}",
                c.kind.name()
            ));
        };
        let bounds = layout.bounds();
        if !include(bounds.min, &mut min, &mut max) || !include(bounds.max, &mut min, &mut max) {
            return Err(format!(
                "Cannot fit: invalid geometry for component #{}",
                c.id.0
            ));
        }
        have_geometry = true;
        for anchor in layout.anchors() {
            anchors.insert(
                PinRef {
                    component: c.id,
                    pin: match anchor.name {
                        "pin-in-0" => PinId {
                            direction: PinDirection::Input,
                            index: 0,
                        },
                        "pin-in-1" => PinId {
                            direction: PinDirection::Input,
                            index: 1,
                        },
                        "pin-out-0" => PinId {
                            direction: PinDirection::Output,
                            index: 0,
                        },
                        _ => continue,
                    },
                },
                anchor.position,
            );
        }
        let galley = painter.layout_no_wrap(
            c.label.clone(),
            egui::FontId::proportional(14.0),
            egui::Color32::WHITE,
        );
        label_width = label_width.max(galley.size().x);
        label_height = label_height.max(galley.size().y);
    }
    match &doc.wires {
        crate::domain::Wiring::GeometricSegments(segments) => {
            for segment in segments {
                for point in [segment.a, segment.b] {
                    if point.x.unsigned_abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER as u32
                        || point.y.unsigned_abs()
                            > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER as u32
                    {
                        return Err(format!(
                            "Cannot fit: wire #{} exceeds exact floating view-bound range (±{} world units)",
                            segment.id,
                            crate::symbol_render::MAX_EXACT_SYMBOL_CENTER
                        ));
                    }
                    if !include([point.x as f32, point.y as f32], &mut min, &mut max) {
                        return Err(format!(
                            "Cannot fit: invalid segment geometry #{}",
                            segment.id
                        ));
                    }
                }
                have_geometry = true;
            }
        }
    }
    if !have_geometry {
        return Ok(None);
    }
    let badge_height = if doc.components.is_empty() { 0.0 } else { 18.0 };
    let margin = (label_width * 0.5).max(36.0) + 12.0;
    // Labels and badges sit below each component (not centred on it), with selection-frame
    // padding growing with zoom; reserve for the maximum supported camera scale conservatively.
    let vertical = 42.0 + label_height + badge_height;
    let margin = margin.max(vertical);
    if !margin.is_finite() {
        return Err("Cannot fit: measured labels exceed finite canvas dimensions".into());
    }
    Ok(Some((min, max, margin)))
}

fn draw_grid(
    painter: &egui::Painter,
    rect: Rect,
    transform: CanvasTransform,
    color: egui::Color32,
) {
    let stroke = Stroke::new(0.5_f32, color);
    for (vertical, coordinate) in grid_lines(rect, transform, GRID_UNIT) {
        let (from, to) = if vertical {
            (
                Pos2::new(coordinate, rect.top()),
                Pos2::new(coordinate, rect.bottom()),
            )
        } else {
            (
                Pos2::new(rect.left(), coordinate),
                Pos2::new(rect.right(), coordinate),
            )
        };
        painter.line_segment([from, to], stroke);
    }
}
impl DigitalApp {
    fn modality_active(&self) -> bool {
        self.file_modal.is_some() || self.label_modal.is_some()
    }
    fn begin_label_edit(&mut self) {
        if self.session.simulation_active() || self.file_modal.is_some() {
            self.error = Some(EDIT_LOCK_NOTE.into());
            return;
        }
        let Some(id) = self.selected else {
            return;
        };
        let Some(text) = self
            .session
            .document()
            .components
            .iter()
            .find(|component| component.id == id)
            .map(|component| component.label.clone())
        else {
            return;
        };
        self.cancel_gestures();
        self.label_modal = Some(LabelDraft {
            id,
            text,
            error: None,
        });
    }
    fn cancel_label_edit(&mut self) {
        if self.label_modal.take().is_some() {
            self.restore_canvas_focus = true;
        }
    }
    fn finish_label_edit(&mut self) {
        let Some((id, text)) = self
            .label_modal
            .as_ref()
            .map(|draft| (draft.id, draft.text.clone()))
        else {
            return;
        };
        match self.session.set_label(id, &text) {
            Ok(()) => {
                self.error = None;
                self.cancel_label_edit();
            }
            Err(error) => {
                if let Some(draft) = self.label_modal.as_mut() {
                    draft.error = Some(error.to_string());
                }
            }
        }
    }
    fn reconcile_selection(&mut self) {
        let components: std::collections::BTreeSet<_> = self
            .session
            .document()
            .components
            .iter()
            .map(|component| component.id)
            .collect();
        self.selection
            .components
            .retain(|id| components.contains(id));
        let wires: std::collections::BTreeSet<_> = self
            .session
            .document()
            .wires
            .geometric()
            .map(|segments| segments.iter().map(|segment| segment.id).collect())
            .unwrap_or_default();
        self.selection.wires.retain(|id, _| wires.contains(id));
        self.selected = self
            .selected
            .filter(|id| self.selection.components.contains(id));
        self.selected_segment = self
            .selected_segment
            .filter(|id| self.selection.wires.contains_key(id));
    }

    fn cancel_gestures(&mut self) {
        self.fragment_draft = None;
        self.pan_owner = None;
        self.placing = None;
        self.wiring = false;
        self.geometric_route_start = None;
        self.focused_pin = None;
        self.focused_route_target = None;
        self.route_pick_cycle = 0;
        self.drag = None;
        self.marquee = None;
    }
    fn begin_pending(&mut self, action: PendingAction) {
        // A published warning is an acknowledgement barrier. In particular, a window-manager
        // close must not replace the New/Open continuation attached to the warning.
        if action == PendingAction::Close
            && matches!(self.file_modal, Some(FileModal::PublishedWarning { .. }))
        {
            return;
        }
        if self.session.simulation_active() && !matches!(action, PendingAction::Close) {
            self.error = Some("Stop the simulation before changing files".into());
            return;
        }
        self.cancel_gestures();
        self.pending_action = Some(action.clone());
        if action == PendingAction::Close && self.session.simulation_active() {
            self.file_modal = Some(FileModal::RunningClose);
            return;
        }
        if self.session.dirty() {
            self.file_modal = Some(FileModal::Unsaved);
        } else {
            self.complete_pending();
        }
    }
    fn complete_pending(&mut self) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        match action {
            PendingAction::New => match self.session.new_document(DiscardIntent::DiscardDirty) {
                Ok(()) => {
                    self.reset_document_ui();
                    self.view.reset();
                    self.fit_view_pending = false;
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.pending_action = Some(PendingAction::New);
                }
            },
            PendingAction::Open(path) => {
                let result = self
                    .file_io
                    .as_mut()
                    .ok_or_else(|| {
                        self.file_io_error
                            .clone()
                            .unwrap_or_else(|| "File access unavailable".into())
                    })
                    .and_then(|io| {
                        self.session
                            .open_path(io.as_mut(), &path, DiscardIntent::DiscardDirty)
                            .map_err(|e| e.to_string())
                    });
                match result {
                    Ok(()) => {
                        self.reset_document_ui();
                        self.fit_view_pending = true;
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.pending_action = Some(PendingAction::Open(path));
                    }
                }
            }
            PendingAction::Close => {
                if self.session.simulation_active() {
                    let _ = self.session.stop_simulation();
                }
                self.settling = false;
                self.approved_close = true;
            }
        }
    }
    fn fit_extent(&mut self, rect: Rect, min: [f32; 2], max: [f32; 2], margin: f32) {
        let limited = CanvasViewState::fit_limited(rect, min, max, margin);
        if !self.view.fit(rect, min, max, margin) {
            self.error = Some("Cannot fit circuit: invalid canvas or geometry extent".into());
        } else if limited {
            self.error = Some(
                "Fit limited by minimum zoom; content may extend beyond the visible canvas".into(),
            );
        } else {
            self.error = None;
        }
    }
    fn reset_document_ui(&mut self) {
        self.cancel_label_edit();
        self.pan_owner = None;
        self.selected = None;
        self.selected_segment = None;
        self.selection.clear();
        self.marquee = None;
        self.cancel_gestures();
        self.failed_start = None;
        self.settling = false;
        self.error = None;
    }
    fn open_path_dialog(&mut self) {
        self.cancel_gestures();
        self.file_path_entry.clear();
        self.file_modal = Some(FileModal::OpenPath);
    }
    fn save_path_dialog(&mut self, continuation: bool) {
        self.cancel_gestures();
        self.file_path_entry.clear();
        self.file_modal = Some(FileModal::SavePath { continuation });
    }
    fn begin_save(&mut self, continuation: bool) {
        self.cancel_gestures();
        if self.session.simulation_active() {
            self.error = Some("Stop the simulation before saving".into());
            return;
        }
        if self.session.path().is_none() {
            self.save_path_dialog(continuation);
        } else {
            self.save_current(continuation);
        }
    }
    fn save_current(&mut self, continuation: bool) {
        let result = self
            .file_io
            .as_mut()
            .ok_or_else(|| {
                self.file_io_error
                    .clone()
                    .unwrap_or_else(|| "File access unavailable".into())
            })
            .and_then(|io| self.session.save(io.as_mut()).map_err(|e| e.to_string()));
        match result {
            Ok(outcome) => self.after_save(outcome, continuation),
            Err(error) => self.error = Some(error),
        }
    }
    fn save_as_path(&mut self, path: std::path::PathBuf, continuation: bool, replace: bool) {
        let Some(io) = self.file_io.as_mut() else {
            self.error = Some(
                self.file_io_error
                    .clone()
                    .unwrap_or_else(|| "File access unavailable".into()),
            );
            return;
        };
        let result = self.session.save_as(
            io.as_mut(),
            &path,
            if replace {
                SaveAsIntent::Replace
            } else {
                SaveAsIntent::CreateOnly
            },
        );
        match result {
            Ok(outcome) => self.after_save(outcome, continuation),
            Err(SaveError::NotPublished(failure))
                if !replace && failure.primary.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                self.file_modal = Some(FileModal::Overwrite { path, continuation });
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn after_save(&mut self, outcome: PublishOutcome, continuation: bool) {
        match outcome {
            PublishOutcome::Published => {
                if continuation {
                    self.complete_pending();
                }
            }
            PublishOutcome::PublishedDurabilityUnconfirmed(diagnostics) => {
                self.file_modal = Some(FileModal::PublishedWarning {
                    diagnostics: diagnostics.iter().map(ToString::to_string).collect(),
                    continuation,
                });
            }
        }
    }
    fn file_actions(&mut self, ctx: &egui::Context) {
        let locked = self.session.simulation_active() || self.modality_active();
        let shortcut = ctx.input_mut(|i| {
            let command = egui::Modifiers::COMMAND;
            let shift = egui::Modifiers {
                shift: true,
                ..command
            };
            if i.consume_key(command, egui::Key::N) {
                Some(0)
            } else if i.consume_key(command, egui::Key::O) {
                Some(1)
            } else if i.consume_key(shift, egui::Key::S) {
                Some(3)
            } else if i.consume_key(command, egui::Key::S) {
                Some(2)
            } else {
                None
            }
        });
        if !locked && let Some(action) = shortcut {
            match action {
                0 => self.begin_pending(PendingAction::New),
                1 => self.open_path_dialog(),
                2 => self.begin_save(false),
                _ => self.save_path_dialog(false),
            }
        }
    }
    fn file_modal_ui(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.file_modal.clone() else {
            return;
        };
        // Durability diagnostics are rendered in the palette panel so the complete list remains
        // visible and the acknowledgement control is operable even for several warnings.
        if matches!(
            modal,
            FileModal::PublishedWarning { .. }
                | FileModal::OpenPath
                | FileModal::SavePath { .. }
                | FileModal::Unsaved
                | FileModal::RunningClose
        ) {
            return;
        }
        enum Choice {
            Cancel,
            Open(std::path::PathBuf),
            Save(std::path::PathBuf),
            SavePending,
            Discard,
            ConfirmReplace(std::path::PathBuf, bool),
            Acknowledge(bool),
            Stop,
            ContinueClose,
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::new("document_file_modal")).show(ctx, |ui| {
            ui.set_min_width(360.);
            match &modal {
                FileModal::OpenPath | FileModal::SavePath { .. } => {
                    let saving = matches!(modal, FileModal::SavePath { .. });
                    ui.heading(if saving { "Save As" } else { "Open document" });
                    ui.label("Path (absolute or relative to the application's startup directory):");
                    ui.text_edit_singleline(&mut self.file_path_entry);
                    ui.horizontal(|ui| {
                        if ui.button(if saving { "Save" } else { "Open" }).clicked() {
                            let path = std::path::PathBuf::from(self.file_path_entry.as_str());
                            if saving { choice = Some(Choice::Save(path)); } else { choice = Some(Choice::Open(path)); }
                        }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                    if let Some(error) = &self.file_io_error { ui.colored_label(ui.visuals().error_fg_color, error); }
                }
                FileModal::Unsaved => {
                    ui.heading("Unsaved changes");
                    ui.label("Save changes before continuing?");
                    if self.session.simulation_active() { ui.colored_label(ui.visuals().warn_fg_color, "Stop simulation before saving. Cancel preserves the running simulation."); }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.session.simulation_active() && self.file_io.is_some(), egui::Button::new("Save")).clicked() { choice = Some(Choice::SavePending); }
                        if ui.button("Discard").clicked() { choice = Some(Choice::Discard); }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                    if self.session.simulation_active() && ui.button("Stop simulation").clicked() { choice = Some(Choice::Stop); }
                }
                FileModal::RunningClose => {
                    ui.heading("Simulation is running");
                    ui.label("Closing will stop the simulation. Cancel preserves its current runtime state.");
                    ui.horizontal(|ui| {
                        if ui.button("Stop and continue closing").clicked() { choice = Some(Choice::ContinueClose); }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                }
                FileModal::Overwrite { path, continuation } => {
                    ui.heading("Replace existing file?");
                    ui.label(format!("{} already exists. Replace it?", path.display()));
                    ui.horizontal(|ui| {
                        if ui.button("Replace").clicked() { choice = Some(Choice::ConfirmReplace(path.clone(), *continuation)); }
                        if ui.button("Cancel").clicked() { choice = Some(Choice::Cancel); }
                    });
                }
                FileModal::PublishedWarning { diagnostics, .. } => {
                    ui.heading("File published; durability unconfirmed");
                    ui.label("The new file is published and the session is clean, but durability is unconfirmed. Review all diagnostics before continuing.");
                    for diagnostic in diagnostics { ui.label(diagnostic); }
                    if ui.button("Acknowledge and continue").clicked()
                        && let FileModal::PublishedWarning { continuation, .. } = modal
                    {
                        choice = Some(Choice::Acknowledge(continuation));
                    }
                }
            }
        });
        if let Some(choice) = choice {
            match choice {
                Choice::Cancel => {
                    self.file_modal = None;
                    self.pending_action = None;
                }
                Choice::Open(path) => {
                    self.file_modal = None;
                    self.begin_pending(PendingAction::Open(path));
                }
                Choice::Save(path) => {
                    let continuation = matches!(modal, FileModal::SavePath { continuation: true });
                    self.file_modal = None;
                    self.save_as_path(path, continuation, false);
                }
                Choice::SavePending => {
                    self.file_modal = None;
                    self.begin_save(true);
                }
                Choice::Discard => {
                    self.file_modal = None;
                    self.complete_pending();
                }
                Choice::ConfirmReplace(path, continuation) => {
                    self.file_modal = None;
                    self.save_as_path(path, continuation, true);
                }
                Choice::Acknowledge(continuation) => {
                    self.file_modal = None;
                    if continuation {
                        self.complete_pending();
                    }
                }
                Choice::Stop => {
                    let _ = self.session.stop_simulation();
                    self.settling = false;
                }
                Choice::ContinueClose => {
                    if self.session.simulation_active() {
                        let _ = self.session.stop_simulation();
                    }
                    self.settling = false;
                    self.file_modal = None;
                    if self.session.dirty() {
                        self.file_modal = Some(FileModal::Unsaved);
                    } else {
                        self.complete_pending();
                    }
                }
            }
        }
    }
    fn label_modal_ui(&mut self, ctx: &egui::Context) {
        if self.label_modal.is_none() {
            return;
        }
        let close =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let apply = ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
        });
        if close {
            self.cancel_label_edit();
            return;
        }
        if apply {
            self.finish_label_edit();
        }
        let mut action = None;
        egui::Modal::new(egui::Id::new("component_label_modal")).show(ctx, |ui| {
            ui.set_min_width(380.);
            if let Some(draft) = self.label_modal.as_mut() {
                ui.heading(format!("Edit label — component #{}", draft.id.0));
                ui.add(
                    egui::TextEdit::multiline(&mut draft.text)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("Component label"),
                );
                ui.label(format!("{} / 4096 UTF-8 bytes", draft.text.len()));
                if let Err(error) = crate::domain::validate_label(&draft.text) {
                    ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                }
                if let Some(error) = &draft.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                });
                ui.label("Ctrl/Cmd+Enter applies; Esc cancels.");
            }
        });
        match action {
            Some(true) => self.finish_label_edit(),
            Some(false) => self.cancel_label_edit(),
            None => {}
        }
    }
    #[cfg(test)]
    fn choose_pin(&mut self, pin: PinRef) {
        self.choose_route_target(crate::engine::RouteTarget::Pin(pin));
    }
    fn choose_route_target(&mut self, target: crate::engine::RouteTarget) {
        if let Some(start) = self.geometric_route_start.take() {
            if start == target {
                self.geometric_route_start = Some(start);
                self.error = Some("Choose a different route endpoint".into());
                return;
            }
            match self
                .session
                .route_geometric(start, target, self.route_orientation)
            {
                Ok(_) => self.error = None,
                Err(error) => {
                    self.geometric_route_start = Some(start);
                    self.error = Some(error.to_string());
                }
            }
        } else {
            self.geometric_route_start = Some(target);
        }
        self.focused_route_target = Some(target);
    }
    fn route_target_at(
        &mut self,
        pointer: Pos2,
        transform: CanvasTransform,
        pins: &[(PinRef, [f32; 2])],
    ) -> Option<crate::engine::RouteTarget> {
        let targets = self.route_targets_at(pointer, transform, pins);
        self.focused_route_target
            .filter(|target| targets.contains(target))
            .or_else(|| {
                targets
                    .get(self.route_pick_cycle % targets.len().max(1))
                    .copied()
            })
    }
    fn active_route_target(
        &mut self,
        pointer: Option<Pos2>,
        rect: Rect,
        transform: CanvasTransform,
        pins: &[(PinRef, [f32; 2])],
    ) -> Option<crate::engine::RouteTarget> {
        pointer
            .filter(|point| rect.contains(*point))
            .and_then(|point| self.route_target_at(point, transform, pins))
            .or(self.focused_route_target)
            .or_else(|| self.focused_pin.map(crate::engine::RouteTarget::Pin))
    }
    fn route_targets_at(
        &mut self,
        pointer: Pos2,
        transform: CanvasTransform,
        pins: &[(PinRef, [f32; 2])],
    ) -> Vec<crate::engine::RouteTarget> {
        use crate::engine::RouteTarget;
        use digital_rust::geometric_wiring::Point;
        let Ok(segments) = self.session.document().wires.geometric() else {
            self.error = Some("Cannot pick route targets: invalid wire geometry".into());
            return Vec::new();
        };
        if segments
            .iter()
            .flat_map(|segment| [segment.a, segment.b])
            .any(|point| {
                point.x.unsigned_abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER as u32
                    || point.y.unsigned_abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER as u32
            })
        {
            self.error = Some(format!(
                "Wire routing is limited to exact canvas coordinates within ±{} world units; stored integer wires are still rendered exactly",
                crate::symbol_render::MAX_EXACT_SYMBOL_CENTER
            ));
            return Vec::new();
        }
        let mut choices: Vec<(u8, f32, u64, u8, i32, i32, RouteTarget)> = Vec::new();
        for (pin, pos) in pins {
            let distance = transform.to_screen(*pos).distance(pointer);
            if distance <= 8. {
                choices.push((
                    0,
                    distance,
                    pin.component.0,
                    pin.pin.direction as u8,
                    pin.pin.index as i32,
                    0,
                    RouteTarget::Pin(*pin),
                ));
            }
        }
        for segment in segments {
            for endpoint in [segment.a, segment.b] {
                let distance = transform
                    .to_screen_i32(endpoint.x, endpoint.y)
                    .distance(pointer);
                if distance <= 8. {
                    choices.push((
                        1,
                        distance,
                        endpoint.x as u32 as u64,
                        0,
                        endpoint.y,
                        0,
                        RouteTarget::ExistingEndpoint(endpoint),
                    ));
                }
            }
            let a = transform.to_screen_i32(segment.a.x, segment.a.y);
            let b = transform.to_screen_i32(segment.b.x, segment.b.y);
            let delta = b - a;
            let t = if delta.length_sq() > 0. {
                ((pointer - a).dot(delta) / delta.length_sq()).clamp(0., 1.)
            } else {
                0.
            };
            let nearest = a + delta * t;
            let [wx, wy] = transform.to_world(nearest);
            if wx.is_finite()
                && wy.is_finite()
                && wx >= i32::MIN as f32
                && wx <= i32::MAX as f32
                && wy >= i32::MIN as f32
                && wy <= i32::MAX as f32
            {
                let point = Point::new(wx.round() as i32, wy.round() as i32);
                let on_segment = (i128::from(point.x) - i128::from(segment.a.x))
                    * (i128::from(segment.b.y) - i128::from(segment.a.y))
                    == (i128::from(point.y) - i128::from(segment.a.y))
                        * (i128::from(segment.b.x) - i128::from(segment.a.x))
                    && point.x >= segment.a.x.min(segment.b.x)
                    && point.x <= segment.a.x.max(segment.b.x)
                    && point.y >= segment.a.y.min(segment.b.y)
                    && point.y <= segment.a.y.max(segment.b.y);
                if on_segment
                    && (segment.a.x == segment.b.x || segment.a.y == segment.b.y)
                    && point != segment.a
                    && point != segment.b
                {
                    let distance = transform.to_screen_i32(point.x, point.y).distance(pointer);
                    if distance <= 8. {
                        choices.push((
                            2,
                            distance,
                            segment.id,
                            0,
                            point.x,
                            point.y,
                            RouteTarget::SegmentInterior {
                                segment_id: segment.id,
                                point,
                            },
                        ));
                    }
                }
            }
        }
        choices.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.total_cmp(&b.1))
                .then((a.2, a.3, a.4, a.5).cmp(&(b.2, b.3, b.4, b.5)))
        });
        let Some(priority) = choices.first().map(|choice| choice.0) else {
            return Vec::new();
        };
        unique_route_targets(
            choices
                .into_iter()
                .filter(|choice| choice.0 == priority)
                .map(|choice| choice.6),
        )
    }
    fn route_target_point(
        &mut self,
        target: crate::engine::RouteTarget,
    ) -> Option<digital_rust::geometric_wiring::Point> {
        use crate::engine::RouteTarget;
        match target {
            RouteTarget::Pin(pin) => {
                let result = self
                    .session
                    .document()
                    .components
                    .iter()
                    .find(|c| c.id == pin.component)
                    .ok_or("component no longer exists")
                    .and_then(|c| {
                        digital_rust::pin_geometry::pin_point(c, pin.pin)
                            .map_err(|_| "pin geometry is invalid")
                    });
                match result {
                    Ok(point) => Some(point),
                    Err(error) => {
                        self.error = Some(format!("Invalid wire route target: {error}"));
                        None
                    }
                }
            }
            RouteTarget::ExistingEndpoint(point) => match self.session.document().wires.geometric()
            {
                Ok(segments)
                    if segments
                        .iter()
                        .any(|segment| segment.a == point || segment.b == point) =>
                {
                    Some(point)
                }
                Ok(_) => None,
                Err(error) => {
                    self.error = Some(format!("Invalid wire route target: {error}"));
                    None
                }
            },
            RouteTarget::SegmentInterior { segment_id, point } => {
                match self.session.document().wires.geometric() {
                    Ok(segments)
                        if segments.iter().any(|s| {
                            s.id == segment_id
                                && (s.a.x == s.b.x || s.a.y == s.b.y)
                                && point != s.a
                                && point != s.b
                                && point.x >= s.a.x.min(s.b.x)
                                && point.x <= s.a.x.max(s.b.x)
                                && point.y >= s.a.y.min(s.b.y)
                                && point.y <= s.a.y.max(s.b.y)
                                && (i128::from(point.x) - i128::from(s.a.x))
                                    * (i128::from(s.b.y) - i128::from(s.a.y))
                                    == (i128::from(point.y) - i128::from(s.a.y))
                                        * (i128::from(s.b.x) - i128::from(s.a.x))
                        }) =>
                    {
                        Some(point)
                    }
                    Ok(_) => {
                        self.error = Some(
                            "Invalid wire route target: segment point is stale or non-orthogonal"
                                .into(),
                        );
                        None
                    }
                    Err(error) => {
                        self.error = Some(format!("Invalid wire route geometry: {error}"));
                        None
                    }
                }
            }
        }
    }
    fn sim_state(&self) -> SimState {
        match self.session.inspect() {
            Some(runtime) if runtime.is_pending() => SimState::Pending {
                settling: self.settling,
            },
            Some(_) => SimState::Settled,
            None if self.session.oscillation_diagnostic().is_some() => SimState::Oscillating,
            None if self.failed_start.as_ref() == Some(self.session.document()) => SimState::Error,
            None => SimState::Idle,
        }
    }
    fn report_simulation_error(&mut self, error: &SimulationError) {
        let mut message = describe_simulation_error(self.session.document(), error);
        if let Some(context) = self.session.terminal_simulation_context() {
            message.push_str(&format!(
                " (startup seed {}, phase {:?})",
                context.startup_seed, context.phase
            ));
        }
        self.error = Some(message);
    }
    /// Applies a simulation command through the engine. Starting cancels every transient
    /// editing gesture; a failed start leaves the editor exactly as it was.
    fn apply_simulation_action(&mut self, action: SimAction) {
        match action {
            SimAction::Start | SimAction::StartPaused => {
                let seed = if self.startup_seed_text.is_empty()
                    || !self.startup_seed_text.bytes().all(|b| b.is_ascii_digit())
                {
                    self.error = Some("Startup seed must be unsigned decimal u64".into());
                    return;
                } else {
                    match self.startup_seed_text.parse::<u64>() {
                        Ok(seed) => seed,
                        Err(_) => {
                            self.error = Some("Startup seed is outside the u64 range".into());
                            return;
                        }
                    }
                };
                match self
                    .session
                    .start_simulation_with_options(SimulationOptions {
                        startup_seed: seed,
                        oscillation_limit: crate::runtime::DEFAULT_OSCILLATION_LIMIT,
                    }) {
                    Ok(()) => {
                        self.fragment_draft = None;
                        self.pan_owner = None;
                        self.placing = None;
                        self.wiring = false;
                        self.geometric_route_start = None;
                        self.focused_pin = None;
                        self.drag = None;
                        self.marquee = None;
                        self.failed_start = None;
                        self.error = None;
                        self.settling = action == SimAction::Start;
                        self.microstepping = false;
                    }
                    Err(error) => {
                        if matches!(error, SimulationError::Compile(_)) {
                            self.failed_start = Some(self.session.document().clone());
                        }
                        self.report_simulation_error(&error);
                    }
                }
            }
            SimAction::Stop => match self.session.stop_simulation() {
                Ok(()) => {
                    self.pan_owner = None;
                    self.settling = false;
                    self.microstepping = false;
                    self.error = None;
                }
                Err(error) => self.report_simulation_error(&error),
            },
            SimAction::Step => {
                self.settling = false;
                self.microstepping = true;
            }
            SimAction::Settle => {
                self.microstepping = false;
                if self.session.simulation_active() {
                    self.settling = true;
                }
            }
            SimAction::Pause => {
                self.settling = false;
                self.microstepping = false;
            }
            SimAction::Toggle(id) => match self.session.toggle_input(id) {
                Ok(InputChange::Changed) => {
                    self.settling = true;
                    self.error = None;
                }
                Ok(InputChange::Unchanged) => {}
                Err(error) => self.report_simulation_error(&error),
            },
        }
    }
    /// Runs one bounded settle chunk. While work remains a repaint is requested, so the next
    /// frame continues and Stop is handled in between.
    fn advance_simulation(&mut self, ctx: &egui::Context) {
        if self.microstepping {
            match self
                .session
                .micro_step_simulation_bounded(self.settle_budget)
            {
                Ok(SettleOutcome::BudgetExhausted { .. }) => {}
                Ok(SettleOutcome::TraceSequenceExhausted { .. }) => {
                    self.microstepping = false;
                    self.error =
                        Some("Trace sequence exhausted; simulation was not advanced".into());
                }
                Ok(SettleOutcome::Settled { .. }) => self.microstepping = false,
                Ok(SettleOutcome::Oscillating { .. }) => self.microstepping = false,
                Err(error) => {
                    self.microstepping = false;
                    self.report_simulation_error(&error);
                }
            }
            ctx.request_repaint();
            return;
        }
        if !self.settling {
            return;
        }
        match self.session.settle_simulation(self.settle_budget) {
            Ok(SettleOutcome::BudgetExhausted { .. }) => {}
            Ok(SettleOutcome::TraceSequenceExhausted { .. }) => {
                self.settling = false;
                self.error = Some("Trace sequence exhausted; simulation was not advanced".into());
            }
            Ok(SettleOutcome::Settled { .. }) | Ok(SettleOutcome::Oscillating { .. }) => {
                self.settling = false
            }
            Err(error) => {
                self.settling = false;
                self.report_simulation_error(&error);
            }
        }
        // One more frame also after the last chunk, so the settled state is displayed.
        ctx.request_repaint();
    }
    fn simulation_shortcuts(&mut self, ctx: &egui::Context) {
        if self.seed_field_focused {
            return;
        }
        let action = ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::SHIFT, egui::Key::F5) {
                Some(SimAction::Stop)
            } else if i.consume_key(egui::Modifiers::SHIFT, egui::Key::F10) {
                Some(SimAction::StartPaused)
            } else if i.consume_key(egui::Modifiers::SHIFT, egui::Key::F6) {
                Some(SimAction::Pause)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::F5) {
                Some(SimAction::Start)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::F10) {
                Some(SimAction::Step)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::F6) {
                Some(SimAction::Settle)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.apply_simulation_action(action);
        }
    }
    fn simulation_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("simulation").show(ctx, |ui| {
            ui.heading("Simulation");
            let active = self.session.simulation_active();
            let modal_blocked = self.modality_active();
            let mut action = None;
            ui.horizontal_wrapped(|ui| {
                for (label, enabled, hint, command) in [
                    (
                        "Start",
                        !active && !modal_blocked,
                        "Start (F5)",
                        SimAction::Start,
                    ),
                    (
                        "Stop",
                        active && !modal_blocked,
                        "Stop (Shift+F5)",
                        SimAction::Stop,
                    ),
                    (
                        "Microstep",
                        active && !modal_blocked,
                        "One microstep (F10)",
                        SimAction::Step,
                    ),
                    (
                        "Full step",
                        active && !modal_blocked,
                        "Run to stable state (F6)",
                        SimAction::Settle,
                    ),
                ] {
                    if ui
                        .add_enabled(enabled, egui::Button::new(label))
                        .on_hover_text(hint)
                        .clicked()
                    {
                        action = Some(command);
                    }
                }
            });
            ui.label(match self.sim_state() {
                SimState::Idle => "State: idle (not simulating)",
                SimState::Error => "State: error (cannot start, see message)",
                SimState::Oscillating => "State: stopped (oscillation detected; see diagnostic)",
                SimState::Pending { settling: true } if self.session.startup_phase() == Some(crate::runtime::StartupPhase::StartupSettling) => "State: initializing (randomized startup settling)",
                SimState::Pending { settling: false } if self.session.startup_phase() == Some(crate::runtime::StartupPhase::StartupSettling) => "State: initializing (paused; Step or Settle)",
                SimState::Pending { settling: true } => "State: pending (settling)",
                SimState::Pending { settling: false } => "State: pending (paused; Step or Settle)",
                SimState::Settled => "State: settled",
            });
            if let Some(diagnostic) = self.session.oscillation_diagnostic() {
                ui.colored_label(ui.visuals().error_fg_color, describe_latched_oscillation(self.session.document(), diagnostic));
                if let Some(context) = self.session.terminal_simulation_context() {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Startup seed {} (failure phase {:?})", context.startup_seed, context.phase));
                }
            }
            ui.label(format!("Randomized startup is deterministic for the displayed seed; policy: {} (SplitMix64 + Fisher–Yates, sorted component IDs). Not Java trace-compatible.", crate::runtime::STARTUP_RANDOM_POLICY));
            ui.horizontal(|ui| {
                ui.label("Startup seed");
                let seed_had_focus = self.seed_field_focused;
                let response = ui.add_enabled(!active, egui::TextEdit::singleline(&mut self.startup_seed_text).id_salt("simulation_startup_seed").desired_width(140.0).hint_text("unsigned decimal u64"));
                self.seed_field_id = Some(response.id);
                if self.seed_focus_requested {
                    response.request_focus();
                    self.seed_focus_requested = false;
                }
                if seed_had_focus && response.lost_focus() && ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                    response.request_focus();
                }
                if response.has_focus()
                    && ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    && let Some(canvas_id) = self.canvas_id
                {
                    ctx.memory_mut(|memory| memory.request_focus(canvas_id));
                }
                self.seed_field_focused = response.has_focus();
                self.seed_field_rect = Some(response.rect);
            });
            if let Some(seed) = self.session.startup_seed() { ui.label(format!("Active startup seed: {seed}")); }
            if let Some(phase) = self.session.startup_phase() { ui.label(format!("Lifecycle: {phase:?}")); }
            ui.label("F5 start, Shift+F10 start paused, Shift+F6 pause, Shift+F5 stop, F10 microstep, F6 full settle. Canvas Ctrl+Tab → seed field; Escape → canvas.");
            if let Some(runtime) = self.session.inspect() {
                let pending = runtime.pending_inspection();
                let document = self.session.document();
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("simulation_values")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label("Inputs (toggle when settled)");
                        for component in &document.components {
                            if component.kind != ComponentKind::Input {
                                continue;
                            }
                            let level = runtime
                                .pin_value(PinRef {
                                    component: component.id,
                                    pin: PinId {
                                        direction: PinDirection::Output,
                                        index: 0,
                                    },
                                })
                                .unwrap_or(Level::Unavailable);
                            let text = format!(
                                "{} #{}: {}",
                                component.label,
                                component.id.0,
                                level_text(level)
                            );
                            if ui
                                .add_enabled(
                                    !runtime.is_pending() && !modal_blocked,
                                    egui::Button::new(text),
                                )
                                .clicked()
                            {
                                action = Some(SimAction::Toggle(component.id));
                            }
                        }
                        ui.separator();
                        ui.label("Outputs");
                        for component in &document.components {
                            if component.kind == ComponentKind::Output {
                                ui.label(format!(
                                    "{} #{}: {}",
                                    component.label,
                                    component.id.0,
                                    runtime
                                        .output_value(component.id)
                                        .map_or("? (unavailable)", level_text)
                                ));
                            }
                        }
                        ui.separator();
                        ui.label("Gates");
                        for component in &document.components {
                            if matches!(
                                component.kind,
                                ComponentKind::And
                                    | ComponentKind::NAnd
                                    | ComponentKind::Or
                                    | ComponentKind::NOr
                                    | ComponentKind::XOr
                                    | ComponentKind::XNOr
                                    | ComponentKind::Not
                            ) {
                                let level = runtime
                                    .pin_value(PinRef {
                                        component: component.id,
                                        pin: PinId {
                                            direction: PinDirection::Output,
                                            index: 0,
                                        },
                                    })
                                    .unwrap_or(Level::Unavailable);
                                ui.label(format!(
                                    "{} gate id {}: {}",
                                    component.kind.name(),
                                    component.id.0,
                                    level_text(level)
                                ));
                            }
                        }
                        ui.collapsing(format!("Pending gates: {} unique", pending.unique_count), |ui| {
                            ui.label(format!("Awaiting evaluation ({}): {:?}", pending.awaiting_evaluation.len(), pending.awaiting_evaluation.iter().map(|id| id.0).collect::<Vec<_>>()));
                            ui.label(format!("Staged, unpublished ({}): {:?}", pending.staged_unpublished.len(), pending.staged_unpublished.iter().map(|id| id.0).collect::<Vec<_>>()));
                            ui.label(format!("Next frontier ({}): {:?}", pending.next_scheduled.len(), pending.next_scheduled.iter().map(|id| id.0).collect::<Vec<_>>()));
                        });
                    });
            }
            if let Some(action) = action.filter(|_| !modal_blocked) {
                self.apply_simulation_action(action);
            }
        });
    }
    fn palette_panel(&mut self, ctx: &egui::Context) {
        let mut acknowledge_warning = None;
        let mut path_choice: Option<(bool, std::path::PathBuf, bool)> = None;
        let mut cancel_path = false;
        let mut unsaved_choice = None;
        let mut running_close_choice = None;
        egui::SidePanel::left("palette").show(ctx, |ui| {
            ui.heading("Components");
            let locked = self.session.simulation_active() || self.modality_active() || self.fragment_draft.is_some();
            ui.add_enabled_ui(!locked, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for kind in KINDS {
                        if ui
                            .selectable_label(self.placing == Some(kind), kind.name())
                            .clicked()
                        {
                            self.placing = Some(kind);
                            self.wiring = false;
                                    self.geometric_route_start = None;
                        }
                    }
                    if ui.add(egui::Button::new("Wire (W)").selected(self.wiring))
                        .on_hover_text("Create a wire; Shift+W flips the L-route orientation")
                        .clicked() {
                        self.wiring = !self.wiring;
                                        self.geometric_route_start = None;
                        self.placing = None;
                        self.drag = None;
                    }
                    if self.wiring {
                        if ui.button("Flip route (Shift+W)").clicked() {
                            self.route_orientation = match self.route_orientation {
                                digital_rust::geometric_wiring::RouteOrientation::HorizontalFirst => digital_rust::geometric_wiring::RouteOrientation::VerticalFirst,
                                digital_rust::geometric_wiring::RouteOrientation::VerticalFirst => digital_rust::geometric_wiring::RouteOrientation::HorizontalFirst,
                            };
                        }
                        ui.label(match self.route_orientation {
                            digital_rust::geometric_wiring::RouteOrientation::HorizontalFirst => "Horizontal first",
                            digital_rust::geometric_wiring::RouteOrientation::VerticalFirst => "Vertical first",
                        });
                        ui.label("Tab/Shift+Tab navigates targets or cycles overlap under pointer; Enter starts/finishes; Esc cancels");
                        if let Some(target) = self.focused_route_target {
                            ui.label(format!("Focused wire target: {}", route_target_description(target)));
                        }
                        if let Some(target) = self.geometric_route_start {
                            ui.label(format!("Route starts at {}", route_target_description(target)));
                        }
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                let nav_enabled = self.drag.is_none() && !self.modality_active();
                if ui.add_enabled(nav_enabled, egui::Button::new("− Zoom out")).on_hover_text("Zoom out (−)").clicked() { self.view_action = Some(ViewAction::ZoomOut); }
                if ui.add_enabled(nav_enabled, egui::Button::new("+ Zoom in")).on_hover_text("Zoom in (+)").clicked() { self.view_action = Some(ViewAction::ZoomIn); }
                if ui.add_enabled(nav_enabled, egui::Button::new("Fit")).on_hover_text("Fit circuit (F)").clicked() { self.view_action = Some(ViewAction::Fit); }
                if ui.add_enabled(nav_enabled, egui::Button::new("Reset")).on_hover_text("Reset view (0)").clicked() { self.view_action = Some(ViewAction::Reset); }
                let can_rotate = !self.session.simulation_active() && !self.modality_active()
                    && self.drag.is_none() && self.marquee.is_none() && self.pan_owner.is_none()
                    && !self.wiring && self.placing.is_none() && self.fragment_draft.is_none()
                    && (!self.selection.is_empty() || self.selected.is_some());
                if ui.add_enabled(can_rotate, egui::Button::new("Rotate selection ↶")).on_hover_text("Rotate selected components and wire endpoints counterclockwise (R)").clicked() {
                    let mut selection = self.selection.clone();
                    if selection.is_empty() && let Some(id)=self.selected { selection.select_component(id,false,false); }
                    match self.session.transform_selection(&selection,1,false) { Ok(_) => self.reconcile_selection(), Err(error) => self.error=Some(error.to_string()) }
                }
            });
            ui.label(format!("{:.0}% · Middle/Alt-drag pan · wheel zoom · +/- zoom · F fit · 0 reset · arrows pan", self.view.zoom() * 100.));
            if locked {
                if self.session.simulation_active() {
                    ui.label(EDIT_LOCK_NOTE);
                } else if self.fragment_draft.is_some() {
                    ui.label("Place clipboard fragment: move pointer, arrows nudge 20, click/Enter commits, Escape cancels.");
                } else {
                    ui.label("Finish or cancel the file action to resume editing.");
                }
            }
            ui.separator();
            ui.add_enabled_ui(!locked, |ui| {
                ui.label("Shape style");
                ui.radio_value(&mut self.style, SymbolStyle::Rect, "Rectangular");
                ui.radio_value(&mut self.style, SymbolStyle::Ieee, "IEEE");
            });
            ui.separator();
            ui.label("Click canvas to place. Drag to move. Escape cancels. Delete removes; Ctrl/Cmd-Z undoes; Shift-Ctrl/Cmd-Z redoes.");
            ui.label(if self.session.dirty() { "Modified" } else { "Unmodified" });
            if self.selection.wires.is_empty()
                && (self.selection.components.len() == 1 || (self.selection.components.is_empty() && self.selected.is_some()))
                && let Some(id) = self.selected.or_else(|| self.selection.components.iter().next().copied()) {
                let label = self.session.document().components.iter().find(|component| component.id == id).map(|component| component.label.clone());
                if let Some(label) = label {
                    ui.label(format!("Selected label: {label}"));
                    let constant_value = self.session.document().components.iter().find(|component| component.id == id && component.kind == ComponentKind::Const).map(|component| component.constant_value);
                    if let Some(value) = constant_value {
                        ui.label(format!("Const configured value: {}", if value { "HIGH (1)" } else { "LOW (0)" }));
                        ui.label("Canvas keyboard: H = HIGH (1), L = LOW (0)");
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!locked && value, egui::Button::new("Set LOW (0)" )).clicked() { let _ = self.session.set_constant_value(id, false); }
                            if ui.add_enabled(!locked && !value, egui::Button::new("Set HIGH (1)" )).clicked() { let _ = self.session.set_constant_value(id, true); }
                        });
                    }
                    if ui.add_enabled(!locked, egui::Button::new("Edit label… (F2)")).clicked() {
                        self.begin_label_edit();
                    }
                }
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(error) = &self.clipboard_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(error) = &self.file_io_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(FileModal::PublishedWarning { diagnostics, continuation }) = self.file_modal.clone() {
                ui.colored_label(ui.visuals().warn_fg_color, "Published; durability unconfirmed. Session is clean; review all diagnostics.");
                egui::ScrollArea::vertical().max_height(180.).show(ui, |ui| {
                    for diagnostic in diagnostics { ui.label(diagnostic); }
                });
                if ui.button("Acknowledge and continue").clicked() { acknowledge_warning = Some(continuation); }
            }
            if matches!(self.file_modal, Some(FileModal::RunningClose)) {
                ui.separator();
                ui.heading("Simulation is running");
                ui.label("Closing will stop the simulation. Cancel preserves its current runtime state.");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Stop and continue closing").clicked() { running_close_choice = Some(true); }
                    if ui.button("Cancel").clicked() { running_close_choice = Some(false); }
                });
            }
            if matches!(self.file_modal, Some(FileModal::Unsaved)) {
                ui.separator();
                ui.heading("Unsaved changes");
                ui.label("Save changes before continuing?");
                if self.session.simulation_active() {
                    ui.colored_label(ui.visuals().warn_fg_color, "Stop simulation before saving. Cancel preserves the running simulation.");
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!self.session.simulation_active() && self.file_io.is_some(), egui::Button::new("Save")).clicked() { unsaved_choice = Some(0); }
                    if ui.button("Discard").clicked() { unsaved_choice = Some(1); }
                    if ui.button("Cancel").clicked() { unsaved_choice = Some(2); }
                    if self.session.simulation_active() && ui.button("Stop simulation").clicked() { unsaved_choice = Some(3); }
                });
            }
            match self.file_modal.clone() {
                Some(FileModal::OpenPath) | Some(FileModal::SavePath { .. }) => {
                    let saving = matches!(self.file_modal, Some(FileModal::SavePath { .. }));
                    let continuation = matches!(self.file_modal, Some(FileModal::SavePath { continuation: true }));
                    ui.separator();
                    ui.heading(if saving { "Save As" } else { "Open document" });
                    ui.label("Path relative to the application's startup directory, or an absolute path:");
                    ui.text_edit_singleline(&mut self.file_path_entry);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(if saving { "Save path" } else { "Open path" }).clicked() {
                            path_choice = Some((saving, std::path::PathBuf::from(self.file_path_entry.as_str()), continuation));
                        }
                        if ui.button("Cancel path").clicked() { cancel_path = true; }
                    });
                }
                _ => {}
            }
            ui.separator();
            let document_name = self
                .session
                .path()
                .and_then(std::path::Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into());
            ui.heading(format!(
                "{}{}",
                if self.session.dirty() { "• " } else { "" },
                document_name
            ));
            let mut file_action = None;
            let mut edit_action = None;
            if !self.modality_active() {
            ui.menu_button("File", |ui| {
                for (label, enabled, action) in [("New  Ctrl/Cmd+N", !locked, 0), ("Open…  Ctrl/Cmd+O", !locked, 1), ("Save  Ctrl/Cmd+S", !locked, 2), ("Save As…  Shift+Ctrl/Cmd+S", !locked, 3)] {
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() { file_action = Some(action); ui.close_menu(); }
                }
            });
            ui.menu_button("Edit", |ui| {
                for (label, action) in [("Copy  Ctrl/Cmd+C", 0), ("Cut  Ctrl/Cmd+X", 1), ("Paste  Ctrl/Cmd+V", 2), ("Duplicate  Ctrl/Cmd+D", 3)] {
                    if ui.add_enabled(!locked && self.fragment_draft.is_none(), egui::Button::new(label)).clicked() {
                        edit_action = Some(action); ui.close_menu();
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (label, action) in [("New", 0), ("Open", 1), ("Save", 2), ("Save As", 3)] {
                    if ui.add_enabled(!locked, egui::Button::new(label)).clicked() { file_action = Some(action); }
                }
            });
            if let Some(action) = edit_action {
                self.run_clipboard_action(match action {
                    0 => ClipboardAction::Copy, 1 => ClipboardAction::Cut,
                    2 => ClipboardAction::Paste(None), _ => ClipboardAction::Duplicate,
                });
            }
            if let Some(action) = file_action {
                match action {
                    0 => self.begin_pending(PendingAction::New),
                    1 => self.open_path_dialog(),
                    2 => self.begin_save(false),
                    _ => self.save_path_dialog(false)
                }
            }
            }
        });
        if let Some(continuation) = acknowledge_warning {
            self.file_modal = None;
            if continuation {
                self.complete_pending();
            }
        }
        if cancel_path {
            self.file_modal = None;
            self.pending_action = None;
        }
        match unsaved_choice {
            Some(0) => {
                self.file_modal = None;
                self.begin_save(true);
            }
            Some(1) => {
                self.file_modal = None;
                self.complete_pending();
            }
            Some(2) => {
                self.file_modal = None;
                self.pending_action = None;
            }
            Some(3) => {
                let _ = self.session.stop_simulation();
                self.settling = false;
            }
            _ => {}
        }
        if let Some(continue_close) = running_close_choice {
            if continue_close {
                let _ = self.session.stop_simulation();
                self.settling = false;
                self.file_modal = None;
                if self.session.dirty() {
                    self.file_modal = Some(FileModal::Unsaved);
                } else {
                    self.complete_pending();
                }
            } else {
                self.file_modal = None;
                self.pending_action = None;
            }
        }
        if let Some((saving, path, continuation)) = path_choice {
            self.file_modal = None;
            if saving {
                self.save_as_path(path, continuation, false);
            } else {
                self.begin_pending(PendingAction::Open(path));
            }
        }
    }
    fn show(&mut self, ctx: &egui::Context) {
        if self.session.oscillation_diagnostic().is_none()
            && self
                .error
                .as_deref()
                .is_some_and(|text| text.starts_with("Circuit appears to oscillate"))
        {
            self.error = None;
        }
        self.file_actions(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.approved_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.cancel_label_edit();
            self.begin_pending(PendingAction::Close);
        }
        if self.approved_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.label_modal.is_some() && ctx.input(|input| !input.focused) {
            self.cancel_label_edit();
        }
        let modal_open = self.modality_active();
        if !modal_open {
            self.simulation_shortcuts(ctx);
        }
        self.palette_panel(ctx);
        self.simulation_panel(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            let locked = self.session.simulation_active();
            let (rect, response) =
                ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            if self.modality_active() {
                self.view_action = None;
            }
            if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 { self.canvas_rect = None; return; }
            self.canvas_rect = Some(rect);
            self.canvas_id = Some(response.id);
            // Placement owns keyboard input only while the canvas retains focus. A click on
            // another widget (notably the startup-seed editor) cancels placement instead of
            // stealing that widget's Enter/text input.
            let focus_check_pending = std::mem::replace(&mut self.fragment_focus_check_pending, false);
            if self.fragment_draft.is_some() && response.has_focus() {
                self.fragment_canvas_had_focus = true;
            }
            if self.fragment_draft.is_some()
                && self.fragment_focus_claimed
                && !focus_check_pending
                && (self.seed_field_focused
                    || (self.fragment_canvas_had_focus
                        && ctx.memory(|memory| memory.focused()).is_some_and(|focused| focused != response.id)))
            {
                self.fragment_draft = None;
                self.fragment_focus_claimed = false;
            }
            // A queued navigation action may outlive the frame/UI event that created it.
            // Revalidate modality at the point of execution, just like the toolbar controls.
            if let Some(action) = self
                .view_action
                .take()
                .filter(|_| self.drag.is_none() && !self.modality_active())
            {
                match action {
                    ViewAction::Reset => self.view.reset(),
                    ViewAction::ZoomIn | ViewAction::ZoomOut => {
                        let pointer = ctx.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)).unwrap_or(rect.center());
                        let factor = if action == ViewAction::ZoomIn { 1.2 } else { 1.0 / 1.2 };
                        let _ = self.view.zoom_at(rect, pointer, factor);
                    }
                    ViewAction::Fit => {
                        match document_view_bounds(self.session.document(), self.style, &ui.painter_at(rect)) {
                            Ok(Some((min, max, margin))) => self.fit_extent(rect, min, max, margin),
                            Ok(None) => self.view.reset(),
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
            }
            if self.fit_view_pending {
                match document_view_bounds(self.session.document(), self.style, &ui.painter_at(rect)) {
                    Ok(Some((min, max, margin))) => { self.fit_extent(rect, min, max, margin); self.fit_view_pending = false; }
                    Ok(None) => { self.view.reset(); self.fit_view_pending = false; }
                    Err(error) => { self.error = Some(error); self.fit_view_pending = false; }
                }
            }
            let style = self.style;
            let lost_pointer = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::PointerGone)));
            let cancelled = modal_open || ctx.input(|i| !i.focused || i.key_pressed(egui::Key::Escape)) || lost_pointer;
            if cancelled {
                self.pan_owner = None;
                self.drag = None;
                self.marquee = None;
                self.placing = None;
                self.fragment_draft = None;
                self.fragment_focus_claimed = false;
                self.fragment_canvas_had_focus = false;
                self.geometric_route_start = None;
            }
            // Gesture ownership is acquired only when unowned. A chorded/extra press must
            // never replace the button whose release terminates the current pan.
            let pan_start = (self.pan_owner.is_none() && !cancelled && self.drag.is_none() && self.fragment_draft.is_none()).then(|| ctx.input(|i| i.events.iter().find_map(|event| match event {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Middle, pressed: true, .. } if rect.contains(*pos) => Some((egui::PointerButton::Middle, *pos)),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers, .. } if modifiers.alt && rect.contains(*pos) => Some((egui::PointerButton::Primary, *pos)),
                _ => None,
            }))).flatten();
            let starting_pan = pan_start.is_some();
            if let Some((owner, _)) = pan_start { self.pan_owner = Some(owner); }
            let pan_sequence = starting_pan || self.pan_owner.is_some();
            if pan_sequence && !starting_pan && !cancelled {
                let delta = ctx.input(|i| i.pointer.delta());
                let _ = self.view.pan(delta);
            }
            let owner_released = ctx.input(|i| i.events.iter().any(|event| matches!(event,
                egui::Event::PointerButton { button, pressed: false, .. } if Some(*button) == self.pan_owner)));
            if owner_released { self.pan_owner = None; }
            if !modal_open && !pan_sequence && self.drag.is_none()
                && let Some(pointer) = ctx.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p))
            {
                let (scroll, pinch) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
                let factor = ((scroll as f64 * 0.002).exp() * pinch as f64) as f32;
                if factor.is_finite() && factor > 0.0 && (scroll != 0.0 || pinch != 1.0) {
                    let _ = self.view.zoom_at(rect, pointer, factor);
                }
            }
            if !modal_open && !pan_sequence && self.drag.is_none() && self.fragment_draft.is_none() && self.marquee.is_none() && !self.wiring && self.placing.is_none() && response.has_focus() {
                // Ctrl+Tab is a documented route to the seed editor that does not steal
                // plain Tab from keyboard pin navigation; Escape returns focus here.
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab)) {
                    self.seed_focus_requested = true;
                    if let Some(seed_id) = self.seed_field_id {
                        ctx.memory_mut(|memory| memory.request_focus(seed_id));
                    }
                }
                if !cancelled
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::R))
                {
                    let mut selection = self.selection.clone();
                    if selection.is_empty() && let Some(id)=self.selected { selection.select_component(id,false,false); }
                    match self.session.transform_selection(&selection,1,false) { Ok(_) => self.reconcile_selection(), Err(error) => self.error=Some(error.to_string()) }
                }
                let nav = ctx.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Plus)
                        || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Plus)
                        || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Equals) { Some(ViewAction::ZoomIn) }
                    else if i.consume_key(egui::Modifiers::NONE, egui::Key::Minus) { Some(ViewAction::ZoomOut) }
                    else if i.consume_key(egui::Modifiers::NONE, egui::Key::F) { Some(ViewAction::Fit) }
                    else if i.consume_key(egui::Modifiers::NONE, egui::Key::Num0) { Some(ViewAction::Reset) }
                    else { None }
                });
                if let Some(action) = nav {
                    match action {
                        ViewAction::ZoomIn | ViewAction::ZoomOut => { let _ = self.view.zoom_at(rect, rect.center(), if action == ViewAction::ZoomIn { 1.2 } else { 1.0 / 1.2 }); }
                        ViewAction::Reset => self.view.reset(),
                        ViewAction::Fit => match document_view_bounds(self.session.document(), self.style, &painter) {
                            Ok(Some((min,max,margin))) => self.fit_extent(rect,min,max,margin),
                            Ok(None) => self.view.reset(),
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
                let pan_keys = ctx.input(|i| i.events.iter().find_map(|event| match event {
                    egui::Event::Key { key, pressed: true, modifiers, .. }
                        if *modifiers == egui::Modifiers::NONE
                            && matches!(key, egui::Key::ArrowLeft|egui::Key::ArrowRight|egui::Key::ArrowUp|egui::Key::ArrowDown) => Some(*key),
                    _ => None,
                }));
                if let Some(key) = pan_keys {
                    let delta = match key { egui::Key::ArrowLeft => Vec2::new(40.,0.), egui::Key::ArrowRight => Vec2::new(-40.,0.), egui::Key::ArrowUp => Vec2::new(0.,40.), _ => Vec2::new(0.,-40.) };
                    let _ = self.view.pan(delta);
                }
            }
            let Some(transform) = self.view.transform(rect) else { return; };
            draw_grid(&painter, rect, transform, ui.visuals().widgets.noninteractive.bg_stroke.color);
            let pointer = (!cancelled && !pan_sequence).then(|| response.interact_pointer_pos()).flatten();
            let draft_pointer = (!cancelled && !pan_sequence)
                .then(|| ctx.input(|input| input.pointer.hover_pos()).filter(|point| rect.contains(*point)))
                .flatten();
            let pointer_moved_over_canvas = draft_pointer.is_some()
                && ctx.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::PointerMoved(_))));
            if let Some(draft) = &mut self.fragment_draft {
                if pointer_moved_over_canvas { draft.keyboard_adjusted = false; }
                if !draft.keyboard_adjusted && let Some(target) = draft_pointer.or_else(|| draft.center_fallback.then_some(rect.center())) {
                    let world = transform.to_world(target);
                    let dx = ((f64::from(world[0]) - draft.anchor[0]) / 20.0).round() * 20.0;
                    let dy = ((f64::from(world[1]) - draft.anchor[1]) / 20.0).round() * 20.0;
                    if dx.is_finite() && dy.is_finite() && dx >= i32::MIN as f64 && dx <= i32::MAX as f64 && dy >= i32::MIN as f64 && dy <= i32::MAX as f64 {
                        draft.delta = [dx as i32, dy as i32];
                        draft.center_fallback = false;
                    }
                }
            }
            let mut pins: Vec<_> = self
                .session
                .document()
                .components
                .iter()
                .flat_map(|c| {
                    let layout =
                        SymbolLayout::oriented_on(style, c.kind, [c.position.x, c.position.y], crate::domain::Orientation { rotation: c.rotation, mirror: c.mirror });
                    [PinDirection::Input, PinDirection::Output]
                        .into_iter()
                        .flat_map(move |direction| {
                            (0..2).filter_map(move |index| {
                                let pin = PinId { direction, index };
                                layout.and_then(|l| l.pin_position(pin)).map(|p| {
                                    (
                                        PinRef {
                                            component: c.id,
                                            pin,
                                        },
                                        p,
                                    )
                                })
                            })
                        })
                })
                .collect();
            let press = ctx.input(|i| {
                i.events.iter().find_map(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers,
                    } => Some((*pos, *modifiers)),
                    _ => None,
                })
            });
            if !cancelled && !pan_sequence
                && let Some((p, modifiers)) = press
                && rect.contains(p)
            {
                response.request_focus();
                if self.fragment_draft.is_some() {
                    // Fragment placement owns the pointer; editing hit targets are suppressed.
                } else if self.wiring && !locked {
                    let hit = self.route_target_at(p, transform, &pins);
                    if let Some(target) = hit {
                        self.choose_route_target(target);
                    }
                } else if self.placing.is_none() {
                    let world = transform.to_world(p);
                    let tolerance = transform.length_to_world(PICK_TOLERANCE);
                    let hit = self
                        .session
                        .document()
                        .components
                        .iter()
                        .rev()
                        .find(|c| {
                            SymbolLayout::oriented_on(style, c.kind, [c.position.x, c.position.y], crate::domain::Orientation { rotation: c.rotation, mirror: c.mirror })
                                .is_some_and(|layout| layout.hit(world, tolerance))
                        })
                        .map(|c| (c.id, c.position));
                    self.selected_segment = if hit.is_none() {
                        match self.session.document().wires.geometric() {
                            Ok(segments) => segments.iter().rev().find(|segment| {
                                crate::wire_geometry::hit(
                                    &[
                                        { let p = transform.to_screen_i32(segment.a.x, segment.a.y); [p.x, p.y] },
                                        { let p = transform.to_screen_i32(segment.b.x, segment.b.y); [p.x, p.y] },
                                    ],
                                    [p.x, p.y],
                                    6.0,
                                )
                            }).map(|segment| segment.id),
                            Err(error) => { self.error = Some(format!("Cannot select wire: {error}")); None }
                        }
                    } else { None };
                    if let Some((id, _)) = hit {
                        let toggle=modifiers.command || modifiers.ctrl;
                        if modifiers.shift || toggle || !self.selection.components.contains(&id) {
                            self.selection.select_component(id, modifiers.shift, toggle);
                        }
                        self.selected = (self.selection.components.len() == 1)
                            .then(|| self.selection.components.iter().next().copied())
                            .flatten();
                        self.selected_segment = None;
                        self.drag = (!locked && self.selection.components.contains(&id)).then(|| (id, hit.map(|v| v.1).unwrap_or(Position{x:0.,y:0.}), p));
                    } else if let Some(id) = self.selected_segment {
                        let toggle=modifiers.command || modifiers.ctrl;
                        if modifiers.shift || toggle || !self.selection.wires.contains_key(&id) {
                            self.selection.select_wire_endpoint(id, crate::selection::EndpointMask::BOTH, modifiers.shift, toggle);
                        }
                        self.selected = None;
                        self.drag = (!locked).then_some((ComponentId(u64::MAX), Position{x:0.,y:0.}, p));
                        if !modifiers.shift && !(modifiers.command || modifiers.ctrl) { self.selected_segment = Some(id); }
                    } else {
                        // Keep the old selection while a marquee is only a preview. Release
                        // applies its modifier semantics; cancellation leaves it untouched.
                        self.marquee = Some((p, modifiers));
                    }
                    if locked
                        && let Some((id, _)) = hit
                        && self
                            .session
                            .document()
                            .components
                            .iter()
                            .any(|c| c.id == id && c.kind == ComponentKind::Input)
                    {
                        self.apply_simulation_action(SimAction::Toggle(id));
                    }
                }
            }
            if !cancelled && !pan_sequence && !locked && response.clicked() && self.fragment_draft.is_some() {
                self.commit_fragment_draft();
            }
            if !cancelled && !pan_sequence
                && !locked
                && response.clicked()
                && self.fragment_draft.is_none()
                && let Some(kind) = self.placing.take()
                && let Some(p) = pointer
            {
                let [x, y] = transform.to_world(p);
                match self.session.place(kind, snap_component_center(style, kind, x, y)) {
                    Ok(id) => {
                        self.selection.clear();
                        self.selection.components.insert(id);
                        self.selected = Some(id);
                        self.selected_segment = None;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            if !cancelled && !pan_sequence && response.clicked() {
                self.drag = None;
                if let Some((_, modifiers)) = self.marquee.take() {
                    self.selection.apply_marquee(
                        crate::selection::Selection::default(),
                        modifiers.shift,
                        modifiers.command || modifiers.ctrl,
                    );
                    self.selected = if self.selection.components.len() == 1 && self.selection.wires.is_empty() {
                        self.selection.components.iter().next().copied()
                    } else { None };
                    self.selected_segment = if self.selection.wires.len() == 1 {
                        self.selection.wires.keys().next().copied()
                    } else { None };
                }
            }
            if !cancelled && !pan_sequence
                && !locked
                && response.drag_stopped()
                && let (Some((_id, start, origin)), Some(p)) = (self.drag.take(), pointer)
            {
                let [dx, dy] = transform.delta_to_world(p - origin);
                // A multi-selection uses one common grid delta; preserve the established single
                // component snap origin for asymmetric artwork placements.
                let (snapped_dx, snapped_dy) = if self.selection.components.len()==1 && self.selection.wires.is_empty() {
                    let id=self.selection.components.iter().next().copied();
                    let component=id.and_then(|id| self.session.document().components.iter().find(|c| c.id==id));
                    component.map(|component| { let snapped=snap_component_center_oriented(style,component.kind,crate::domain::Orientation{rotation:component.rotation,mirror:component.mirror},start.x+dx,start.y+dy); ((snapped.x-start.x) as i32,(snapped.y-start.y) as i32) }).unwrap_or(((dx/20.).round() as i32*20,(dy/20.).round() as i32*20))
                } else { ((dx/20.).round() as i32*20,(dy/20.).round() as i32*20) };
                match self.session.move_selection(&self.selection, snapped_dx, snapped_dy) {
                    Ok(_) => {
                        self.reconcile_selection();
                        for (pin, pos) in &mut pins {
                            if self.selection.components.contains(&pin.component) {
                                pos[0] += snapped_dx as f32;
                                pos[1] += snapped_dy as f32;
                            }
                        }
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            if !cancelled && !pan_sequence && !locked
                && response.drag_stopped()
                && let (Some((start, modifiers)), Some(end)) = (self.marquee.take(), pointer)
            {
                let world_rect = crate::selection::Rect::normalized(transform.to_world(start), transform.to_world(end));
                let doc = self.session.document();
                let components = doc.components.iter().filter_map(|c| {
                    SymbolLayout::oriented_on(style, c.kind, [c.position.x,c.position.y], crate::domain::Orientation { rotation: c.rotation, mirror: c.mirror }).map(|layout| { let b=layout.bounds(); (c.id,b.min,b.max) })
                }).collect::<Vec<_>>();
                let wires = doc.wires.geometric().map(|s| s.to_vec()).unwrap_or_default();
                self.selection.apply_marquee(crate::selection::marquee(components,&wires,world_rect), modifiers.shift, modifiers.command || modifiers.ctrl);
                self.selected = if self.selection.components.len()==1 && self.selection.wires.is_empty() { self.selection.components.iter().next().copied() } else { None };
                self.selected_segment = if self.selection.wires.len()==1 { self.selection.wires.keys().next().copied() } else { None };
            }
            let palette = Palette::from_visuals(ui.visuals());
            if let Some((start, _)) = self.marquee
                && let Some(end) = pointer
            {
                let selection_rect = Rect::from_two_pos(start, end);
                painter.rect_stroke(selection_rect, 0.0, Stroke::new(1.0_f32, palette.selection), egui::StrokeKind::Middle);
            }
            let gesture_delta = if let (Some((drag_id,start,origin)),Some(p))=(self.drag,pointer) {
                let [dx,dy]=transform.delta_to_world(p-origin);
                if self.selection.components.len()==1 && self.selection.wires.is_empty() && drag_id != ComponentId(u64::MAX) {
                    let component=self.selection.components.iter().next().and_then(|id| self.session.document().components.iter().find(|c| c.id==*id));
                    component.map(|component| { let snapped=snap_component_center_oriented(style,component.kind,crate::domain::Orientation{rotation:component.rotation,mirror:component.mirror},start.x+dx,start.y+dy); [snapped.x-start.x,snapped.y-start.y] }).unwrap_or([((dx/20.).round()*20.),((dy/20.).round()*20.)])
                } else { [((dx/20.).round()*20.),((dy/20.).round()*20.)] }
            } else {[0.,0.]};
            // Use a single snapped preview delta for every selected symbol and endpoint.
            let wire_pins: Vec<_> = pins
                .iter()
                .map(|(pin, pos)| {
                    let mut pos = *pos;
                    if let (Some((_id, _, origin)), Some(p)) = (self.drag, pointer)
                        && self.selection.components.contains(&pin.component)
                    {
                        let _ = (p, origin);
                        pos[0] += gesture_delta[0];
                        pos[1] += gesture_delta[1];
                    }
                    (*pin, pos)
                })
                .collect();
            let segments = match self.session.document().wires.geometric() {
                Ok(segments) => segments.to_vec(),
                Err(error) => {
                    self.error = Some(format!("Cannot paint wire geometry: {error}"));
                    Vec::new()
                }
            };
            if segments.iter().flat_map(|segment| [segment.a, segment.b]).any(|point| {
                point.x.unsigned_abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER as u32
                    || point.y.unsigned_abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER as u32
            }) {
                self.error = Some(format!("Large wire coordinates are drawn from exact integer points; fit, route picking, and interior editing are limited to ±{} world units", crate::symbol_render::MAX_EXACT_SYMBOL_CENTER));
            }
            let mut incidence = std::collections::BTreeMap::<digital_rust::geometric_wiring::Point, usize>::new();
                for segment in segments {
                    let mut a=segment.a; let mut b=segment.b;
                    if let Some(mask)=self.selection.wires.get(&segment.id) {
                        if mask.contains_a() { a.x=a.x.saturating_add(gesture_delta[0] as i32); a.y=a.y.saturating_add(gesture_delta[1] as i32); }
                        if mask.contains_b() { b.x=b.x.saturating_add(gesture_delta[0] as i32); b.y=b.y.saturating_add(gesture_delta[1] as i32); }
                    }
                    *incidence.entry(a).or_default() += 1;
                    *incidence.entry(b).or_default() += 1;
                    let color = if self.selected_segment == Some(segment.id) || self.selection.wires.contains_key(&segment.id) { palette.selection } else { palette.ink };
                    painter.line_segment(
                        [transform.to_screen_i32(a.x, a.y), transform.to_screen_i32(b.x, b.y)],
                        Stroke::new(2.0_f32, color),
                    );
                }
                for (point, count) in incidence {
                    if count > 2 {
                        painter.circle_filled(transform.to_screen_i32(point.x, point.y), 3.0, palette.ink);
                    }
                }
            if self.wiring {
                let pointer_route_target = ctx
                    .input(|i| i.pointer.hover_pos())
                    .filter(|point| rect.contains(*point))
                    .and_then(|point| self.route_target_at(point, transform, &pins));
                let marker_target = pointer_route_target.or(self.focused_route_target);
                if let Some(target) = marker_target
                    && let Some(point) = self.route_target_point(target)
                {
                    painter.circle_stroke(
                        transform.to_screen_i32(point.x, point.y),
                        9.0,
                        Stroke::new(2.0_f32, palette.selection),
                    );
                }
                if let Some(target) = self.geometric_route_start
                    && let Some(point) = self.route_target_point(target)
                {
                    painter.circle_filled(
                        transform.to_screen_i32(point.x, point.y),
                        3.5,
                        palette.selection,
                    );
                }
                for (pin, pos) in &wire_pins {
                    painter.circle_stroke(
                        transform.to_screen(*pos),
                        if self.focused_pin == Some(*pin) {
                            7.
                        } else {
                            4.
                        },
                        Stroke::new(2_f32, palette.selection),
                    );
                }
                if !cancelled
                    && !pan_sequence
                    && response.hover_pos().is_some()
                    && let Some(start) = self.geometric_route_start
                    && let Some(a) = self.route_target_point(start)
                    && let Some(b) = pointer_route_target
                        .and_then(|target| self.route_target_point(target))
                {
                    let points = digital_rust::geometric_wiring::plan_orthogonal_route(a, b, self.route_orientation);
                    for pair in points.windows(2) {
                        painter.line_segment(
                            [transform.to_screen_i32(pair[0].x, pair[0].y), transform.to_screen_i32(pair[1].x, pair[1].y)],
                            Stroke::new(1_f32, palette.selection),
                        );
                    }
                }
            }
            let selection_width = ui.visuals().selection.stroke.width;
            let pending_view = self.session.inspect().map(|runtime| runtime.pending_inspection());
            for c in &self.session.document().components {
                let mut pos = c.position;
                if let (Some((_id, _start, _origin)), Some(_p)) = (self.drag, pointer)
                    && self.selection.components.contains(&c.id)
                {
                    pos = Position { x: c.position.x + gesture_delta[0], y: c.position.y + gesture_delta[1] };
                }
                let affected = self.session.oscillation_diagnostic().is_some_and(|diagnostic| diagnostic.components.iter().any(|component| component.id == c.id));
                let state = if affected { PaintState::Error } else if self.selection.components.contains(&c.id) || self.selected == Some(c.id) { PaintState::Selection } else { PaintState::Ink };
                let Some(layout) = SymbolLayout::oriented_on(style, c.kind, [pos.x, pos.y], crate::domain::Orientation { rotation: c.rotation, mirror: c.mirror }) else {
                    self.error = Some(if [pos.x, pos.y].into_iter().any(|value| value.abs() > crate::symbol_render::MAX_EXACT_SYMBOL_CENTER) {
                        format!("Component #{} is outside exact symbol-rendering range (±{} world units); its precise pin/wire coordinates are retained but canvas geometry is withheld", c.id.0, crate::symbol_render::MAX_EXACT_SYMBOL_CENTER)
                    } else {
                        format!("No symbol is drawn for {}", c.kind.name())
                    });
                    continue;
                };
                if matches!(
                    c.kind,
                    ComponentKind::Const
                        | ComponentKind::NAnd
                        | ComponentKind::NOr
                        | ComponentKind::XOr
                        | ComponentKind::XNOr
                ) {
                    // No cleared IEEE artwork is available for these gates. Keep one shared
                    // bounds/anchor/picking layout and render an explicit named text box instead
                    // of misleadingly reusing a plain AND/OR symbol.
                    let screen = Rect::from_min_max(
                        transform.to_screen(layout.bounds().min),
                        transform.to_screen(layout.bounds().max),
                    );
                    painter.rect_stroke(
                        screen,
                        3.0,
                        Stroke::new(STROKE_WIDTH, palette.color(state)),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        screen.center(),
                        egui::Align2::CENTER_CENTER,
                        if c.kind == ComponentKind::Const {
                            if c.constant_value { "1" } else { "0" }
                        } else {
                            c.kind.name()
                        },
                        egui::FontId::proportional(13.0),
                        palette.color(state),
                    );
                } else if let Err(e) = self
                    .renderer
                    .draw(&painter, &layout, &transform, &palette, state)
                {
                    self.error = Some(e.to_string());
                }
                let bounds = layout.bounds();
                if let Some(pending) = &pending_view {
                    let marker_pos = transform.to_screen(bounds.min) + Vec2::new(0.0, -4.0);
                    let mut offset=0.0;
                    for (ids, label, color) in [
                        (&pending.awaiting_evaluation, "EVAL", ui.visuals().hyperlink_color),
                        (&pending.staged_unpublished, "STAGED", ui.visuals().warn_fg_color),
                        (&pending.next_scheduled, "NEXT", ui.visuals().weak_text_color()),
                    ] {
                        if ids.binary_search(&c.id).is_ok() {
                            let frame=Rect::from_min_max(transform.to_screen(bounds.min),transform.to_screen(bounds.max)).expand(3.0+offset);
                            painter.rect_stroke(frame,3.0,Stroke::new(1.5_f32,color),egui::StrokeKind::Inside);
                            painter.text(marker_pos-Vec2::new(0.0,offset),egui::Align2::LEFT_BOTTOM,label,egui::FontId::monospace(9.0),color);
                            offset+=4.0;
                        }
                    }
                }
                if self.selection.components.contains(&c.id) || self.selected == Some(c.id) {
                    let frame = Rect::from_min_max(
                        transform.to_screen(bounds.min),
                        transform.to_screen(bounds.max),
                    )
                    .expand(SELECTION_MARGIN * transform.zoom());
                    painter.rect_stroke(
                        frame,
                        3.,
                        Stroke::new(selection_width.max(STROKE_WIDTH), palette.selection),
                        egui::StrokeKind::Inside,
                    );
                }
                painter.text(
                    transform.to_screen([(bounds.min[0] + bounds.max[0]) / 2., bounds.max[1]])
                        + Vec2::new(0., SELECTION_MARGIN * transform.zoom() + LABEL_GAP),
                    egui::Align2::CENTER_TOP,
                    &c.label,
                    egui::FontId::proportional(14.),
                    palette.color(state),
                );
                if let Some(runtime) = self.session.inspect() {
                    let pin = if c.kind == ComponentKind::Output {
                        PinId {
                            direction: PinDirection::Input,
                            index: 0,
                        }
                    } else {
                        PinId {
                            direction: PinDirection::Output,
                            index: 0,
                        }
                    };
                    let level = runtime
                        .pin_value(PinRef {
                            component: c.id,
                            pin,
                        })
                        .unwrap_or(Level::Unavailable);
                    painter.text(
                        transform.to_screen([(bounds.min[0] + bounds.max[0]) / 2., bounds.max[1]])
                            + Vec2::new(0., SELECTION_MARGIN * transform.zoom() + LABEL_GAP + 16.),
                        egui::Align2::CENTER_TOP,
                        level_badge(level),
                        egui::FontId::monospace(14.),
                        palette.color(state),
                    );
                }
            }
            if let Some(draft) = &self.fragment_draft
                && let Ok(document) = fragment_preview_document(draft)
            {
                    for segment in document.wires.geometric().unwrap_or(&[]) {
                        painter.line_segment([
                            transform.to_screen_i32(segment.a.x, segment.a.y),
                            transform.to_screen_i32(segment.b.x, segment.b.y),
                        ], Stroke::new(2.0_f32, palette.selection));
                    }
                    for component in &document.components {
                        if let Some(layout) = SymbolLayout::oriented_on(style, component.kind, [component.position.x, component.position.y], crate::domain::Orientation { rotation: component.rotation, mirror: component.mirror }) {
                            if matches!(component.kind, ComponentKind::Const | ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XOr | ComponentKind::XNOr) {
                                let bounds = layout.bounds();
                                let screen = Rect::from_min_max(transform.to_screen(bounds.min), transform.to_screen(bounds.max));
                                painter.rect_stroke(screen, 3.0, Stroke::new(STROKE_WIDTH, palette.selection), egui::StrokeKind::Inside);
                                let text = if component.kind == ComponentKind::Const { if component.constant_value { "1" } else { "0" } } else { component.kind.name() };
                                painter.text(screen.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), palette.selection);
                            } else if let Err(error) = self.renderer.draw(&painter, &layout, &transform, &palette, PaintState::Selection) {
                                self.clipboard_error = Some(error.to_string());
                            }
                            let bounds = layout.bounds();
                            painter.text(transform.to_screen([(bounds.min[0] + bounds.max[0]) / 2.0, bounds.max[1]]) + Vec2::new(0.0, SELECTION_MARGIN * transform.zoom() + LABEL_GAP), egui::Align2::CENTER_TOP, &component.label, egui::FontId::proportional(14.0), palette.selection);
                        }
                    }
            }
            // A widget click later in this same frame has not yet updated egui's
            // focused-widget memory. Do not let a queued placement-focus request
            // override that click when the draft was started by a shortcut/menu.
            let clicked_seed_field = self.seed_field_rect.is_some_and(|seed_rect| {
                ctx.input(|input| input.events.iter().any(|event| matches!(event,
                    egui::Event::PointerButton { pos, pressed: true, .. } if seed_rect.contains(*pos)
                )))
            });
            if clicked_seed_field && self.fragment_draft.is_some() {
                self.fragment_draft = None;
                self.fragment_focus_claimed = false;
                self.fragment_canvas_had_focus = false;
                self.restore_canvas_focus = false;
                self.fragment_focus_check_pending = false;
            }
            if self.restore_canvas_focus
                && !self.modality_active()
                && ctx.input(|input| input.focused)
            {
                response.request_focus();
                if self.fragment_draft.is_some() {
                    self.fragment_focus_claimed = true;
                    self.fragment_focus_check_pending = true;
                }
                self.restore_canvas_focus = false;
            }
            let placement_keyboard = response.has_focus() && !self.modality_active() && !pan_sequence && self.fragment_draft.is_some();
            if placement_keyboard && !locked {
                let key = ctx.input(|input| input.events.iter().find_map(|event| match event {
                    egui::Event::Key { key, pressed: true, modifiers, .. }
                        if !modifiers.ctrl && !modifiers.command && !modifiers.alt && !modifiers.shift
                            && matches!(key, egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Enter | egui::Key::R) => Some(*key),
                    _ => None,
                }));
                match key {
                    Some(egui::Key::ArrowLeft) => self.adjust_fragment_draft(-20, 0),
                    Some(egui::Key::ArrowRight) => self.adjust_fragment_draft(20, 0),
                    Some(egui::Key::ArrowUp) => self.adjust_fragment_draft(0, -20),
                    Some(egui::Key::ArrowDown) => self.adjust_fragment_draft(0, 20),
                    Some(egui::Key::Enter) => self.commit_fragment_draft(),
                    Some(egui::Key::R) => self.rotate_fragment_draft(),
                    _ => {}
                }
            }
            let owns_keyboard = response.has_focus() && !self.modality_active() && !pan_sequence && self.fragment_draft.is_none();
            if owns_keyboard && !locked && self.fragment_draft.is_none() {
                let platform_action = ctx.input(|input| input.events.iter().find_map(|event| match event {
                    egui::Event::Copy => Some(ClipboardAction::Copy),
                    egui::Event::Cut => Some(ClipboardAction::Cut),
                    egui::Event::Paste(text) => Some(ClipboardAction::Paste(Some(text.clone()))),
                    _ => None,
                }));
                let shortcut_action = ctx.input(|input| input.events.iter().find_map(|event| match event {
                    egui::Event::Key { key, pressed: true, modifiers, .. }
                        if (modifiers.ctrl || modifiers.command) && !modifiers.alt && !modifiers.shift => match key {
                            egui::Key::C => Some(ClipboardAction::Copy),
                            egui::Key::X => Some(ClipboardAction::Cut),
                            egui::Key::V => Some(ClipboardAction::Paste(None)),
                            egui::Key::D => Some(ClipboardAction::Duplicate),
                            _ => None,
                        },
                    _ => None,
                }));
                if let Some(action) = platform_action.or(shortcut_action) {
                    self.run_clipboard_action(action);
                }
            }
            if owns_keyboard && self.fragment_draft.is_none() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        response.id,
                        egui::EventFilter {
                            tab: true,
                            ..Default::default()
                        },
                    )
                });
            }
            if owns_keyboard && !locked && self.fragment_draft.is_none() {
                let select_all = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A) || i.consume_key(egui::Modifiers::CTRL, egui::Key::A));
                if select_all {
                    self.selection.components = self.session.document().components.iter().map(|c| c.id).collect();
                    self.selection.wires = self.session.document().wires.geometric().map(|segments| segments.iter().map(|s| (s.id, crate::selection::EndpointMask::BOTH)).collect()).unwrap_or_default();
                    self.selected = (self.selection.components.len()==1).then(|| self.selection.components.iter().next().copied()).flatten();
                    self.selected_segment = None;
                }
                // Shift+arrows nudge the mixed selection; unmodified arrows remain camera pan.
                let nudge = ctx.input(|i| i.events.iter().find_map(|event| match event {
                    egui::Event::Key { key, pressed: true, modifiers, .. }
                        if modifiers.shift && !modifiers.ctrl && !modifiers.command
                            && matches!(key, egui::Key::ArrowLeft|egui::Key::ArrowRight|egui::Key::ArrowUp|egui::Key::ArrowDown) => Some(*key),
                    _ => None,
                }));
                if let Some(key) = nudge {
                    let (dx,dy) = match key { egui::Key::ArrowLeft=>(-20,0),egui::Key::ArrowRight=>(20,0),egui::Key::ArrowUp=>(0,-20),_=>(0,20) };
                    match self.session.move_selection(&self.selection,dx,dy) {
                        Ok(_) => self.reconcile_selection(),
                        Err(error) => self.error=Some(error.to_string()),
                    }
                }
            }
            if owns_keyboard && locked {
                // Edit shortcuts are refused here, before the engine would refuse them.
                let command = egui::Modifiers::COMMAND;
                let shifted = egui::Modifiers {
                    shift: true,
                    ..command
                };
                let attempted = ctx.input_mut(|i| {
                    [
                        (egui::Modifiers::NONE, egui::Key::W),
                        (egui::Modifiers::NONE, egui::Key::F2),
                        (egui::Modifiers::NONE, egui::Key::Delete),
                        (command, egui::Key::Z),
                        (shifted, egui::Key::Z),
                        (command, egui::Key::Y),
                    ]
                    .into_iter()
                    .fold(false, |any, (modifiers, key)| {
                        i.consume_key(modifiers, key) | any
                    })
                });
                if attempted {
                    self.error = Some(EditError::SimulationActive.to_string());
                }
                if self.selection.wires.is_empty() && (self.selection.components.len()==1 || (self.selection.components.is_empty() && self.selected.is_some())) && self.selected.is_some_and(|id| {
                    self.session
                        .document()
                        .components
                        .iter()
                        .any(|c| c.id == id && c.kind == ComponentKind::Input)
                }) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space))
                    && let Some(id) = self.selected
                {
                    self.apply_simulation_action(SimAction::Toggle(id));
                }
            }
            if owns_keyboard
                && !locked
                && self.selection.wires.is_empty()
                && (self.selection.components.len()==1 || (self.selection.components.is_empty() && self.selected.is_some()))
                && let Some(id) = self.selected
                && self
                    .session
                    .document()
                    .components
                    .iter()
                    .any(|c| c.id == id && c.kind == ComponentKind::Const)
            {
                let value = if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::H))
                {
                    Some(true)
                } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::L)) {
                    Some(false)
                } else {
                    None
                };
                if let Some(value) = value
                    && let Err(error) = self.session.set_constant_value(id, value)
                {
                    self.error = Some(error.to_string());
                }
            }
            if owns_keyboard
                && !locked
                && self.selection.components.len() <= 1
                && self.selection.wires.is_empty()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2))
            {
                self.begin_label_edit();
            }
            if owns_keyboard
                && !locked
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::W))
            {
                self.wiring = !self.wiring;
                        self.geometric_route_start = None;
                self.placing = None;
                self.drag = None;
            }
            if owns_keyboard
                && self.wiring
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::W))
            {
                self.route_orientation = match self.route_orientation {
                    digital_rust::geometric_wiring::RouteOrientation::HorizontalFirst => digital_rust::geometric_wiring::RouteOrientation::VerticalFirst,
                    digital_rust::geometric_wiring::RouteOrientation::VerticalFirst => digital_rust::geometric_wiring::RouteOrientation::HorizontalFirst,
                };
            }
            if owns_keyboard {
                let backwards =
                    ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
                let forwards =
                    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
                if backwards || forwards {
                    if self.wiring {
                        let pointer = ctx.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
                        let mut targets = pointer
                            .map(|p| self.route_targets_at(p, transform, &pins))
                            .filter(|targets| !targets.is_empty())
                            .unwrap_or_default();
                        if targets.is_empty() {
                            let visible_pins: Vec<_> = pins
                                .iter()
                                .filter(|(_, p)| rect.contains(transform.to_screen(*p)))
                                .collect();
                            targets.extend(visible_pins.iter().map(|v| crate::engine::RouteTarget::Pin(v.0)));
                            match self.session.document().wires.geometric() {
                                Ok(segments) => for segment in segments {
                                    for point in [segment.a, segment.b] {
                                        if rect.contains(transform.to_screen_i32(point.x, point.y)) {
                                            targets.push(crate::engine::RouteTarget::ExistingEndpoint(point));
                                        }
                                    }
                                    let midpoint = digital_rust::geometric_wiring::Point::new(
                                        ((i64::from(segment.a.x)+i64::from(segment.b.x))/2) as i32,
                                        ((i64::from(segment.a.y)+i64::from(segment.b.y))/2) as i32);
                                    if (segment.a.x == segment.b.x || segment.a.y == segment.b.y)
                                        && midpoint != segment.a
                                        && midpoint != segment.b
                                        && rect.contains(transform.to_screen_i32(midpoint.x, midpoint.y)) {
                                        targets.push(crate::engine::RouteTarget::SegmentInterior { segment_id: segment.id, point: midpoint });
                                    }
                                },
                                Err(error) => self.error = Some(format!("Cannot navigate wire targets: {error}")),
                            }
                            targets = unique_route_targets(targets);
                        }
                        if !targets.is_empty() {
                            let current = self.focused_route_target.and_then(|target| targets.iter().position(|v| *v == target));
                            let next = match current { Some(i) if backwards => (i+targets.len()-1)%targets.len(), Some(i)=>(i+1)%targets.len(), None if backwards=>targets.len()-1, None=>0 };
                            self.focused_route_target = Some(targets[next]);
                            self.route_pick_cycle = next;
                            self.focused_pin = match targets[next] { crate::engine::RouteTarget::Pin(pin)=>Some(pin), _=>None };
                        }
                    } else if !self.wiring {
                        match self.session.document().wires.geometric() {
                            Ok(segments) if !segments.is_empty() => {
                                let current = self.selected_segment.and_then(|id| segments.iter().position(|s| s.id == id));
                                let next = match current {
                                    Some(i) if backwards => (i + segments.len() - 1) % segments.len(),
                                    Some(i) => (i + 1) % segments.len(),
                                    None if backwards => segments.len() - 1,
                                    None => 0,
                                };
                                self.selected_segment = Some(segments[next].id);
                                self.selected = None;
                                self.selection.clear();
                                self.selection.wires.insert(segments[next].id, crate::selection::EndpointMask::BOTH);
                            }
                            Ok(_) => {}
                            Err(error) => self.error = Some(format!("Cannot navigate wire selection: {error}")),
                        }
                    }
                }
                if self.wiring
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                    && let Some(target) = self.active_route_target(
                        ctx.input(|i| i.pointer.hover_pos()),
                        rect,
                        transform,
                        &pins,
                    )
                {
                    if self.route_target_point(target).is_some() {
                        self.choose_route_target(target);
                    } else {
                        self.focused_route_target = None;
                        self.focused_pin = None;
                        self.route_pick_cycle = 0;
                        self.error = Some("Focused wire target is stale; select a current target".into());
                    }
                }
            }
            if owns_keyboard
                && !locked
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace))
            {
                let result = if let Some(id)=self.selected.filter(|id| !self.selection.components.contains(id)) {
                    self.session.delete(id).map(|_| true)
                } else if !self.selection.is_empty() {
                    self.session.delete_selection(&self.selection)
                } else if let Some(id)=self.selected_segment {
                    self.session.delete_geometric_segment(id)
                } else if let Some(id)=self.selected {
                    self.session.delete(id).map(|_| true)
                } else { Ok(false) };
                match result {
                    Ok(_) => { self.selection.clear(); self.selected=None; self.selected_segment=None; self.drag=None; self.error=None; }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            let (undo, redo) = if owns_keyboard && !locked && self.fragment_draft.is_none() {
                ctx.input_mut(|i| {
                    let command = egui::Modifiers::COMMAND;
                    let shifted = egui::Modifiers {
                        shift: true,
                        ..command
                    };
                    {
                        let redo = i.consume_key(shifted, egui::Key::Z)
                            || i.consume_key(command, egui::Key::Y);
                        let undo = i.consume_key(command, egui::Key::Z);
                        (undo, redo)
                    }
                })
            } else {
                (false, false)
            };
            if undo {
                self.focused_pin = None;
                self.focused_route_target = None;
                self.geometric_route_start = None;
                if self.session.undo().is_ok() {
                    self.selected = None;
                    self.selected_segment = None;
                    self.selection.clear();
                    self.drag = None;
                }
            }
            if redo {
                self.focused_pin = None;
                self.focused_route_target = None;
                self.geometric_route_start = None;
                if self.session.redo().is_ok() {
                    self.selected = None;
                    self.selected_segment = None;
                    self.selection.clear();
                    self.drag = None;
                }
            }
        });
        if !modal_open {
            self.advance_simulation(ctx);
        }
        self.file_modal_ui(ctx);
        self.label_modal_ui(ctx);
    }
}
impl eframe::App for DigitalApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.show(ctx);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct FakeClipboardState {
        text: String,
        reads: usize,
        writes: usize,
        fail_read: bool,
        fail_write: bool,
    }
    struct FakeClipboard(Rc<RefCell<FakeClipboardState>>);
    impl TextClipboard for FakeClipboard {
        fn read_text(&mut self) -> Result<String, String> {
            let mut state = self.0.borrow_mut();
            state.reads += 1;
            if state.fail_read {
                Err("injected read failure".into())
            } else {
                Ok(state.text.clone())
            }
        }
        fn write_text(&mut self, text: &str) -> Result<(), String> {
            let mut state = self.0.borrow_mut();
            state.writes += 1;
            if state.fail_write {
                Err("injected write failure".into())
            } else {
                state.text = text.to_owned();
                Ok(())
            }
        }
    }

    #[test]
    fn partial_wire_only_cut_is_a_visible_noop_preserving_clipboard_and_history() {
        let state = Rc::new(RefCell::new(FakeClipboardState {
            text: "keep clipboard".into(),
            ..Default::default()
        }));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let output = app
            .session
            .place(ComponentKind::Output, Position { x: 80.0, y: 0.0 })
            .unwrap();
        app.session
            .connect([
                PinRef {
                    component: input,
                    pin: PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                },
                PinRef {
                    component: output,
                    pin: PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                },
            ])
            .unwrap();
        let segment = app.session.document().wires.geometric().unwrap()[0];
        app.selection
            .wires
            .insert(segment.id, crate::selection::EndpointMask::A);
        let document = app.session.document().clone();
        app.run_clipboard_action(ClipboardAction::Cut);
        assert_eq!(app.session.document(), &document);
        app.session.undo().unwrap();
        assert_ne!(
            app.session.document(),
            &document,
            "prior undo history remains available"
        );
        app.session.redo().unwrap();
        assert_eq!(app.session.document(), &document);
        assert_eq!(state.borrow().text, "keep clipboard");
        assert_eq!(state.borrow().writes, 0);
        assert!(
            app.clipboard_error
                .as_deref()
                .is_some_and(|message| message.contains("partial boundary wire"))
        );
    }

    #[test]
    fn cut_write_failure_preserves_document_history_selection_and_clipboard() {
        let state = Rc::new(RefCell::new(FakeClipboardState {
            text: "old clipboard".into(),
            fail_write: true,
            ..Default::default()
        }));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        app.selection.components.insert(id);
        app.selected = Some(id);
        let document = app.session.document().clone();
        app.run_clipboard_action(ClipboardAction::Cut);
        assert_eq!(app.session.document(), &document);
        assert_eq!(app.selection.components, [id].into_iter().collect());
        assert_eq!(state.borrow().text, "old clipboard");
        assert!(
            app.clipboard_error
                .as_deref()
                .is_some_and(|message| message.contains("injected"))
        );
        app.session.undo().unwrap();
        let next = app
            .session
            .place(ComponentKind::Output, Position { x: 40.0, y: 0.0 })
            .unwrap();
        assert_eq!(next.0, id.0 + 1);
    }

    #[test]
    fn egui_copy_and_synthetic_shortcut_dispatch_once_and_text_modal_keeps_clipboard_ownership() {
        let state = Rc::new(RefCell::new(FakeClipboardState::default()));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, vec![], true);
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let position = app.session.document().components[0].position;
        let screen = transform.to_screen([position.x, position.y]);
        click_at(&ctx, &mut app, screen);
        assert!(app.selection.components.contains(&id));
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::C, ctrl), egui::Event::Copy],
            true,
        );
        assert_eq!(
            state.borrow().writes,
            1,
            "key and egui clipboard events must not dispatch twice"
        );
        assert!(state.borrow().text.contains("<list>"));
        app.begin_label_edit();
        let writes_before = state.borrow().writes;
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::C, ctrl), egui::Event::Copy],
            true,
        );
        assert_eq!(
            state.borrow().writes,
            writes_before,
            "text editor owns its normal clipboard shortcuts"
        );
    }

    #[test]
    fn clipboard_read_failure_and_empty_copy_are_visible_noops() {
        let state = Rc::new(RefCell::new(FakeClipboardState {
            text: "keep".into(),
            fail_read: true,
            ..Default::default()
        }));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        app.run_clipboard_action(ClipboardAction::Copy);
        assert_eq!(state.borrow().text, "keep");
        assert_eq!(state.borrow().writes, 0);
        app.run_clipboard_action(ClipboardAction::Paste(None));
        assert!(app.fragment_draft.is_none());
        assert!(app.session.document().components.is_empty());
        assert!(
            app.clipboard_error
                .as_deref()
                .is_some_and(|message| message.contains("injected read failure"))
        );
        assert_eq!(state.borrow().reads, 1);
    }

    #[test]
    fn fragment_placement_click_commits_and_escape_or_focus_loss_cancels_without_edits() {
        let fragment = digital_rust::formats::DigV2 {
            elements: vec![digital_rust::formats::DigElement {
                name: "In".into(),
                x: 0,
                y: 0,
                label: None,
                value: None,
                rotation: 0,
                mirror: false,
            }],
            wires: vec![],
        };
        let ctx = egui::Context::default();
        let mut app = DigitalApp::with_file_io(None, None);
        app.begin_fragment_draft(fragment.clone(), [0, 0], true)
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let document = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document(), &document);
        assert!(app.fragment_draft.is_none());

        app.begin_fragment_draft(fragment.clone(), [0, 0], true)
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        frame(&ctx, &mut app, vec![], false);
        assert_eq!(app.session.document(), &document);
        assert!(app.fragment_draft.is_none());

        app.begin_fragment_draft(fragment, [0, 0], true).unwrap();
        frame(&ctx, &mut app, vec![], true);
        let center = app.canvas_rect.unwrap().center();
        click_at(&ctx, &mut app, center);
        assert_eq!(app.session.document().components.len(), 1);
        assert_eq!(app.selection.components.len(), 1);
        assert!(app.fragment_draft.is_none());
    }

    #[test]
    fn edit_menu_copy_uses_same_injected_clipboard_action() {
        let state = Rc::new(RefCell::new(FakeClipboardState {
            text: "preserve".into(),
            ..Default::default()
        }));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
            .unwrap();
        app.selection.components.insert(id);
        let ctx = egui::Context::default();
        click_text(&ctx, &mut app, "Edit");
        click_text(&ctx, &mut app, "Copy  Ctrl/Cmd+C");
        assert_eq!(state.borrow().writes, 1);
        assert!(state.borrow().text.starts_with("<?xml"));
    }

    #[test]
    fn duplicate_uses_fragment_preview_without_reading_or_writing_clipboard() {
        let state = Rc::new(RefCell::new(FakeClipboardState::default()));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        let id = app
            .session
            .place(ComponentKind::Not, Position { x: 0.0, y: 0.0 })
            .unwrap();
        app.selection.components.insert(id);
        app.selected = Some(id);
        let document = app.session.document().clone();
        app.run_clipboard_action(ClipboardAction::Duplicate);
        assert_eq!(app.session.document(), &document);
        assert!(app.fragment_draft.is_some(), "{:?}", app.clipboard_error);
        assert_eq!((state.borrow().reads, state.borrow().writes), (0, 0));
    }
    #[test]
    fn pointer_move_updates_paste_preview_and_click_or_enter_commits_same_transformed_position() {
        use crate::canvas::CanvasViewState;

        let fragment = digital_rust::formats::DigV2 {
            elements: vec![digital_rust::formats::DigElement {
                name: "In".into(),
                x: 0,
                y: 0,
                label: Some("Hover paste".into()),
                value: None,
                rotation: 0,
                mirror: false,
            }],
            wires: vec![],
        };
        for commit_by_enter in [false, true] {
            let mut app = DigitalApp::default();
            app.begin_fragment_draft(fragment.clone(), [0, 0], false)
                .unwrap();
            let ctx = egui::Context::default();
            frame(&ctx, &mut app, vec![], true);
            let rect = app.canvas_rect.unwrap();
            app.view = CanvasViewState::default();
            app.view.pan(Vec2::new(37.0, -23.0));
            app.view.zoom_at(rect, rect.center(), 1.7);
            let pointer = rect.center() + Vec2::new(41.0, 29.0);
            let transform = app.view.transform(rect).unwrap();
            let world = transform.to_world(pointer);
            let anchor = app.fragment_draft.as_ref().unwrap().anchor;
            let expected = [
                (((f64::from(world[0]) - anchor[0]) / 20.0).round() * 20.0) as i32,
                (((f64::from(world[1]) - anchor[1]) / 20.0).round() * 20.0) as i32,
            ];
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(pointer)],
                true,
            );
            assert_eq!(
                app.fragment_draft.as_ref().unwrap().delta,
                expected,
                "pointer-move-only preview must use canvas hover under pan/zoom"
            );
            let outside = Pos2::new(rect.right() + 30.0, rect.bottom() + 30.0);
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(outside)],
                true,
            );
            assert_eq!(
                app.fragment_draft.as_ref().unwrap().delta,
                expected,
                "leaving the canvas retains the last valid draft position"
            );
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(pointer)],
                true,
            );
            if commit_by_enter {
                frame(
                    &ctx,
                    &mut app,
                    vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
                    true,
                );
            } else {
                frame(&ctx, &mut app, vec![button(pointer, true)], true);
                frame(&ctx, &mut app, vec![button(pointer, false)], true);
            }
            assert_eq!(app.session.document().components.len(), 1);
            let source = digital_rust::formats::to_domain(&fragment).unwrap();
            assert_eq!(
                app.session.document().components[0].position,
                Position {
                    x: source.components[0].position.x + expected[0] as f32,
                    y: source.components[0].position.y + expected[1] as f32,
                },
                "source={:?} expected delta={expected:?}",
                source.components[0].position
            );
            assert!(app.fragment_draft.is_none());
        }
    }

    #[test]
    fn placement_draft_releases_focus_to_seed_editor_and_enter_only_edits_seed() {
        let fragment = digital_rust::formats::DigV2 {
            elements: vec![digital_rust::formats::DigElement {
                name: "In".into(),
                x: 0,
                y: 0,
                label: Some("Must not insert".into()),
                value: None,
                rotation: 0,
                mirror: false,
            }],
            wires: vec![],
        };
        let mut app = DigitalApp::default();
        app.begin_fragment_draft(fragment, [0, 0], false).unwrap();
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, vec![], true);
        let seed_rect = app.seed_field_rect.unwrap();
        let seed = seed_rect.center();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(seed), button(seed, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(seed, false)], true);
        assert!(
            app.fragment_draft.is_none(),
            "focus transfer cancels placement"
        );
        assert!(
            app.seed_field_focused,
            "the clicked seed editor retains focus"
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::A, egui::Modifiers::COMMAND)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::Text("42".into())], true);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.startup_seed_text, "42");
        assert!(app.seed_field_focused);
        assert!(
            app.session.document().components.is_empty(),
            "seed Enter must not commit a fragment"
        );
    }

    #[test]
    fn production_paste_and_duplicate_focus_request_yields_to_immediate_seed_click() {
        let fragment = digital_rust::formats::DigV2 {
            elements: vec![digital_rust::formats::DigElement {
                name: "In".into(),
                x: 0,
                y: 0,
                label: Some("Must not insert".into()),
                value: None,
                rotation: 0,
                mirror: false,
            }],
            wires: vec![],
        };
        let xml =
            String::from_utf8(digital_rust::formats::serialize_clipboard_list(&fragment).unwrap())
                .unwrap();
        for duplicate in [false, true] {
            let clipboard_state = Rc::new(RefCell::new(FakeClipboardState {
                text: xml.clone(),
                ..Default::default()
            }));
            let mut app = DigitalApp::with_file_io(None, None)
                .with_clipboard(Box::new(FakeClipboard(clipboard_state)));
            let source_id = app
                .session
                .place(ComponentKind::Input, Position { x: 0.0, y: 0.0 })
                .unwrap();
            if duplicate {
                app.selection.components.insert(source_id);
                app.selected = Some(source_id);
            }
            let ctx = egui::Context::default();
            frame(&ctx, &mut app, vec![], true);
            let canvas = app.canvas_rect.unwrap().center();
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(canvas), button(canvas, true)],
                true,
            );
            frame(&ctx, &mut app, vec![button(canvas, false)], true);
            ctx.memory_mut(|memory| memory.request_focus(app.canvas_id.unwrap()));
            if duplicate {
                app.selection.components.insert(source_id);
                app.selected = Some(source_id);
            }
            frame(&ctx, &mut app, vec![], true);
            let action = if duplicate {
                key(
                    egui::Key::D,
                    egui::Modifiers {
                        ctrl: true,
                        ..Default::default()
                    },
                )
            } else {
                egui::Event::Paste(xml.clone())
            };
            frame(&ctx, &mut app, vec![action], true);
            assert!(
                app.fragment_draft.is_some(),
                "production action should create placement draft (duplicate={duplicate}, focus={:?}, canvas={:?}, error={:?})",
                ctx.memory(|memory| memory.focused()),
                app.canvas_id,
                app.clipboard_error
            );
            let seed = app.seed_field_rect.unwrap().center();
            // This is deliberately the very next frame: egui's deferred canvas focus
            // restoration must not steal the text field click.
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(seed), button(seed, true)],
                true,
            );
            frame(&ctx, &mut app, vec![button(seed, false)], true);
            assert!(
                app.fragment_draft.is_none(),
                "seed click cancels pending placement"
            );
            assert!(app.seed_field_focused, "seed editor retains focus");
            frame(&ctx, &mut app, vec![egui::Event::Text("42".into())], true);
            frame(
                &ctx,
                &mut app,
                vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
                true,
            );
            assert_eq!(app.startup_seed_text, "42");
            assert_eq!(
                app.session.document().components.len(),
                1,
                "no paste/duplicate commit"
            );
        }
    }

    #[test]
    fn paste_preview_is_transient_and_keyboard_offset_commits_once_with_fresh_selection() {
        let fragment = digital_rust::formats::DigV2 {
            elements: vec![digital_rust::formats::DigElement {
                name: "In".into(),
                x: 0,
                y: 0,
                label: Some("Paste λ".into()),
                value: None,
                rotation: 0,
                mirror: false,
            }],
            wires: vec![],
        };
        let xml =
            String::from_utf8(digital_rust::formats::serialize_clipboard_list(&fragment).unwrap())
                .unwrap();
        let state = Rc::new(RefCell::new(FakeClipboardState {
            text: xml,
            ..Default::default()
        }));
        let mut app = DigitalApp::with_file_io(None, None)
            .with_clipboard(Box::new(FakeClipboard(state.clone())));
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, vec![], true);
        let canvas = app.canvas_rect.unwrap().center();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(canvas), button(canvas, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(canvas, false)], true);
        let original = app.session.document().clone();
        app.run_clipboard_action(ClipboardAction::Paste(None));
        assert_eq!(app.session.document(), &original);
        assert_eq!(state.borrow().reads, 1);
        frame(&ctx, &mut app, vec![], true);
        let before_arrow = app.fragment_draft.as_ref().unwrap().delta;
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            true,
        );
        let offset = app.fragment_draft.as_ref().unwrap().delta;
        assert_eq!(offset[0], before_arrow[0] + 20);
        let canvas = app.canvas_rect.unwrap().center();
        frame(&ctx, &mut app, vec![button(canvas, true)], true);
        frame(&ctx, &mut app, vec![button(canvas, false)], true);
        assert_eq!(
            app.session.document().components.len(),
            1,
            "draft={:?} clipboard_error={:?}",
            app.fragment_draft.as_ref().map(|draft| draft.delta),
            app.clipboard_error
        );
        assert_eq!(app.session.document().components[0].label, "Paste λ");
        assert_eq!(
            app.selection.components,
            [ComponentId(0)].into_iter().collect()
        );
        assert!(app.fragment_draft.is_none());
        app.session.undo().unwrap();
        assert!(app.session.document().components.is_empty());
    }

    #[test]
    fn keyboard_target_dedup_scales_to_maximum_wire_candidate_count_and_keeps_order() {
        use crate::engine::RouteTarget;
        use digital_rust::geometric_wiring::Point;
        let mut candidates = Vec::with_capacity(180_000);
        for id in 0..30_000_u64 {
            let x = (id * 10) as i32;
            candidates.extend([
                RouteTarget::ExistingEndpoint(Point::new(x, 0)),
                RouteTarget::ExistingEndpoint(Point::new(x + 2, 0)),
                RouteTarget::SegmentInterior {
                    segment_id: id,
                    point: Point::new(x + 1, 0),
                },
            ]);
        }
        let expected_first = candidates[0];
        let expected_last = *candidates.last().unwrap();
        candidates.extend_from_within(..90_000);
        let unique = unique_route_targets(candidates);
        assert_eq!(unique.len(), 90_000);
        assert_eq!(unique.first(), Some(&expected_first));
        assert_eq!(unique.last(), Some(&expected_last));
    }

    #[test]
    fn wire_retry_and_cancel_are_non_destructive() {
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 10., y: 10. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 10. })
            .unwrap();
        let a = PinRef {
            component: a,
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        };
        let b = PinRef {
            component: b,
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        app.choose_pin(a);
        app.choose_pin(a);
        assert_eq!(
            app.geometric_route_start,
            Some(crate::engine::RouteTarget::Pin(a))
        );
        assert!(app.error.is_some());
        app.choose_pin(b);
        assert_eq!(app.session.document().wires.len(), 1);
        assert!(app.error.is_none());
        app.choose_pin(a);
        let ctx = egui::Context::default();
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.geometric_route_start, None);
        assert_eq!(app.session.document().wires.len(), 1);
    }
    #[test]
    fn production_canvas_routes_from_wire_interior_to_endpoint_and_undoes_once() {
        use digital_rust::geometric_wiring::Point;
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .add_geometric_segments(&[
                (Point::new(100, 100), Point::new(300, 100)),
                (Point::new(100, 200), Point::new(300, 200)),
            ])
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        click_at(&ctx, &mut app, Pos2::new(500., 400.));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        let rect = app.canvas_rect.unwrap();
        let initial_transform = app.view.transform(rect).unwrap();
        let zoom_anchor = initial_transform.to_screen([200., 100.]);
        assert!(app.view.zoom_at(rect, zoom_anchor, 1.7));
        assert!(app.view.pan(Vec2::new(12., 8.)));
        let transform = app.view.transform(rect).unwrap();
        let interior = transform.to_screen([200., 100.]);
        let endpoint = transform.to_screen([100., 200.]);
        assert!(
            rect.contains(interior),
            "transformed interior should be in canvas: {interior:?} {rect:?}"
        );
        assert_eq!(
            app.route_target_at(interior, transform, &[]),
            Some(crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 100)
            })
        );
        click_at(&ctx, &mut app, interior);
        assert_eq!(
            app.geometric_route_start,
            Some(crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 100)
            })
        );
        click_at(&ctx, &mut app, endpoint);
        let segments = app.session.document().wires.geometric().unwrap();
        assert_eq!(segments.len(), 5, "split plus two L-route legs");
        assert!(
            segments
                .iter()
                .any(|s| s.a == Point::new(100, 100) && s.b == Point::new(100, 200))
                || segments
                    .iter()
                    .any(|s| s.b == Point::new(100, 100) && s.a == Point::new(100, 200))
        );
        app.session.undo().unwrap();
        assert_eq!(
            app.session.document().wires.geometric().unwrap().len(),
            2,
            "a single undo removes the entire route and restores the unsplit original"
        );
        app.session.redo().unwrap();
        assert_eq!(app.session.document().wires.geometric().unwrap().len(), 5);
    }
    #[test]
    fn keyboard_selected_crossing_agrees_with_marker_preview_enter_and_pointer_commit() {
        use digital_rust::geometric_wiring::Point;
        fn route_via_key(enter: bool) -> Vec<digital_rust::geometric_wiring::Segment> {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            app.session
                .add_geometric_segments(&[
                    (Point::new(100, 200), Point::new(300, 200)),
                    (Point::new(200, 100), Point::new(200, 300)),
                    (Point::new(100, 100), Point::new(120, 100)),
                ])
                .unwrap();
            frame(&ctx, &mut app, vec![], true);
            click_at(&ctx, &mut app, Pos2::new(500., 400.));
            frame(
                &ctx,
                &mut app,
                vec![key(egui::Key::W, egui::Modifiers::NONE)],
                true,
            );
            let rect = app.canvas_rect.unwrap();
            let transform = app.view.transform(rect).unwrap();
            let start = transform.to_screen([120., 100.]);
            let crossing = transform.to_screen([200., 200.]);
            click_at(&ctx, &mut app, start);
            assert_eq!(
                app.geometric_route_start,
                Some(crate::engine::RouteTarget::ExistingEndpoint(Point::new(
                    120, 100
                )))
            );
            frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(crossing),
                    key(egui::Key::Tab, egui::Modifiers::NONE),
                ],
                true,
            );
            let selected = crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 200),
            };
            assert_eq!(app.focused_route_target, Some(selected));
            assert_eq!(
                app.route_target_at(crossing, transform, &[]),
                Some(selected)
            );
            let preview = frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(crossing)],
                true,
            );
            let route = digital_rust::geometric_wiring::plan_orthogonal_route(
                Point::new(120, 100),
                Point::new(200, 200),
                app.route_orientation,
            );
            assert!(route.windows(2).all(|pair| has_line(
                &preview,
                transform.to_screen_i32(pair[0].x, pair[0].y),
                transform.to_screen_i32(pair[1].x, pair[1].y),
                1.0,
            )));
            if enter {
                frame(
                    &ctx,
                    &mut app,
                    vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
                    true,
                );
            } else {
                click_at(&ctx, &mut app, crossing);
            }
            assert!(app.geometric_route_start.is_none());
            let segments = app.session.document().wires.geometric().unwrap();
            assert!(
                segments.iter().any(|s| s.id == 1
                    && s.a == Point::new(200, 100)
                    && s.b == Point::new(200, 300))
            );
            segments.to_vec()
        }
        assert_eq!(route_via_key(true), route_via_key(false));
    }

    #[test]
    fn hover_target_and_enter_match_preview_and_pointer_commit_without_tab() {
        use digital_rust::geometric_wiring::Point;

        fn route_with_enter(enter: bool) -> Vec<digital_rust::geometric_wiring::Segment> {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            app.session
                .add_geometric_segments(&[
                    (Point::new(100, 200), Point::new(300, 200)),
                    (Point::new(200, 100), Point::new(200, 300)),
                    (Point::new(100, 100), Point::new(120, 100)),
                ])
                .unwrap();
            frame(&ctx, &mut app, vec![], true);
            click_at(&ctx, &mut app, Pos2::new(500., 400.));
            frame(
                &ctx,
                &mut app,
                vec![key(egui::Key::W, egui::Modifiers::NONE)],
                true,
            );
            let rect = app.canvas_rect.unwrap();
            let transform = app.view.transform(rect).unwrap();
            let start_point = Point::new(120, 100);
            let start = transform.to_screen_i32(start_point.x, start_point.y);
            click_at(&ctx, &mut app, start);
            let stale_focus = crate::engine::RouteTarget::ExistingEndpoint(start_point);
            assert_eq!(app.focused_route_target, Some(stale_focus));

            let target = crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 200),
            };
            let hover = transform.to_screen([200., 200.]);
            let preview = frame(&ctx, &mut app, vec![egui::Event::PointerMoved(hover)], true);
            assert_eq!(app.route_target_at(hover, transform, &[]), Some(target));
            assert!(has_line(
                &preview,
                start,
                transform.to_screen([200., 100.]),
                1.
            ));
            assert!(has_line(
                &preview,
                transform.to_screen([200., 100.]),
                hover,
                1.
            ));
            assert!(
                preview.shapes.iter().any(|clipped| matches!(
                    &clipped.shape,
                    egui::Shape::Circle(circle) if circle.center == hover && circle.radius == 9.0
                )),
                "selection marker follows the hovered preview target, not stale focus"
            );

            if enter {
                frame(
                    &ctx,
                    &mut app,
                    vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
                    true,
                );
            } else {
                click_at(&ctx, &mut app, hover);
            }
            assert!(app.geometric_route_start.is_none());
            let segments = app.session.document().wires.geometric().unwrap();
            assert!(segments.iter().any(|segment| {
                segment.id == 1
                    && segment.a == Point::new(200, 100)
                    && segment.b == Point::new(200, 300)
            }));
            segments.to_vec()
        }

        assert_eq!(route_with_enter(true), route_with_enter(false));
    }

    #[test]
    fn crossing_target_cycle_is_explicit_and_route_does_not_split_other_crossing_wire() {
        use digital_rust::geometric_wiring::Point;
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .add_geometric_segments(&[
                (Point::new(100, 200), Point::new(300, 200)),
                (Point::new(200, 100), Point::new(200, 300)),
                (Point::new(100, 100), Point::new(120, 100)),
            ])
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        click_at(&ctx, &mut app, Pos2::new(500., 400.));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let crossing = transform.to_screen([200., 200.]);
        click_at(&ctx, &mut app, crossing);
        assert_eq!(
            app.geometric_route_start,
            Some(crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 200)
            })
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(crossing),
                key(egui::Key::Tab, egui::Modifiers::NONE),
            ],
            true,
        );
        let selected = crate::engine::RouteTarget::SegmentInterior {
            segment_id: 1,
            point: Point::new(200, 200),
        };
        assert_eq!(app.focused_route_target, Some(selected));
        assert_eq!(
            app.route_target_at(crossing, transform, &[]),
            Some(selected)
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(crossing),
                key(egui::Key::Tab, egui::Modifiers::SHIFT),
            ],
            true,
        );
        let backwards = crate::engine::RouteTarget::SegmentInterior {
            segment_id: 0,
            point: Point::new(200, 200),
        };
        assert_eq!(app.focused_route_target, Some(backwards));
        assert_eq!(
            app.route_target_at(crossing, transform, &[]),
            Some(backwards)
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(crossing),
                key(egui::Key::Tab, egui::Modifiers::NONE),
            ],
            true,
        );
        assert_eq!(app.focused_route_target, Some(selected));
        assert_eq!(
            app.route_target_at(crossing, transform, &[]),
            Some(selected)
        );
        let endpoint = transform.to_screen([100., 100.]);
        click_at(&ctx, &mut app, endpoint);
        let wires = app.session.document().wires.geometric().unwrap();
        assert!(
            wires
                .iter()
                .any(|s| s.id == 1 && s.a == Point::new(200, 100) && s.b == Point::new(200, 300)),
            "nonselected crossing segment remains unsplit and disconnected"
        );
    }
    #[test]
    fn keyboard_rebuilds_candidates_when_the_focused_route_target_becomes_stale() {
        use digital_rust::geometric_wiring::Point;
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .add_geometric_segments(&[
                (Point::new(100, 200), Point::new(300, 200)),
                (Point::new(200, 100), Point::new(200, 300)),
            ])
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        click_at(&ctx, &mut app, Pos2::new(500., 400.));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let crossing = transform.to_screen([200., 200.]);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(crossing),
                key(egui::Key::Tab, egui::Modifiers::NONE),
            ],
            true,
        );
        assert_eq!(
            app.focused_route_target,
            Some(crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 200),
            })
        );
        app.session.delete_geometric_segment(0).unwrap();
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(crossing),
                key(egui::Key::Tab, egui::Modifiers::NONE),
            ],
            true,
        );
        let current = crate::engine::RouteTarget::SegmentInterior {
            segment_id: 1,
            point: Point::new(200, 200),
        };
        assert_eq!(app.focused_route_target, Some(current));
        assert_eq!(app.route_target_at(crossing, transform, &[]), Some(current));
    }

    #[test]
    fn keyboard_can_start_a_route_at_an_existing_segment_midpoint() {
        use digital_rust::geometric_wiring::Point;
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .add_geometric_segments(&[(Point::new(100, 100), Point::new(300, 100))])
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        click_at(&ctx, &mut app, Pos2::new(500., 400.));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        for _ in 0..3 {
            frame(
                &ctx,
                &mut app,
                vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
                true,
            );
        }
        assert_eq!(
            app.focused_route_target,
            Some(crate::engine::RouteTarget::SegmentInterior {
                segment_id: 0,
                point: Point::new(200, 100)
            })
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.geometric_route_start.is_some());
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.geometric_route_start, None);
        assert_eq!(app.session.document().wires.geometric().unwrap().len(), 3);
    }
    #[test]
    fn keyboard_wire_creation_and_deletion() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        app.session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(400., 300.);
        frame(&ctx, &mut app, vec![button(p, true)], true);
        frame(&ctx, &mut app, vec![button(p, false)], true);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document().wires.len(), 1);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.session.document().wires.is_empty());
    }
    #[test]
    fn placement_replaces_wire_selection_before_delete() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        let pin = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        app.session
            .connect([pin(a, PinDirection::Output), pin(b, PinDirection::Input)])
            .unwrap();
        app.selected_segment = Some(app.session.document().wires.geometric().unwrap()[0].id);
        app.placing = Some(ComponentKind::And);
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(500., 300.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(p, false)], true);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document().components.len(), 2);
        assert_eq!(app.session.document().wires.len(), 1);
    }
    #[test]
    fn undo_clears_keyboard_pin_and_tab_skips_offscreen() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: -500., y: 100. })
            .unwrap();
        let visible = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(400., 300.);
        frame(&ctx, &mut app, vec![button(p, true)], true);
        frame(&ctx, &mut app, vec![button(p, false)], true);
        app.wiring = true;
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.focused_pin.unwrap().component, visible);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Z, egui::Modifiers::COMMAND)],
            true,
        );
        assert!(app.focused_pin.is_none());
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.geometric_route_start.is_none());
    }
    fn frame(
        ctx: &egui::Context,
        app: &mut DigitalApp,
        events: Vec<egui::Event>,
        focused: bool,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused,
            events,
            ..Default::default()
        };
        ctx.run(input, |ctx| app.show(ctx))
    }
    fn has_line(output: &egui::FullOutput, from: Pos2, to: Pos2, width: f32) -> bool {
        output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::LineSegment { points, stroke } if *points == [from, to] && stroke.width == width))
    }

    #[test]
    fn startup_seed_is_reachable_from_canvas_by_keyboard_without_shortcut_leakage() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        inverter_chain(&mut app, 1);
        frame(&ctx, &mut app, vec![], true);
        let canvas = Pos2::new(350., 300.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(canvas), button(canvas, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(canvas, false)], true);
        assert!(!app.seed_field_focused);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Tab, egui::Modifiers::CTRL)],
            true,
        );
        frame(&ctx, &mut app, vec![], true);
        assert!(
            app.seed_field_focused,
            "Tab on canvas must focus seed editor"
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::A, egui::Modifiers::COMMAND)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::Text("42".into())], true);
        assert_eq!(app.startup_seed_text, "42");
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(
            !app.session.simulation_active(),
            "F5 must not leak from focused seed editor"
        );
        press(&ctx, &mut app, egui::Key::Escape, egui::Modifiers::NONE);
        frame(&ctx, &mut app, vec![], true);
        assert!(
            !app.seed_field_focused,
            "Escape returns focus from seed editor"
        );
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert_eq!(app.session.startup_seed(), Some(42));
        app.apply_simulation_action(SimAction::Stop);
    }

    #[test]
    fn startup_seed_control_locks_during_initialization_and_reports_phase_policy() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (input, _) = inverter_chain(&mut app, 1);
        app.startup_seed_text = "18446744073709551615".into();
        app.settle_budget = 1;
        app.apply_simulation_action(SimAction::Start);
        assert_eq!(app.session.startup_seed(), Some(u64::MAX));
        assert_eq!(app.startup_seed_text, u64::MAX.to_string());
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "Startup seed"));
        assert!(has_text(&shown, "SplitMix64"));
        assert!(has_text(&shown, "initializing"));
        assert!(app.session.simulation_active());
        assert_eq!(
            app.session.set_input(input, true),
            Err(SimulationError::Runtime(
                crate::runtime::RuntimeError::Pending
            ))
        );
        app.startup_seed_text = "not-a-seed".into();
        app.apply_simulation_action(SimAction::Start);
        assert!(
            app.error
                .as_deref()
                .is_some_and(|text| text.contains("unsigned decimal u64"))
        );
        assert_eq!(app.session.startup_seed(), Some(u64::MAX));
        app.apply_simulation_action(SimAction::Stop);
        assert!(!app.session.simulation_active());
        app.startup_seed_text = "42".into();
        app.apply_simulation_action(SimAction::Start);
        assert_eq!(app.session.startup_seed(), Some(42));
        app.apply_simulation_action(SimAction::Stop);
    }

    #[test]
    fn automatic_settle_surfaces_terminal_oscillation_and_keeps_runtime_out_of_ui() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let affected = app
            .session
            .place(ComponentKind::Not, Position { x: 100., y: 100. })
            .unwrap();
        app.session
            .connect([
                pin_ref(affected, PinDirection::Output, 0),
                pin_ref(affected, PinDirection::Input, 0),
            ])
            .unwrap();
        app.selected = Some(affected);
        app.session
            .start_simulation_with_oscillation_limit(1)
            .unwrap();
        app.settling = true;
        let mut rendered = None;
        for _ in 0..3 {
            rendered = Some(frame(&ctx, &mut app, vec![], true));
        }
        assert!(!app.session.simulation_active());
        assert!(!app.settling);
        assert!(
            app.error
                .as_deref()
                .is_some_and(|text| text.contains("appears to oscillate")
                    && text.contains("NOT")
                    && text.contains(&format!("#{}", affected.0)))
        );
        let diagnostic = app.session.oscillation_diagnostic().unwrap();
        assert_eq!(app.sim_state(), SimState::Oscillating);
        let explanation = describe_latched_oscillation(app.session.document(), diagnostic);
        let output = rendered.unwrap();
        assert!(has_text(&output, "State: stopped (oscillation detected"));
        assert!(has_text(&output, &explanation));

        // View operations and an unchanged label Apply clear transient UI errors, not the
        // engine-owned terminal diagnostic rendered by the simulation panel.
        app.fit_extent(
            Rect::from_min_max(Pos2::ZERO, Pos2::new(800., 600.)),
            [0., 0.],
            [100., 100.],
            20.,
        );
        let output = frame(&ctx, &mut app, vec![], true);
        assert!(app.error.is_none());
        assert!(has_text(&output, "State: stopped (oscillation detected"));
        assert!(has_text(&output, &explanation));

        app.begin_label_edit();
        app.finish_label_edit();
        let output = frame(&ctx, &mut app, vec![], true);
        assert!(app.error.is_none());
        assert!(has_text(&output, "State: stopped (oscillation detected"));
        assert!(has_text(&output, &explanation));

        app.apply_simulation_action(SimAction::Start);
        assert!(app.session.oscillation_diagnostic().is_none());
        assert!(matches!(
            app.sim_state(),
            SimState::Pending { .. } | SimState::Settled
        ));
        app.apply_simulation_action(SimAction::Stop);

        assert_eq!(app.session.document().components.len(), 1);
        assert_eq!(app.session.undo().unwrap(), ());
    }

    #[test]
    fn hover_preview_is_painted_without_a_pressed_button() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        app.session
            .place(ComponentKind::Output, Position { x: 300., y: 200. })
            .unwrap();
        app.wiring = true;
        frame(&ctx, &mut app, vec![], true);
        let pin = PinId {
            direction: PinDirection::Output,
            index: 0,
        };
        let anchor = SymbolLayout::centered_on(app.style, ComponentKind::Input, [100., 100.])
            .unwrap()
            .pin_position(pin)
            .unwrap();
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let start = transform.to_screen(anchor);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(start, false)], true);
        assert!(app.geometric_route_start.is_some());
        let target_layout =
            SymbolLayout::centered_on(app.style, ComponentKind::Output, [300., 200.]).unwrap();
        let target = target_layout
            .pin_position(PinId {
                direction: PinDirection::Input,
                index: 0,
            })
            .unwrap();
        let hover = transform.to_screen(target);
        click_text(&ctx, &mut app, "Flip route (Shift+W)");
        assert_eq!(
            app.route_orientation,
            digital_rust::geometric_wiring::RouteOrientation::VerticalFirst
        );
        let output = frame(&ctx, &mut app, vec![egui::Event::PointerMoved(hover)], true);
        let corner = Pos2::new(start.x, hover.y);
        assert!(has_line(&output, start, corner, 1.));
        assert!(has_line(&output, corner, hover, 1.));
        click_at(&ctx, &mut app, hover);
        let points = digital_rust::geometric_wiring::plan_orthogonal_route(
            digital_rust::geometric_wiring::Point::new(
                anchor[0].round() as i32,
                anchor[1].round() as i32,
            ),
            digital_rust::geometric_wiring::Point::new(
                target[0].round() as i32,
                target[1].round() as i32,
            ),
            digital_rust::geometric_wiring::RouteOrientation::VerticalFirst,
        );
        let actual: Vec<_> = app
            .session
            .document()
            .wires
            .geometric()
            .unwrap()
            .iter()
            .map(|s| (s.a, s.b))
            .collect();
        let expected: Vec<_> = points.windows(2).map(|pair| (pair[0], pair[1])).collect();
        assert_eq!(
            actual, expected,
            "committed geometry matches the flipped preview planner"
        );
    }

    #[test]
    fn drag_release_frame_paints_wire_at_committed_anchor() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        let pin = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        app.session
            .connect([pin(a, PinDirection::Output), pin(b, PinDirection::Input)])
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let start = Pos2::new(308., 108.);
        let end = Pos2::new(358., 158.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        let output = frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(
            app.session.document().components[0].position,
            snap_component_center(app.style, ComponentKind::Input, 150., 150.)
        );
        let input_pin = PinId {
            direction: PinDirection::Output,
            index: 0,
        };
        let output_pin = PinId {
            direction: PinDirection::Input,
            index: 0,
        };
        let anchor = |kind, center, pin| {
            let p = SymbolLayout::centered_on(app.style, kind, center)
                .unwrap()
                .pin_position(pin)
                .unwrap();
            Pos2::new(p[0] + 208., p[1] + 8.)
        };
        let moved = anchor(ComponentKind::Input, [150., 150.], input_pin);
        let old = anchor(ComponentKind::Input, [100., 100.], input_pin);
        let target = anchor(ComponentKind::Output, [200., 100.], output_pin);
        // Digital leaves a geometric route stationary when only one component moves.
        assert!(has_line(&output, old, target, 2.));
        assert!(!has_line(&output, moved, target, 2.));
    }
    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        pointer_button(
            pos,
            egui::PointerButton::Primary,
            pressed,
            egui::Modifiers::NONE,
        )
    }
    fn pointer_button(
        pos: Pos2,
        button: egui::PointerButton,
        pressed: bool,
        modifiers: egui::Modifiers,
    ) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        }
    }
    fn camera_rect(ctx: &egui::Context, app: &mut DigitalApp) -> Rect {
        frame(ctx, app, vec![], true);
        app.canvas_rect.expect("canvas has a usable viewport")
    }
    fn component_screen(app: &DigitalApp, id: ComponentId) -> Pos2 {
        let c = app
            .session
            .document()
            .components
            .iter()
            .find(|c| c.id == id)
            .expect("component exists");
        app.view
            .transform(
                app.canvas_rect
                    .expect("canvas rect measured from egui frame"),
            )
            .expect("valid camera")
            .to_screen([c.position.x, c.position.y])
    }
    #[test]
    fn production_canvas_wheel_zoom_preserves_pointer_and_fit_uses_document_extent() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: -400., y: -200. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 500., y: 300. })
            .unwrap();
        let rect = camera_rect(&ctx, &mut app);
        let pointer = rect.center() + Vec2::new(36., 20.);
        let before = app.view.transform(rect).unwrap().to_world(pointer);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0., 80.),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            true,
        );
        assert!(app.view.zoom() > 1.0);
        let after = app.view.transform(rect).unwrap().to_world(pointer);
        assert!((before[0] - after[0]).abs() < 0.01 && (before[1] - after[1]).abs() < 0.01);
        app.view_action = Some(ViewAction::Fit);
        let rect = camera_rect(&ctx, &mut app);
        let t = app.view.transform(rect).unwrap();
        for id in [a, b] {
            let c = app
                .session
                .document()
                .components
                .iter()
                .find(|c| c.id == id)
                .unwrap();
            let layout =
                SymbolLayout::centered_on(app.style, c.kind, [c.position.x, c.position.y]).unwrap();
            let bounds = layout.bounds();
            let pmin = t.to_screen(bounds.min);
            let pmax = t.to_screen(bounds.max);
            assert!(rect.expand(1.).contains(pmin) && rect.expand(1.).contains(pmax));
        }
        assert!(t.zoom() >= crate::canvas::MIN_ZOOM);
    }
    #[test]
    fn fit_keeps_symbols_wires_and_measured_multiline_unicode_labels_visible() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: -420., y: -180. })
            .unwrap();
        let output = app
            .session
            .place(ComponentKind::Output, Position { x: 520., y: 260. })
            .unwrap();
        app.session.set_label(input, "Source\nΔ温度 🧭").unwrap();
        app.session.set_label(output, "Output & result").unwrap();
        let pins = |component, direction| PinRef {
            component,
            pin: PinId {
                direction,
                index: 0,
            },
        };
        app.session
            .connect([
                pins(input, PinDirection::Output),
                pins(output, PinDirection::Input),
            ])
            .unwrap();
        let _ = camera_rect(&ctx, &mut app);
        app.view_action = Some(ViewAction::Fit);
        let output_frame = frame(&ctx, &mut app, vec![], true);
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        for component in &app.session.document().components {
            let layout = SymbolLayout::centered_on(
                app.style,
                component.kind,
                [component.position.x, component.position.y],
            )
            .unwrap();
            let bounds = layout.bounds();
            assert!(
                rect.contains(transform.to_screen(bounds.min))
                    && rect.contains(transform.to_screen(bounds.max))
            );
            let position = transform
                .to_screen([(bounds.min[0] + bounds.max[0]) * 0.5, bounds.max[1]])
                + Vec2::new(0., SELECTION_MARGIN * transform.zoom() + LABEL_GAP);
            let painter = ctx.layer_painter(egui::LayerId::background());
            let galley = painter.layout_no_wrap(
                component.label.clone(),
                egui::FontId::proportional(14.),
                egui::Color32::WHITE,
            );
            let label = Rect::from_center_size(
                position + Vec2::new(0., galley.size().y * 0.5),
                galley.size(),
            );
            assert!(
                rect.contains_rect(label),
                "measured label should fit: {:?}",
                component.label
            );
        }
        for segment in app.session.document().wires.geometric().unwrap() {
            for point in [segment.a, segment.b] {
                assert!(
                    rect.contains(transform.to_screen_i32(point.x, point.y)),
                    "fitted wire path remains visible"
                );
            }
        }
        assert!(output_frame.shapes.iter().all(|shape| match &shape.shape {
            egui::Shape::LineSegment { points, .. } => points.iter().all(|p| p.is_finite()),
            _ => true,
        }));
        app.apply_simulation_action(SimAction::Start);
        app.view_action = Some(ViewAction::Fit);
        let _ = frame(&ctx, &mut app, vec![], true);
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let painter = ctx.layer_painter(egui::LayerId::background());
        for component in &app.session.document().components {
            let layout = SymbolLayout::centered_on(
                app.style,
                component.kind,
                [component.position.x, component.position.y],
            )
            .unwrap();
            let bounds = layout.bounds();
            let value_pin = if component.kind == ComponentKind::Output {
                PinRef {
                    component: component.id,
                    pin: PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                }
            } else {
                PinRef {
                    component: component.id,
                    pin: PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                }
            };
            let value = app
                .session
                .inspect()
                .unwrap()
                .pin_value(value_pin)
                .unwrap_or(Level::Unavailable);
            let galley = painter.layout_no_wrap(
                level_badge(value).into(),
                egui::FontId::monospace(14.),
                egui::Color32::WHITE,
            );
            let position = transform
                .to_screen([(bounds.min[0] + bounds.max[0]) * 0.5, bounds.max[1]])
                + Vec2::new(0., SELECTION_MARGIN * transform.zoom() + LABEL_GAP + 16.);
            assert!(
                rect.contains_rect(Rect::from_center_size(
                    position + Vec2::new(0., galley.size().y * 0.5),
                    galley.size()
                )),
                "runtime badge remains in fitted canvas"
            );
        }
    }
    #[test]
    fn camera_shortcuts_are_canvas_focused_and_modal_safe_without_document_edits() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let rect = camera_rect(&ctx, &mut app);
        click_at(&ctx, &mut app, rect.center());
        let before = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Plus, egui::Modifiers::SHIFT)],
            true,
        );
        assert!(app.view.zoom() > 1.0);
        assert_eq!(*app.session.document(), before);
        assert!(!app.session.dirty());
        let zoom = app.view.zoom();
        app.file_modal = Some(FileModal::OpenPath);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::F, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(
            app.view.zoom(),
            zoom,
            "file modality blocks camera shortcuts"
        );
        assert_eq!(*app.session.document(), before);
    }
    #[test]
    fn file_and_label_modals_reject_toolbar_and_stale_camera_actions() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let rect = camera_rect(&ctx, &mut app);
        assert!(app.view.zoom_at(rect, rect.center(), 2.0));
        click_text(&ctx, &mut app, "Save As");
        assert!(matches!(app.file_modal, Some(FileModal::SavePath { .. })));
        type_file_path(&ctx, &mut app, "pending.dig");
        assert_eq!(app.file_path_entry, "pending.dig");
        let snapshot = app.view;
        for label in ["Reset", "Fit", "+ Zoom in", "− Zoom out"] {
            click_text(&ctx, &mut app, label);
            assert_eq!(app.view, snapshot, "file modal blocks {label}");
        }
        app.view_action = Some(ViewAction::Reset);
        let _ = frame(&ctx, &mut app, vec![], true);
        assert_eq!(
            app.view, snapshot,
            "stale queued action is rejected under file modality"
        );
        assert_eq!(
            app.view_action, None,
            "rejected queued actions are discarded"
        );
        assert!(matches!(app.file_modal, Some(FileModal::SavePath { .. })));
        assert_eq!(app.file_path_entry, "pending.dig");

        app.file_modal = None;
        let id = app
            .session
            .place(ComponentKind::Output, Position { x: 0., y: 0. })
            .unwrap();
        app.selected = Some(id);
        app.begin_label_edit();
        assert!(app.label_modal.is_some());
        app.view_action = Some(ViewAction::Fit);
        let _ = frame(&ctx, &mut app, vec![], true);
        assert_eq!(
            app.view, snapshot,
            "stale queued action is rejected under label modality"
        );
        assert_eq!(app.view_action, None);
        for label in ["Reset", "Fit", "+ Zoom in", "− Zoom out"] {
            click_text(&ctx, &mut app, label);
            assert_eq!(app.view, snapshot, "label modal blocks {label}");
        }
    }
    #[test]
    fn pan_owns_its_button_and_suppresses_canvas_edits_until_release() {
        for owner in [egui::PointerButton::Middle, egui::PointerButton::Primary] {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            let id = app
                .session
                .place(ComponentKind::Input, Position { x: 100., y: 100. })
                .unwrap();
            app.placing = Some(ComponentKind::And);
            app.wiring = true;
            app.geometric_route_start = Some(crate::engine::RouteTarget::Pin(PinRef {
                component: id,
                pin: PinId {
                    direction: PinDirection::Output,
                    index: 0,
                },
            }));
            let rect = camera_rect(&ctx, &mut app);
            let start = app.view.transform(rect).unwrap().to_screen([100., 100.]);
            let mods = if owner == egui::PointerButton::Primary {
                egui::Modifiers::ALT
            } else {
                egui::Modifiers::NONE
            };
            frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(start),
                    pointer_button(start, owner, true, mods),
                ],
                true,
            );
            assert_eq!(app.pan_owner, Some(owner));
            assert_eq!(app.session.document().components.len(), 1);
            let before = app.view;
            let outside = Pos2::new(rect.left() - 40., rect.top() - 30.);
            frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(outside),
                    pointer_button(
                        outside,
                        egui::PointerButton::Secondary,
                        false,
                        egui::Modifiers::NONE,
                    ),
                ],
                true,
            );
            assert_eq!(
                app.pan_owner,
                Some(owner),
                "a non-owner release cannot finish navigation"
            );
            assert_ne!(app.view, before, "drag remains live outside the viewport");
            frame(
                &ctx,
                &mut app,
                vec![pointer_button(outside, owner, false, egui::Modifiers::NONE)],
                true,
            );
            assert_eq!(app.pan_owner, None);
            assert_eq!(
                app.session.document().components.len(),
                1,
                "pan must not place or move components"
            );
            assert_eq!(
                app.selected, None,
                "pan must not select the component underneath"
            );
            assert_eq!(
                app.placing,
                Some(ComponentKind::And),
                "armed placement intent survives navigation"
            );
            assert!(
                app.wiring && app.geometric_route_start.is_some(),
                "pending wire intent survives navigation"
            );
        }
    }
    #[test]
    fn camera_lifecycle_save_history_simulation_and_open_outcomes() {
        let dir = test_dir("camera-lifecycle");
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = egui::Context::default();
        let mut app = ui_file_app(&dir);
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 600., y: 400. })
            .unwrap();
        let path = dir.join("camera.dig");
        app.save_as_path(path.clone(), false, false);
        assert!(path.exists());
        let rect = camera_rect(&ctx, &mut app);
        assert!(app.view.zoom_at(rect, rect.center(), 2.0));
        assert!(app.view.pan(Vec2::new(30., -12.)));
        let view = app.view;

        app.session.undo().unwrap();
        app.session.redo().unwrap();
        assert_eq!(
            app.view, view,
            "document history does not affect the camera"
        );
        app.apply_simulation_action(SimAction::Start);
        assert!(app.session.simulation_active());
        app.apply_simulation_action(SimAction::Stop);
        assert_eq!(
            app.view, view,
            "simulation lifecycle does not affect the camera"
        );
        app.save_current(false);
        assert_eq!(app.view, view, "save does not affect the camera");

        app.pending_action = Some(PendingAction::Open(dir.join("missing.dig")));
        app.complete_pending();
        assert_eq!(app.view, view, "failed Open preserves the camera");
        assert!(app.pending_action.is_some());
        app.pending_action = None;
        app.open_path_dialog();
        type_file_path(&ctx, &mut app, "cancelled.dig");
        click_text(&ctx, &mut app, "Cancel path");
        assert!(app.file_modal.is_none());
        assert_eq!(app.view, view, "cancelled Open preserves the camera");

        app.pending_action = Some(PendingAction::New);
        app.complete_pending();
        assert_eq!(
            app.view,
            CanvasViewState::default(),
            "New resets the camera"
        );
        app.pending_action = Some(PendingAction::Open(path));
        app.complete_pending();
        assert!(app.fit_view_pending, "successful Open schedules fit");
        assert_eq!(app.session.document().components[0].id, id);
        let _ = camera_rect(&ctx, &mut app);
        assert!(!app.fit_view_pending);
        let rect = app.canvas_rect.unwrap();
        let component = app.session.document().components.first().unwrap();
        let layout = SymbolLayout::centered_on(
            app.style,
            component.kind,
            [component.position.x, component.position.y],
        )
        .unwrap();
        let transform = app.view.transform(rect).unwrap();
        assert!(rect.contains(transform.to_screen(layout.bounds().min)));
        assert!(rect.contains(transform.to_screen(layout.bounds().max)));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn additional_chorded_press_cannot_steal_pan_ownership_in_either_order() {
        for (owner, extra, owner_modifiers) in [
            (
                egui::PointerButton::Middle,
                egui::PointerButton::Primary,
                egui::Modifiers::NONE,
            ),
            (
                egui::PointerButton::Primary,
                egui::PointerButton::Middle,
                egui::Modifiers::ALT,
            ),
        ] {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            let id = app
                .session
                .place(ComponentKind::Input, Position { x: 80., y: 80. })
                .unwrap();
            let rect = camera_rect(&ctx, &mut app);
            let point = app.view.transform(rect).unwrap().to_screen([80., 80.]);
            frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(point),
                    pointer_button(point, owner, true, owner_modifiers),
                ],
                true,
            );
            assert_eq!(app.pan_owner, Some(owner));

            let extra_modifiers = if extra == egui::PointerButton::Primary {
                egui::Modifiers::ALT
            } else {
                egui::Modifiers::NONE
            };
            frame(
                &ctx,
                &mut app,
                vec![pointer_button(point, extra, true, extra_modifiers)],
                true,
            );
            assert_eq!(
                app.pan_owner,
                Some(owner),
                "extra press cannot steal the gesture"
            );
            frame(
                &ctx,
                &mut app,
                vec![pointer_button(point, extra, false, extra_modifiers)],
                true,
            );
            assert_eq!(
                app.pan_owner,
                Some(owner),
                "extra release cannot finish the gesture"
            );
            assert_eq!(app.selected, None, "chorded actions remain navigation-only");
            assert_eq!(
                app.session.document().components[0].position,
                Position { x: 80., y: 80. }
            );

            frame(
                &ctx,
                &mut app,
                vec![pointer_button(point, owner, false, egui::Modifiers::NONE)],
                true,
            );
            assert_eq!(
                app.pan_owner, None,
                "only the owner release ends navigation"
            );
            click_at(&ctx, &mut app, point);
            assert_eq!(
                app.selected,
                Some(id),
                "ordinary pointer interaction resumes after owner release"
            );
        }
    }
    #[test]
    fn escape_and_focus_loss_cancel_pan_without_committing_edits() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::And, Position { x: 80., y: 80. })
            .unwrap();
        let rect = camera_rect(&ctx, &mut app);
        let p = rect.center();
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p),
                pointer_button(p, egui::PointerButton::Middle, true, egui::Modifiers::NONE),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.pan_owner, None);
        assert_eq!(app.session.document().components[0].position.x, 80.);
        frame(
            &ctx,
            &mut app,
            vec![pointer_button(
                p,
                egui::PointerButton::Middle,
                true,
                egui::Modifiers::NONE,
            )],
            true,
        );
        frame(&ctx, &mut app, vec![], false);
        assert_eq!(app.pan_owner, None);
        assert_eq!(app.session.document().components[0].position.x, 80.);
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    #[test]
    fn fast_drag_and_escape_and_focus_cancel() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::And, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        // The side panel default width is 200, plus the central margin.
        let start = Pos2::new(308., 108.);
        let end = start + Vec2::new(100., 50.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(
            app.session.document().components[0].position,
            snap_component_center(app.style, ComponentKind::And, 200., 150.)
        );
        app.session.undo().unwrap();
        assert_eq!(
            app.session.document().components[0].position,
            Position { x: 100., y: 100. }
        );
        app.session.redo().unwrap();
        let snapshot = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end), button(end, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(end + Vec2::new(50., 0.)),
                key(egui::Key::Escape, egui::Modifiers::NONE),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(end + Vec2::new(50., 0.), false)],
            true,
        );
        assert_eq!(*app.session.document(), snapshot);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end), button(end, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end + Vec2::new(50., 0.))],
            false,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(end + Vec2::new(50., 0.), false)],
            true,
        );
        assert_eq!(*app.session.document(), snapshot);
        assert_eq!(app.session.document().components[0].id, id);
    }
    #[test]
    fn batched_selection_shortcuts_and_delete_drag() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p),
                button(p, true),
                button(p, false),
            ],
            true,
        );
        assert_eq!(app.selected, Some(id));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Z, egui::Modifiers::COMMAND)],
            true,
        );
        assert!(!app.session.dirty());
        let shifted = egui::Modifiers {
            shift: true,
            ..egui::Modifiers::COMMAND
        };
        frame(&ctx, &mut app, vec![key(egui::Key::Z, shifted)], true);
        assert_eq!(app.session.document().components[0].id, id);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p + Vec2::new(30., 0.))],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(p + Vec2::new(30., 0.), false)],
            true,
        );
        assert!(app.session.document().components.is_empty());
        assert!(app.error.is_none());
    }
    #[test]
    fn placement_escape_does_not_edit() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        frame(&ctx, &mut app, vec![], true);
        app.placing = Some(ComponentKind::Or);
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p + Vec2::new(30., 0.)),
                key(egui::Key::Escape, egui::Modifiers::NONE),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![button(p + Vec2::new(30., 0.), false)],
            true,
        );
        assert!(!app.session.dirty());
        assert!(app.session.document().components.is_empty());
    }
    #[test]
    fn palette_focus_and_placement() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        frame(&ctx, &mut app, vec![], true);
        let palette = Pos2::new(30., 29.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(palette), button(palette, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(palette, false)], true);
        assert_eq!(app.placing, Some(ComponentKind::Input));
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert_eq!(app.session.document().components.len(), 1);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(palette), button(palette, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(palette, false)], true);
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.session.document().components.len(), 1);
    }
    #[test]
    fn new_logic_gate_palette_placement_anchors_and_styles_are_shared() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let placements = [
            ("NAND", ComponentKind::NAnd),
            ("NOR", ComponentKind::NOr),
            ("XOR", ComponentKind::XOr),
            ("XNOR", ComponentKind::XNOr),
        ];
        let mut canvas_centers = Vec::new();
        for (index, (name, kind)) in placements.into_iter().enumerate() {
            click_text(&ctx, &mut app, name);
            let center = Pos2::new(
                350. + (index % 2) as f32 * 120.,
                180. + (index / 2) as f32 * 100.,
            );
            click_at(&ctx, &mut app, center);
            assert_eq!(app.session.document().components.last().unwrap().kind, kind);
            canvas_centers.push((name, center));
        }
        for style in [SymbolStyle::Rect, SymbolStyle::Ieee] {
            app.style = style;
            for component in app.session.document().components.iter().filter(|c| {
                matches!(
                    c.kind,
                    ComponentKind::NAnd
                        | ComponentKind::NOr
                        | ComponentKind::XOr
                        | ComponentKind::XNOr
                )
            }) {
                let layout = SymbolLayout::centered_on(
                    style,
                    component.kind,
                    [component.position.x, component.position.y],
                )
                .unwrap();
                assert_eq!(layout.anchors().count(), 3);
                assert_eq!(
                    layout
                        .pin_position(PinId {
                            direction: PinDirection::Input,
                            index: 0
                        })
                        .unwrap()[0],
                    layout
                        .pin_position(PinId {
                            direction: PinDirection::Input,
                            index: 1
                        })
                        .unwrap()[0]
                );
                let expected = if component.kind == ComponentKind::XOr {
                    60.
                } else {
                    80.
                };
                assert_eq!(
                    layout
                        .pin_position(PinId {
                            direction: PinDirection::Output,
                            index: 0
                        })
                        .unwrap()[0]
                        - layout
                            .pin_position(PinId {
                                direction: PinDirection::Input,
                                index: 0
                            })
                            .unwrap()[0],
                    expected
                );
            }
            let painted = frame(&ctx, &mut app, vec![], true);
            for (name, center) in &canvas_centers {
                let has_canvas_name = painted.shapes.iter().any(|shape| {
                    matches!(
                        &shape.shape,
                        egui::Shape::Text(text)
                            if text.galley.job.text == *name
                                && text.pos.distance(*center) < 80.
                    )
                });
                assert!(
                    has_canvas_name,
                    "{name} must be painted near its placed canvas component at {center:?}"
                );
            }
        }
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(app.session.document().components.len(), 3);
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert_eq!(app.session.document().components.len(), 4);
    }

    #[test]
    fn redo_y_and_error_display() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::Or, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let p = Pos2::new(308., 108.);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(p),
                button(p, true),
                button(p, false),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Z, egui::Modifiers::COMMAND)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Y, egui::Modifiers::COMMAND)],
            true,
        );
        assert_eq!(app.session.document().components[0].id, id);
        app.selected = Some(ComponentId(999));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert_eq!(app.error.as_deref(), Some("Unknown component"));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("Unknown component"))));
    }
    #[test]
    fn edge_drag_and_noop_preserve_redo() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Not, Position { x: 100., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        // Just inside the right edge of the NOT symbol's bounds (the canvas origin is at x = 208).
        let edge = SymbolLayout::centered_on(DEFAULT_STYLE, ComponentKind::Not, [100., 100.])
            .unwrap()
            .bounds()
            .max[0];
        let p = Pos2::new(208. + edge - 1., 108.);
        let end = p + Vec2::new(120., 0.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(app.session.document().components[0].position.x, 220.);
        app.session.undo().unwrap();
        let before = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p + Vec2::new(20., 0.))],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(p)], true);
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert_eq!(*app.session.document(), before);
        app.session.redo().unwrap();
        assert_eq!(app.session.document().components[0].position.x, 220.);
    }
    #[test]
    fn asymmetric_symbol_positions_snap_the_logical_origin_not_the_artwork_center() {
        for kind in [ComponentKind::Input, ComponentKind::Output] {
            let position = snap_component_center(DEFAULT_STYLE, kind, 101.0, 103.0);
            let origin =
                crate::symbols::origin_from_center(DEFAULT_STYLE, kind, [position.x, position.y])
                    .unwrap();
            assert_eq!(origin[0] % GRID_UNIT, 0.0, "{kind:?} x origin");
            assert_eq!(origin[1] % GRID_UNIT, 0.0, "{kind:?} y origin");
            let center_offset =
                crate::symbols::center_from_origin_offset(DEFAULT_STYLE, kind).unwrap();
            if center_offset[0] != 0.0 {
                assert_ne!(
                    position.x % GRID_UNIT,
                    0.0,
                    "asymmetric x center need not lie on grid"
                );
            }
        }
    }
    #[test]
    fn headless_frame() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let output = ctx.run(Default::default(), |ctx| app.show(ctx));
        assert!(!output.shapes.is_empty());
    }

    fn mesh_vertices(output: &egui::FullOutput) -> Vec<(Pos2, egui::Color32)> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Mesh(mesh) => Some(mesh),
                _ => None,
            })
            .flat_map(|mesh| mesh.vertices.iter().map(|v| (v.pos, v.color)))
            .collect()
    }
    fn run_frame(ctx: &egui::Context, app: &mut DigitalApp) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            ..Default::default()
        };
        ctx.run(input, |ctx| app.show(ctx))
    }
    fn app_with(kind: ComponentKind) -> DigitalApp {
        let mut app = DigitalApp::default();
        app.session
            .place(kind, Position { x: 100., y: 100. })
            .unwrap();
        app
    }
    #[test]
    fn const_palette_configuration_and_named_fallback_are_visible_in_both_styles() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let initial = run_frame(&ctx, &mut app);
        assert!(has_text(&initial, "Const"));
        click_text(&ctx, &mut app, "Const");
        assert_eq!(app.placing, Some(ComponentKind::Const));
        let id = app
            .session
            .place(ComponentKind::Const, Position { x: 100., y: 100. })
            .unwrap();
        app.selected = Some(id);
        for style in [SymbolStyle::Rect, SymbolStyle::Ieee] {
            app.style = style;
            let shown = run_frame(&ctx, &mut app);
            assert!(has_text(&shown, "Const configured value: HIGH (1)"));
            assert!(has_text(&shown, "1"));
            let bounds = SymbolLayout::centered_on(style, ComponentKind::Const, [100., 100.])
                .unwrap()
                .bounds();
            assert_eq!(bounds.min, [80., 80.]);
            assert_eq!(bounds.max, [120., 120.]);
            app.session.start_simulation().unwrap();
            app.session.settle_simulation(10).unwrap();
            click_text(&ctx, &mut app, "Set LOW (0)");
            assert!(app.session.document().components[0].constant_value);
            app.session.stop_simulation().unwrap();
            click_text(&ctx, &mut app, "Set LOW (0)");
            assert!(!app.session.document().components[0].constant_value);
            app.session.undo().unwrap();
            assert!(app.session.document().components[0].constant_value);
            app.session.redo().unwrap();
            if style == SymbolStyle::Rect {
                app.session.set_constant_value(id, true).unwrap();
            }
        }
    }

    #[test]
    fn const_workflow_uses_canvas_keyboard_and_file_simulation_controls() {
        let ctx = egui::Context::default();
        let dir = test_dir("const-ui-workflow");
        let mut app = ui_file_app(&dir);
        for (palette, center) in [
            ("Const", Pos2::new(408., 158.)),
            ("Output", Pos2::new(528., 158.)),
        ] {
            click_text(&ctx, &mut app, palette);
            click_at(&ctx, &mut app, center);
        }
        let const_id = app.session.document().components[0].id;
        let output_id = app.session.document().components[1].id;
        click_at(&ctx, &mut app, Pos2::new(408., 158.));
        assert_eq!(app.selected, Some(const_id));
        assert!(has_text(
            &frame(&ctx, &mut app, vec![], true),
            "H = HIGH (1), L = LOW (0)"
        ));

        click_text(&ctx, &mut app, "Wire (W)");
        let style = app.style;
        let transform = app.view.transform(app.canvas_rect.unwrap()).unwrap();
        let pin_screen = |id, pin| {
            let component = app
                .session
                .document()
                .components
                .iter()
                .find(|c| c.id == id)
                .unwrap();
            let point = SymbolLayout::centered_on(
                style,
                component.kind,
                [component.position.x, component.position.y],
            )
            .unwrap()
            .pin_position(pin)
            .unwrap();
            transform.to_screen(point)
        };
        let const_pin_screen = pin_screen(
            const_id,
            PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        );
        let output_pin_screen = pin_screen(
            output_id,
            PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        );
        click_at(&ctx, &mut app, const_pin_screen);
        click_at(&ctx, &mut app, output_pin_screen);
        click_text(&ctx, &mut app, "Wire (W)");
        assert_eq!(app.session.document().wires.len(), 1);

        click_at(&ctx, &mut app, Pos2::new(408., 158.));
        press(&ctx, &mut app, egui::Key::L, egui::Modifiers::NONE);
        assert!(!app.session.document().components[0].constant_value);
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert!(app.session.document().components[0].constant_value);
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert!(!app.session.document().components[0].constant_value);
        click_at(&ctx, &mut app, Pos2::new(408., 158.));
        press(&ctx, &mut app, egui::Key::H, egui::Modifiers::NONE);
        assert!(app.session.document().components[0].constant_value);

        click_text(&ctx, &mut app, "Save As");
        assert!(matches!(app.file_modal, Some(FileModal::SavePath { .. })));
        type_file_path(&ctx, &mut app, "const.dig");
        click_text(&ctx, &mut app, "Save path");
        assert_eq!(app.session.path(), Some(dir.join("const.dig").as_path()));
        click_text(&ctx, &mut app, "New");
        assert!(app.session.document().components.is_empty());
        click_text(&ctx, &mut app, "Open");
        assert!(matches!(app.file_modal, Some(FileModal::OpenPath)));
        // Open modal has modality priority over canvas keyboard shortcuts.
        press(&ctx, &mut app, egui::Key::L, egui::Modifiers::NONE);
        assert_eq!(app.session.document().components.len(), 0);
        type_file_path(&ctx, &mut app, "const.dig");
        click_text(&ctx, &mut app, "Open path");
        assert_eq!(app.session.document().components.len(), 2);
        assert!(app.session.document().components[0].constant_value);
        assert_eq!(app.session.document().wires.len(), 1);
        let component_pos = component_screen(&app, const_id);
        click_at(&ctx, &mut app, component_pos);
        assert_eq!(app.selected, Some(const_id));
        click_text(&ctx, &mut app, "Start");
        for _ in 0..10 {
            let shown = frame(&ctx, &mut app, vec![], true);
            if has_text(&shown, "State: settled") {
                break;
            }
        }
        assert_eq!(output_level(&app, output_id), Some(Level::High));
        let component_pos = component_screen(&app, const_id);
        click_at(&ctx, &mut app, component_pos);
        press(&ctx, &mut app, egui::Key::L, egui::Modifiers::NONE);
        assert!(
            app.session.document().components[0].constant_value,
            "simulation locks value editing"
        );
        click_text(&ctx, &mut app, "Stop");
        assert!(!app.session.simulation_active());
        let component_pos = component_screen(&app, const_id);
        click_at(&ctx, &mut app, component_pos);
        press(&ctx, &mut app, egui::Key::L, egui::Modifiers::NONE);
        assert!(!app.session.document().components[0].constant_value);
        assert!(app.session.dirty());
        let component_pos = component_screen(&app, const_id);
        click_at(&ctx, &mut app, component_pos);
        press(&ctx, &mut app, egui::Key::O, egui::Modifiers::COMMAND);
        assert!(matches!(app.file_modal, Some(FileModal::OpenPath)));
        let before_modal = app.session.document().clone();
        press(&ctx, &mut app, egui::Key::H, egui::Modifiers::NONE);
        assert_eq!(
            *app.session.document(),
            before_modal,
            "file modality blocks Const shortcuts"
        );
        click_text(&ctx, &mut app, "Cancel path");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_simulation_controls_start_and_stop_feedback_without_startup_claims() {
        use crate::domain::{PinDirection, PinId};
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::Not);
        let id = app.session.document().components[0].id;
        app.session
            .connect([
                PinRef {
                    component: id,
                    pin: PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                },
                PinRef {
                    component: id,
                    pin: PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                },
            ])
            .unwrap();
        click_text(&ctx, &mut app, "Start");
        assert!(app.session.simulation_active());
        assert!(app.failed_start.is_none());
        click_text(&ctx, &mut app, "Stop");
        assert!(!app.session.simulation_active());
    }

    #[test]
    fn shape_style_defaults_to_rectangular_and_is_offered_in_the_palette() {
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::And);
        assert_eq!(app.style, SymbolStyle::Rect);
        let output = run_frame(&ctx, &mut app);
        for label in ["Rectangular", "IEEE"] {
            assert!(
                output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text.contains(label))),
                "palette lacks {label}"
            );
        }
    }
    #[test]
    fn style_toggle_changes_the_painted_shapes_but_not_the_document() {
        let ctx = egui::Context::default();
        for kind in [ComponentKind::And, ComponentKind::Or, ComponentKind::Not] {
            let mut app = app_with(kind);
            let rect = mesh_vertices(&run_frame(&ctx, &mut app));
            let document = app.session.document().clone();
            let dirty = app.session.dirty();
            app.style = SymbolStyle::Ieee;
            let ieee = mesh_vertices(&run_frame(&ctx, &mut app));
            assert!(!rect.is_empty() && !ieee.is_empty(), "{kind:?}");
            assert_ne!(rect, ieee, "{kind:?}");
            assert_eq!(*app.session.document(), document, "{kind:?}");
            assert_eq!(app.session.dirty(), dirty, "{kind:?}");
        }
    }
    #[test]
    fn symbols_are_painted_in_ink_and_selected_ones_in_the_selection_color() {
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::And);
        let visuals = egui::Visuals::dark();
        let ink = visuals.text_color();
        let selection = visuals.selection.stroke.color;
        let colors = |app: &mut DigitalApp| -> Vec<egui::Color32> {
            mesh_vertices(&run_frame(&ctx, app))
                .into_iter()
                .map(|(_, color)| color)
                .collect()
        };
        let plain = colors(&mut app);
        assert!(plain.contains(&ink) && !plain.contains(&selection));
        app.selected = Some(app.session.document().components[0].id);
        let selected = colors(&mut app);
        assert!(selected.contains(&selection) && !selected.contains(&ink));
    }
    #[test]
    fn clicks_pick_symbol_bounds_not_the_legacy_box() {
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::Input);
        frame(&ctx, &mut app, vec![], true);
        let bounds = SymbolLayout::centered_on(DEFAULT_STYLE, ComponentKind::Input, [100., 100.])
            .unwrap()
            .bounds();
        // The canvas origin is at (208, 8); the former 80 x 40 box reached 40 units right of
        // the position, beyond the much smaller input symbol.
        assert!(bounds.max[0] < 100. + 35.);
        let click = |ctx: &egui::Context, app: &mut DigitalApp, world: [f32; 2]| {
            let p = Pos2::new(208. + world[0], 8. + world[1]);
            frame(
                ctx,
                app,
                vec![egui::Event::PointerMoved(p), button(p, true)],
                true,
            );
            frame(ctx, app, vec![button(p, false)], true);
        };
        click(&ctx, &mut app, [bounds.max[0] + 10., 100.]);
        assert_eq!(app.selected, None);
        click(&ctx, &mut app, [bounds.max[0] - 1., 100.]);
        assert!(app.selected.is_some());
        click(&ctx, &mut app, [100., bounds.max[1] + 10.]);
        assert_eq!(app.selected, None);
    }
    #[test]
    fn picking_follows_the_chosen_style() {
        // An IEEE gate is wider or narrower than its rectangular twin by definition of the
        // bounds; picking must use the bounds of the style currently shown.
        let ctx = egui::Context::default();
        let mut app = app_with(ComponentKind::Or);
        app.style = SymbolStyle::Ieee;
        frame(&ctx, &mut app, vec![], true);
        let bounds = SymbolLayout::centered_on(SymbolStyle::Ieee, ComponentKind::Or, [100., 100.])
            .unwrap()
            .bounds();
        let p = Pos2::new(208. + bounds.min[0] + 1., 8. + 100.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(p, false)], true);
        assert!(app.selected.is_some());
    }
    #[test]
    fn transformed_symbol_and_wire_picking_keep_screen_space_tolerances() {
        for zoom in [0.5, 3.0] {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            let rect = camera_rect(&ctx, &mut app);
            let anchor = app.view.transform(rect).unwrap().to_world(rect.center());
            let input_position = Position {
                x: anchor[0],
                y: anchor[1],
            };
            let output_position = Position {
                x: anchor[0] + 100.,
                y: anchor[1],
            };
            let input = app
                .session
                .place(ComponentKind::Input, input_position)
                .unwrap();
            let output = app
                .session
                .place(ComponentKind::Output, output_position)
                .unwrap();
            let pins = [
                PinRef {
                    component: input,
                    pin: PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                },
                PinRef {
                    component: output,
                    pin: PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                },
            ];
            app.session.connect(pins).unwrap();
            let wire_segment_id = app
                .session
                .document()
                .wires
                .geometric()
                .unwrap()
                .iter()
                .max_by_key(|segment| {
                    (
                        segment.a.x.abs_diff(segment.b.x),
                        segment.a.y.abs_diff(segment.b.y),
                    )
                })
                .unwrap()
                .id;
            assert!(app.view.zoom_at(rect, rect.center(), zoom));
            assert!(app.view.pan(Vec2::new(17., -9.)));
            let rect = camera_rect(&ctx, &mut app);
            let transform = app.view.transform(rect).unwrap();
            let symbol = SymbolLayout::centered_on(
                app.style,
                ComponentKind::Input,
                [input_position.x, input_position.y],
            )
            .unwrap();
            let bounds = symbol.bounds();
            let inside = transform
                .to_screen([bounds.max[0], (bounds.min[1] + bounds.max[1]) * 0.5])
                + Vec2::new(1.75, 0.);
            click_at(&ctx, &mut app, inside);
            assert_eq!(
                app.selected,
                Some(input),
                "symbol pick includes 1.75 screen points at zoom {zoom}; point={inside:?}, rect={rect:?}, world={:?}, bounds={bounds:?}",
                transform.to_world(inside)
            );
            let outside = transform
                .to_screen([bounds.max[0], (bounds.min[1] + bounds.max[1]) * 0.5])
                + Vec2::new(2.25, 0.);
            click_at(&ctx, &mut app, outside);
            assert_eq!(
                app.selected, None,
                "symbol pick excludes 2.25 screen points at zoom {zoom}"
            );

            let a = SymbolLayout::centered_on(
                app.style,
                ComponentKind::Input,
                [input_position.x, input_position.y],
            )
            .unwrap()
            .pin_position(pins[0].pin)
            .unwrap();
            let b = SymbolLayout::centered_on(
                app.style,
                ComponentKind::Output,
                [output_position.x, output_position.y],
            )
            .unwrap()
            .pin_position(pins[1].pin)
            .unwrap();
            let path = crate::wire_geometry::path((pins[0], a), (pins[1], b));
            let (from, to) = path
                .windows(2)
                .max_by(|a, b| {
                    Pos2::new(a[0][0], a[0][1])
                        .distance(Pos2::new(a[1][0], a[1][1]))
                        .total_cmp(
                            &Pos2::new(b[0][0], b[0][1]).distance(Pos2::new(b[1][0], b[1][1])),
                        )
                })
                .map(|pair| (pair[0], pair[1]))
                .unwrap();
            let mid = [(from[0] + to[0]) * 0.5, (from[1] + to[1]) * 0.5];
            let along = if (to[0] - from[0]).abs() >= (to[1] - from[1]).abs() {
                Vec2::new(0., 1.)
            } else {
                Vec2::new(1., 0.)
            };
            let wire_inside = transform.to_screen(mid) + along * 5.5;
            click_at(&ctx, &mut app, wire_inside);
            assert_eq!(
                app.selected_segment,
                Some(wire_segment_id),
                "wire pick includes 5.5 screen points at zoom {zoom}"
            );
            let wire_outside = transform.to_screen(mid) + along * 6.5;
            click_at(&ctx, &mut app, wire_outside);
            assert_eq!(
                app.selected_segment, None,
                "wire pick excludes 6.5 screen points at zoom {zoom}"
            );
        }
    }
    fn pin_ref(component: ComponentId, direction: PinDirection, index: u8) -> PinRef {
        PinRef {
            component,
            pin: PinId { direction, index },
        }
    }
    /// Input (100,100) -> `nots` NOT gates -> Output, wired through the engine.
    fn inverter_chain(app: &mut DigitalApp, nots: usize) -> (ComponentId, ComponentId) {
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let mut source = input;
        for n in 0..nots {
            let not = app
                .session
                .place(
                    ComponentKind::Not,
                    Position {
                        x: 200. + 100. * n as f32,
                        y: 100.,
                    },
                )
                .unwrap();
            app.session
                .connect([
                    pin_ref(source, PinDirection::Output, 0),
                    pin_ref(not, PinDirection::Input, 0),
                ])
                .unwrap();
            source = not;
        }
        let output = app
            .session
            .place(
                ComponentKind::Output,
                Position {
                    x: 200. + 100. * nots as f32,
                    y: 100.,
                },
            )
            .unwrap();
        app.session
            .connect([
                pin_ref(source, PinDirection::Output, 0),
                pin_ref(output, PinDirection::Input, 0),
            ])
            .unwrap();
        (input, output)
    }
    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }
    fn has_text(output: &egui::FullOutput, needle: &str) -> bool {
        texts(output).iter().any(|text| text.contains(needle))
    }
    fn click_at(ctx: &egui::Context, app: &mut DigitalApp, p: Pos2) {
        frame(
            ctx,
            app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
            true,
        );
        frame(ctx, app, vec![button(p, false)], true);
    }
    /// Clicks the widget whose text is exactly `label`, as found in a freshly painted frame.
    fn click_text(ctx: &egui::Context, app: &mut DigitalApp, label: &str) {
        let output = frame(ctx, app, vec![], true);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => Some(text.pos),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {label:?} in {:?}", texts(&output)));
        click_at(ctx, app, pos + Vec2::new(4., 4.));
    }
    fn type_file_path(ctx: &egui::Context, app: &mut DigitalApp, path: &str) {
        let output = frame(ctx, app, vec![], true);
        let label = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if text
                        .galley
                        .job
                        .text
                        .contains("Path relative to the application's startup directory") =>
                {
                    Some(text.pos)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("path field label is missing: {:?}", texts(&output)));
        click_at(ctx, app, label + Vec2::new(40., 47.));
        frame(
            ctx,
            app,
            vec![
                key(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text(path.to_owned()),
            ],
            true,
        );
        assert_eq!(app.file_path_entry, path);
    }
    fn press(
        ctx: &egui::Context,
        app: &mut DigitalApp,
        key_: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        frame(ctx, app, vec![key(key_, modifiers)], true);
    }
    fn output_level(app: &DigitalApp, output: ComponentId) -> Option<Level> {
        app.session.inspect()?.output_value(output)
    }
    fn wants_immediate_repaint(output: &egui::FullOutput) -> bool {
        output
            .viewport_output
            .values()
            .any(|viewport| viewport.repaint_delay.is_zero())
    }

    #[test]
    fn start_toggle_step_settle_inspect_stop_then_edit_and_undo() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (input, output) = inverter_chain(&mut app, 1);
        let wired = app.session.document().clone();
        click_text(&ctx, &mut app, "Start");
        assert!(app.session.simulation_active());
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: settled"), "{:?}", texts(&shown));
        assert_eq!(output_level(&app, output), Some(Level::High));
        assert!(has_text(&shown, "= 1"));
        assert!(has_text(&shown, "1 (high)"));
        // Toggle the input from the keyboard-reachable list; the inverter follows.
        click_text(&ctx, &mut app, "Input #0: 0 (low)");
        let shown = frame(&ctx, &mut app, vec![], true);
        assert_eq!(output_level(&app, output), Some(Level::Low));
        assert!(has_text(&shown, "Input #0: 1 (high)"));
        assert!(has_text(&shown, "= 0"));
        // Toggling creates no history and does not touch the document.
        assert_eq!(*app.session.document(), wired);
        assert!(app.session.dirty());
        click_text(&ctx, &mut app, "Stop");
        assert!(!app.session.simulation_active());
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: idle"));
        assert!(!has_text(&shown, "= 0"));
        // Editing works again, and undo removes exactly the edit.
        app.placing = Some(ComponentKind::And);
        click_at(&ctx, &mut app, Pos2::new(408., 300.));
        assert_eq!(app.session.document().components.len(), 4);
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(*app.session.document(), wired);
        assert_eq!(input, app.session.document().components[0].id);
    }

    #[test]
    fn gate_values_are_exposed_as_labelled_widgets() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, 2);
        click_text(&ctx, &mut app, "Start");
        let shown = frame(&ctx, &mut app, vec![], true);
        let gates: Vec<_> = app
            .session
            .document()
            .components
            .iter()
            .filter(|c| c.kind == ComponentKind::Not)
            .map(|c| c.id.0)
            .collect();
        assert_eq!(gates.len(), 2);
        // Input is low, so the first NOT is high and the second low.
        assert!(has_text(
            &shown,
            &format!("NOT gate id {}: 1 (high)", gates[0])
        ));
        assert!(has_text(
            &shown,
            &format!("NOT gate id {}: 0 (low)", gates[1])
        ));
        assert_eq!(output_level(&app, output), Some(Level::Low));
        // Standard widgets, unlike painted badges, reach the AccessKit tree.
        let accessible: Vec<String> = shown
            .platform_output
            .accesskit_update
            .iter()
            .flat_map(|update| update.nodes.iter())
            .filter_map(|(_, node)| node.value().or_else(|| node.label()).map(str::to_owned))
            .collect();
        for (gate, expected) in gates.iter().zip(["1 (high)", "0 (low)"]) {
            let label = format!("NOT gate id {gate}: {expected}");
            assert!(
                accessible.contains(&label),
                "{label:?} not in {accessible:?}"
            );
        }
    }

    /// Position of the first text shape equal to `label` and whether it lies inside its clip.
    fn text_position(output: &egui::FullOutput, label: &str) -> Option<(Pos2, bool)> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some((text.pos, shape.clip_rect.contains(text.pos)))
            }
            _ => None,
        })
    }

    #[test]
    fn overflowing_simulation_lists_scroll_and_keep_controls_fixed() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let inputs: Vec<_> = (0..30)
            .map(|n| {
                app.session
                    .place(
                        ComponentKind::Input,
                        Position {
                            x: 100.,
                            y: 20. * n as f32,
                        },
                    )
                    .unwrap()
            })
            .collect();
        let last = inputs[29];
        let last_label = format!("Input #{}: 0 (low)", last.0);
        click_text(&ctx, &mut app, "Start");
        let shown = frame(&ctx, &mut app, vec![], true);
        // egui culls shapes outside the clip rectangle, so "absent" also means not visible.
        assert!(
            !text_position(&shown, &last_label).is_some_and(|(_, visible)| visible),
            "30 inputs must overflow the 600-point viewport"
        );
        let (start, start_visible) = text_position(&shown, "Start").unwrap();
        assert!(start_visible);
        // Wheel over the panel scrolls the list; the controls stay where they were.
        let (input_header, input_header_visible) =
            text_position(&shown, "Inputs (toggle when settled)").unwrap();
        assert!(input_header_visible);
        let over_panel = input_header + Vec2::new(6.0, 6.0);
        let mut events = vec![
            egui::Event::PointerMoved(over_panel),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0., -2_000.),
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let mut shown = frame(&ctx, &mut app, std::mem::take(&mut events), true);
        for _ in 0..30 {
            shown = frame(&ctx, &mut app, vec![], true);
        }
        let Some((_, visible)) = text_position(&shown, &last_label) else {
            panic!("last input not found after scroll: {:?}", texts(&shown));
        };
        assert!(visible, "last input scrolled into view");
        assert_eq!(text_position(&shown, "Start"), Some((start, true)));
        assert!(!app.session.inspect().unwrap().is_pending());
        // Hover first: a pointer that moves while pressing would drag-scroll the list.
        let shown = frame(&ctx, &mut app, vec![], true);
        let (label_pos, visible) = text_position(&shown, &last_label).unwrap();
        assert!(visible);
        let target = label_pos + Vec2::new(4., 4.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(target)],
            true,
        );
        click_at(&ctx, &mut app, target);
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(
            has_text(&shown, &format!("Input #{}: 1 (high)", last.0)),
            "{:?}",
            texts(&shown)
        );
        let runtime = app.session.inspect().unwrap();
        assert_eq!(
            runtime.pin_value(pin_ref(last, PinDirection::Output, 0)),
            Some(Level::High)
        );
        assert_eq!(
            runtime.pin_value(pin_ref(inputs[0], PinDirection::Output, 0)),
            Some(Level::Low)
        );
    }

    #[test]
    fn step_runs_one_microstep_and_settle_resumes_with_the_same_result() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, 2);
        app.settle_budget = 1;
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(app.session.inspect().unwrap().is_pending());
        assert_eq!(output_level(&app, output), Some(Level::Low));
        press(&ctx, &mut app, egui::Key::F10, egui::Modifiers::NONE);
        assert!(!app.settling);
        // F10's logical request can span frames when the per-frame work budget is one.
        for _ in 0..50 {
            frame(&ctx, &mut app, vec![], true);
            if !app.microstepping {
                break;
            }
        }
        assert!(!app.microstepping, "bounded microstep eventually completes");
        let paused = app.session.inspect().unwrap().pending_gate_count();
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "paused"));
        assert!(
            has_text(&shown, "NEXT"),
            "successor frontier is visibly identified"
        );
        assert_eq!(
            app.session.inspect().unwrap().pending_gate_count(),
            paused,
            "a paused simulation must not advance"
        );
        // One microstep cannot carry the value through both gates.
        assert_eq!(output_level(&app, output), Some(Level::Low));
        press(&ctx, &mut app, egui::Key::F6, egui::Modifiers::NONE);
        for _ in 0..50 {
            frame(&ctx, &mut app, vec![], true);
        }
        assert_eq!(output_level(&app, output), Some(Level::Low));
        assert!(!app.session.inspect().unwrap().is_pending());
    }

    #[test]
    fn paused_start_microstep_full_step_and_pause_shortcuts_route_to_engine() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, 2);
        press(
            &ctx,
            &mut app,
            egui::Key::F10,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::NONE
            },
        );
        assert!(app.session.simulation_active());
        assert!(!app.settling);
        assert!(app.session.inspect().unwrap().is_pending());
        let pending = app.session.inspect().unwrap().pending_gate_count();
        press(&ctx, &mut app, egui::Key::F10, egui::Modifiers::NONE);
        assert!(
            !app.session
                .inspect()
                .unwrap()
                .pending_inspection()
                .next_scheduled
                .is_empty()
        );
        assert!(!app.settling);
        assert_eq!(output_level(&app, output), Some(Level::Low));
        press(&ctx, &mut app, egui::Key::F6, egui::Modifiers::SHIFT);
        assert!(!app.settling);
        // The shortcut route did not turn the F10 frontier into an unbounded successor loop.
        assert!(pending > 0);
        app.apply_simulation_action(SimAction::Stop);
    }

    #[test]
    fn invalid_circuit_shows_a_located_diagnostic_and_stays_editable() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let and = app
            .session
            .place(ComponentKind::And, Position { x: 200., y: 100. })
            .unwrap();
        app.session
            .connect([
                pin_ref(input, PinDirection::Output, 0),
                pin_ref(and, PinDirection::Input, 0),
            ])
            .unwrap();
        let before = app.session.document().clone();
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(!app.session.simulation_active());
        assert_eq!(*app.session.document(), before);
        let message = app.error.clone().unwrap();
        assert!(message.contains("input pin 2 of AND #1"), "{message}");
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "input pin 2 of AND #1"));
        assert!(has_text(&shown, "State: error"));
        // Still editable; the error state ends once the document changes.
        app.session
            .place(ComponentKind::Input, Position { x: 50., y: 50. })
            .unwrap();
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: idle"));
    }

    #[test]
    fn pointer_and_keyboard_edits_are_blocked_while_active() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let (input, output) = inverter_chain(&mut app, 1);
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        frame(&ctx, &mut app, vec![], true);
        assert!(app.session.simulation_active());
        let before = app.session.document().clone();
        let history_probe = before.wires.len();
        // Drag the NOT gate (canvas origin is at 208, 8).
        let start = Pos2::new(308. + 100., 108.);
        let end = start + Vec2::new(50., 50.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        assert!(app.drag.is_none());
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(*app.session.document(), before);
        // Keyboard edit paths: delete, undo, redo and wiring mode.
        for (key_, modifiers) in [
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Z, egui::Modifiers::COMMAND),
            (
                egui::Key::Z,
                egui::Modifiers {
                    shift: true,
                    ..egui::Modifiers::COMMAND
                },
            ),
            (egui::Key::Y, egui::Modifiers::COMMAND),
            (egui::Key::W, egui::Modifiers::NONE),
        ] {
            app.error = None;
            press(&ctx, &mut app, key_, modifiers);
            assert_eq!(*app.session.document(), before, "{key_:?}");
            assert!(!app.wiring);
            assert_eq!(
                app.error.as_deref(),
                Some("Stop the simulation before editing"),
                "{key_:?}"
            );
        }
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "Editing is locked"));
        // A click on an Input component toggles it instead of selecting for a drag.
        click_at(&ctx, &mut app, Pos2::new(308., 108.));
        frame(&ctx, &mut app, vec![], true);
        assert_eq!(output_level(&app, output), Some(Level::Low));
        assert_eq!(*app.session.document(), before);
        // Space on the selected Input toggles it back from the keyboard.
        app.selected = Some(input);
        press(&ctx, &mut app, egui::Key::Space, egui::Modifiers::NONE);
        frame(&ctx, &mut app, vec![], true);
        assert_eq!(output_level(&app, output), Some(Level::High));
        // The history survived untouched: after Stop, undo removes the last wire only.
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::SHIFT);
        assert!(!app.session.simulation_active());
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(app.session.document().wires.len(), history_probe - 1);
        assert_eq!(app.session.document().components.len(), 3);
    }

    #[test]
    fn starting_cancels_placement_wiring_and_drag_gestures() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        inverter_chain(&mut app, 1);
        let before = app.session.document().clone();
        // A pointer drag in progress when the simulation starts must not move anything later.
        let start = Pos2::new(408., 108.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        assert!(app.drag.is_some());
        app.placing = Some(ComponentKind::And);
        app.wiring = true;
        app.geometric_route_start = Some(crate::engine::RouteTarget::Pin(pin_ref(
            ComponentId(1),
            PinDirection::Output,
            0,
        )));
        app.focused_pin = Some(pin_ref(ComponentId(1), PinDirection::Output, 0));
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(app.session.simulation_active());
        assert!(app.drag.is_none());
        assert!(app.placing.is_none());
        assert!(!app.wiring);
        assert!(app.geometric_route_start.is_none());
        assert!(app.focused_pin.is_none());
        let end = start + Vec2::new(60., 0.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(*app.session.document(), before);
    }

    fn ui_file_app(dir: &std::path::Path) -> DigitalApp {
        DigitalApp::with_file_io(Some(Box::new(LocalFileIo::new(dir))), None)
    }

    struct PostPublishFailureIo(LocalFileIo);
    struct DelegatingStage(Box<dyn crate::file_session::StagedFile>);
    impl crate::file_session::StagedFile for DelegatingStage {
        fn path(&self) -> &std::path::Path {
            self.0.path()
        }
        fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.write_all(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
        fn sync_close(self: Box<Self>) -> std::io::Result<()> {
            self.0.sync_close()
        }
    }
    impl FileIo for PostPublishFailureIo {
        fn base_dir(&self) -> &std::path::Path {
            self.0.base_dir()
        }
        fn read_bounded_regular(
            &mut self,
            path: &std::path::Path,
            limit: usize,
        ) -> std::io::Result<Vec<u8>> {
            self.0.read_bounded_regular(path, limit)
        }
        fn validate_target(
            &mut self,
            target: &std::path::Path,
            intent: SaveAsIntent,
        ) -> std::io::Result<()> {
            self.0.validate_target(target, intent)
        }
        fn create_temp(
            &mut self,
            path: &std::path::Path,
        ) -> std::io::Result<Box<dyn crate::file_session::StagedFile>> {
            self.0.create_temp(path).map(|stage| {
                Box::new(DelegatingStage(stage)) as Box<dyn crate::file_session::StagedFile>
            })
        }
        fn publish(
            &mut self,
            temp: &std::path::Path,
            target: &std::path::Path,
            intent: SaveAsIntent,
        ) -> std::io::Result<()> {
            self.0.publish(temp, target, intent)
        }
        fn sync_parent(&mut self, parent: &std::path::Path) -> std::io::Result<()> {
            self.0.sync_parent(parent)
        }
        fn remove_temp(&mut self, _temp: &std::path::Path) -> std::io::Result<()> {
            Err(std::io::Error::other(
                "injected post-publish cleanup failure",
            ))
        }
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("digital-ui-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_menu_and_shortcut_share_pending_document_actions() {
        let ctx = egui::Context::default();
        let dir = test_dir("menu");
        let mut app = ui_file_app(&dir);
        let original = app
            .session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        press(&ctx, &mut app, egui::Key::N, egui::Modifiers::COMMAND);
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        assert_eq!(app.session.document().components[0].id, original);
        click_text(&ctx, &mut app, "Cancel");
        assert!(app.file_modal.is_none());
        assert!(app.session.dirty());
        // The visible File menu's New item goes through the same pending path.
        click_text(&ctx, &mut app, "New");
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        click_text(&ctx, &mut app, "Discard");
        assert!(app.session.document().components.is_empty());
        assert!(!app.session.dirty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_open_draws_geometric_segments_and_enables_wire_mode() {
        let ctx = egui::Context::default();
        let dir = test_dir("native-geometric-readonly");
        let geometric = digital_rust::formats::DigV2 {
            elements: vec![],
            wires: vec![digital_rust::formats::DigWire {
                p1: digital_rust::formats::DigPoint { x: 9, y: 8 },
                p2: digital_rust::formats::DigPoint { x: 2, y: 3 },
            }],
        };
        std::fs::write(
            dir.join("geometry.dig"),
            digital_rust::formats::serialize_v2(&geometric).unwrap(),
        )
        .unwrap();
        let mut app = ui_file_app(&dir);
        frame(&ctx, &mut app, vec![], true);
        app.begin_pending(PendingAction::Open(dir.join("geometry.dig")));
        assert!(matches!(
            app.session.document().wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        assert_eq!(app.session.document().wires.geometric().unwrap().len(), 1);
        assert_eq!(app.session.path(), Some(dir.join("geometry.dig").as_path()));
        assert!(!app.session.dirty());
        assert!(!app.wiring);
        if let Some(rect) = app.canvas_rect {
            click_at(&ctx, &mut app, rect.center());
        }
        let output = frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::W, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.wiring, "W must enable geometric wire creation");
        assert!(app.error.is_none());
        let transform = app.view.transform(app.canvas_rect.unwrap()).unwrap();
        let a = transform.to_screen([9.0, 8.0]);
        let b = transform.to_screen([2.0, 3.0]);
        assert!(
            has_line(&output, a, b, 2.0) || has_line(&output, b, a, 2.0),
            "the exact geometric segment must be drawn"
        );
        click_at(&ctx, &mut app, transform.to_screen([5.5, 5.5]));
        assert_eq!(app.selected_segment, Some(0));
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.session.document().wires.geometric().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn production_selection_shortcuts_nudge_groups_in_one_undo_step() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let a = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let b = app
            .session
            .place(ComponentKind::Output, Position { x: 200., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let point = transform.to_screen([100., 100.]);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(point),
                button(point, true),
                button(point, false),
            ],
            true,
        );
        press(&ctx, &mut app, egui::Key::A, egui::Modifiers::COMMAND);
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([a, b])
        );
        let before = app.session.document().clone();
        press(
            &ctx,
            &mut app,
            egui::Key::ArrowRight,
            egui::Modifiers::SHIFT,
        );
        assert_eq!(
            app.session.document().components[0].position.x,
            before.components[0].position.x + 20.
        );
        assert_eq!(
            app.session.document().components[1].position.x,
            before.components[1].position.x + 20.
        );
        let after_nudge = app.session.document().clone();
        let drag_start = app
            .view
            .transform(app.canvas_rect.unwrap())
            .unwrap()
            .to_screen([120., 100.]);
        let drag_end = drag_start + Vec2::new(40., 0.);
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(drag_start),
                button(drag_start, true),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(drag_end)],
            true,
        );
        assert_eq!(
            app.session.document(),
            &after_nudge,
            "drag preview must not mutate the session"
        );
        frame(&ctx, &mut app, vec![button(drag_end, false)], true);
        assert_eq!(
            app.session.document().components[0].position.x,
            after_nudge.components[0].position.x + 40.
        );
        assert_eq!(
            app.session.document().components[1].position.x,
            after_nudge.components[1].position.x + 40.
        );
        app.session.undo().unwrap();
        assert_eq!(
            app.session.document(),
            &after_nudge,
            "one release creates one grouped history snapshot"
        );
        app.session.undo().unwrap();
        assert_eq!(app.session.document(), &before);
    }

    #[test]
    fn production_marquee_modifiers_and_cancellation_preserve_selection_until_release() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let first = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let second = app
            .session
            .place(ComponentKind::Output, Position { x: 220., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let transform = app.view.transform(app.canvas_rect.unwrap()).unwrap();
        let style = app.style;
        let bounds = |kind, center| {
            SymbolLayout::centered_on(style, kind, center)
                .unwrap()
                .bounds()
        };
        let marquee_points = |kind, center| {
            let b = bounds(kind, center);
            (
                transform.to_screen([b.min[0] - 5., b.min[1] - 5.]),
                transform.to_screen([b.max[0] + 5., b.max[1] + 5.]),
            )
        };
        let (a0, a1) = marquee_points(ComponentKind::Input, [100., 100.]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(a0), button(a0, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(a1)], true);
        frame(&ctx, &mut app, vec![button(a1, false)], true);
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first])
        );

        let (b0, b1) = marquee_points(ComponentKind::Output, [220., 100.]);
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(b0),
                pointer_button(b0, egui::PointerButton::Primary, true, shift),
            ],
            true,
        );
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first]),
            "press must not clear the old selection"
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(b1)], true);
        frame(
            &ctx,
            &mut app,
            vec![pointer_button(
                b1,
                egui::PointerButton::Primary,
                false,
                shift,
            )],
            true,
        );
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first, second])
        );

        let (a0, a1) = marquee_points(ComponentKind::Input, [100., 100.]);
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(a0),
                pointer_button(a0, egui::PointerButton::Primary, true, ctrl),
            ],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(a1)], true);
        frame(
            &ctx,
            &mut app,
            vec![pointer_button(
                a1,
                egui::PointerButton::Primary,
                false,
                ctrl,
            )],
            true,
        );
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([second]),
            "Ctrl marquee toggles membership"
        );

        // Plain release replaces the old selection with the marquee result.
        let (a0, a1) = marquee_points(ComponentKind::Input, [100., 100.]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(a0), button(a0, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(a1)], true);
        frame(&ctx, &mut app, vec![button(a1, false)], true);
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first])
        );

        // Begin an unmodified replacement marquee, then cancel it. The prior selection remains.
        let empty = transform.to_screen([400., 300.]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(empty), button(empty, true)],
            true,
        );
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first])
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        frame(&ctx, &mut app, vec![button(empty, false)], true);
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first])
        );

        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(empty), button(empty, true)],
            true,
        );
        frame(&ctx, &mut app, vec![], false);
        frame(&ctx, &mut app, vec![button(empty, false)], true);
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([first]),
            "focus loss cancels without replacing selection"
        );
    }

    #[test]
    fn production_nudge_reconciles_wire_selection_after_collapsed_segment() {
        use digital_rust::geometric_wiring::{Point as P, Segment};
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .add_geometric_segments(&[
                (P::new(0, 0), P::new(20, 0)),
                (P::new(100, 0), P::new(120, 0)),
            ])
            .unwrap();
        app.session
            .place(ComponentKind::Input, Position { x: 200., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let point = app
            .view
            .transform(app.canvas_rect.unwrap())
            .unwrap()
            .to_screen([200., 100.]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(point), button(point, true)],
            true,
        );
        frame(&ctx, &mut app, vec![button(point, false)], true);
        app.selection
            .wires
            .insert(0, crate::selection::EndpointMask::A);
        app.selection
            .wires
            .insert(1, crate::selection::EndpointMask::BOTH);
        press(
            &ctx,
            &mut app,
            egui::Key::ArrowRight,
            egui::Modifiers::SHIFT,
        );
        assert_eq!(
            app.session.document().wires.geometric().unwrap(),
            &[Segment {
                id: 1,
                a: P::new(120, 0),
                b: P::new(140, 0)
            }]
        );
        assert_eq!(
            app.selection.wires.keys().copied().collect::<Vec<_>>(),
            vec![1]
        );
        ctx.memory_mut(|memory| memory.request_focus(app.canvas_id.unwrap()));
        press(
            &ctx,
            &mut app,
            egui::Key::ArrowRight,
            egui::Modifiers::SHIFT,
        );
        assert_eq!(
            app.session.document().wires.geometric().unwrap()[0],
            Segment {
                id: 1,
                a: P::new(140, 0),
                b: P::new(160, 0)
            }
        );
        ctx.memory_mut(|memory| memory.request_focus(app.canvas_id.unwrap()));
        press(&ctx, &mut app, egui::Key::Delete, egui::Modifiers::NONE);
        assert!(app.session.document().wires.geometric().unwrap().is_empty());
    }

    #[test]
    fn production_marquee_selects_wholly_contained_symbol_after_release() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let inside = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let outside = app
            .session
            .place(ComponentKind::Output, Position { x: 220., y: 100. })
            .unwrap();
        frame(&ctx, &mut app, vec![], true);
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let bounds = SymbolLayout::centered_on(app.style, ComponentKind::Input, [100., 100.])
            .unwrap()
            .bounds();
        let start = transform.to_screen([bounds.min[0] - 5., bounds.min[1] - 5.]);
        let end = transform.to_screen([bounds.max[0] + 5., bounds.max[1] + 5.]);
        let before = app.session.document().clone();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            true,
        );
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
        assert_eq!(app.session.document(), &before);
        frame(&ctx, &mut app, vec![button(end, false)], true);
        assert_eq!(
            app.selection.components,
            std::collections::BTreeSet::from([inside])
        );
        assert!(!app.selection.components.contains(&outside));
    }

    #[test]
    fn geometric_wire_mode_routes_between_real_canvas_pins_through_the_session() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let dig = digital_rust::formats::DigV2 {
            elements: vec![
                digital_rust::formats::DigElement {
                    name: "In".into(),
                    x: 0,
                    y: 0,
                    label: None,
                    value: None,
                    rotation: 0,
                    mirror: false,
                },
                digital_rust::formats::DigElement {
                    name: "Out".into(),
                    x: 100,
                    y: 80,
                    label: None,
                    value: None,
                    rotation: 0,
                    mirror: false,
                },
            ],
            wires: vec![],
        };
        app.session
            .open_dig_bytes(&digital_rust::formats::serialize_v2(&dig).unwrap())
            .unwrap();
        let input = app.session.document().components[0].id;
        let output = app.session.document().components[1].id;
        frame(&ctx, &mut app, vec![], true);
        app.wiring = true;
        let rect = app.canvas_rect.unwrap();
        let transform = app.view.transform(rect).unwrap();
        let input_pin = PinRef {
            component: input,
            pin: PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        };
        let output_pin = PinRef {
            component: output,
            pin: PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        };
        let pin_screen = |component: ComponentId, kind: ComponentKind, pin: PinId| {
            let doc_component = app
                .session
                .document()
                .components
                .iter()
                .find(|c| c.id == component)
                .unwrap();
            let position = SymbolLayout::centered_on(
                app.style,
                kind,
                [doc_component.position.x, doc_component.position.y],
            )
            .unwrap()
            .pin_position(pin)
            .unwrap();
            transform.to_screen(position)
        };
        let input_screen = pin_screen(input, ComponentKind::Input, input_pin.pin);
        let output_screen = pin_screen(output, ComponentKind::Output, output_pin.pin);
        click_at(&ctx, &mut app, input_screen);
        click_at(&ctx, &mut app, output_screen);
        let segments = app.session.document().wires.geometric().unwrap();
        assert_eq!(
            segments.len(),
            2,
            "diagonal pin displacement makes a two-leg L"
        );
        assert!(segments.iter().all(|s| s.a.x == s.b.x || s.a.y == s.b.y));
        assert!(app.session.dirty());
    }

    #[test]
    fn opening_replacement_clears_selected_segment_before_delete_shortcut() {
        use digital_rust::geometric_wiring::Point;
        let ctx = egui::Context::default();
        let dir = test_dir("open-clears-wire-selection");
        let mut app = ui_file_app(&dir);
        app.session
            .add_geometric_segments(&[(Point::new(100, 100), Point::new(300, 100))])
            .unwrap();
        let rect = camera_rect(&ctx, &mut app);
        let transform = app.view.transform(rect).unwrap();
        click_at(&ctx, &mut app, transform.to_screen([200., 100.]));
        assert_eq!(app.selected_segment, Some(0));

        let mut destination = Session::default();
        destination
            .add_geometric_segments(&[(Point::new(500, 500), Point::new(700, 500))])
            .unwrap();
        destination
            .save_as(
                app.file_io.as_mut().unwrap().as_mut(),
                std::path::Path::new("replacement.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();

        app.open_path_dialog();
        type_file_path(&ctx, &mut app, "replacement.dig");
        click_text(&ctx, &mut app, "Open path");
        if matches!(app.file_modal, Some(FileModal::Unsaved)) {
            click_text(&ctx, &mut app, "Discard");
        }
        assert_eq!(app.session.document().wires.geometric().unwrap().len(), 1);
        assert_eq!(app.session.document().wires.geometric().unwrap()[0].id, 0);
        press(&ctx, &mut app, egui::Key::Delete, egui::Modifiers::NONE);
        assert_eq!(app.session.document().wires.geometric().unwrap().len(), 1);
        assert_eq!(app.selected_segment, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dirty_open_save_discard_and_cancel_all_preserve_or_replace_the_document_as_chosen() {
        let ctx = egui::Context::default();
        let dir = test_dir("dirty-open-choices");
        let mut app = ui_file_app(&dir);
        frame(&ctx, &mut app, vec![], true);
        let source = app
            .session
            .place(ComponentKind::Input, Position { x: 10., y: 10. })
            .unwrap();
        app.save_as_path("source.dig".into(), false, false);

        let mut destination = Session::default();
        destination
            .place(ComponentKind::Output, Position { x: 90., y: 90. })
            .unwrap();
        destination
            .save_as(
                app.file_io.as_mut().unwrap().as_mut(),
                std::path::Path::new("destination.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();
        let old = app.session.document().clone();
        app.session
            .move_component(source, Position { x: 30., y: 30. })
            .unwrap();
        let changed = app.session.document().clone();

        app.begin_pending(PendingAction::Open(dir.join("destination.dig")));
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        click_text(&ctx, &mut app, "Cancel");
        assert_eq!(*app.session.document(), changed);

        app.begin_pending(PendingAction::Open(dir.join("destination.dig")));
        click_text(&ctx, &mut app, "Discard");
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Output
        );
        assert!(!app.session.dirty());

        let destination_id = app.session.document().components[0].id;
        app.session
            .move_component(destination_id, Position { x: 120., y: 120. })
            .unwrap();
        app.begin_pending(PendingAction::Open(dir.join("source.dig")));
        click_text(&ctx, &mut app, "Save");
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Input
        );
        assert_eq!(app.session.path(), Some(dir.join("source.dig").as_path()));
        assert!(!app.session.dirty());
        assert_eq!(app.session.document().components, old.components);
        assert!(matches!(
            app.session.document().wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn path_cancel_failed_open_and_create_only_collision_never_discard_the_document() {
        let ctx = egui::Context::default();
        let dir = test_dir("file-failures");
        let mut app = ui_file_app(&dir);
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 4., y: 5. })
            .unwrap();
        app.file_path_entry = "unused.dig".into();
        app.file_modal = Some(FileModal::SavePath { continuation: true });
        click_text(&ctx, &mut app, "Cancel path");
        assert!(app.session.dirty() && app.session.path().is_none());
        assert!(!dir.join("unused.dig").exists());

        app.save_as_path("old.dig".into(), false, false);
        let before = app.session.document().clone();
        app.begin_pending(PendingAction::Open(dir.join("missing.dig")));
        assert_eq!(*app.session.document(), before);
        assert!(
            app.error
                .as_deref()
                .is_some_and(|e| e.contains("missing.dig"))
        );

        let collision = dir.join("exists.dig");
        std::fs::write(&collision, b"foreign bytes").unwrap();
        app.file_modal = Some(FileModal::SavePath {
            continuation: false,
        });
        app.file_path_entry = "exists.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert!(matches!(app.file_modal, Some(FileModal::Overwrite { .. })));
        click_text(&ctx, &mut app, "Cancel");
        assert_eq!(std::fs::read(&collision).unwrap(), b"foreign bytes");
        assert_eq!(*app.session.document(), before);
        assert_eq!(app.session.document().components[0].id, id);

        app.file_modal = Some(FileModal::SavePath {
            continuation: false,
        });
        app.file_path_entry = "exists.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert!(matches!(app.file_modal, Some(FileModal::Overwrite { .. })));
        click_text(&ctx, &mut app, "Replace");
        assert!(app.session.path().is_some());
        assert!(!app.session.dirty());
        assert_ne!(std::fs::read(&collision).unwrap(), b"foreign bytes");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_continuation_clears_pending_action_only_after_publish_and_failed_save_blocks_it() {
        let ctx = egui::Context::default();
        let dir = test_dir("save-continuation");
        let mut app = ui_file_app(&dir);
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let original = app.session.document().clone();
        app.begin_pending(PendingAction::New);
        app.begin_save(true);
        assert!(matches!(
            app.file_modal,
            Some(FileModal::SavePath { continuation: true })
        ));
        app.file_path_entry = "kept.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert!(dir.join("kept.dig").exists());
        assert!(app.session.document().components.is_empty());
        assert!(!app.session.dirty());

        app.session
            .place(ComponentKind::Output, Position { x: 1., y: 2. })
            .unwrap();
        let before_failure = app.session.document().clone();
        app.begin_pending(PendingAction::New);
        app.begin_save(true);
        app.save_as_path("missing-dir/fail.dig".into(), true, false);
        assert!(app.error.is_some());
        assert_eq!(*app.session.document(), before_failure);
        assert_eq!(app.pending_action, Some(PendingAction::New));
        assert!(app.session.dirty());
        assert_ne!(before_failure, original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dirty_close_save_continuation_approves_exit_only_after_publication() {
        let dir = test_dir("close-save");
        let mut app = ui_file_app(&dir);
        let id = app
            .session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        app.save_as_path("close.dig".into(), false, false);
        app.session
            .move_component(id, Position { x: 20., y: 30. })
            .unwrap();
        app.begin_pending(PendingAction::Close);
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        app.begin_save(true);
        assert!(app.approved_close);
        assert!(!app.session.dirty());
        assert!(dir.join("close.dig").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_modal_blocks_edit_simulation_and_close_cancel_preserves_runtime() {
        let ctx = egui::Context::default();
        let dir = test_dir("modal-lock");
        let mut app = ui_file_app(&dir);
        let (input, _) = inverter_chain(&mut app, 1);
        let before = app.session.document().clone();
        app.selected = Some(input);
        app.selected_segment = Some(app.session.document().wires.geometric().unwrap()[0].id);
        app.placing = Some(ComponentKind::And);
        app.wiring = true;
        app.geometric_route_start = Some(crate::engine::RouteTarget::Pin(pin_ref(
            input,
            PinDirection::Output,
            0,
        )));
        app.focused_pin = Some(pin_ref(input, PinDirection::Output, 0));
        app.drag = Some((input, Position { x: 0., y: 0. }, Pos2::ZERO));
        app.begin_pending(PendingAction::New);
        assert!(app.drag.is_none() && app.placing.is_none() && !app.wiring);
        assert!(app.geometric_route_start.is_none() && app.focused_pin.is_none());
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        for (key, modifiers) in [
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Z, egui::Modifiers::COMMAND),
            (egui::Key::W, egui::Modifiers::NONE),
            (egui::Key::F5, egui::Modifiers::NONE),
        ] {
            press(&ctx, &mut app, key, modifiers);
        }
        assert_eq!(*app.session.document(), before);
        assert!(!app.session.simulation_active());
        assert!(app.geometric_route_start.is_none());

        click_text(&ctx, &mut app, "Cancel");
        app.session.start_simulation().unwrap();
        let runtime_input =
            app.session
                .inspect()
                .unwrap()
                .pin_value(pin_ref(input, PinDirection::Output, 0));
        app.begin_pending(PendingAction::Close);
        assert!(matches!(app.file_modal, Some(FileModal::RunningClose)));
        click_text(&ctx, &mut app, "Cancel");
        assert!(app.session.simulation_active());
        assert_eq!(
            app.session
                .inspect()
                .unwrap()
                .pin_value(pin_ref(input, PinDirection::Output, 0)),
            runtime_input
        );
        assert!(!app.approved_close);
        app.begin_pending(PendingAction::Close);
        click_text(&ctx, &mut app, "Stop and continue closing");
        assert!(!app.session.simulation_active());
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        click_text(&ctx, &mut app, "Discard");
        assert!(app.approved_close);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn synthetic_close_is_cancelled_until_dirty_workflow_approves() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused: true,
            viewports: [(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        assert!(!app.approved_close);
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        click_text(&ctx, &mut app, "Cancel");
        assert!(!app.approved_close);
        assert!(app.session.dirty());

        app.begin_pending(PendingAction::Close);
        click_text(&ctx, &mut app, "Discard");
        assert!(app.approved_close);
        assert_eq!(app.session.document().components.len(), 1);
    }

    #[test]
    fn published_warning_close_keeps_original_open_continuation_until_acknowledged() {
        let ctx = egui::Context::default();
        let dir = test_dir("published-warning-close");
        let destination = dir.join("destination.dig");
        let mut destination_session = Session::default();
        destination_session
            .place(ComponentKind::Output, Position { x: 40., y: 50. })
            .unwrap();
        destination_session
            .save_as(
                &mut LocalFileIo::new(&dir),
                &destination,
                SaveAsIntent::CreateOnly,
            )
            .unwrap();

        let mut app = DigitalApp::with_file_io(
            Some(Box::new(PostPublishFailureIo(LocalFileIo::new(&dir)))),
            None,
        );
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        app.begin_pending(PendingAction::Open(destination.clone()));
        assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
        app.begin_save(true);
        app.file_path_entry = "current.dig".into();
        click_text(&ctx, &mut app, "Save path");

        assert!(destination.exists());
        assert!(dir.join("current.dig").exists());
        assert!(
            !app.session.dirty(),
            "the published save commits the baseline"
        );
        assert_eq!(
            app.pending_action,
            Some(PendingAction::Open(destination.clone()))
        );
        assert!(matches!(
            app.file_modal,
            Some(FileModal::PublishedWarning {
                continuation: true,
                ..
            })
        ));
        let warning_frame = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(
            &warning_frame,
            "Published; durability unconfirmed"
        ));
        let close_input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused: true,
            viewports: [(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let output = ctx.run(close_input, |ctx| app.show(ctx));
        assert!(!app.approved_close);
        assert_eq!(
            app.pending_action,
            Some(PendingAction::Open(destination.clone()))
        );
        assert!(matches!(
            app.file_modal,
            Some(FileModal::PublishedWarning {
                continuation: true,
                ..
            })
        ));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Input
        );

        click_text(&ctx, &mut app, "Acknowledge and continue");
        assert_eq!(
            app.session.document().components[0].kind,
            ComponentKind::Output
        );
        assert_eq!(app.session.path(), Some(destination.as_path()));
        assert!(app.pending_action.is_none());
        assert!(app.file_modal.is_none());
        assert!(
            !app.approved_close,
            "the original Open, not Close, continues"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn close_without_pending_warning_continuation_only_dismisses_after_ack() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp {
            file_modal: Some(FileModal::PublishedWarning {
                diagnostics: vec!["post-publish warning".into()],
                continuation: false,
            }),
            pending_action: None,
            ..Default::default()
        };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
            focused: true,
            viewports: [(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        assert!(!app.approved_close);
        assert!(app.pending_action.is_none());
        assert!(matches!(
            app.file_modal,
            Some(FileModal::PublishedWarning {
                continuation: false,
                ..
            })
        ));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        click_text(&ctx, &mut app, "Acknowledge and continue");
        assert!(app.file_modal.is_none());
        assert!(!app.approved_close);
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn published_warning_displays_every_diagnostic_and_blocks_continuation_until_ack() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        app.session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        app.pending_action = Some(PendingAction::New);
        app.file_modal = Some(FileModal::PublishedWarning {
            diagnostics: vec![
                "first post-publish warning".into(),
                "second post-publish warning".into(),
            ],
            continuation: true,
        });
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(
            matches!(app.file_modal, Some(FileModal::PublishedWarning { .. })),
            "{:?}",
            app.file_modal
        );
        assert!(
            has_text(&shown, "first post-publish warning"),
            "{:?}",
            texts(&shown)
        );
        assert!(has_text(&shown, "second post-publish warning"));
        assert_eq!(app.session.document().components.len(), 1);
        click_text(&ctx, &mut app, "Acknowledge and continue");
        assert!(app.file_modal.is_none(), "{:?}", app.file_modal);
        assert!(app.session.document().components.is_empty());
        assert!(!app.session.dirty());
    }

    #[test]
    fn label_editor_modal_applies_cancels_validates_and_blocks_every_background_route() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 100. })
            .unwrap();
        app.selected = Some(id);
        let original = app.session.document().clone();
        click_text(&ctx, &mut app, "Edit label… (F2)");
        assert!(app.label_modal.is_some());
        assert_eq!(app.label_modal.as_ref().unwrap().text, "Output");
        app.label_modal.as_mut().unwrap().text = "invalid\u{1}".into();
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "U+0001"), "{:?}", texts(&shown));
        assert!(has_text(&shown, "Apply") && has_text(&shown, "Cancel"));
        ctx.set_visuals(egui::Visuals::light());
        assert!(has_text(&frame(&ctx, &mut app, vec![], true), "U+0001"));
        ctx.set_visuals(egui::Visuals::dark());
        assert!(has_text(&frame(&ctx, &mut app, vec![], true), "U+0001"));
        let before = app.session.document().clone();
        for (pressed, modifiers) in [
            (egui::Key::F5, egui::Modifiers::NONE),
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Z, egui::Modifiers::COMMAND),
            (egui::Key::N, egui::Modifiers::COMMAND),
            (egui::Key::O, egui::Modifiers::COMMAND),
            (egui::Key::S, egui::Modifiers::COMMAND),
        ] {
            press(&ctx, &mut app, pressed, modifiers);
        }
        assert_eq!(*app.session.document(), before);
        assert!(!app.session.simulation_active());
        assert!(app.label_modal.is_some());
        assert!(app.file_modal.is_none());
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.label_modal.is_none());
        assert_eq!(*app.session.document(), original);
        press(&ctx, &mut app, egui::Key::F2, egui::Modifiers::NONE);
        assert!(
            app.label_modal.is_some(),
            "F2 should open the selected component label editor"
        );
        press(&ctx, &mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(app.label_modal.is_none());

        app.selected = Some(id);
        click_text(&ctx, &mut app, "Edit label… (F2)");
        app.label_modal.as_mut().unwrap().text = "Résultat & <Q>".into();
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::COMMAND)],
            true,
        );
        assert!(app.label_modal.is_none());
        assert_eq!(app.session.document().components[0].label, "Résultat & <Q>");
        assert!(app.session.dirty());
        app.session.undo().unwrap();
        assert_eq!(*app.session.document(), original);
        assert!(app.session.dirty());
        app.session.redo().unwrap();
        assert!(app.session.dirty());
    }

    #[test]
    fn label_modal_blocks_canvas_pointer_gestures_everywhere() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let selected = app
            .session
            .place(ComponentKind::Input, Position { x: 100., y: 100. })
            .unwrap();
        let underlying = app
            .session
            .place(ComponentKind::Output, Position { x: 192., y: 292. })
            .unwrap();
        app.selected = Some(selected);
        app.begin_label_edit();
        assert!(app.label_modal.is_some());
        let before = app.session.document().clone();

        // The first click is on empty canvas behind the dialog; the second is on a
        // component whose symbol is underneath the dialog itself.
        for (start, end) in [
            (Pos2::new(700., 500.), Pos2::new(740., 530.)),
            (Pos2::new(400., 300.), Pos2::new(470., 350.)),
        ] {
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(start), button(start, true)],
                true,
            );
            frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)], true);
            frame(&ctx, &mut app, vec![button(end, false)], true);
            assert_eq!(app.selected, Some(selected));
            assert!(app.drag.is_none());
            assert!(app.placing.is_none());
            assert!(!app.wiring);
            assert!(app.geometric_route_start.is_none());
            assert_eq!(*app.session.document(), before);
        }
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == underlying)
                .unwrap()
                .position,
            Position { x: 192., y: 292. }
        );
    }

    #[test]
    fn label_modal_focus_loss_cancels_and_running_simulation_blocks_open() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let input = app
            .session
            .place(ComponentKind::Input, Position { x: 0., y: 0. })
            .unwrap();
        let id = app
            .session
            .place(ComponentKind::Output, Position { x: 100., y: 0. })
            .unwrap();
        app.session
            .connect([
                pin_ref(input, PinDirection::Output, 0),
                pin_ref(id, PinDirection::Input, 0),
            ])
            .unwrap();
        app.selected = Some(id);
        app.begin_label_edit();
        app.label_modal.as_mut().unwrap().text = "draft".into();
        frame(&ctx, &mut app, vec![], false);
        assert!(app.label_modal.is_none());
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap()
                .label,
            "Output"
        );
        app.session.start_simulation().unwrap();
        app.selected = Some(id);
        app.begin_label_edit();
        assert!(app.label_modal.is_none());
        assert!(app.session.simulation_active());
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap()
                .label,
            "Output"
        );
        app.session.stop_simulation().unwrap();
    }

    #[test]
    fn label_editing_full_editor_workflow_through_production_ui_events() {
        let ctx = egui::Context::default();
        let dir = test_dir("m4-label-ui");
        let mut app = ui_file_app(&dir);
        // Construct a small inverter circuit solely with the visible palette and canvas controls.
        for (palette, canvas) in [
            ("Input", Pos2::new(308., 108.)),
            ("NOT", Pos2::new(408., 108.)),
            ("Output", Pos2::new(508., 108.)),
        ] {
            click_text(&ctx, &mut app, palette);
            click_at(&ctx, &mut app, canvas);
        }
        assert_eq!(app.session.document().components.len(), 3);
        click_text(&ctx, &mut app, "Wire (W)");
        let screen_pin = |kind, center, pin| {
            let point = SymbolLayout::centered_on(app.style, kind, center)
                .unwrap()
                .pin_position(pin)
                .unwrap();
            Pos2::new(point[0] + 208., point[1] + 8.)
        };
        let routes = [
            (
                screen_pin(
                    ComponentKind::Input,
                    [100., 100.],
                    PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                ),
                screen_pin(
                    ComponentKind::Not,
                    [200., 100.],
                    PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                ),
            ),
            (
                screen_pin(
                    ComponentKind::Not,
                    [200., 100.],
                    PinId {
                        direction: PinDirection::Output,
                        index: 0,
                    },
                ),
                screen_pin(
                    ComponentKind::Output,
                    [300., 100.],
                    PinId {
                        direction: PinDirection::Input,
                        index: 0,
                    },
                ),
            ),
        ];
        for (start, end) in routes {
            click_at(&ctx, &mut app, start);
            click_at(&ctx, &mut app, end);
        }
        click_text(&ctx, &mut app, "Wire (W)");
        assert_eq!(app.session.document().wires.len(), 2);
        let output = app
            .session
            .document()
            .components
            .iter()
            .find(|component| component.kind == ComponentKind::Output)
            .unwrap()
            .id;
        click_at(&ctx, &mut app, Pos2::new(508., 108.));
        assert_eq!(app.selected, Some(output));
        click_text(&ctx, &mut app, "Edit label… (F2)");
        assert!(app.label_modal.is_some());
        let modal = frame(&ctx, &mut app, vec![], true);
        let text_pos = modal
            .shapes
            .iter()
            .rev()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Output" => Some(text.pos),
                _ => None,
            })
            .unwrap();
        click_at(&ctx, &mut app, text_pos + Vec2::new(8., 8.));
        let document_before_text_undo = app.session.document().clone();
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(
            *app.session.document(),
            document_before_text_undo,
            "text-editor undo must not reach document history"
        );
        frame(
            &ctx,
            &mut app,
            vec![
                key(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("Résultat & <Q>".into()),
            ],
            true,
        );
        frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::Enter, egui::Modifiers::COMMAND)],
            true,
        );
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Résultat & <Q>"
        );
        assert!(has_text(
            &frame(&ctx, &mut app, vec![], true),
            "Résultat & <Q>"
        ));
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Output"
        );
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Résultat & <Q>"
        );
        click_text(&ctx, &mut app, "Save As");
        app.file_path_entry = "ui-demo.dig".into();
        click_text(&ctx, &mut app, "Save path");
        assert_eq!(app.session.path(), Some(dir.join("ui-demo.dig").as_path()));
        assert!(!app.session.dirty());
        let xml = std::fs::read_to_string(dir.join("ui-demo.dig")).unwrap();
        assert!(xml.contains("Résultat &amp; &lt;Q&gt;"));
        click_text(&ctx, &mut app, "New");
        assert!(app.session.document().components.is_empty());
        click_text(&ctx, &mut app, "Open");
        app.file_path_entry = "ui-demo.dig".into();
        click_text(&ctx, &mut app, "Open path");
        assert_eq!(app.session.document().components.len(), 3);
        assert_eq!(app.session.document().wires.len(), 2);
        assert_eq!(
            app.session
                .document()
                .components
                .iter()
                .find(|component| component.id == output)
                .unwrap()
                .label,
            "Résultat & <Q>"
        );
        click_text(&ctx, &mut app, "Start");
        let mut frames = 0;
        while app
            .session
            .inspect()
            .is_some_and(|runtime| runtime.is_pending())
        {
            let output_frame = frame(&ctx, &mut app, vec![], true);
            frames += 1;
            assert!(frames < 100, "settling stalled: {:?}", app.error);
            if app.session.inspect().unwrap().is_pending() {
                assert!(wants_immediate_repaint(&output_frame));
            }
        }
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::High)
        );
        assert!(has_text(
            &frame(&ctx, &mut app, vec![], true),
            "Résultat & <Q>"
        ));
        click_text(&ctx, &mut app, "Input #0: 0 (low)");
        let settled = frame(&ctx, &mut app, vec![], true);
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::Low)
        );
        assert!(has_text(&settled, "Résultat & <Q>"));
        let document = app.session.document().clone();
        let output_pos = component_screen(&app, output);
        click_at(&ctx, &mut app, output_pos);
        assert_eq!(app.selected, Some(output));
        click_text(&ctx, &mut app, "Edit label… (F2)");
        assert!(
            app.label_modal.is_none(),
            "label editing must be disabled during simulation"
        );
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::NONE);
        assert!(app.session.simulation_active());
        assert_eq!(*app.session.document(), document);
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::SHIFT);
        click_text(&ctx, &mut app, "Edit label… (F2)");
        let modal = frame(&ctx, &mut app, vec![], true);
        let text_pos = modal
            .shapes
            .iter()
            .rev()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Résultat & <Q>" => {
                    Some(text.pos)
                }
                _ => None,
            })
            .unwrap();
        click_at(&ctx, &mut app, text_pos + Vec2::new(8., 8.));
        frame(
            &ctx,
            &mut app,
            vec![
                key(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("Final".into()),
            ],
            true,
        );
        press(&ctx, &mut app, egui::Key::Enter, egui::Modifiers::COMMAND);
        assert!(app.session.dirty());
        press(&ctx, &mut app, egui::Key::Z, egui::Modifiers::COMMAND);
        assert!(!app.session.dirty());
        press(
            &ctx,
            &mut app,
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        );
        assert!(app.session.dirty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn supported_document_file_lifecycle_save_new_reopen_run_toggle_inspect_stop() {
        let dir = test_dir("m4-lifecycle");
        let mut app = ui_file_app(&dir);
        let (input, output) = inverter_chain(&mut app, 1);
        app.save_as_path("fixture.dig".into(), false, false);
        assert!(dir.join("fixture.dig").exists());
        assert!(!app.session.dirty());
        app.begin_pending(PendingAction::New);
        assert!(app.session.document().components.is_empty());
        app.begin_pending(PendingAction::Open(dir.join("fixture.dig")));
        assert_eq!(app.session.document().components.len(), 3);
        app.session.start_simulation().unwrap();
        app.session.settle_simulation(100).unwrap();
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::High)
        );
        app.session.toggle_input(input).unwrap();
        app.session.settle_simulation(100).unwrap();
        assert_eq!(
            app.session.inspect().unwrap().output_value(output),
            Some(Level::Low)
        );
        app.session.stop_simulation().unwrap();
        assert!(!app.session.simulation_active());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_chunks_span_frames_keep_stop_responsive_and_do_not_change_the_result() {
        let ctx = egui::Context::default();
        let chain = 6;
        let mut reference = DigitalApp::default();
        let (_, reference_output) = inverter_chain(&mut reference, chain);
        press(&ctx, &mut reference, egui::Key::F5, egui::Modifiers::NONE);
        frame(&ctx, &mut reference, vec![], true);
        let expected = output_level(&reference, reference_output);
        assert_eq!(expected, Some(Level::Low));

        let mut app = DigitalApp::default();
        let (_, output) = inverter_chain(&mut app, chain);
        app.settle_budget = 2;
        let started = frame(
            &ctx,
            &mut app,
            vec![key(egui::Key::F5, egui::Modifiers::NONE)],
            true,
        );
        assert!(app.session.inspect().unwrap().is_pending());
        assert!(wants_immediate_repaint(&started));
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "initializing"));
        assert!(has_text(&shown, "= 0"));
        assert!(has_text(&shown, "0 (low)"));
        let mut frames = 1;
        while app.session.inspect().unwrap().is_pending() {
            let shown = frame(&ctx, &mut app, vec![], true);
            frames += 1;
            assert!(frames < 200, "settling did not finish");
            if app.session.inspect().unwrap().is_pending() {
                assert!(wants_immediate_repaint(&shown));
            }
        }
        assert!(frames > 3, "the small budget must need several frames");
        assert_eq!(output_level(&app, output), expected);
        let shown = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&shown, "State: settled"));
        // Stop while chunks are still pending: the click is handled between chunks.
        press(&ctx, &mut app, egui::Key::F5, egui::Modifiers::SHIFT);
        let before = app.session.document().clone();
        app.settle_budget = 1;
        click_text(&ctx, &mut app, "Start");
        assert!(app.session.inspect().unwrap().is_pending());
        click_text(&ctx, &mut app, "Stop");
        assert!(!app.session.simulation_active());
        assert!(!app.settling);
        assert_eq!(*app.session.document(), before);
        frame(&ctx, &mut app, vec![], true);
        let idle = frame(&ctx, &mut app, vec![], true);
        assert!(has_text(&idle, "State: idle"));
        assert!(
            !wants_immediate_repaint(&idle),
            "an idle editor must not spin repaints"
        );
    }

    #[test]
    fn canvas_r_rotates_selected_component_through_the_session_command() {
        let ctx = egui::Context::default();
        let mut app = DigitalApp::default();
        let id = app
            .session
            .place(ComponentKind::And, Position { x: 100., y: 100. })
            .unwrap();
        app.selected = Some(id);
        frame(&ctx, &mut app, vec![], true);
        click_at(&ctx, &mut app, Pos2::new(500., 400.));
        app.selected = Some(id);
        press(&ctx, &mut app, egui::Key::R, egui::Modifiers::NONE);
        assert_eq!(app.session.document().components[0].rotation, 1);
        assert_eq!(app.session.undo(), Ok(()));
        assert_eq!(app.session.document().components[0].rotation, 0);
    }

    #[test]
    fn cancelled_canvas_gesture_cannot_rotate_selection_in_the_same_frame() {
        for cancel in [
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerGone,
        ] {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            let id = app
                .session
                .place(ComponentKind::And, Position { x: 100., y: 100. })
                .unwrap();
            app.selected = Some(id);
            app.selection.select_component(id, false, false);
            frame(&ctx, &mut app, vec![], true);
            click_at(&ctx, &mut app, Pos2::new(500., 400.));
            app.selected = Some(id);
            app.selection.select_component(id, false, false);
            let document = app.session.document().clone();
            let dirty = app.session.dirty();
            frame(
                &ctx,
                &mut app,
                vec![cancel, key(egui::Key::R, egui::Modifiers::NONE)],
                true,
            );
            assert_eq!(app.session.document(), &document);
            assert_eq!(app.session.dirty(), dirty);
            app.session.undo().unwrap();
            assert!(
                app.session.document().components.is_empty(),
                "cancel must not add a history entry"
            );
            app.session.redo().unwrap();
            assert_eq!(app.session.document(), &document);
            assert!(app.drag.is_none());
            assert!(app.marquee.is_none());
        }
    }

    #[test]
    fn paste_draft_r_rotates_stationary_preview_geometry_and_commit_matches() {
        let fragment = digital_rust::formats::DigV2 {
            elements: vec![digital_rust::formats::DigElement {
                name: "Not".into(),
                x: 100,
                y: 100,
                label: Some("preview".into()),
                value: None,
                rotation: 0,
                mirror: false,
            }],
            wires: vec![digital_rust::formats::DigWire {
                p1: digital_rust::formats::DigPoint { x: 80, y: 100 },
                p2: digital_rust::formats::DigPoint { x: 120, y: 100 },
            }],
        };
        for commit_by_click in [false, true] {
            let ctx = egui::Context::default();
            let mut app = DigitalApp::default();
            app.begin_fragment_draft(fragment.clone(), [0, 0], true)
                .unwrap();
            frame(&ctx, &mut app, vec![], true);
            let pointer = app.canvas_rect.unwrap().center();
            frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(pointer)],
                true,
            );
            let anchor = app.fragment_draft.as_ref().unwrap().anchor;
            let delta = app.fragment_draft.as_ref().unwrap().delta;
            assert_ne!(
                delta,
                [0, 0],
                "preview must use the stationary canvas pointer"
            );
            frame(
                &ctx,
                &mut app,
                vec![key(egui::Key::R, egui::Modifiers::NONE)],
                true,
            );
            assert_eq!(app.fragment_draft.as_ref().unwrap().anchor, anchor);
            assert_eq!(app.fragment_draft.as_ref().unwrap().delta, delta);
            assert_eq!(
                app.fragment_draft.as_ref().unwrap().fragment.elements[0].rotation,
                1
            );
            let preview = {
                let draft = app.fragment_draft.as_ref().unwrap();
                fragment_preview_document(draft).unwrap()
            };
            assert_ne!(preview.components, app.session.document().components);
            let preview_component = &preview.components[0];
            let preview_layout = SymbolLayout::oriented_on(
                app.style,
                preview_component.kind,
                [preview_component.position.x, preview_component.position.y],
                crate::domain::Orientation {
                    rotation: preview_component.rotation,
                    mirror: preview_component.mirror,
                },
            )
            .unwrap();
            let preview_bounds = preview_layout.bounds();
            let preview_pins: Vec<_> =
                digital_rust::pin_geometry::all_pin_points(preview_component).unwrap();
            let preview_wires = preview.wires.geometric().unwrap().to_vec();
            if commit_by_click {
                click_at(&ctx, &mut app, pointer);
            } else {
                frame(
                    &ctx,
                    &mut app,
                    vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
                    true,
                );
            }
            let committed_component = &app.session.document().components[0];
            let committed_layout = SymbolLayout::oriented_on(
                app.style,
                committed_component.kind,
                [
                    committed_component.position.x,
                    committed_component.position.y,
                ],
                crate::domain::Orientation {
                    rotation: committed_component.rotation,
                    mirror: committed_component.mirror,
                },
            )
            .unwrap();
            assert_eq!(committed_component.position, preview_component.position);
            assert_eq!(
                (committed_component.rotation, committed_component.mirror),
                (preview_component.rotation, preview_component.mirror)
            );
            assert_eq!(committed_layout.bounds(), preview_bounds);
            assert_eq!(
                digital_rust::pin_geometry::all_pin_points(committed_component).unwrap(),
                preview_pins
            );
            assert_eq!(
                app.session
                    .document()
                    .wires
                    .geometric()
                    .unwrap()
                    .iter()
                    .map(|wire| (wire.a, wire.b))
                    .collect::<Vec<_>>(),
                preview_wires
                    .iter()
                    .map(|wire| (wire.a, wire.b))
                    .collect::<Vec<_>>(),
            );
            assert!(app.fragment_draft.is_none());
        }
    }
}
