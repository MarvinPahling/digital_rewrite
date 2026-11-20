//! Rasterizes the delivery SVGs listed in `assets/icons/manifest.toml` into PNGs under
//! `$OUT_DIR/assets/` and generates `$OUT_DIR/assets.rs` with tables embedding them.
//! PNGs are build outputs and are never committed.
//!
//! It also converts the component symbol SVGs listed in `assets/components/manifest.toml` into
//! toolkit-neutral vector data (`$OUT_DIR/component_symbols.rs`); see `build_support`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};
use serde::Deserialize;

#[path = "build_support/component_symbols.rs"]
mod component_symbols;

const ICON_PIXEL_SIZES: [u32; 2] = [24, 48];
const SPLASH_SCALES: [u32; 2] = [1, 2];

#[derive(Deserialize)]
struct Manifest {
    icon: Vec<Icon>,
    brand: Brand,
}

#[derive(Deserialize)]
struct Icon {
    id: String,
    svg: String,
}

#[derive(Deserialize)]
struct Brand {
    app_icon: SquareBrand,
    app_icon_small: SquareBrand,
    splash: SplashBrand,
}

#[derive(Deserialize)]
struct SquareBrand {
    svg: String,
    sizes: Vec<u32>,
}

#[derive(Deserialize)]
struct SplashBrand {
    svg: String,
    sizes: Vec<[u32; 2]>,
}

/// A rendered PNG: `file` is relative to `$OUT_DIR/assets/`.
struct Output {
    file: String,
    key: u32,
}

fn main() {
    if let Err(message) = run() {
        eprintln!("error: asset generation failed: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env_var("OUT_DIR")?);
    let assets_dir = manifest_dir.join("assets");
    let manifest_path = assets_dir.join("icons").join("manifest.toml");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/component_symbols.rs");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: cannot read manifest: {e}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest_text)
        .map_err(|e| format!("{}: cannot parse manifest: {e}", manifest_path.display()))?;

    let png_root = out_dir.join("assets");
    let mut seen_ids = BTreeSet::new();
    let mut icon_outputs: Vec<(String, u32, String)> = Vec::new();
    for icon in &manifest.icon {
        if !seen_ids.insert(icon.id.clone()) {
            return Err(format!(
                "{}: duplicate icon id {:?}",
                manifest_path.display(),
                icon.id
            ));
        }
        let svg_path = assets_dir.join(&icon.svg);
        let tree = load_svg(&svg_path)?;
        for (index, px) in ICON_PIXEL_SIZES.into_iter().enumerate() {
            let file = format!("icons/{}@{}x.png", icon.id, index + 1);
            render_png(&svg_path, &tree, px, px, &png_root.join(&file))?;
            icon_outputs.push((icon.id.clone(), px, file));
        }
    }

    let mut app_icon_outputs = Vec::new();
    for group in [&manifest.brand.app_icon, &manifest.brand.app_icon_small] {
        let svg_path = assets_dir.join(&group.svg);
        let tree = load_svg(&svg_path)?;
        for &px in &group.sizes {
            let file = format!("brand/app-icon-{px}.png");
            render_png(&svg_path, &tree, px, px, &png_root.join(&file))?;
            app_icon_outputs.push(Output { file, key: px });
        }
    }

    let splash_path = assets_dir.join(&manifest.brand.splash.svg);
    let splash_tree = load_svg(&splash_path)?;
    let splash_base = manifest
        .brand
        .splash
        .sizes
        .iter()
        .min_by_key(|size| size[0])
        .copied()
        .ok_or_else(|| format!("{}: splash has no sizes", manifest_path.display()))?;
    let mut splash_outputs = Vec::new();
    for scale in SPLASH_SCALES {
        let (width, height) = (splash_base[0] * scale, splash_base[1] * scale);
        if !manifest.brand.splash.sizes.contains(&[width, height]) {
            return Err(format!(
                "{}: splash size {width}x{height} (scale {scale}) is not listed in the manifest",
                manifest_path.display()
            ));
        }
        let file = format!("brand/app-splash@{scale}x.png");
        render_png(
            &splash_path,
            &splash_tree,
            width,
            height,
            &png_root.join(&file),
        )?;
        splash_outputs.push(Output { file, key: scale });
    }

    let generated = generate_tables(
        &png_root,
        &manifest.icon,
        &icon_outputs,
        &app_icon_outputs,
        &splash_outputs,
    );
    let generated_path = out_dir.join("assets.rs");
    fs::write(&generated_path, generated).map_err(|e| {
        format!(
            "{}: cannot write generated tables: {e}",
            generated_path.display()
        )
    })?;

    generate_component_symbols(&assets_dir, &out_dir)
}

/// Writes `$OUT_DIR/component_symbols.rs` and registers the manifest and SVGs for rerun.
fn generate_component_symbols(assets_dir: &Path, out_dir: &Path) -> Result<(), String> {
    let manifest_path = assets_dir.join("components").join("manifest.toml");
    let generated = component_symbols::generate(&manifest_path, &|path| fs::read_to_string(path))?;
    for input in &generated.inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    for warning in &generated.warnings {
        println!("cargo:warning={warning}");
    }
    let output_path = out_dir.join("component_symbols.rs");
    fs::write(&output_path, generated.code).map_err(|e| {
        format!(
            "{}: cannot write generated symbols: {e}",
            output_path.display()
        )
    })
}

fn env_var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|e| format!("environment variable {name} is unavailable: {e}"))
}

