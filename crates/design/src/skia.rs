//! The Skia renderer: a raster [`surface`] (or pixels of one's own,
//! [`draw_into`]), fonts from given files only ([`fonts`]), text shaped by
//! Skia's paragraph layout ([`Typesetter`]), [`png`] encoding and the
//! [`SkiaPainter`] node types draw with, and the skia-safe they're made of.

pub use skia_safe;

use skia_safe::canvas::SaveLayerRec;
use skia_safe::matrix::ScaleToFit;
use skia_safe::paint::{Cap as PaintCap, Join as PaintJoin, Style as PaintStyle};
use skia_safe::textlayout::{
    FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle, TextAlign as SkiaAlign, TextDecoration, TextStyle,
    TypefaceFontProvider,
};
use skia_safe::{
    AlphaType, BlurStyle, ClipOp, Color4f, ColorType, Data, EncodedImageFormat, FontMgr, FontStyle,
    ImageInfo, MaskFilter, Matrix, PathBuilder, PathFillType, Point, RRect, Surface, TileMode,
    Vector, dash_path_effect, font_style, gradient, image_filters, surfaces, svg,
};

use crate::assets::{AssetKey, Assets};
use crate::model::{
    BlendMode, Cap, Color, FillRule, Gradient, GradientShape, Join, Paint, PaintKind, Shadow,
    Sides, StrokeAlign, StrokeStyle, Text, TextAlign, VerticalAlign,
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

    /// `text` as a paragraph drawn with `foreground`, not yet laid out.
    fn paragraph(&self, text: &Text, foreground: Option<&skia_safe::Paint>) -> Paragraph {
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
        if let Some(foreground) = foreground {
            // (A gradient's lines take its paint's own colour.)
            style.set_foreground_paint(foreground).set_decoration_color(foreground.color());
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

/// SVG path data as a Skia path, or `None` when it isn't path data.
pub fn svg_path(geometry: &str) -> Option<skia_safe::Path> {
    skia_safe::utils::parse_path::from_svg(geometry)
}

/// The [`Painter`] node types draw with, on a Skia canvas, setting text
/// with a [`Typesetter`].
pub struct SkiaPainter<'a> {
    canvas: &'a skia_safe::Canvas,
    typesetter: &'a Typesetter,
    assets: Option<&'a dyn Assets>,
}

impl<'a> SkiaPainter<'a> {
    pub fn new(canvas: &'a skia_safe::Canvas, typesetter: &'a Typesetter) -> Self {
        Self { canvas, typesetter, assets: None }
    }

    /// Draws icons and other assets from `assets` (without, they draw
    /// nothing).
    pub fn with_assets(mut self, assets: &'a dyn Assets) -> Self {
        self.assets = Some(assets);
        self
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
/// Only rounded rectangles are drawn this way; another shape is its bounds.
fn inset(outline: &Outline, sides: Sides) -> RRect {
    let (bounds, radii) = match *outline {
        Outline::Rounded { rect, radii } => (rect, radii),
        _ => (outline.bounds(), [0.; 4]),
    };
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

/// An outline as Skia draws it: a rounded rectangle as one, any other
/// shape as a path.
enum Shape {
    Rounded(RRect),
    Path(skia_safe::Path),
}

fn shape(outline: &Outline) -> Shape {
    match *outline {
        Outline::Rounded { .. } => Shape::Rounded(rrect(outline)),
        Outline::Ellipse { rect: bounds, start, sweep, inner } => {
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
            Shape::Path(path.detach())
        }
        Outline::Polygon { rect, sides } => {
            let corners: Vec<Point> = crate::render::polygon(rect, sides)
                .into_iter()
                .map(|(x, y)| Point::new(x, y))
                .collect();
            Shape::Path(PathBuilder::new().add_polygon(&corners, true).detach())
        }
        Outline::Path { rect: bounds, geometry, view_box, fill_rule } => {
            // Geometry that can't be read or placed draws nothing.
            let placed = svg_path(geometry).and_then(|path| {
                let from = view_box.map(rect).unwrap_or_else(|| path.compute_tight_bounds());
                let onto = Matrix::rect_2_rect(from, rect(bounds), ScaleToFit::Fill)?;
                Some(path.make_transform(&onto))
            });
            let fill_type = match fill_rule {
                FillRule::Nonzero => PathFillType::Winding,
                FillRule::Evenodd => PathFillType::EvenOdd,
            };
            Shape::Path(placed.unwrap_or_default().with_fill_type(fill_type))
        }
    }
}

impl Shape {
    fn clip(&self, canvas: &skia_safe::Canvas, op: ClipOp) {
        match self {
            Shape::Rounded(rrect) => canvas.clip_rrect(rrect, op, true),
            Shape::Path(path) => canvas.clip_path(path, op, true),
        };
    }

    fn draw(&self, canvas: &skia_safe::Canvas, paint: &skia_safe::Paint) {
        match self {
            Shape::Rounded(rrect) => canvas.draw_rrect(rrect, paint),
            Shape::Path(path) => canvas.draw_path(path, paint),
        };
    }
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
        shape(outline).clip(self.canvas, ClipOp::Intersect);
    }

    fn fill(&mut self, outline: &Outline, paints: &[Paint]) {
        let shape = shape(outline);
        for paint in paints {
            if let Some(fill) = skia_paint(paint, outline.bounds()) {
                shape.draw(self.canvas, &fill);
            }
        }
    }

    fn stroke(&mut self, outline: &Outline, stroke: &StrokeStyle) {
        let thickness = stroke.thickness;
        let shape = shape(outline);
        // A shape other than a rectangle has no sides: its widest is its width.
        let width = match shape {
            Shape::Rounded(_) => thickness.uniform(),
            Shape::Path(_) => {
                let Sides { top, right, bottom, left } = thickness;
                Some(top.max(right).max(bottom).max(left))
            }
        };
        let Some(width) = width else {
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
        // Inside and outside are a centred line twice as wide, cut to the
        // outline's one side.
        self.canvas.save();
        let width = match stroke.align {
            StrokeAlign::Center => width,
            StrokeAlign::Inside => {
                shape.clip(self.canvas, ClipOp::Intersect);
                width * 2.
            }
            StrokeAlign::Outside => {
                shape.clip(self.canvas, ClipOp::Difference);
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
            shape.draw(self.canvas, &line);
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
        if let Shape::Path(path) = shape(outline) {
            self.path_shadow(&path, shadow, paint);
            return;
        }
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
        shape(outline).clip(self.canvas, ClipOp::Intersect);
        self.canvas.save_layer(
            &SaveLayerRec::default()
                .backdrop(&blur)
                .backdrop_tile_mode(TileMode::Clamp),
        );
        self.canvas.restore();
        self.canvas.restore();
    }

    fn text(&mut self, text: &Text, (width, height): (f32, f32)) {
        for paint in &text.fill.fill {
            // Gradients laid over the text's box.
            let Some(foreground) = skia_paint(paint, Rect::sized((width, height))) else {
                continue;
            };
            let mut paragraph = self.typesetter.paragraph(text, Some(&foreground));
            paragraph.layout(width);
            let top = match text.text_align_vertical {
                VerticalAlign::Top => 0.,
                VerticalAlign::Middle => (height - paragraph.height()) / 2.,
                VerticalAlign::Bottom => height - paragraph.height(),
            };
            // Skia puts half of each letter's spacing before it; CSS (and
            // so an export) puts it all after.
            paragraph.paint(self.canvas, (-text.letter_spacing / 2., top));
        }
    }

    fn icon(&mut self, icon: &str, weight: Option<u16>, color: Color, bounds: Rect) {
        let Some(svg) = crate::icons::name(icon, weight)
            .ok()
            .and_then(|name| self.assets?.get(&AssetKey::icon(name)))
        else {
            return;
        };
        let Ok(svg) = std::str::from_utf8(&svg) else {
            return;
        };
        let svg = crate::icons::prepare(svg, color, weight, bounds.width, bounds.height);
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

impl SkiaPainter<'_> {
    /// A shadow of a shape other than a rectangle: the spread grows it by
    /// a line around it, as a path has no sides to move.
    fn path_shadow(&mut self, path: &skia_safe::Path, shadow: &Shadow, mut paint: skia_safe::Paint) {
        let (dx, dy) = shadow.offset;
        let moved = path.make_transform(&Matrix::translate((dx, dy)));
        let spread = shadow.spread.max(0.);
        if !shadow.inner {
            if spread > 0. {
                paint.set_style(PaintStyle::StrokeAndFill).set_stroke_width(spread * 2.);
            }
            self.canvas.draw_path(&moved, &paint);
            return;
        }
        // Inside the shape, the shade around it moved over by the offset.
        let reach = shadow.blur * 2. + dx.abs() + dy.abs() + spread + 1.;
        let mut around = path.compute_tight_bounds();
        around.outset((reach, reach));
        let mut shade = PathBuilder::new_with_fill_type(PathFillType::EvenOdd);
        shade.add_rect(around, None, None).add_path(&moved, None);
        self.canvas.save();
        self.canvas.clip_path(path, ClipOp::Intersect, true);
        self.canvas.draw_path(&shade.detach(), &paint);
        if spread > 0. {
            paint.set_style(PaintStyle::Stroke).set_stroke_width(spread * 2.);
            self.canvas.draw_path(&moved, &paint);
        }
        self.canvas.restore();
    }
}
