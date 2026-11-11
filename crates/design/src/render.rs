//! Drawing a Design, behind an interface (ADR-0002): node types draw
//! through a [`Painter`], and [`screenshot`] (an agent's), [`region`] (the
//! Design tab's tiles) and [`is_path_data`] are the only places that pick
//! the backend (Skia's raster, [`crate::skia`]), so a tiny-skia renderer
//! would replace those and a painter of its own. Both draw a node with
//! [`paint_node`], its assets from an [`Assets`], so the tab shows what
//! the agent's screenshot does.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::assets::Assets;
use crate::fonts::Fonts;
use crate::layout::LaidOut;
use crate::model::{
    Color, Design, EffectKind, Effects, FillRule, ImageFill, Node, Paint, PaintKind, Shadow, StrokeStyle, Text,
};
use crate::skia::Typesetter;

/// The longest side of an agent's screenshot, in pixels.
pub const SCREENSHOT_MAX_SIDE: f32 = 400.;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// A rectangle of this size at the origin.
    pub fn sized((width, height): (f32, f32)) -> Self {
        Self { x: 0., y: 0., width, height }
    }

    pub fn meets(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// The smallest rectangle holding both.
    pub fn union(&self, other: &Rect) -> Rect {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
}

/// The shape a node's fills, stroke and effects follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outline<'a> {
    /// A rectangle with a radius per corner: top left, top right, bottom
    /// right, bottom left.
    Rounded { rect: Rect, radii: [f32; 4] },
    /// The ellipse filling `rect`, or the part of it from `start` through
    /// `sweep` degrees (0 at 3 o'clock, clockwise), less a hole of `inner`
    /// times its radii.
    Ellipse { rect: Rect, start: f32, sweep: f32, inner: f32 },
    /// The regular polygon of `sides` filling `rect` ([`polygon`]).
    Polygon { rect: Rect, sides: u32 },
    /// SVG path data whose `view_box` (or, without one, its own bounds) is
    /// stretched onto `rect`.
    Path {
        rect: Rect,
        geometry: &'a str,
        view_box: Option<Rect>,
        fill_rule: FillRule,
    },
}

impl Outline<'_> {
    pub fn rounded(rect: Rect, radii: [f32; 4]) -> Self {
        Outline::Rounded { rect, radii }
    }

    pub fn bounds(&self) -> Rect {
        match *self {
            Outline::Rounded { rect, .. }
            | Outline::Ellipse { rect, .. }
            | Outline::Polygon { rect, .. }
            | Outline::Path { rect, .. } => rect,
        }
    }
}

/// The corners of a regular polygon of `sides` (3 to 64), one at the top,
/// clockwise, stretched to fill `rect`.
pub fn polygon(rect: Rect, sides: u32) -> Vec<(f32, f32)> {
    let sides = sides.clamp(3, 64);
    let unit: Vec<(f32, f32)> = (0..sides)
        .map(|i| {
            let angle = i as f32 * std::f32::consts::TAU / sides as f32 - std::f32::consts::FRAC_PI_2;
            (angle.cos(), angle.sin())
        })
        .collect();
    // The corners' own bounds, mapped onto the rectangle.
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in &unit {
        (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
    }
    unit.into_iter()
        .map(|(x, y)| {
            (
                rect.x + (x - left) / (right - left) * rect.width,
                rect.y + (y - top) / (bottom - top) * rect.height,
            )
        })
        .collect()
}

/// Whether `geometry` is SVG path data that draws something, as the
/// backend reads it.
pub fn is_path_data(geometry: &str) -> bool {
    crate::skia::svg_path(geometry).is_some_and(|path| !path.is_empty())
}

/// How a node type is drawn from its properties ([`NodeKind::look`]).
///
/// [`NodeKind::look`]: crate::model::NodeKind::look
pub struct Look<'a> {
    pub outline: Outline<'a>,
    pub fill: &'a [Paint],
    pub stroke: Option<&'a StrokeStyle>,
    pub effects: &'a Effects,
}

