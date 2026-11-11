//! Finding room on the Canvas, as the snippet API's `FindEmptySpace` does,
//! so a new Screen never overlaps the ones already there.

use serde::Deserialize;

use crate::edit::EditError;
use crate::layout::LaidOut;
use crate::model::Design;
use crate::render::Rect;

/// Space kept between the room found and the nodes around it, unless
/// asked otherwise.
const DEFAULT_PADDING: f32 = 100.;

/// What `FindEmptySpace` is asked for.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wanted {
    pub width: f32,
    pub height: f32,
    /// Which way from where it starts to look: right (unset), left, top
    /// or bottom.
    #[serde(default)]
    pub direction: Option<String>,
    /// Space kept from every top-level node.
    #[serde(default)]
    pub padding: Option<f32>,
    /// A node to start from, instead of all the Canvas's nodes.
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum Toward {
    Right,
    Left,
    Top,
    Bottom,
}

/// The top-left corner of a `wanted`-sized space beside where it starts
/// (the node `from`, or all the Canvas's nodes together) toward its
/// direction, padding clear of every top-level node; the origin on an
/// empty Canvas.
pub fn find(design: &Design, laid_out: &LaidOut, wanted: &Wanted) -> Result<(f32, f32), EditError> {
    let padding = wanted.padding.unwrap_or(DEFAULT_PADDING);
    let sizes = [wanted.width, wanted.height, padding];
    if sizes.iter().any(|size| !size.is_finite() || *size < 0.) || wanted.width == 0. || wanted.height == 0. {
        return Err(EditError::Invalid(
            "FindEmptySpace takes a width and height above 0, and a padding of 0 or more".into(),
        ));
    }
    let toward = match wanted.direction.as_deref() {
        None | Some("right") => Toward::Right,
        Some("left") => Toward::Left,
        Some("top") => Toward::Top,
        Some("bottom") => Toward::Bottom,
        Some(other) => {
            return Err(EditError::Invalid(format!(
                "FindEmptySpace's direction is right, left, top or bottom, not \"{other}\""
            )));
        }
    };
    let taken: Vec<Rect> = design
        .children
        .iter()
        .filter_map(|node| laid_out.get(&node.id).map(|placed| placed.bounds))
        .collect();
    if let Some(id) = &wanted.from {
        design.find(id).ok_or_else(|| EditError::NotFound(id.clone()))?;
    }
    // A node the layout doesn't place (of an unknown type) starts from all.
    let start = wanted
        .from
        .as_ref()
        .and_then(|id| laid_out.get(id))
        .map(|placed| placed.bounds)
        .or_else(|| taken.iter().copied().reduce(union));
    let Some(start) = start else {
        return Ok((0., 0.));
    };
    let (width, height) = (wanted.width, wanted.height);
    let mut at = match toward {
        Toward::Right => (right(start) + padding, start.y),
        Toward::Left => (start.x - padding - width, start.y),
        Toward::Bottom => (start.x, bottom(start) + padding),
        Toward::Top => (start.x, start.y - padding - height),
    };
    // Each step moves past a node in the way, never back, so each node
    // stops it at most once.
    while let Some(blocking) = taken.iter().find(|node| {
        at.0 < right(**node) + padding
            && at.0 + width + padding > node.x
            && at.1 < bottom(**node) + padding
            && at.1 + height + padding > node.y
    }) {
        match toward {
            Toward::Right => at.0 = right(*blocking) + padding,
            Toward::Left => at.0 = blocking.x - padding - width,
            Toward::Bottom => at.1 = bottom(*blocking) + padding,
            Toward::Top => at.1 = blocking.y - padding - height,
        }
    }
    Ok(at)
}

fn right(rect: Rect) -> f32 {
    rect.x + rect.width
}

fn bottom(rect: Rect) -> f32 {
    rect.y + rect.height
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect {
        x,
        y,
        width: right(a).max(right(b)) - x,
        height: bottom(a).max(bottom(b)) - y,
    }
}