/// Loads an SVG, rejecting `<text>` so output never depends on installed fonts.
fn load_svg(path: &Path) -> Result<usvg::Tree, String> {
    println!("cargo:rerun-if-changed={}", path.display());
    let data = fs::read_to_string(path)
        .map_err(|e| format!("{}: cannot read SVG: {e}", path.display()))?;
    if data.contains("<text") || data.contains(":text") {
        return Err(format!(
            "{}: contains a <text> element; outline text so rendering does not depend on fonts",
            path.display()
        ));
    }
    usvg::Tree::from_str(&data, &usvg::Options::default())
        .map_err(|e| format!("{}: cannot parse SVG: {e}", path.display()))
}

fn render_png(
    svg_path: &Path,
    tree: &usvg::Tree,
    width: u32,
    height: u32,
    destination: &Path,
) -> Result<(), String> {
    let size = tree.size();
    // Cross-multiplied aspect comparison with a small tolerance for float rounding.
    let svg_ratio = f64::from(size.width()) * f64::from(height);
    let target_ratio = f64::from(size.height()) * f64::from(width);
    if (svg_ratio - target_ratio).abs() > 1e-3 * target_ratio.max(1.0) {
        return Err(format!(
            "{}: aspect ratio mismatch: SVG is {}x{} but target is {width}x{height}",
            svg_path.display(),
            size.width(),
            size.height()
        ));
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
        format!(
            "{}: cannot allocate a {width}x{height} pixmap",
            svg_path.display()
        )
    })?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let png = pixmap.encode_png().map_err(|e| {
        format!(
            "{}: cannot encode PNG {}: {e}",
            svg_path.display(),
            destination.display()
        )
    })?;
    write_file(destination, &png)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("{}: cannot create directory: {e}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|e| format!("{}: cannot write file: {e}", path.display()))
}

fn generate_tables(
    png_root: &Path,
    icons: &[Icon],
    icon_outputs: &[(String, u32, String)],
    app_icons: &[Output],
    splash: &[Output],
) -> String {
    let embed = |file: &str| {
        format!(
            "include_bytes!({:?})",
            png_root.join(file).display().to_string()
        )
    };
    let mut out = String::from("// @generated by build.rs; do not edit.\n\n");
    out.push_str("pub static ICON_IDS: &[&str] = &[\n");
    for icon in icons {
        let _ = writeln!(out, "    {:?},", icon.id);
    }
    out.push_str("];\n\n");
    out.push_str("pub static ICON_PNGS: &[(&str, u32, &[u8])] = &[\n");
    for (id, px, file) in icon_outputs {
        let _ = writeln!(out, "    ({id:?}, {px}, {}),", embed(file));
    }
    out.push_str("];\n\n");
    out.push_str("pub static APP_ICON_PNGS: &[(u32, &[u8])] = &[\n");
    for output in app_icons {
        let _ = writeln!(out, "    ({}, {}),", output.key, embed(&output.file));
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// Keyed by scale factor (1 or 2).\npub static SPLASH_PNGS: &[(u32, &[u8])] = &[\n",
    );
    for output in splash {
        let _ = writeln!(out, "    ({}, {}),", output.key, embed(&output.file));
    }
    out.push_str("];\n");
    out
}
