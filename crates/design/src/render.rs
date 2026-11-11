//! Drawing a Design, behind an interface (ADR-0002): node types draw
//! through a [`Painter`], and [`screenshot`] (an agent's) and [`region`]
//! (the Design tab's tiles) are the only places that pick the backend
//! (Skia's raster, [`crate::skia`]), so a tiny-skia renderer would replace
//! those and a painter of its own. Both draw a node with [`paint_node`],
//! so the tab shows what the agent's screenshot does.

use crate::model::{Design, Node, Paint};

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

/// Where `node` draws, in its parent's space: its own box, and its
/// descendants' unless it clips them.
pub fn bounds(node: &Node) -> Rect {
    let kind = node.kind.get();
    let mut drawn = Rect::sized(kind.size());
    if !kind.clips() {
        for child in kind.children() {
            drawn = drawn.union(&bounds(child));
        }
    }
    Rect {
        x: drawn.x + node.x,
        y: drawn.y + node.y,
        ..drawn
    }
}

/// The part `area` of the Canvas at `scale` pixels a unit, as a
/// transparent `width` × `height` image (RGBA, premultiplied): the
/// Canvas's top-level nodes at `nodes` (indices into its children, the
/// ones meeting `area`, in order) drawn where they sit. `None` for an
/// empty or oversized image.
pub fn region(design: &Design, nodes: &[usize], area: Rect, scale: f32, width: u32, height: u32) -> Option<Vec<u8>> {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    crate::skia::draw_into(&mut pixels, width, height, |canvas| {
        let mut painter = crate::skia::SkiaPainter::new(canvas);
        painter.scale(scale);
        painter.translate(-area.x, -area.y);
        for node in nodes.iter().filter_map(|&i| design.children.get(i)) {
            painter.save();
            painter.translate(node.x, node.y);
            paint_node(node, &mut painter);
            painter.restore();
        }
    })?;
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
/// smaller when its longest side is over `max_side`, and encodes it.
pub fn screenshot(design: &Design, id: &str, max_side: f32) -> Result<Screenshot, RenderError> {
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
        let mut painter = crate::skia::SkiaPainter::new(surface.canvas());
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
