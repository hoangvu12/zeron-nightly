//! The Skia renderer: a raster [`surface`], fonts from given files only
//! ([`fonts`]), [`png`] encoding and the [`SkiaPainter`] node types draw
//! with, and the skia-safe they're made of.

pub use skia_safe;

use skia_safe::matrix::ScaleToFit;
use skia_safe::textlayout::{FontCollection, TypefaceFontProvider};
use skia_safe::{
    Data, EncodedImageFormat, FontMgr, Matrix, PathBuilder, PathFillType, Point, Surface, surfaces,
    svg,
};

use crate::icons::Icons;
use crate::model::{Color, FillRule, Paint};
use crate::render::{Painter, Rect, Shape};

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
/// 1440 × 1024 image, ten times its drawing: encode only what is returned.
pub fn png(surface: &mut Surface) -> Option<Vec<u8>> {
    surface.peek_pixels()?.encode(EncodedImageFormat::PNG, None)
}

/// SVG path data as a Skia path, or `None` when it isn't path data.
pub fn svg_path(geometry: &str) -> Option<skia_safe::Path> {
    skia_safe::utils::parse_path::from_svg(geometry)
}

/// The [`Painter`] node types draw with, on a Skia canvas.
pub struct SkiaPainter<'a> {
    canvas: &'a skia_safe::Canvas,
    icons: Option<&'a Icons>,
}

impl<'a> SkiaPainter<'a> {
    pub fn new(canvas: &'a skia_safe::Canvas) -> Self {
        Self { canvas, icons: None }
    }

    /// Draws icons from `icons` (without, icons draw nothing).
    pub fn with_icons(mut self, icons: &'a Icons) -> Self {
        self.icons = Some(icons);
        self
    }
}

fn rect(rect: Rect) -> skia_safe::Rect {
    skia_safe::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
}

/// Each paint as a Skia fill.
fn fills(paints: &[Paint]) -> impl Iterator<Item = skia_safe::Paint> + '_ {
    paints.iter().map(|paint| {
        let mut fill = skia_safe::Paint::default();
        fill.set_anti_alias(true);
        match *paint {
            Paint::Solid(Color { r, g, b, a }) => {
                fill.set_color(skia_safe::Color::from_argb(a, r, g, b));
            }
        }
        fill
    })
}

fn outline(shape: Shape<'_>) -> Option<skia_safe::Path> {
    match shape {
        Shape::Ellipse { rect: bounds, start, sweep, inner } => {
            let outer = rect(bounds);
            let inner = inner.clamp(0., 1.);
            let hole = (inner > 0.).then(|| {
                let (dx, dy) = (outer.width() * (1. - inner) / 2., outer.height() * (1. - inner) / 2.);
                outer.with_inset((dx, dy))
            });
            let mut path = PathBuilder::new();
            if sweep.abs() >= 360. {
                path.add_oval(outer, None, None);
                if let Some(hole) = hole {
                    path.add_oval(hole, None, None);
                    path.set_fill_type(PathFillType::EvenOdd);
                }
            } else {
                match hole {
                    None => {
                        path.move_to(outer.center());
                        path.arc_to(outer, start, sweep, false);
                    }
                    Some(hole) => {
                        path.arc_to(outer, start, sweep, true);
                        path.arc_to(hole, start + sweep, -sweep, false);
                    }
                }
                path.close();
            }
            Some(path.detach())
        }
        Shape::Polygon(corners) => {
            let corners: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
            Some(PathBuilder::new().add_polygon(&corners, true).detach())
        }
        Shape::Path { geometry, view_box, rect: bounds, fill_rule } => {
            let path = svg_path(geometry)?;
            let from = view_box.map(rect).unwrap_or_else(|| path.compute_tight_bounds());
            let onto = Matrix::rect_2_rect(from, rect(bounds), ScaleToFit::Fill)?;
            let fill_type = match fill_rule {
                FillRule::Nonzero => PathFillType::Winding,
                FillRule::Evenodd => PathFillType::EvenOdd,
            };
            Some(path.make_transform(&onto).with_fill_type(fill_type))
        }
    }
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
        for fill in fills(paints) {
            self.canvas.draw_rect(rect(bounds), &fill);
        }
    }

    fn fill_shape(&mut self, shape: Shape<'_>, paints: &[Paint]) {
        let Some(path) = outline(shape) else {
            return;
        };
        for fill in fills(paints) {
            self.canvas.draw_path(&path, &fill);
        }
    }

    fn icon(&mut self, icon: &str, weight: Option<u16>, color: Color, bounds: Rect) {
        let Some(svg) = self.icons.and_then(|icons| icons.svg(icon, weight).ok()) else {
            return;
        };
        let svg = crate::icons::prepare(&svg, color, weight, bounds.width, bounds.height);
        let Ok(dom) = svg::Dom::from_str(svg, FontMgr::empty()) else {
            return;
        };
        let before = self.canvas.save();
        self.canvas.translate((bounds.x, bounds.y));
        if color.a < 255 {
            self.canvas.save_layer_alpha(None, color.a.into());
        }
        dom.render(self.canvas);
        self.canvas.restore_to_count(before);
    }
}
