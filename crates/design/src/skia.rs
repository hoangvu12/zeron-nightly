//! The Skia renderer: a raster [`surface`] (or pixels of one's own,
//! [`draw_into`]), fonts from given files only
//! ([`fonts`]), [`png`] encoding and the [`SkiaPainter`] node types draw
//! with, and the skia-safe they're made of.

pub use skia_safe;

use skia_safe::textlayout::{FontCollection, TypefaceFontProvider};
use skia_safe::{AlphaType, ColorType, Data, EncodedImageFormat, FontMgr, ImageInfo, Surface, surfaces};

use crate::model::{Color, Paint};
use crate::render::{Painter, Rect};

/// A transparent raster surface of `width` × `height` pixels (RGBA,
/// premultiplied), or `None` for an empty or oversized one.
pub fn surface(width: i32, height: i32) -> Option<Surface> {
    surfaces::raster_n32_premul((width, height))
}

/// Draws with `draw` into `pixels`, `width` × `height` RGBA
/// (premultiplied), as they are: `None` when they don't make such an image.
pub fn draw_into(pixels: &mut [u8], width: u32, height: u32, draw: impl FnOnce(&skia_safe::Canvas)) -> Option<()> {
    let info = ImageInfo::new(
        (i32::try_from(width).ok()?, i32::try_from(height).ok()?),
        ColorType::RGBA8888,
        AlphaType::Premul,
        None,
    );
    let row = width as usize * 4;
    let mut surface = surfaces::wrap_pixels(&info, pixels, row, None)?;
    draw(surface.canvas());
    Some(())
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
/// 1440 × 1024 image, ten times its drawing: encode only what is returned.
pub fn png(surface: &mut Surface) -> Option<Vec<u8>> {
    surface.peek_pixels()?.encode(EncodedImageFormat::PNG, None)
}

/// The [`Painter`] node types draw with, on a Skia canvas.
pub struct SkiaPainter<'a> {
    canvas: &'a skia_safe::Canvas,
}

impl<'a> SkiaPainter<'a> {
    pub fn new(canvas: &'a skia_safe::Canvas) -> Self {
        Self { canvas }
    }
}

fn rect(rect: Rect) -> skia_safe::Rect {
    skia_safe::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
}

impl Painter for SkiaPainter<'_> {
    fn save(&mut self) {
        self.canvas.save();
    }

    fn restore(&mut self) {
        self.canvas.restore();
    }

    fn translate(&mut self, x: f32, y: f32) {
        self.canvas.translate((x, y));
    }

    fn scale(&mut self, factor: f32) {
        self.canvas.scale((factor, factor));
    }

    fn clip_rect(&mut self, bounds: Rect) {
        self.canvas.clip_rect(rect(bounds), None, true);
    }

    fn fill_rect(&mut self, bounds: Rect, paints: &[Paint]) {
        for paint in paints {
            let mut fill = skia_safe::Paint::default();
            fill.set_anti_alias(true);
            match *paint {
                Paint::Solid(Color { r, g, b, a }) => {
                    fill.set_color(skia_safe::Color::from_argb(a, r, g, b));
                }
            }
            self.canvas.draw_rect(rect(bounds), &fill);
        }
    }
}
