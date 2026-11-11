use std::f32::consts::{FRAC_PI_2, TAU};

use serde::{Deserialize, Serialize};

use super::{Fill, NodeKind, Size, Sizing};
use crate::render::{Painter, Shape};

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
    #[serde(default = "triangle")]
    pub polygon_count: u32,
}

impl Default for Polygon {
    fn default() -> Self {
        Self {
            size: Size::default(),
            fill: Fill::default(),
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

    /// Its corners in a box of this size, clockwise from the top.
    pub fn corners(&self, (width, height): (f32, f32)) -> Vec<(f32, f32)> {
        let sides = self.polygon_count.clamp(*SIDES.start(), *SIDES.end());
        let unit: Vec<(f32, f32)> = (0..sides)
            .map(|i| {
                let angle = i as f32 * TAU / sides as f32 - FRAC_PI_2;
                (angle.cos(), angle.sin())
            })
            .collect();
        // The corners' own bounds, mapped onto the box.
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in &unit {
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
        }
        unit.into_iter()
            .map(|(x, y)| {
                (
                    (x - left) / (right - left) * width,
                    (y - top) / (bottom - top) * height,
                )
            })
            .collect()
    }
}

impl NodeKind for Polygon {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        painter.fill_shape(Shape::Polygon(&self.corners(size)), &self.fill.fill);
    }
}
