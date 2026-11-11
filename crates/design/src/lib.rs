//! Zeron Design's renderer: Designs drawn by Skia on the CPU raster backend
//! (apps/zgx/docs/design/adr/0002), shared by Zeron's engine (agents'
//! screenshots and exports) and its zgx app (the Design tab's tiles), so the
//! two draw alike.
//!
//! So far the parts every later one builds on: a raster [`surface`], fonts
//! from given files only ([`fonts`]) and [`png`] encoding.

pub use skia_safe;

use skia_safe::textlayout::{FontCollection, TypefaceFontProvider};
use skia_safe::{Data, EncodedImageFormat, FontMgr, Surface, surfaces};

/// A transparent raster surface of `width` × `height` pixels (RGBA,
/// premultiplied), or `None` for an empty or oversized one.
pub fn surface(width: i32, height: i32) -> Option<Surface> {
    surfaces::raster_n32_premul((width, height))
}

/// A font collection holding these font files and nothing of the
/// computer's: text lays out and draws the same on every computer, and a
/// family not given draws nothing rather than a system fallback. Files that
/// aren't fonts are skipped.
pub fn fonts(files: &[&[u8]]) -> FontCollection {
    let decoder = FontMgr::new();
    let mut provider = TypefaceFontProvider::new();
    for file in files {
        if let Some(typeface) = decoder.new_from_data(Data::new_copy(file), None) {
            provider.register_typeface(typeface, None);
        }
    }
    let mut collection = FontCollection::new();
    collection.set_asset_font_manager(Some(provider.into()));
    collection.disable_font_fallback();
    collection
}

/// The surface's pixels as a PNG file. Encoding takes about 100 ms for a
/// 1440 × 1024 frame, ten times its drawing: encode only what is returned.
pub fn png(surface: &mut Surface) -> Option<Vec<u8>> {
    surface.peek_pixels()?.encode(EncodedImageFormat::PNG, None)
}

/// Keeps the renderer in a binary that links this crate before anything in
/// it draws, so its size and link are what they will be. The engine and the
/// zgx app call it at start-up until their design tools call the renderer.
pub fn keep_linked() {
    std::hint::black_box((
        surface as fn(i32, i32) -> Option<Surface>,
        fonts as fn(&[&[u8]]) -> FontCollection,
        png as fn(&mut Surface) -> Option<Vec<u8>>,
    ));
}