/// Where a drawing gets the images its image paints refer to
/// ([`ImageFill`]): files from the Design's folder, and downloads.
pub trait Images {
    /// The encoded image `url` refers to, or why it can't be had (an image
    /// that can't be had draws as a placeholder).
    fn get(&self, url: &str) -> Result<Arc<[u8]>, String>;
}

/// No images to be had: every image paint draws as a placeholder.
pub struct NoImages;

impl Images for NoImages {
    fn get(&self, _url: &str) -> Result<Arc<[u8]>, String> {
        Err("no images are given to this drawing".into())
    }
}

/// Images from files already on this computer, by url: what a Design tab
/// draws with, the engine having found each url's file (the Design's
/// folder, or its download cache). Nothing is downloaded; a url without a
/// file draws as a placeholder. Each file is read once.
#[derive(Debug, Default)]
pub struct ImageFiles {
    paths: HashMap<String, PathBuf>,
    read: Mutex<HashMap<String, Result<Arc<[u8]>, String>>>,
}

impl ImageFiles {
    pub fn new(paths: HashMap<String, PathBuf>) -> Self {
        Self { paths, read: Mutex::default() }
    }

    /// Each url's file.
    pub fn paths(&self) -> &HashMap<String, PathBuf> {
        &self.paths
    }
}

impl Images for ImageFiles {
    fn get(&self, url: &str) -> Result<Arc<[u8]>, String> {
        let mut read = self.read.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        read.entry(url.to_owned())
            .or_insert_with(|| {
                let path = self.paths.get(url).ok_or("not had yet")?;
                std::fs::read(path)
                    .map(Arc::from)
                    .map_err(|e| format!("can't read {}: {e}", path.display()))
            })
            .clone()
    }
}

/// Every image url `design` draws.
pub fn image_urls(design: &Design) -> BTreeSet<String> {
    // As drawn: images inside Instances, and those Tokens name, count.
    let design = &crate::resolve::drawn(design);
    let mut found = Vec::new();
    for node in &design.children {
        images_in(node, &mut found);
    }
    found.into_iter().map(|(_, image)| image.url.clone()).collect()
}

/// The image paints `node` and its descendants draw, each with the node
/// it's on: their enabled fills (a stroke draws no image).
pub fn images_in<'a>(node: &'a Node, found: &mut Vec<(&'a Node, &'a ImageFill)>) {
    let kind = node.kind.get();
    // A paint doesn't depend on the size it's drawn at.
    if let Some(look) = kind.look((0., 0.)) {
        for paint in look.fill.iter().filter(|paint| paint.enabled) {
            if let PaintKind::Image(image) = &paint.kind {
                found.push((node, image));
            }
        }
    }
    for child in kind.children() {
        images_in(child, found);
    }
}

/// What node types draw with. Calls stack as on a canvas: `save` and
/// `restore` bracket a `translate`, `scale`, `rotate`, `flip` or clip, and
/// `restore` ends a `save_layer`.
pub trait Painter {
    fn save(&mut self);
    fn restore(&mut self);
    fn translate(&mut self, x: f32, y: f32);
    fn scale(&mut self, factor: f32);
    /// Turns what follows by `degrees` clockwise about the origin.
    fn rotate(&mut self, degrees: f32);
    /// Mirrors what follows across the origin's vertical axis
    /// (`horizontal`) and its horizontal one (`vertical`).
    fn flip(&mut self, horizontal: bool, vertical: bool);
    /// Draws what follows, up to its `restore`, as one layer composited
    /// at `opacity` and blurred by `blur` (a radius).
    fn save_layer(&mut self, opacity: f32, blur: f32);
    fn clip_rect(&mut self, rect: Rect);
    fn clip(&mut self, outline: &Outline);
    /// Fills `outline` with each paint in turn, gradients laid over its
    /// bounds.
    fn fill(&mut self, outline: &Outline, paints: &[Paint]);
    /// Fills `rect` with each paint in turn.
    fn fill_rect(&mut self, rect: Rect, paints: &[Paint]) {
        self.fill(&Outline::rounded(rect, [0.; 4]), paints);
    }
    fn stroke(&mut self, outline: &Outline, stroke: &StrokeStyle);
    /// A drop shadow behind `outline`, or an inner one within it.
    fn shadow(&mut self, outline: &Outline, shadow: &Shadow);
    /// Blurs what's already drawn within `outline` by `radius`.
    fn background_blur(&mut self, outline: &Outline, radius: f32);
    /// Draws an icon (`set:name`) into `rect` in `color`, or nothing when
    /// the painter's assets haven't that one.
    fn icon(&mut self, icon: &str, weight: Option<u16>, color: Color, rect: Rect);
    /// Draws `text` in a box of `size` at the origin, in each of its fill's
    /// paints in turn.
    fn text(&mut self, text: &Text, size: (f32, f32));
}

