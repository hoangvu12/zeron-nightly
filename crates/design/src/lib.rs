//! Zeron Design's renderer: Designs drawn by Skia on the CPU raster backend
//! (apps/zgx/docs/design/adr/0002), shared by Zeron's engine (agents'
//! screenshots and exports) and its zgx app (the Design tab's tiles), so the
//! two draw alike.
//!
//! So far only [`skia`], the parts of the Skia renderer every later one
//! builds on. The rest of the crate is to reach it through a renderer
//! interface (ADR-0002), so a tiny-skia renderer could replace it.

pub mod skia;

use skia_safe::Surface;
use skia_safe::textlayout::FontCollection;

/// Keeps Skia's raster, text and PNG code in a binary that links this crate
/// before anything in it draws, so the binary's size and link are close to
/// what they will be. The engine and the zgx app call it at start-up until
/// their design tools call the renderer.
pub fn keep_linked() {
    std::hint::black_box((
        skia::surface as fn(i32, i32) -> Option<Surface>,
        skia::fonts as fn(&[&[u8]]) -> FontCollection,
        skia::png as fn(&mut Surface) -> Option<Vec<u8>>,
    ));
}
