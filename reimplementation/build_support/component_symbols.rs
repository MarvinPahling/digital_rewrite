//! Build-time component symbol pipeline.
//!
//! Reads `assets/components/manifest.toml`, validates every drawn delivery SVG, normalizes its
//! artwork with `resvg::usvg` and renders toolkit-neutral Rust source (`PathOp`, `SymbolPath`,
//! `SymbolDef` tables, defined in `src/symbols.rs`). The module is shared by `build.rs` and, in
//! test builds, by the main crate, so that its failure diagnostics are unit-tested. All inputs
//! come through an injected reader; the module touches no filesystem and no environment itself.
//!
//! Validation is two-staged. The raw XML is checked first (allowed elements and attributes,
//! black ink, anchors); the anchors group is then read and cut out of the source text, because
//! hidden nodes must not be assumed to survive normalization. The remaining artwork is parsed by
//! usvg, flattened to absolute coordinates and converted to path operations relative to the
//! binding origin. Curves and compound contours are preserved as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use resvg::usvg;
use serde::Deserialize;

const MAX_SVG_BYTES: usize = 1 << 20;
const MAX_XML_NODES: u32 = 10_000;
/// Name VectorCraft gives the layer group; it carries no part identity.
const LAYER_GROUP_ID: &str = "Layer_1";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const INK: &str = "#000000";
const MARKER_FILL: &str = "#ff00ff";
/// Coordinates are rounded to this many steps per world unit (three decimals).
const COORDINATE_STEPS: f64 = 1000.0;
const GEOMETRY_TOLERANCE: f64 = 1e-3;

const ALLOWED_ELEMENTS: [&str; 10] = [
    "svg", "title", "g", "path", "rect", "line", "polyline", "polygon", "circle", "ellipse",
];
const SHAPE_ELEMENTS: [&str; 7] = [
    "path", "rect", "line", "polyline", "polygon", "circle", "ellipse",
];
const ALLOWED_ATTRIBUTES: [&str; 27] = [
    "id",
    "data-name",
    "version",
    "width",
    "height",
    "viewBox",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "d",
    "points",
    "fill",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "fill-rule",
    "opacity",
];
/// Element names that get a pointed diagnostic instead of the generic one.
const FORBIDDEN_ELEMENT_HINTS: [(&str, &str); 6] = [
    ("text", "outline text so symbols never depend on fonts"),
    ("tspan", "outline text so symbols never depend on fonts"),
    ("image", "embedded images are not vector artwork"),
    ("filter", "filters are not supported"),
    ("mask", "masks are not supported"),
    ("clipPath", "clip paths are not supported"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    meta: Meta,
    master: Vec<Master>,
    binding: Vec<Binding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    version: u32,
    grid_unit: u32,
    artboard: [u32; 2],
    stroke_width: f64,
    default_style: String,
    anchor_group: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Master {
    id: String,
    #[allow(
        dead_code,
        reason = "editable source listed for humans; only the delivery SVG is consumed"
    )]
    src: String,
    svg: String,
    drawn: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    id: String,
    kind: String,
    style: String,
    master: String,
    origin: [i32; 2],
    anchors: Vec<ManifestAnchor>,
    drawn: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestAnchor {
    name: String,
    x: i32,
    y: i32,
    direction: String,
}

/// Result of a successful generation run.
pub struct Generated {
    /// Rust source for `$OUT_DIR/component_symbols.rs`.
    pub code: String,
    /// The manifest and every consumed SVG: the files `build.rs` must watch.
    pub inputs: Vec<PathBuf>,
    /// Non-fatal notes, for example skipped undrawn bindings.
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Body,
    Glyph,
    Lead,
    Indicator,
}

impl Role {
    fn from_part_id(id: &str) -> Option<Self> {
        let numbered = |prefix: &str| {
            id == prefix
                || id
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_prefix('-'))
                    .is_some_and(is_index)
        };
        let directed_lead = ["lead-in-", "lead-out-"]
            .iter()
            .any(|prefix| id.strip_prefix(prefix).is_some_and(is_index));
        if numbered("body") {
            Some(Self::Body)
        } else if numbered("glyph") {
            Some(Self::Glyph)
        } else if directed_lead {
            Some(Self::Lead)
        } else if numbered("indicator") {
            Some(Self::Indicator)
        } else {
            None
        }
    }

    fn variant(self) -> &'static str {
        match self {
            Self::Body => "Body",
            Self::Glyph => "Glyph",
            Self::Lead => "Lead",
            Self::Indicator => "Indicator",
        }
    }
}