/// Draws `node` and its descendants where `laid_out` puts them, with its
/// top-left at the painter's origin wherever it sits in its parent, turned
/// and flipped as it is.
pub fn paint_node(node: &Node, laid_out: &LaidOut, painter: &mut dyn Painter) {
    let Some(placed) = laid_out.get(&node.id) else {
        return;
    };
    let turned = node.rotation != 0. || node.flip_x || node.flip_y;
    if turned {
        painter.save();
        painter.rotate(-node.rotation);
        if node.flip_x || node.flip_y {
            let (width, height) = (placed.bounds.width, placed.bounds.height);
            painter.translate(width / 2., height / 2.);
            painter.flip(node.flip_x, node.flip_y);
            painter.translate(-width / 2., -height / 2.);
        }
    }
    paint_upright(node, laid_out, painter);
    if turned {
        painter.restore();
    }
}

/// Draws `node` and its descendants as [`paint_node`] does, without the
/// node's own rotation and flip: a node seen on its own.
pub fn paint_upright(node: &Node, laid_out: &LaidOut, painter: &mut dyn Painter) {
    let Some(placed) = laid_out.get(&node.id) else {
        return;
    };
    let bounds = placed.bounds;
    let size = (bounds.width, bounds.height);
    let kind = node.kind.get();
    let look = kind.look(size);
    if let Some(look) = &look {
        // Before any layer of the node's own, which would hide what's
        // behind it from the blur.
        for effect in look.effects.enabled() {
            if let EffectKind::BackgroundBlur(radius) = effect {
                painter.background_blur(&look.outline, *radius);
            }
        }
    }
    let blur = look.as_ref().map_or(0., |look| look.effects.layer_blur());
    let layered = node.opacity < 1. || blur > 0.;
    if layered {
        painter.save_layer(node.opacity, blur);
    }
    if let Some(look) = &look {
        let shadows = |inner: bool| {
            look.effects.enabled().filter_map(move |effect| match effect {
                EffectKind::Shadow(shadow) if shadow.inner == inner => Some(shadow),
                _ => None,
            })
        };
        for shadow in shadows(false) {
            painter.shadow(&look.outline, shadow);
        }
        painter.fill(&look.outline, look.fill);
        for shadow in shadows(true) {
            painter.shadow(&look.outline, shadow);
        }
    }
    kind.paint(size, painter);
    if !kind.children().is_empty() {
        painter.save();
        if kind.clips() {
            match &look {
                Some(look) => painter.clip(&look.outline),
                None => painter.clip_rect(Rect::sized(size)),
            }
        }
        for child in kind.children() {
            let Some(at) = laid_out.get(&child.id) else {
                continue;
            };
            painter.save();
            painter.translate(at.bounds.x - bounds.x, at.bounds.y - bounds.y);
            paint_node(child, laid_out, painter);
            painter.restore();
        }
        painter.restore();
    }
    // Over the children, as a Frame's border is.
    if let Some(look) = &look
        && let Some(stroke) = look.stroke
    {
        painter.stroke(&look.outline, stroke);
    }
    if layered {
        painter.restore();
    }
}

