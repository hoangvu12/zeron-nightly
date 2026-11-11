//! The Skia renderer: a raster [`surface`] (or pixels of one's own,
//! [`draw_into`]), fonts from given files only
//! ([`fonts`]), [`png`] encoding and the [`SkiaPainter`] node types draw
//! with, and the skia-safe they're made of.

pub use skia_safe;

use skia_safe::canvas::SaveLayerRec;
use skia_safe::paint::{Cap as PaintCap, Join as PaintJoin, Style as PaintStyle};
use skia_safe::textlayout::{FontCollection, TypefaceFontProvider};
use skia_safe::{
    AlphaType, BlurStyle, ClipOp, Color4f, ColorType, Data, EncodedImageFormat, FontMgr,
    ImageInfo, MaskFilter, Matrix, PathBuilder, PathFillType, Point, RRect, Surface, TileMode,
    Vector, dash_path_effect, gradient, image_filters, surfaces,
};

use crate::model::{
    BlendMode, Cap, Color, Gradient, GradientShape, Join, Paint, PaintKind, Shadow,
    Sides, StrokeAlign, StrokeStyle,
};
use crate::render::{Outline, Painter, Rect};

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

/// A blur's sigma for its radius, as CSS takes a blur's radius.
fn sigma(radius: f32) -> f32 {
    radius / 2.
}

/// The outline moved in by `sides` (out, where they're negative), its
/// corners' radii shrinking (growing) with it; a square corner stays square.
fn inset(outline: &Outline, sides: Sides) -> RRect {
    let Outline::Rounded { rect: bounds, radii } = *outline;
    let Sides { top, right, bottom, left } = sides;
    let moved = skia_safe::Rect::from_ltrb(
        bounds.x + left,
        bounds.y + top,
        (bounds.x + bounds.width - right).max(bounds.x + left),
        (bounds.y + bounds.height - bottom).max(bounds.y + top),
    );
    let corner = |r: f32, dx: f32, dy: f32| {
        if r <= 0. {
            Vector::new(0., 0.)
        } else {
            Vector::new((r - dx).max(0.), (r - dy).max(0.))
        }
    };
    RRect::new_rect_radii(
        moved,
        &[
            corner(radii[0], left, top),
            corner(radii[1], right, top),
            corner(radii[2], right, bottom),
            corner(radii[3], left, bottom),
        ],
    )
}

fn rrect(outline: &Outline) -> RRect {
    inset(outline, Sides::default())
}

/// The area between two rounded rectangles, `inner` within `outer`.
fn ring(outer: &RRect, inner: &RRect) -> skia_safe::Path {
    let mut path = PathBuilder::new_with_fill_type(PathFillType::EvenOdd);
    path.add_rrect(outer, None, None).add_rrect(inner, None, None);
    path.detach()
}

fn color(Color { r, g, b, a }: Color) -> skia_safe::Color {
    skia_safe::Color::from_argb(a, r, g, b)
}

fn blend_mode(mode: BlendMode) -> skia_safe::BlendMode {
    use skia_safe::BlendMode as Sk;
    match mode {
        BlendMode::Normal => Sk::SrcOver,
        BlendMode::Darken => Sk::Darken,
        BlendMode::Multiply => Sk::Multiply,
        BlendMode::ColorBurn => Sk::ColorBurn,
        BlendMode::Lighten => Sk::Lighten,
        BlendMode::Screen => Sk::Screen,
        BlendMode::ColorDodge => Sk::ColorDodge,
        BlendMode::LinearDodge => Sk::Plus,
        BlendMode::Overlay => Sk::Overlay,
        BlendMode::SoftLight => Sk::SoftLight,
        BlendMode::HardLight => Sk::HardLight,
        BlendMode::Difference => Sk::Difference,
        BlendMode::Exclusion => Sk::Exclusion,
        BlendMode::Hue => Sk::Hue,
        BlendMode::Saturation => Sk::Saturation,
        BlendMode::Color => Sk::Color,
        BlendMode::Luminosity => Sk::Luminosity,
    }
}

