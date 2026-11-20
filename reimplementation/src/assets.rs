//! Build-time rasterized icon and brand PNGs (see `build.rs` and `assets/icons/manifest.toml`).

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Returns the PNG for UI icon `id` at `px` pixels square (24 or 48).
pub fn icon_png(id: &str, px: u32) -> Option<&'static [u8]> {
    ICON_PNGS
        .iter()
        .find(|(icon_id, icon_px, _)| *icon_id == id && *icon_px == px)
        .map(|(_, _, bytes)| *bytes)
}

/// Returns the application icon PNG at `px` pixels square (16, 32, 48, 64, 128, 256 or 512).
pub fn app_icon_png(px: u32) -> Option<&'static [u8]> {
    APP_ICON_PNGS
        .iter()
        .find(|(size, _)| *size == px)
        .map(|(_, bytes)| *bytes)
}

/// Returns the splash PNG at scale 1 (512x320) or 2 (1024x640).
pub fn splash_png(scale: u32) -> Option<&'static [u8]> {
    SPLASH_PNGS
        .iter()
        .find(|(entry_scale, _)| *entry_scale == scale)
        .map(|(_, bytes)| *bytes)
}

/// Returns the ids of all UI icons in manifest order.
pub fn icon_ids() -> &'static [&'static str] {
    ICON_IDS
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_ICON_SIZES: [u32; 7] = [512, 256, 128, 64, 48, 32, 16];
    /// Size floors that separate rendered artwork from a blank (fully transparent or
    /// single-colour) image of the same dimensions. A blank PNG deflates to almost
    /// nothing: measured 75-126 bytes for 16-48 px, about 700-1200 bytes at 512x320
    /// and about 2.6-3.6 KB at 1024x640 (a blank 128 px square is a few hundred bytes).
    /// The smallest real outputs measured when these were chosen were 183 bytes for a
    /// 24 px icon, 511 bytes for the 16 px app icon, 2.7 KB for the 128 px app icon and
    /// 7.6 KB for the 512x320 splash; each floor sits between the two.
    const MIN_ICON_BYTES: usize = 140;
    const MIN_SMALL_APP_ICON_BYTES: usize = 300;
    const MIN_LARGE_APP_ICON_BYTES: usize = 1000;
    const MIN_SPLASH_BYTES: usize = 5000;

    fn dimensions(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
        assert_eq!(&png[12..16], b"IHDR", "first chunk is IHDR");
        let read = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
        (read(16), read(20))
    }

    #[test]
    fn manifest_lists_all_icons() {
        assert_eq!(icon_ids().len(), 25);
    }

    #[test]
    fn every_icon_has_both_sizes_with_content() {
        for id in icon_ids() {
            for px in [24, 48] {
                let png = icon_png(id, px).unwrap_or_else(|| panic!("{id} at {px}px missing"));
                assert_eq!(dimensions(png), (px, px), "{id} at {px}px");
                assert!(
                    png.len() >= MIN_ICON_BYTES,
                    "{id} at {px}px looks blank ({} bytes)",
                    png.len()
                );
            }
        }
        assert!(icon_png("no-such-icon", 24).is_none());
        assert!(icon_png("file-new", 32).is_none());
    }

    #[test]
    fn app_icon_has_all_sizes_with_content() {
        for px in APP_ICON_SIZES {
            let png = app_icon_png(px).unwrap_or_else(|| panic!("app icon {px}px missing"));
            assert_eq!(dimensions(png), (px, px), "app icon {px}px");
            let floor = if px <= 64 {
                MIN_SMALL_APP_ICON_BYTES
            } else {
                MIN_LARGE_APP_ICON_BYTES
            };
            assert!(
                png.len() >= floor,
                "app icon {px}px looks blank ({} bytes)",
                png.len()
            );
        }
        assert!(app_icon_png(20).is_none());
    }

    #[test]
    fn splash_has_both_scales_with_content() {
        for (scale, size) in [(1, (512, 320)), (2, (1024, 640))] {
            let png = splash_png(scale).unwrap_or_else(|| panic!("splash @{scale}x missing"));
            assert_eq!(dimensions(png), size, "splash @{scale}x");
            assert!(
                png.len() >= MIN_SPLASH_BYTES,
                "splash @{scale}x looks blank ({} bytes)",
                png.len()
            );
        }
        assert!(splash_png(3).is_none());
    }
}