/// How far past its outline a node's own drawing goes: an outer shadow
/// (moved, grown and blurred), a layer blur and a stroke outside it. A
/// blur reaches about three of its sigmas (`skia::sigma`).
fn reach(look: &Look) -> f32 {
    let blurred = |radius: f32| radius * 1.5;
    let shadows = look.effects.enabled().filter_map(|effect| match effect {
        EffectKind::Shadow(shadow) if !shadow.inner => {
            Some(shadow.offset.0.abs().max(shadow.offset.1.abs()) + shadow.spread.max(0.) + blurred(shadow.blur))
        }
        _ => None,
    });
    let stroke = look.stroke.map_or(0., |stroke| {
        let t = stroke.thickness;
        t.top.max(t.right).max(t.bottom).max(t.left)
    });
    // (And a pixel for the edges' anti-aliasing.)
    shadows.fold(blurred(look.effects.layer_blur()) + stroke, f32::max) + 1.
}

/// Where `node` draws on the Canvas, as `laid_out` puts it: its own box
/// and what its effects and stroke reach past it, its descendants' unless
/// it clips them, all turned as it is.
pub fn bounds(node: &Node, laid_out: &LaidOut) -> Rect {
    let Some(placed) = laid_out.get(&node.id) else {
        return Rect::sized((0., 0.));
    };
    let kind = node.kind.get();
    let size = (placed.bounds.width, placed.bounds.height);
    let out = kind.look(size).map_or(0., |look| reach(&look));
    let mut drawn = Rect {
        x: placed.bounds.x - out,
        y: placed.bounds.y - out,
        width: placed.bounds.width + 2. * out,
        height: placed.bounds.height + 2. * out,
    };
    if !kind.clips() {
        for child in kind.children() {
            let child = bounds(child, laid_out);
            if child.width > 0. || child.height > 0. {
                drawn = drawn.union(&child);
            }
        }
    }
    if node.rotation == 0. {
        return drawn;
    }
    // Turned about its top-left corner (`paint_node`), either way round:
    // the box holding its corners.
    let (ox, oy) = (placed.bounds.x, placed.bounds.y);
    let (sin, cos) = node.rotation.to_radians().sin_cos();
    let corners = [(drawn.x, drawn.y), (drawn.x + drawn.width, drawn.y), (drawn.x, drawn.y + drawn.height), (drawn.x + drawn.width, drawn.y + drawn.height)];
    let mut turned: Option<Rect> = None;
    for sign in [1., -1.] {
        for (x, y) in corners {
            let (dx, dy) = (x - ox, y - oy);
            let at = Rect { x: ox + dx * cos - dy * sin * sign, y: oy + dx * sin * sign + dy * cos, width: 0., height: 0. };
            turned = Some(turned.map_or(at, |t| t.union(&at)));
        }
    }
    turned.unwrap_or(drawn)
}

/// How far around itself any node of `design` reads or spreads what's
/// drawn: its widest background or layer blur. A part of the Canvas drawn
/// on its own needs this much more drawn round it to come out as it does
/// drawn whole ([`region`]'s `margin`).
pub fn blur_reach(design: &Design, laid_out: &LaidOut) -> f32 {
    let mut most: f32 = 0.;
    design.walk(&mut |node| {
        let Some(placed) = laid_out.get(&node.id) else { return };
        if let Some(look) = node.kind.get().look((placed.bounds.width, placed.bounds.height)) {
            for effect in look.effects.enabled() {
                if let EffectKind::BackgroundBlur(radius) | EffectKind::LayerBlur(radius) = effect {
                    most = most.max(radius * 1.5);
                }
            }
        }
    });
    most
}