#[derive(Clone, Copy)]
enum RawOp {
    Move([f64; 2]),
    Line([f64; 2]),
    Quad([f64; 2], [f64; 2]),
    Cubic([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

struct RawStroke {
    width: f64,
    cap: &'static str,
    join: &'static str,
    miter_limit: f64,
}

/// One normalized path in artboard coordinates.
struct RawPath {
    id: String,
    role: Role,
    ops: Vec<RawOp>,
    fill_rule: Option<&'static str>,
    stroke: Option<RawStroke>,
    /// Left, top, right, bottom including stroke.
    bounds: [f64; 4],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerDirection {
    In,
    Out,
}

struct Marker {
    direction: MarkerDirection,
    index: u32,
    position: [f64; 2],
}

struct ParsedMaster {
    markers: BTreeMap<String, Marker>,
    paths: Vec<RawPath>,
}

fn fail(path: &Path, reason: impl std::fmt::Display) -> String {
    format!("{}: {reason}", path.display())
}

fn is_index(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_number(path: &Path, what: &str, text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| fail(path, format!("{what} {text:?} is not a finite number")))
}

fn style_variant(style: &str) -> Option<&'static str> {
    match style {
        "rect" => Some("Rect"),
        "ieee" => Some("Ieee"),
        _ => None,
    }
}

fn kind_variant(kind: &str) -> Option<&'static str> {
    match kind {
        "input" => Some("Input"),
        "output" => Some("Output"),
        "and" => Some("And"),
        "or" => Some("Or"),
        "not" => Some("Not"),
        _ => None,
    }
}

/// Generates the symbol tables described by the manifest at `manifest_path`.
///
/// `read` supplies file contents; SVG paths are resolved relative to the manifest's directory.
pub fn generate(
    manifest_path: &Path,
    read: &dyn Fn(&Path) -> io::Result<String>,
) -> Result<Generated, String> {
    let manifest_text = read(manifest_path)
        .map_err(|e| fail(manifest_path, format!("cannot read manifest: {e}")))?;
    let manifest: Manifest = toml::from_str(&manifest_text)
        .map_err(|e| fail(manifest_path, format!("cannot parse manifest: {e}")))?;
    let meta = &manifest.meta;
    validate_meta(manifest_path, meta)?;
    let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));

    let mut master_ids = BTreeSet::new();
    for master in &manifest.master {
        if !master_ids.insert(master.id.as_str()) {
            return Err(fail(
                manifest_path,
                format!("duplicate master id {:?}", master.id),
            ));
        }
    }
    let mut binding_ids = BTreeSet::new();
    let mut binding_keys = BTreeSet::new();
    for binding in &manifest.binding {
        if !binding_ids.insert(binding.id.as_str()) {
            return Err(fail(
                manifest_path,
                format!("duplicate binding id {:?}", binding.id),
            ));
        }
        if !binding_keys.insert((binding.style.as_str(), binding.kind.as_str())) {
            return Err(fail(
                manifest_path,
                format!(
                    "binding {:?}: duplicate (style, kind) pair ({:?}, {:?})",
                    binding.id, binding.style, binding.kind
                ),
            ));
        }
    }

    let mut inputs = vec![manifest_path.to_path_buf()];
    let mut warnings = Vec::new();
    let mut parsed: BTreeMap<&str, ParsedMaster> = BTreeMap::new();
    let mut code =
        String::from("// @generated by build.rs from assets/components; do not edit.\n\n");
    let _ = writeln!(
        code,
        "pub const GRID_UNIT: f32 = {};",
        float(f64::from(meta.grid_unit))
    );
    let _ = writeln!(
        code,
        "pub const STROKE_WIDTH: f32 = {};",
        float(meta.stroke_width)
    );
    let default_style = style_variant(&meta.default_style).unwrap_or("Rect");
    let _ = writeln!(
        code,
        "pub const DEFAULT_STYLE: SymbolStyle = SymbolStyle::{default_style};\n"
    );
    code.push_str("pub static SYMBOLS: &[SymbolDef] = &[\n");

    for binding in &manifest.binding {
        let context = format!("binding {:?}", binding.id);
        let master = manifest
            .master
            .iter()
            .find(|master| master.id == binding.master)
            .ok_or_else(|| {
                fail(
                    manifest_path,
                    format!("{context}: unknown master {:?}", binding.master),
                )
            })?;
        if !binding.drawn {
            warnings.push(format!(
                "{}: {context} is not drawn yet and is skipped",
                manifest_path.display()
            ));
            continue;
        }
        if !master.drawn {
            return Err(fail(
                manifest_path,
                format!("{context} is drawn but its master {:?} is not", master.id),
            ));
        }
        let style = style_variant(&binding.style).ok_or_else(|| {
            fail(
                manifest_path,
                format!(
                    "{context}: unknown style {:?} (expected rect or ieee)",
                    binding.style
                ),
            )
        })?;
        let kind = kind_variant(&binding.kind).ok_or_else(|| {
            fail(
                manifest_path,
                format!("{context}: unknown kind {:?}", binding.kind),
            )
        })?;

        let svg_path = root.join(&master.svg);
        if !parsed.contains_key(master.id.as_str()) {
            let text =
                read(&svg_path).map_err(|e| fail(&svg_path, format!("cannot read SVG: {e}")))?;
            inputs.push(svg_path.clone());
            parsed.insert(master.id.as_str(), load_master(&svg_path, &text, meta)?);
        }
        let master_data = &parsed[master.id.as_str()];
        validate_binding(manifest_path, &svg_path, binding, master_data, meta)?;
        write_symbol(&mut code, binding, style, kind, master_data);
    }
    code.push_str("];\n");
    Ok(Generated {
        code,
        inputs,
        warnings,
    })
}

