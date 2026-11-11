//! The Skia renderer: a raster [`surface`], fonts from given files only
//! ([`fonts`]), text shaped by Skia's paragraph layout ([`Typesetter`]),
//! [`png`] encoding and the [`SkiaPainter`] node types draw with, and the
//! skia-safe they're made of.

pub use skia_safe;

use skia_safe::textlayout::{
    FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle, TextAlign as SkiaAlign, TextDecoration, TextStyle,
    TypefaceFontProvider,
};
use skia_safe::{Data, EncodedImageFormat, FontMgr, FontStyle, Surface, font_style, surfaces};

use crate::model::{Color, Paint, Text, TextAlign, VerticalAlign};
use crate::render::{Painter, Rect};

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

/// Shapes and lays out text with the fonts a Design's text uses (made by
/// [`crate::fonts::Fonts::typesetter`]), for the layout pass to measure and
/// painters to draw alike.
pub struct Typesetter {
    fonts: FontCollection,
}

impl Typesetter {
    pub(crate) fn new(provider: TypefaceFontProvider) -> Self {
        let mut fonts = FontCollection::new();
        fonts.set_asset_font_manager(Some(provider.into()));
        fonts.disable_font_fallback();
        Self { fonts }
    }

    /// `text`'s size: its own on one line for each paragraph without a
    /// `width`, else wrapped at `width` and as tall as its lines.
    pub fn measure(&self, text: &Text, width: Option<f32>) -> [f32; 2] {
        let mut paragraph = self.paragraph(text, None);
        match width {
            Some(width) => {
                paragraph.layout(width.max(0.));
                [width, paragraph.height()]
            }
            None => {
                paragraph.layout(f32::INFINITY);
                // Up to a whole pixel, so laid out again at it, it doesn't wrap.
                [paragraph.max_intrinsic_width().ceil(), paragraph.height()]
            }
        }
    }

    /// `text` as a paragraph in `color`, not yet laid out.
    fn paragraph(&self, text: &Text, color: Option<Color>) -> Paragraph {
        let mut style = TextStyle::new();
        let families = crate::fonts::families(text);
        style
            .set_font_families(&families)
            .set_font_size(text.font_size())
            .set_font_style(FontStyle::new(
                font_style::Weight::from(i32::from(text.weight().0)),
                font_style::Width::NORMAL,
                font_style::Slant::Upright,
            ))
            .set_letter_spacing(text.letter_spacing);
        if let Some(line_height) = text.line_height {
            // Each line `line_height` times the size, the room split above
            // and below its characters, as CSS does.
            style.set_height_override(true).set_height(line_height).set_half_leading(true);
        }
        let mut decoration = TextDecoration::NO_DECORATION;
        if text.underline {
            decoration |= TextDecoration::UNDERLINE;
        }
        if text.strikethrough {
            decoration |= TextDecoration::LINE_THROUGH;
        }
        style.set_decoration_type(decoration);
        if let Some(Color { r, g, b, a }) = color {
            let color = skia_safe::Color::from_argb(a, r, g, b);
            style.set_color(color).set_decoration_color(color);
        }
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&style).set_text_align(match text.text_align {
            TextAlign::Left => SkiaAlign::Left,
            TextAlign::Center => SkiaAlign::Center,
            TextAlign::Right => SkiaAlign::Right,
            TextAlign::Justify => SkiaAlign::Justify,
        });
        let mut builder = ParagraphBuilder::new(&paragraph_style, self.fonts.clone());
        builder.push_style(&style);
        builder.add_text(&text.content);
        builder.build()
    }
}

/// The surface's pixels as a PNG file. Encoding takes about 100 ms for a
/// 1440 × 1024 image, ten times its drawing: encode only what is returned.
pub fn png(surface: &mut Surface) -> Option<Vec<u8>> {
    surface.peek_pixels()?.encode(EncodedImageFormat::PNG, None)
}

/// The [`Painter`] node types draw with, on a Skia canvas, setting text
/// with a [`Typesetter`].
pub struct SkiaPainter<'a> {
    canvas: &'a skia_safe::Canvas,
    typesetter: &'a Typesetter,
}

impl<'a> SkiaPainter<'a> {
    pub fn new(canvas: &'a skia_safe::Canvas, typesetter: &'a Typesetter) -> Self {
        Self { canvas, typesetter }
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

    fn text(&mut self, text: &Text, (width, height): (f32, f32)) {
        for paint in &text.fill.fill {
            let color = match *paint {
                Paint::Solid(color) => color,
            };
            let mut paragraph = self.typesetter.paragraph(text, Some(color));
            paragraph.layout(width);
            let top = match text.text_align_vertical {
                VerticalAlign::Top => 0.,
                VerticalAlign::Middle => (height - paragraph.height()) / 2.,
                VerticalAlign::Bottom => height - paragraph.height(),
            };
            paragraph.paint(self.canvas, (0., top));
        }
    }
}
