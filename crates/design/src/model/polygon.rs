use serde::{Deserialize, Serialize};

use super::{Effects, Fill, NodeKind, Size, Sizing, Stroke};
use crate::render::{Look, Outline, Rect};

/// The fewest and most sides a polygon draws with.
const SIDES: std::ops::RangeInclusive<u32> = 3..=64;

/// A regular polygon of `polygonCount` sides, one corner at the top,
/// stretched to fill its box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Polygon {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
    #[serde(flatten)]
    pub stroke: Stroke,
    #[serde(flatten)]
    pub effects: Effects,
    #[serde(default = "triangle")]
    pub polygon_count: u32,
}

impl Default for Polygon {
    fn default() -> Self {
        Self {
            size: Size::default(),
            fill: Fill::default(),
            stroke: Stroke::default(),
            effects: Effects::default(),
            polygon_count: triangle(),
        }
    }
}

fn triangle() -> u32 {
    3
}

impl Polygon {
    /// Why it can't be drawn as written, if it can't.
    pub fn problem(&self) -> Option<String> {
        (!SIDES.contains(&self.polygon_count)).then(|| {
            format!(
                "a polygon has {} to {} sides (polygonCount), not {}",
                SIDES.start(),
                SIDES.end(),
                self.polygon_count
            )
        })
    }
}

impl NodeKind for Polygon {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn look(&self, size: (f32, f32)) -> Option<Look<'_>> {
        Some(Look {
            outline: Outline::Polygon {
                rect: Rect::sized(size),
                sides: self.polygon_count,
            },
            fill: &self.fill.fill,
            stroke: self.stroke.stroke.as_ref(),
            effects: &self.effects,
        })
    }
}