fn validate_meta(manifest_path: &Path, meta: &Meta) -> Result<(), String> {
    if meta.version != 1 {
        return Err(fail(
            manifest_path,
            format!("unsupported manifest version {}", meta.version),
        ));
    }
    if meta.grid_unit == 0 || meta.artboard.iter().any(|side| side % meta.grid_unit != 0) {
        return Err(fail(
            manifest_path,
            "artboard sides must be non-zero multiples of grid_unit",
        ));
    }
    if !(meta.stroke_width.is_finite() && meta.stroke_width > 0.0) {
        return Err(fail(manifest_path, "stroke_width must be positive"));
    }
    if style_variant(&meta.default_style).is_none() {
        return Err(fail(
            manifest_path,
            format!("unknown default_style {:?}", meta.default_style),
        ));
    }
    Ok(())
}

fn validate_binding(
    manifest_path: &Path,
    svg_path: &Path,
    binding: &Binding,
    master: &ParsedMaster,
    meta: &Meta,
) -> Result<(), String> {
    let context = format!("binding {:?}", binding.id);
    let grid = i64::from(meta.grid_unit);
    let off_grid = |value: i32| i64::from(value) % grid != 0;
    if binding.origin.iter().copied().any(off_grid) {
        return Err(fail(
            manifest_path,
            format!(
                "{context}: origin {:?} is off the {grid}-unit grid",
                binding.origin
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    for anchor in &binding.anchors {
        if !seen.insert(anchor.name.as_str()) {
            return Err(fail(
                manifest_path,
                format!("{context}: duplicate anchor {:?}", anchor.name),
            ));
        }
        if off_grid(anchor.x) || off_grid(anchor.y) {
            return Err(fail(
                manifest_path,
                format!(
                    "{context}: anchor {} at ({}, {}) is off the {grid}-unit grid",
                    anchor.name, anchor.x, anchor.y
                ),
            ));
        }
        let marker = master.markers.get(&anchor.name).ok_or_else(|| {
            fail(
                svg_path,
                format!("{context}: missing anchor marker {:?}", anchor.name),
            )
        })?;
        let expected = match marker.direction {
            MarkerDirection::In => "in",
            MarkerDirection::Out => "out",
        };
        if anchor.direction != expected {
            return Err(fail(
                manifest_path,
                format!(
                    "{context}: anchor {} has direction {:?} but its name implies {expected:?}",
                    anchor.name, anchor.direction
                ),
            ));
        }
        let want = [
            f64::from(binding.origin[0]) + f64::from(anchor.x),
            f64::from(binding.origin[1]) + f64::from(anchor.y),
        ];
        if (marker.position[0] - want[0]).abs() > GEOMETRY_TOLERANCE
            || (marker.position[1] - want[1]).abs() > GEOMETRY_TOLERANCE
        {
            return Err(fail(
                svg_path,
                format!(
                    "{context}: anchor marker {} is at ({}, {}) but the manifest expects ({}, {})",
                    anchor.name, marker.position[0], marker.position[1], want[0], want[1]
                ),
            ));
        }
    }
    if let Some(extra) = master
        .markers
        .keys()
        .find(|name| !seen.contains(name.as_str()))
    {
        return Err(fail(
            svg_path,
            format!("{context}: anchor marker {extra:?} is not listed in the manifest"),
        ));
    }
    for direction in [MarkerDirection::In, MarkerDirection::Out] {
        let mut indices: Vec<u32> = master
            .markers
            .values()
            .filter(|marker| marker.direction == direction)
            .map(|marker| marker.index)
            .collect();
        indices.sort_unstable();
        if indices.iter().copied().ne(0..indices.len() as u32) {
            return Err(fail(
                svg_path,
                format!("{context}: anchor indices must run 0, 1, 2, ... without gaps"),
            ));
        }
    }
    Ok(())
}

/// Validates the raw XML, extracts the anchors, then normalizes the artwork through usvg.
fn load_master(path: &Path, text: &str, meta: &Meta) -> Result<ParsedMaster, String> {
    if text.len() > MAX_SVG_BYTES {
        return Err(fail(path, format!("SVG exceeds {MAX_SVG_BYTES} bytes")));
    }
    let options = usvg::roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_XML_NODES,
        ..Default::default()
    };
    let document = usvg::roxmltree::Document::parse_with_options(text, options)
        .map_err(|e| fail(path, format!("cannot parse XML: {e}")))?;
    let root = document.root_element();
    check_root(path, root, meta)?;

    let mut anchor_groups = Vec::new();
    let mut ids = BTreeSet::new();
    let mut shape_count = 0usize;
    for node in root.descendants().filter(|node| node.is_element()) {
        if let Some(id) = node.attribute("id")
            && !ids.insert(id)
        {
            return Err(fail(path, format!("duplicate id {id:?}")));
        }
        if node.attribute("id") == Some(meta.anchor_group.as_str()) {
            anchor_groups.push(node);
        }
    }
    let [anchors_node] = anchor_groups[..] else {
        return Err(fail(
            path,
            format!(
                "expected exactly one anchors group with id {:?}, found {}",
                meta.anchor_group,
                anchor_groups.len()
            ),
        ));
    };
    let markers = read_markers(path, anchors_node, meta)?;
    for node in root.descendants().filter(|node| node.is_element()) {
        let inside_anchors = node.ancestors().any(|ancestor| ancestor == anchors_node);
        if node != root && !inside_anchors {
            check_element(path, node)?;
            if SHAPE_ELEMENTS.contains(&node.tag_name().name()) {
                shape_count += 1;
            }
        }
    }

    let range = anchors_node.range();
    let stripped = format!("{}{}", &text[..range.start], &text[range.end..]);
    let tree = usvg::Tree::from_str(&stripped, &usvg::Options::default())
        .map_err(|e| fail(path, format!("cannot parse SVG: {e}")))?;
    let size = tree.size();
    if (f64::from(size.width()) - f64::from(meta.artboard[0])).abs() > GEOMETRY_TOLERANCE
        || (f64::from(size.height()) - f64::from(meta.artboard[1])).abs() > GEOMETRY_TOLERANCE
    {
        return Err(fail(
            path,
            format!(
                "artboard is {}x{} but the manifest requires {}x{}",
                size.width(),
                size.height(),
                meta.artboard[0],
                meta.artboard[1]
            ),
        ));
    }
    let mut paths = Vec::new();
    collect_paths(path, tree.root(), None, meta, &mut paths)?;
    if paths.len() != shape_count {
        return Err(fail(
            path,
            format!(
                "{shape_count} shape elements but only {} paths survived normalization; \
                 a shape without visible fill or stroke would be silently dropped",
                paths.len()
            ),
        ));
    }
    if paths.is_empty() {
        return Err(fail(path, "contains no artwork"));
    }
    Ok(ParsedMaster { markers, paths })
}

fn check_root(path: &Path, root: usvg::roxmltree::Node, meta: &Meta) -> Result<(), String> {
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NAMESPACE) {
        return Err(fail(path, "root element is not an SVG <svg>"));
    }
    check_attributes(path, root)?;
    let [width, height] = meta.artboard.map(f64::from);
    for (name, want) in [("width", width), ("height", height)] {
        let value = root
            .attribute(name)
            .ok_or_else(|| fail(path, format!("<svg> lacks {name}")))?;
        if (parse_number(path, name, value)? - want).abs() > GEOMETRY_TOLERANCE {
            return Err(fail(
                path,
                format!("<svg> {name}={value:?} differs from the manifest artboard {want}"),
            ));
        }
    }
    let view_box = root
        .attribute("viewBox")
        .ok_or_else(|| fail(path, "<svg> lacks viewBox"))?;
    let numbers: Result<Vec<f64>, String> = view_box
        .split([' ', ','])
        .filter(|part| !part.is_empty())
        .map(|part| parse_number(path, "viewBox entry", part))
        .collect();
    if numbers?.as_slice() != [0.0, 0.0, width, height] {
        return Err(fail(
            path,
            format!("viewBox {view_box:?} must be \"0 0 {width} {height}\""),
        ));
    }
    Ok(())
}

fn check_element(path: &Path, node: usvg::roxmltree::Node) -> Result<(), String> {
    let name = node.tag_name().name();
    if let Some((_, hint)) = FORBIDDEN_ELEMENT_HINTS.iter().find(|(tag, _)| *tag == name) {
        return Err(fail(path, format!("contains a <{name}> element: {hint}")));
    }
    if node.tag_name().namespace() != Some(SVG_NAMESPACE) || !ALLOWED_ELEMENTS.contains(&name) {
        return Err(fail(path, format!("unsupported element <{name}>")));
    }
    if name == "title" {
        if node.attributes().next().is_some() {
            return Err(fail(path, "<title> must not carry attributes"));
        }
        return Ok(());
    }
    check_attributes(path, node)?;
    if node.attribute("opacity").is_some() {
        return Err(fail(
            path,
            format!(
                "<{name}> sets opacity; only the anchors group may (artwork is opaque black ink)"
            ),
        ));
    }
    Ok(())
}

fn check_attributes(path: &Path, node: usvg::roxmltree::Node) -> Result<(), String> {
    let element = node.tag_name().name();
    for attribute in node.attributes() {
        let name = attribute.name();
        let value = attribute.value();
        if attribute.namespace().is_some() || !ALLOWED_ATTRIBUTES.contains(&name) {
            return Err(fail(
                path,
                format!("<{element}> uses unsupported attribute {name:?}"),
            ));
        }
        let allowed: &[&str] = match name {
            "fill" | "stroke" => &["none", INK],
            "fill-rule" => &["nonzero", "evenodd"],
            "stroke-linecap" => &["butt", "round", "square"],
            "stroke-linejoin" => &["miter", "round", "bevel"],
            _ => continue,
        };
        if !allowed.contains(&value) {
            return Err(fail(
                path,
                format!(
                    "<{element}> {name}={value:?} is not allowed (expected one of {allowed:?})"
                ),
            ));
        }
    }
    Ok(())
}

/// Reads the anchor markers; the group and its children use a stricter attribute set.
fn read_markers(
    path: &Path,
    group: usvg::roxmltree::Node,
    meta: &Meta,
) -> Result<BTreeMap<String, Marker>, String> {
    if group.tag_name().name() != "g" || group.parent_element().is_none() {
        return Err(fail(path, "the anchors group must be a <g> inside <svg>"));
    }
    for attribute in group.attributes() {
        let valid = match attribute.name() {
            "id" => true,
            "opacity" => attribute.value() == "0",
            _ => false,
        };
        if !valid || attribute.namespace().is_some() {
            return Err(fail(
                path,
                format!(
                    "anchors group attribute {:?}={:?} is not allowed (only id and opacity=\"0\")",
                    attribute.name(),
                    attribute.value()
                ),
            ));
        }
    }
    if group.attribute("opacity") != Some("0") {
        return Err(fail(path, "the anchors group must set opacity=\"0\""));
    }
    let grid = f64::from(meta.grid_unit);
    let mut markers = BTreeMap::new();
    for child in group.children().filter(|node| node.is_element()) {
        let name = child.attribute("id").unwrap_or("");
        if child.tag_name().name() != "circle" {
            return Err(fail(
                path,
                format!(
                    "anchors group child <{}> is not a <circle>",
                    child.tag_name().name()
                ),
            ));
        }
        for attribute in child.attributes() {
            if attribute.namespace().is_some()
                || !["id", "cx", "cy", "r", "fill"].contains(&attribute.name())
            {
                return Err(fail(
                    path,
                    format!(
                        "anchor {name:?} uses unsupported attribute {:?} (markers are plain circles)",
                        attribute.name()
                    ),
                ));
            }
        }
        let (direction, index_text) = if let Some(rest) = name.strip_prefix("pin-in-") {
            (MarkerDirection::In, rest)
        } else if let Some(rest) = name.strip_prefix("pin-out-") {
            (MarkerDirection::Out, rest)
        } else if name.starts_with("join-") {
            return Err(fail(
                path,
                format!("anchor {name:?}: join guides are not supported by the build pipeline yet"),
            ));
        } else {
            return Err(fail(
                path,
                format!("anchor id {name:?} is not pin-in-<n> or pin-out-<n>"),
            ));
        };
        if !is_index(index_text) {
            return Err(fail(
                path,
                format!("anchor id {name:?} has no numeric index"),
            ));
        }
        let attribute = |key: &str| {
            child
                .attribute(key)
                .ok_or_else(|| fail(path, format!("anchor {name} lacks {key}")))
        };
        if child.attribute("fill") != Some(MARKER_FILL) {
            return Err(fail(
                path,
                format!("anchor {name} must have fill=\"{MARKER_FILL}\""),
            ));
        }
        if parse_number(path, "r", attribute("r")?)? != 1.0 {
            return Err(fail(path, format!("anchor {name} must have r=\"1\"")));
        }
        let position = [
            parse_number(path, "cx", attribute("cx")?)?,
            parse_number(path, "cy", attribute("cy")?)?,
        ];
        for coordinate in position {
            if coordinate % grid != 0.0
                || coordinate < 0.0
                || coordinate > f64::from(meta.artboard[0].max(meta.artboard[1]))
            {
                return Err(fail(
                    path,
                    format!(
                        "anchor {name} at ({}, {}) is off the {grid}-unit grid or outside the artboard",
                        position[0], position[1]
                    ),
                ));
            }
        }
        let index = index_text.parse::<u32>().map_err(|_| {
            fail(
                path,
                format!("anchor id {name:?} has an index out of range"),
            )
        })?;
        markers.insert(
            name.to_owned(),
            Marker {
                direction,
                index,
                position,
            },
        );
    }
    Ok(markers)
}

fn collect_paths(
    path: &Path,
    group: &usvg::Group,
    part: Option<&str>,
    meta: &Meta,
    out: &mut Vec<RawPath>,
) -> Result<(), String> {
    for node in group.children() {
        match node {
            usvg::Node::Group(inner) => {
                if inner.opacity().get() != 1.0
                    || inner.clip_path().is_some()
                    || inner.mask().is_some()
                    || !inner.filters().is_empty()
                {
                    return Err(fail(
                        path,
                        format!("group {:?} applies effects", inner.id()),
                    ));
                }
                let id = inner.id();
                let next = if id.is_empty() || id == LAYER_GROUP_ID {
                    part
                } else {
                    Some(id)
                };
                collect_paths(path, inner, next, meta, out)?;
            }
            usvg::Node::Path(shape) => out.push(convert_path(path, shape, part, meta)?),
            usvg::Node::Image(_) => return Err(fail(path, "contains an image")),
            usvg::Node::Text(_) => return Err(fail(path, "contains text; outline it")),
        }
    }
    Ok(())
}

fn convert_path(
    path: &Path,
    shape: &usvg::Path,
    inherited_part: Option<&str>,
    meta: &Meta,
) -> Result<RawPath, String> {
    let own_id = shape.id();
    let part = if own_id.is_empty() {
        inherited_part
    } else {
        Some(own_id)
    };
    let part = part.ok_or_else(|| {
        fail(
            path,
            "a path has no part id (name it body, glyph, lead-in-<n>, lead-out-<n> or indicator)",
        )
    })?;
    let role = Role::from_part_id(part).ok_or_else(|| {
        fail(
            path,
            format!(
                "part id {part:?} has no known role \
                 (expected body, glyph, lead-in-<n>, lead-out-<n> or indicator)"
            ),
        )
    })?;

    let transform = shape.abs_transform();
    let scale_x = f64::from(transform.sx).hypot(f64::from(transform.ky));
    let scale_y = f64::from(transform.kx).hypot(f64::from(transform.sy));
    let shear = f64::from(transform.sx) * f64::from(transform.kx)
        + f64::from(transform.ky) * f64::from(transform.sy);
    if (scale_x - scale_y).abs() > 1e-4 * scale_x.max(1.0) || shear.abs() > 1e-4 * scale_x * scale_y
    {
        return Err(fail(
            path,
            format!("part {part:?} has a non-similarity transform (skew or non-uniform scale)"),
        ));
    }
    let map = |point: usvg::tiny_skia_path::Point| {
        let (x, y) = (f64::from(point.x), f64::from(point.y));
        [
            f64::from(transform.sx) * x + f64::from(transform.kx) * y + f64::from(transform.tx),
            f64::from(transform.ky) * x + f64::from(transform.sy) * y + f64::from(transform.ty),
        ]
    };
    let ops = shape
        .data()
        .segments()
        .map(|segment| match segment {
            usvg::tiny_skia_path::PathSegment::MoveTo(p) => RawOp::Move(map(p)),
            usvg::tiny_skia_path::PathSegment::LineTo(p) => RawOp::Line(map(p)),
            usvg::tiny_skia_path::PathSegment::QuadTo(c, p) => RawOp::Quad(map(c), map(p)),
            usvg::tiny_skia_path::PathSegment::CubicTo(c1, c2, p) => {
                RawOp::Cubic(map(c1), map(c2), map(p))
            }
            usvg::tiny_skia_path::PathSegment::Close => RawOp::Close,
        })
        .collect();

    let require_black = |paint: &usvg::Paint, opacity: f32, what: &str| match paint {
        usvg::Paint::Color(color)
            if (color.red, color.green, color.blue) == (0, 0, 0) && opacity == 1.0 =>
        {
            Ok(())
        }
        _ => Err(fail(
            path,
            format!("part {part:?} {what} is not opaque black {INK}"),
        )),
    };
    let fill_rule = match shape.fill() {
        Some(fill) => {
            require_black(fill.paint(), fill.opacity().get(), "fill")?;
            Some(match fill.rule() {
                usvg::FillRule::NonZero => "NonZero",
                usvg::FillRule::EvenOdd => "EvenOdd",
            })
        }
        None => None,
    };
    let stroke = match shape.stroke() {
        Some(stroke) => {
            require_black(stroke.paint(), stroke.opacity().get(), "stroke")?;
            if stroke.dasharray().is_some() {
                return Err(fail(path, format!("part {part:?} uses a dashed stroke")));
            }
            let width = f64::from(stroke.width().get()) * scale_x;
            if (width - meta.stroke_width).abs() > GEOMETRY_TOLERANCE {
                return Err(fail(
                    path,
                    format!(
                        "part {part:?} has stroke width {width} but the manifest requires {}",
                        meta.stroke_width
                    ),
                ));
            }
            let cap = match stroke.linecap() {
                usvg::LineCap::Butt => "Butt",
                usvg::LineCap::Round => "Round",
                usvg::LineCap::Square => "Square",
            };
            let join = match stroke.linejoin() {
                usvg::LineJoin::Miter => "Miter",
                usvg::LineJoin::Round => "Round",
                usvg::LineJoin::Bevel => "Bevel",
                usvg::LineJoin::MiterClip => {
                    return Err(fail(path, format!("part {part:?} uses miter-clip joins")));
                }
            };
            Some(RawStroke {
                width,
                cap,
                join,
                miter_limit: f64::from(stroke.miterlimit().get()),
            })
        }
        None => None,
    };

    let shape_ok = match role {
        Role::Body | Role::Lead => stroke.is_some() && fill_rule.is_none(),
        Role::Glyph => fill_rule.is_some() && stroke.is_none(),
        Role::Indicator => fill_rule.is_some() || stroke.is_some(),
    };
    if !shape_ok {
        return Err(fail(
            path,
            format!(
                "part {part:?} has the wrong paint for its role (body and lead are stroke only, \
                 glyph is fill only, indicator is filled or stroked)"
            ),
        ));
    }

    let bounds = if stroke.is_some() {
        shape.abs_stroke_bounding_box()
    } else {
        shape.abs_bounding_box()
    }
    .ok_or_else(|| fail(path, format!("part {part:?} has empty geometry")))?;
    Ok(RawPath {
        id: part.to_owned(),
        role,
        ops,
        fill_rule,
        stroke,
        bounds: [
            f64::from(bounds.left()),
            f64::from(bounds.top()),
            f64::from(bounds.right()),
            f64::from(bounds.bottom()),
        ],
    })
}

/// Rounds a coordinate to three decimals and normalizes negative zero.
fn quantize(value: f64) -> f64 {
    let rounded = (value * COORDINATE_STEPS).round() / COORDINATE_STEPS;
    if rounded == 0.0 { 0.0 } else { rounded }
}

fn float(value: f64) -> String {
    format!("{:?}", quantize(value) as f32)
}

fn point(origin: [i32; 2], p: [f64; 2]) -> String {
    format!(
        "[{}, {}]",
        float(p[0] - f64::from(origin[0])),
        float(p[1] - f64::from(origin[1]))
    )
}

fn write_symbol(
    code: &mut String,
    binding: &Binding,
    style: &str,
    kind: &str,
    master: &ParsedMaster,
) {
    let origin = binding.origin;
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for shape in &master.paths {
        min[0] = min[0].min(shape.bounds[0]);
        min[1] = min[1].min(shape.bounds[1]);
        max[0] = max[0].max(shape.bounds[2]);
        max[1] = max[1].max(shape.bounds[3]);
    }
    let _ = writeln!(code, "    SymbolDef {{");
    let _ = writeln!(code, "        id: {:?},", binding.id);
    let _ = writeln!(code, "        style: SymbolStyle::{style},");
    let _ = writeln!(code, "        kind: ComponentKind::{kind},");
    let _ = writeln!(code, "        master: {:?},", binding.master);
    let _ = writeln!(
        code,
        "        origin: [{}, {}],",
        float(f64::from(origin[0])),
        float(f64::from(origin[1]))
    );
    let _ = writeln!(
        code,
        "        bounds: SymbolBounds {{ min: {}, max: {} }},",
        point(origin, min),
        point(origin, max)
    );
    code.push_str("        paths: &[\n");
    for shape in &master.paths {
        code.push_str("            SymbolPath {\n");
        let _ = writeln!(code, "                id: {:?},", shape.id);
        let _ = writeln!(
            code,
            "                role: SymbolRole::{},",
            shape.role.variant()
        );
        code.push_str("                ops: &[\n");
        for op in &shape.ops {
            let text = match *op {
                RawOp::Move(p) => format!("PathOp::MoveTo({})", point(origin, p)),
                RawOp::Line(p) => format!("PathOp::LineTo({})", point(origin, p)),
                RawOp::Quad(c, p) => {
                    format!("PathOp::QuadTo({}, {})", point(origin, c), point(origin, p))
                }
                RawOp::Cubic(c1, c2, p) => format!(
                    "PathOp::CubicTo({}, {}, {})",
                    point(origin, c1),
                    point(origin, c2),
                    point(origin, p)
                ),
                RawOp::Close => "PathOp::Close".to_owned(),
            };
            let _ = writeln!(code, "                    {text},");
        }
        code.push_str("                ],\n");
        let _ = writeln!(
            code,
            "                fill_rule: FillRule::{},",
            shape.fill_rule.unwrap_or("NonZero")
        );
        let _ = writeln!(
            code,
            "                filled: {},",
            shape.fill_rule.is_some()
        );
        let _ = writeln!(code, "                stroked: {},", shape.stroke.is_some());
        let (width, cap, join, miter) = match &shape.stroke {
            Some(stroke) => (stroke.width, stroke.cap, stroke.join, stroke.miter_limit),
            None => (0.0, "Butt", "Miter", 4.0),
        };
        let _ = writeln!(code, "                stroke_width: {},", float(width));
        let _ = writeln!(code, "                cap: StrokeCap::{cap},");
        let _ = writeln!(code, "                join: StrokeJoin::{join},");
        let _ = writeln!(code, "                miter_limit: {},", float(miter));
        code.push_str("            },\n");
    }
    code.push_str("        ],\n        anchors: &[\n");
    for anchor in &binding.anchors {
        let direction = if anchor.direction == "in" {
            "Input"
        } else {
            "Output"
        };
        let _ = writeln!(
            code,
            "            SymbolAnchor {{ name: {:?}, position: [{}, {}], direction: PortDirection::{direction} }},",
            anchor.name,
            float(f64::from(anchor.x)),
            float(f64::from(anchor.y))
        );
    }
    code.push_str("        ],\n    },\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT_AND: &str = include_str!("../assets/components/svg/rect-and.svg");

    fn real_manifest_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/components/manifest.toml")
    }

    fn read_disk(path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    /// One AND binding on a single master "m", whose SVG text is supplied by the caller.
    fn run_single(svg: &str, drawn: bool) -> Result<Generated, String> {
        let manifest = format!(
            r#"
[meta]
version = 1
grid_unit = 20
artboard = [120, 120]
stroke_width = 1.5
default_style = "rect"
anchor_group = "anchors"

[[master]]
id = "m"
src = "src/m.svg"
svg = "svg/m.svg"
drawn = {drawn}

[[binding]]
id = "rect-and"
kind = "and"
style = "rect"
master = "m"
origin = [20, 40]
anchors = [
  {{ name = "pin-in-0", x = 0, y = 0, direction = "in" }},
  {{ name = "pin-in-1", x = 0, y = 40, direction = "in" }},
  {{ name = "pin-out-0", x = 60, y = 20, direction = "out" }},
]
drawn = {drawn}
"#
        );
        let svg = svg.to_owned();
        generate(Path::new("/t/manifest.toml"), &move |path| {
            if path.ends_with("manifest.toml") {
                Ok(manifest.clone())
            } else if path.ends_with("svg/m.svg") {
                Ok(svg.clone())
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "unexpected read"))
            }
        })
    }

    fn expect_error(svg: &str, needles: &[&str]) {
        let message = match run_single(svg, true) {
            Ok(_) => panic!("expected a failure mentioning {needles:?}"),
            Err(message) => message,
        };
        assert!(
            message.starts_with("/t/svg/m.svg: "),
            "the message must name the file first: {message}"
        );
        for needle in needles {
            assert!(message.contains(needle), "{message:?} lacks {needle:?}");
        }
    }

    fn replaced(from: &str, to: &str) -> String {
        assert!(RECT_AND.contains(from), "fixture lacks {from:?}");
        RECT_AND.replacen(from, to, 1)
    }

    #[test]
    fn unmodified_fixture_generates() {
        let generated = run_single(RECT_AND, true).expect("the committed rect-and master is valid");
        assert!(generated.code.contains("id: \"rect-and\""));
        assert!(generated.warnings.is_empty());
        assert_eq!(generated.inputs.len(), 2);
    }

    #[test]
    fn missing_anchor_marker_is_rejected() {
        let svg = replaced(
            r##"<circle id="pin-out-0" cx="80" cy="60" r="1" fill="#ff00ff"/>"##,
            "",
        );
        expect_error(&svg, &["missing anchor marker", "pin-out-0"]);
    }

    #[test]
    fn off_grid_anchor_is_rejected() {
        let svg = replaced(r#"id="pin-in-0" cx="20""#, r#"id="pin-in-0" cx="21""#);
        expect_error(&svg, &["pin-in-0", "off the 20-unit grid"]);
    }

    #[test]
    fn anchor_that_disagrees_with_the_manifest_is_rejected() {
        let svg = replaced(
            r#"id="pin-in-0" cx="20" cy="40""#,
            r#"id="pin-in-0" cx="20" cy="60""#,
        );
        expect_error(&svg, &["pin-in-0", "manifest expects"]);
    }

    #[test]
    fn text_elements_are_rejected() {
        let svg = replaced(
            "</g>\n  </g>",
            "</g><text id=\"glyph-9\">&amp;</text>\n  </g>",
        );
        expect_error(&svg, &["<text>", "outline"]);
    }

    #[test]
    fn gradients_and_filters_are_rejected() {
        let gradient = replaced("<title>", "<linearGradient id=\"g\"/><title>");
        expect_error(&gradient, &["unsupported element <linearGradient>"]);
        let filter = replaced(r#"id="body""#, r#"id="body" filter="url(#f)""#);
        expect_error(&filter, &["unsupported attribute \"filter\""]);
    }

    #[test]
    fn transforms_are_rejected_on_root_groups_shapes_and_markers() {
        for (from, to) in [
            ("<svg ", "<svg transform=\"translate(20)\" "),
            (
                r#"id="Layer_1""#,
                r#"id="Layer_1" transform="translate(20)""#,
            ),
            (r#"id="body""#, r#"id="body" transform="translate(20)""#),
            (
                r#"id="anchors""#,
                r#"id="anchors" transform="translate(20)""#,
            ),
            (
                r#"id="pin-in-0""#,
                r#"id="pin-in-0" transform="translate(20)""#,
            ),
        ] {
            expect_error(&replaced(from, to), &["transform"]);
        }
    }

    #[test]
    fn non_black_ink_is_rejected() {
        let svg = replaced(r##"stroke="#000000""##, r##"stroke="#ff0000""##);
        expect_error(&svg, &["stroke", "not allowed"]);
    }

    #[test]
    fn unknown_part_id_is_rejected() {
        let svg = replaced(r#"id="body""#, r#"id="frame""#);
        expect_error(&svg, &["frame", "no known role"]);
    }

    #[test]
    fn path_without_a_part_id_is_rejected() {
        let svg = replaced(r#"id="lead-in-0" "#, "");
        expect_error(&svg, &["no part id"]);
    }

    #[test]
    fn wrong_stroke_width_is_rejected() {
        let svg = replaced(r#"stroke-width="1.5""#, r#"stroke-width="2""#);
        expect_error(&svg, &["stroke width"]);
    }

    #[test]
    fn dropped_shapes_are_rejected() {
        let degenerate = concat!(
            r#"<rect id="body-1" x="1" y="1" width="0" height="2" fill="none" "#,
            r##"stroke="#000000" stroke-width="1.5"/><title>"##
        );
        expect_error(&replaced("<title>", degenerate), &["silently dropped"]);
    }

    #[test]
    fn unpainted_shapes_are_rejected() {
        let unpainted = concat!(
            r#"<rect id="body-1" x="1" y="1" width="2" height="2" fill="none"/>"#,
            "<title>"
        );
        expect_error(&replaced("<title>", unpainted), &["body-1", "wrong paint"]);
    }

    #[test]
    fn undrawn_bindings_are_skipped_with_a_warning() {
        let generated = run_single("not read at all", false).expect("undrawn entries are skipped");
        assert!(!generated.code.contains("SymbolDef {"));
        assert_eq!(generated.warnings.len(), 1);
        assert!(generated.warnings[0].contains("rect-and"));
    }

    #[test]
    fn committed_manifest_generates_every_binding_stably() {
        let first = generate(&real_manifest_path(), &read_disk).expect("committed assets");
        let second = generate(&real_manifest_path(), &read_disk).expect("committed assets");
        assert_eq!(
            first.code, second.code,
            "unchanged input must regenerate identically"
        );
        assert_eq!(first.code.matches("    SymbolDef {").count(), 10);
        assert!(first.warnings.is_empty());
        // Manifest plus the eight masters.
        assert_eq!(first.inputs.len(), 9);
    }

    #[test]
    fn missing_manifest_names_the_file() {
        let message = match generate(Path::new("/t/none.toml"), &read_disk) {
            Ok(_) => panic!("a missing manifest must fail"),
            Err(message) => message,
        };
        assert!(message.starts_with("/t/none.toml: cannot read manifest"));
    }
}