/// Skia's paint for one of ours over `bounds`, or `None` for one not drawn.
fn skia_paint(paint: &Paint, bounds: Rect) -> Option<skia_safe::Paint> {
    if !paint.enabled {
        return None;
    }
    let mut out = skia_safe::Paint::default();
    out.set_anti_alias(true);
    out.set_blend_mode(blend_mode(paint.blend));
    match &paint.kind {
        PaintKind::Solid(solid) => {
            out.set_color(color(*solid));
        }
        PaintKind::Gradient(gradient) => {
            out.set_shader(gradient_shader(gradient, bounds)?);
        }
        PaintKind::Unsupported(_) => return None,
    }
    out.set_alpha_f(out.alpha_f() * paint.opacity);
    Some(out)
}

/// The shader for `gradient` laid over `bounds` (see [`Gradient`]).
fn gradient_shader(gradient: &Gradient, bounds: Rect) -> Option<skia_safe::Shader> {
    let colors: Vec<Color4f> = gradient.stops.iter().map(|stop| color(stop.color).into()).collect();
    let positions: Vec<f32> = gradient.stops.iter().map(|stop| stop.position).collect();
    let spec = gradient::Gradient::new(
        gradient::Colors::new(&colors, Some(&positions), TileMode::Clamp, None),
        gradient::Interpolation::default(),
    );
    let (w, h) = (bounds.width, bounds.height);
    let center = Point::new(bounds.x + gradient.center.0 * w, bounds.y + gradient.center.1 * h);
    let (width, height) = (gradient.size.0 * w, gradient.size.1 * h);
    match gradient.shape {
        GradientShape::Linear => {
            // Upwards at 0, turning counter-clockwise, in the box's own
            // proportions.
            let (turn, length) = (gradient.rotation.to_radians(), gradient.size.1);
            let half = Vector::new(-turn.sin() * length * w / 2., -turn.cos() * length * h / 2.);
            gradient::shaders::linear_gradient((center - half, center + half), &spec, None)
        }
        GradientShape::Radial | GradientShape::Angular => {
            if width <= 0. || height <= 0. {
                return None;
            }
            // A unit circle at the origin, stretched to the ellipse.
            let mut matrix = Matrix::translate(center);
            matrix.pre_rotate(-gradient.rotation, None);
            matrix.pre_scale((width / 2., height / 2.), None);
            if gradient.shape == GradientShape::Radial {
                gradient::shaders::radial_gradient(((0., 0.), 1.), &spec, &matrix)
            } else {
                // From the top, clockwise.
                matrix.pre_rotate(-90., None);
                gradient::shaders::sweep_gradient((0., 0.), (0., 360.), &spec, &matrix)
            }
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

    fn rotate(&mut self, degrees: f32) {
        self.canvas.rotate(degrees, None);
    }

    fn flip(&mut self, horizontal: bool, vertical: bool) {
        let sign = |flipped: bool| if flipped { -1. } else { 1. };
        self.canvas.scale((sign(horizontal), sign(vertical)));
    }

    fn save_layer(&mut self, opacity: f32, blur: f32) {
        let mut layer = skia_safe::Paint::default();
        layer.set_alpha_f(opacity);
        if blur > 0. {
            layer.set_image_filter(image_filters::blur((sigma(blur), sigma(blur)), None, None, None));
        }
        self.canvas.save_layer(&SaveLayerRec::default().paint(&layer));
    }

    fn clip_rect(&mut self, bounds: Rect) {
        self.canvas.clip_rect(rect(bounds), None, true);
    }

    fn clip(&mut self, outline: &Outline) {
        self.canvas.clip_rrect(rrect(outline), None, true);
    }

    fn fill(&mut self, outline: &Outline, paints: &[Paint]) {
        let shape = rrect(outline);
        for paint in paints {
            if let Some(fill) = skia_paint(paint, outline.bounds()) {
                self.canvas.draw_rrect(shape, &fill);
            }
        }
    }

    fn stroke(&mut self, outline: &Outline, stroke: &StrokeStyle) {
        let thickness = stroke.thickness;
        let Some(width) = thickness.uniform() else {
            // A width per side: the ring between the outline moved out and
            // in by them, as the alignment splits them.
            let (outer, inner) = match stroke.align {
                StrokeAlign::Inside => (Sides::default(), thickness),
                StrokeAlign::Center => (thickness.scaled(-0.5), thickness.scaled(0.5)),
                StrokeAlign::Outside => (thickness.scaled(-1.), Sides::default()),
            };
            let ring = ring(&inset(outline, outer), &inset(outline, inner));
            for paint in &stroke.fill {
                if let Some(fill) = skia_paint(paint, outline.bounds()) {
                    self.canvas.draw_path(&ring, &fill);
                }
            }
            return;
        };
        if width <= 0. {
            return;
        }
        let shape = rrect(outline);
        // Inside and outside are a centred line twice as wide, cut to the
        // outline's one side.
        self.canvas.save();
        let width = match stroke.align {
            StrokeAlign::Center => width,
            StrokeAlign::Inside => {
                self.canvas.clip_rrect(shape, ClipOp::Intersect, true);
                width * 2.
            }
            StrokeAlign::Outside => {
                self.canvas.clip_rrect(shape, ClipOp::Difference, true);
                width * 2.
            }
        };
        for paint in &stroke.fill {
            let Some(mut line) = skia_paint(paint, outline.bounds()) else {
                continue;
            };
            line.set_style(PaintStyle::Stroke)
                .set_stroke_width(width)
                .set_stroke_join(match stroke.join {
                    Join::Miter => PaintJoin::Miter,
                    Join::Round => PaintJoin::Round,
                    Join::Bevel => PaintJoin::Bevel,
                })
                .set_stroke_cap(match stroke.cap {
                    Cap::None => PaintCap::Butt,
                    Cap::Round => PaintCap::Round,
                    Cap::Square => PaintCap::Square,
                });
            if !stroke.dashes.is_empty() {
                // Skia takes an even count of lengths, as SVG repeats an odd one.
                let mut dashes = stroke.dashes.clone();
                if dashes.len() % 2 == 1 {
                    dashes.extend_from_within(..);
                }
                line.set_path_effect(dash_path_effect::new(&dashes, 0.));
            }
            self.canvas.draw_rrect(shape, &line);
        }
        self.canvas.restore();
    }

    fn shadow(&mut self, outline: &Outline, shadow: &Shadow) {
        let mut paint = skia_safe::Paint::default();
        paint
            .set_anti_alias(true)
            .set_color(color(shadow.color))
            .set_blend_mode(blend_mode(shadow.blend));
        if shadow.blur > 0. {
            paint.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, sigma(shadow.blur), true));
        }
        let (dx, dy) = shadow.offset;
        if !shadow.inner {
            let mut shape = inset(outline, Sides::all(-shadow.spread));
            shape.offset((dx, dy));
            self.canvas.draw_rrect(shape, &paint);
            return;
        }
        // Inside the outline, the shade around a hole: the outline moved
        // in by the spread and over by the offset.
        let shape = rrect(outline);
        let mut hole = inset(outline, Sides::all(shadow.spread));
        hole.offset((dx, dy));
        let reach = shadow.blur * 2. + dx.abs() + dy.abs() + shadow.spread.abs() + 1.;
        let mut around = *shape.rect();
        around.outset((reach, reach));
        let mut path = PathBuilder::new_with_fill_type(PathFillType::EvenOdd);
        path.add_rect(around, None, None).add_rrect(hole, None, None);
        self.canvas.save();
        self.canvas.clip_rrect(shape, ClipOp::Intersect, true);
        self.canvas.draw_path(&path.detach(), &paint);
        self.canvas.restore();
    }

    fn background_blur(&mut self, outline: &Outline, radius: f32) {
        if radius <= 0. {
            return;
        }
        let Some(blur) = image_filters::blur((sigma(radius), sigma(radius)), TileMode::Clamp, None, None) else {
            return;
        };
        self.canvas.save();
        self.canvas.clip_rrect(rrect(outline), ClipOp::Intersect, true);
        self.canvas.save_layer(
            &SaveLayerRec::default()
                .backdrop(&blur)
                .backdrop_tile_mode(TileMode::Clamp),
        );
        self.canvas.restore();
        self.canvas.restore();
    }
}