/// The part `area` of the Canvas at `scale` pixels a unit, as a
/// transparent `width` × `height` image (RGBA, premultiplied): the
/// Canvas's top-level nodes at `nodes` (indices into its children, the
/// ones meeting `area` grown by `margin`, in order) drawn where `laid_out`
/// puts them, their text set by `typesetter` (as it was laid out). It's
/// drawn `margin` (Canvas units, [`blur_reach`]) wider all round and cut
/// to `area`, so blurs at its edges read what they would with the whole
/// Canvas drawn. Image paints draw what `images` has, icons and other
/// assets what `assets` has. `None` for an empty or oversized image.
#[allow(clippy::too_many_arguments)]
pub fn region(
    design: &Design,
    laid_out: &LaidOut,
    typesetter: &Typesetter,
    nodes: &[usize],
    area: Rect,
    scale: f32,
    margin: f32,
    (width, height): (u32, u32),
    images: &dyn Images,
    assets: &dyn Assets,
) -> Option<Vec<u8>> {
    // (At most a tile's worth each side: past that a blur is faint.)
    let pad = (margin * scale).ceil().clamp(0., 512.) as u32;
    let (wide, high) = (width + 2 * pad, height + 2 * pad);
    let mut drawn = vec![0; wide as usize * high as usize * 4];
    crate::skia::draw_into(&mut drawn, wide, high, |canvas| {
        let mut painter = crate::skia::SkiaPainter::new(canvas, typesetter).with_images(images).with_assets(assets);
        painter.translate(pad as f32, pad as f32);
        painter.scale(scale);
        painter.translate(-area.x, -area.y);
        for node in nodes.iter().filter_map(|&i| design.children.get(i)) {
            let Some(at) = laid_out.get(&node.id) else {
                continue;
            };
            painter.save();
            painter.translate(at.bounds.x, at.bounds.y);
            paint_node(node, laid_out, &mut painter);
            painter.restore();
        }
    })?;
    if pad == 0 {
        return Some(drawn);
    }
    let (row, wide_row) = (width as usize * 4, wide as usize * 4);
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in pad as usize..(pad + height) as usize {
        let from = y * wide_row + pad as usize * 4;
        pixels.extend_from_slice(&drawn[from..from + row]);
    }
    Some(pixels)
}

/// A node drawn on its own at 1x, scaled down to fit `max_side`, as PNG.
#[derive(Debug, Clone)]
pub struct Screenshot {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no node has the id \"{0}\"")]
    NotFound(String),
    #[error("\"{0}\" has no area to draw")]
    Empty(String),
    #[error("couldn't draw \"{0}\"")]
    Failed(String),
}

/// Draws one node of `design` on a transparent background at 1x, or
/// smaller when its longest side is over `max_side`, with the images
/// `images` has and the icons and other assets `assets` has, and encodes
/// it. Its text is set in `fonts`.
pub fn screenshot(
    design: &Design,
    id: &str,
    max_side: f32,
    fonts: &Fonts,
    images: &dyn Images,
    assets: &dyn Assets,
) -> Result<Screenshot, RenderError> {
    let design = &crate::resolve::drawn(design);
    let node = design.find(id).ok_or_else(|| RenderError::NotFound(id.into()))?;
    let typesetter = fonts.typesetter(design);
    let laid_out = LaidOut::of(design, &typesetter);
    let bounds = laid_out.get(id).ok_or_else(|| RenderError::NotFound(id.into()))?.bounds;
    let (width, height) = (bounds.width, bounds.height);
    if !(width > 0. && height > 0.) {
        return Err(RenderError::Empty(id.into()));
    }
    let scale = (max_side / width.max(height)).min(1.);
    let (pixels_wide, pixels_high) = (
        (width * scale).round().max(1.) as u32,
        (height * scale).round().max(1.) as u32,
    );
    let failed = || RenderError::Failed(id.into());
    let mut surface =
        crate::skia::surface(pixels_wide as i32, pixels_high as i32).ok_or_else(failed)?;
    {
        let mut painter = crate::skia::SkiaPainter::new(surface.canvas(), &typesetter).with_images(images).with_assets(assets);
        painter.scale(scale);
        paint_upright(node, &laid_out, &mut painter);
    }
    let png = crate::skia::png(&mut surface).ok_or_else(failed)?;
    Ok(Screenshot {
        id: id.into(),
        width: pixels_wide,
        height: pixels_high,
        png,
    })
}
