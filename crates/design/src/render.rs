//! Drawing a Design, behind an interface (ADR-0002): node types draw
//! through a [`Painter`], and [`screenshot`] is the one place that picks
//! the backend (Skia's raster, [`crate::skia`]), so a tiny-skia renderer
//! would replace that and a painter of its own.

use std::sync::Arc;

use crate::layout::LaidOut;
use crate::model::{
    Design, EffectKind, Effects, ImageFill, Node, Paint, PaintKind, Shadow, StrokeStyle,
};

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
}

/// The shape a node's fills, stroke and effects follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outline {
    /// A rectangle with a radius per corner: top left, top right, bottom
    /// right, bottom left.
    Rounded { rect: Rect, radii: [f32; 4] },
}

impl Outline {
    pub fn rounded(rect: Rect, radii: [f32; 4]) -> Self {
        Outline::Rounded { rect, radii }
    }

    pub fn bounds(&self) -> Rect {
        match *self {
            Outline::Rounded { rect, .. } => rect,
        }
    }
}

/// How a node type is drawn from its properties ([`NodeKind::look`]).
///
/// [`NodeKind::look`]: crate::model::NodeKind::look
pub struct Look<'a> {
    pub outline: Outline,
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

/// The image paints `node` and its descendants draw (fills and strokes),
/// each with the node it's on.
pub fn images_in<'a>(node: &'a Node, found: &mut Vec<(&'a Node, &'a ImageFill)>) {
    let kind = node.kind.get();
    // A paint doesn't depend on the size it's drawn at.
    if let Some(look) = kind.look((0., 0.)) {
        let stroke = look.stroke.map_or(&[][..], |stroke| &stroke.fill[..]);
        for paint in look.fill.iter().chain(stroke) {
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
/// `images` has, and encodes it.
pub fn screenshot(
    design: &Design,
    id: &str,
    max_side: f32,
    images: &dyn Images,
) -> Result<Screenshot, RenderError> {
    let node = design.find(id).ok_or_else(|| RenderError::NotFound(id.into()))?;
    let laid_out = LaidOut::of(design);
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
        let mut painter = crate::skia::SkiaPainter::with_images(surface.canvas(), images);
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
