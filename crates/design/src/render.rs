//! Drawing a Design, behind an interface (ADR-0002): node types draw
//! through a [`Painter`], and [`screenshot`] is the one place that picks
//! the backend (Skia's raster, [`crate::skia`]), so a tiny-skia renderer
//! would replace that and a painter of its own.

use crate::icons::Icons;
use crate::model::{Color, Design, FillRule, Node, Paint};

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

/// An outline other than a rectangle, as the shape node types describe it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape<'a> {
    /// The ellipse filling `rect`, or the part of it from `start` through
    /// `sweep` degrees (0 at 3 o'clock, clockwise), less a hole of `inner`
    /// times its radii.
    Ellipse { rect: Rect, start: f32, sweep: f32, inner: f32 },
    /// The polygon through these corners.
    Polygon(&'a [(f32, f32)]),
    /// SVG path data whose `view_box` (or, without one, its own bounds) is
    /// stretched onto `rect`.
    Path {
        geometry: &'a str,
        view_box: Option<Rect>,
        rect: Rect,
        fill_rule: FillRule,
    },
}

/// What node types draw with. Calls stack as on a canvas: `save` and
/// `restore` bracket a `translate`, `scale` or `clip_rect`.
pub trait Painter {
    fn save(&mut self);
    fn restore(&mut self);
    fn translate(&mut self, x: f32, y: f32);
    fn scale(&mut self, factor: f32);
    fn clip_rect(&mut self, rect: Rect);
    /// Fills `rect` with each paint in turn.
    fn fill_rect(&mut self, rect: Rect, paints: &[Paint]);
    /// Fills `shape` with each paint in turn.
    fn fill_shape(&mut self, shape: Shape<'_>, paints: &[Paint]);
    /// Draws an icon (`set:name`) into `rect` in `color`, or nothing when
    /// the painter has no icons or not that one.
    fn icon(&mut self, icon: &str, weight: Option<u16>, color: Color, rect: Rect);
}

/// Draws `node` and its descendants with its top-left at the painter's
/// origin, wherever it sits in its parent.
pub fn paint_node(node: &Node, painter: &mut dyn Painter) {
    let kind = node.kind.get();
    kind.paint(painter);
    if kind.children().is_empty() {
        return;
    }
    painter.save();
    if kind.clips() {
        painter.clip_rect(Rect::sized(kind.size()));
    }
    for child in kind.children() {
        painter.save();
        painter.translate(child.x, child.y);
        paint_node(child, painter);
        painter.restore();
    }
    painter.restore();
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
/// smaller when its longest side is over `max_side`, and encodes it. Its
/// icons come from `icons`.
pub fn screenshot(
    design: &Design,
    id: &str,
    max_side: f32,
    icons: &Icons,
) -> Result<Screenshot, RenderError> {
    let node = design.find(id).ok_or_else(|| RenderError::NotFound(id.into()))?;
    let (width, height) = node.kind.get().size();
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
        let mut painter = crate::skia::SkiaPainter::new(surface.canvas()).with_icons(icons);
        painter.scale(scale);
        paint_node(node, &mut painter);
    }
    let png = crate::skia::png(&mut surface).ok_or_else(failed)?;
    Ok(Screenshot {
        id: id.into(),
        width: pixels_wide,
        height: pixels_high,
        png,
    })
}
