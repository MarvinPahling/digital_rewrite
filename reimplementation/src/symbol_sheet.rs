//! Visual smoke check for the symbol renderer (test builds only).
//!
//! The meshes the renderer hands to egui are rasterized with a tiny software triangle filler
//! (pixel-centre sampling, barycentric interpolation of the premultiplied vertex colors, source
//! over blending) into a sheet showing all ten style/kind bindings with their pin anchors.
//! The default tests only check that the sheet is inked; the ignored `write_symbol_sheet` test
//! writes `target/symbol-sheet.png` (git-ignored) for looking at, via `just symbol-sheet`.
use crate::{
    canvas::CanvasTransform,
    domain::ComponentKind,
    symbol_render::{PaintState, Palette, SymbolLayout, SymbolRenderer},
    symbols::SymbolStyle,
};
use eframe::egui::{Mesh, Pos2, Visuals};
use resvg::tiny_skia::Pixmap;

const CELL: [f32; 2] = [120.0, 100.0];
const KINDS: [ComponentKind; 5] = [
    ComponentKind::Input,
    ComponentKind::Output,
    ComponentKind::And,
    ComponentKind::Or,
    ComponentKind::Not,
];

/// Premultiplied RGBA, one `[f32; 4]` per pixel.
struct Image {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 4]>,
}

impl Image {
    fn new(width: usize, height: usize, background: [f32; 4]) -> Self {
        Self {
            width,
            height,
            pixels: vec![background; width * height],
        }
    }

    fn blend(&mut self, x: usize, y: usize, source: [f32; 4]) {
        let destination = &mut self.pixels[y * self.width + x];
        for (d, s) in destination.iter_mut().zip(source) {
            *d = s + *d * (1.0 - source[3]);
        }
    }

    fn fill_mesh(&mut self, mesh: &Mesh) {
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let v = triangle.map(|i| mesh.vertices[i as usize]);
            let [a, b, c] = v.map(|v| v.pos);
            let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
            if area == 0.0 {
                continue;
            }
            let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
            let max_x = (a.x.max(b.x).max(c.x).ceil().max(0.0) as usize).min(self.width);
            let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
            let max_y = (a.y.max(b.y).max(c.y).ceil().max(0.0) as usize).min(self.height);
            for y in min_y..max_y {
                for x in min_x..max_x {
                    let p = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((b.x - p.x) * (c.y - p.y) - (c.x - p.x) * (b.y - p.y)) / area;
                    let w1 = ((c.x - p.x) * (a.y - p.y) - (a.x - p.x) * (c.y - p.y)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let mut color = [0.0; 4];
                    for (weight, vertex) in [w0, w1, w2].into_iter().zip(v) {
                        let rgba = vertex.color.to_array();
                        for (out, channel) in color.iter_mut().zip(rgba) {
                            *out += weight * f32::from(channel) / 255.0;
                        }
                    }
                    self.blend(x, y, color);
                }
            }
        }
    }

    fn to_pixmap(&self) -> Pixmap {
        let mut pixmap = Pixmap::new(self.width as u32, self.height as u32).expect("sheet size");
        for (out, pixel) in pixmap
            .data_mut()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&self.pixels)
        {
            for (byte, channel) in out.iter_mut().zip(pixel) {
                *byte = (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
        pixmap
    }
}

/// Renders every binding (rows: rectangular, IEEE; columns: the five kinds) at `zoom` screen
/// pixels per world unit onto a light background. Returns the pixels and the inked pixel count
/// of each cell in row-major order.
fn render_sheet(zoom: f32) -> (Image, Vec<usize>) {
    let styles = [SymbolStyle::Rect, SymbolStyle::Ieee];
    let width = (CELL[0] * zoom) as usize * KINDS.len();
    let height = (CELL[1] * zoom) as usize * styles.len();
    let mut image = Image::new(width, height, [0.96, 0.96, 0.96, 1.0]);
    let palette = Palette::from_visuals(&Visuals::light());
    let transform = CanvasTransform::new(Pos2::ZERO, zoom).expect("zoom");
    let mut renderer = SymbolRenderer::default();
    let mut layouts = Vec::new();
    for (row, style) in styles.into_iter().enumerate() {
        for (column, kind) in KINDS.into_iter().enumerate() {
            let center = [
                CELL[0] * (column as f32 + 0.5),
                CELL[1] * (row as f32 + 0.5),
            ];
            let layout = SymbolLayout::centered_on(style, kind, center).expect("symbol");
            // Ink in black rather than the theme's grey so thin strokes are easy to judge.
            let mesh = renderer
                .mesh(
                    &layout,
                    &transform,
                    1.0,
                    palette.color(PaintState::Ink).to_opaque(),
                )
                .expect("tessellation");
            image.fill_mesh(&mesh);
            layouts.push(layout);
        }
    }
    // Pin anchors as small red squares, drawn last.
    for layout in &layouts {
        for anchor in layout.anchors() {
            let p = transform.to_screen(anchor.position);
            for dy in -1..=1_i32 {
                for dx in -1..=1_i32 {
                    let (x, y) = ((p.x as i32 + dx) as usize, (p.y as i32 + dy) as usize);
                    if x < image.width && y < image.height {
                        image.blend(x, y, [0.8, 0.0, 0.0, 0.8]);
                    }
                }
            }
        }
    }
    let cell_w = (CELL[0] * zoom) as usize;
    let cell_h = (CELL[1] * zoom) as usize;
    let mut inked = vec![0; styles.len() * KINDS.len()];
    for y in 0..image.height {
        for x in 0..image.width {
            let cell = (y / cell_h) * KINDS.len() + x / cell_w;
            let pixel = image.pixels[y * image.width + x];
            if pixel[0] < 0.5 && pixel[1] < 0.5 {
                inked[cell] += 1;
            }
        }
    }
    (image, inked)
}

#[test]
fn every_binding_is_inked_in_the_rasterized_sheet() {
    let (_, inked) = render_sheet(3.0);
    assert_eq!(inked.len(), 10);
    for (cell, count) in inked.iter().enumerate() {
        assert!(*count > 100, "cell {cell} has only {count} ink pixels");
    }
}

/// `just symbol-sheet`: writes `target/symbol-sheet.png`.
#[test]
#[ignore = "developer preview; run with `just symbol-sheet`"]
fn write_symbol_sheet() {
    let (image, _) = render_sheet(4.0);
    let png = image.to_pixmap().encode_png().expect("PNG encoding");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/symbol-sheet.png");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("target directory");
    std::fs::write(&path, png).expect("write sheet");
    println!("wrote {}", path.display());
}
