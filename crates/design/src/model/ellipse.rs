use serde::{Deserialize, Serialize};

use super::{Fill, NodeKind, Size, Sizing};
use crate::render::{Painter, Rect, Shape};

/// The ellipse filling its box: whole, or an arc from `startAngle` through
/// `sweepAngle` degrees (0 at 3 o'clock, clockwise), and a ring with an
/// `innerRadius` (a fraction of its radii, 0 to 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ellipse {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_angle: f32,
    #[serde(default = "whole", skip_serializing_if = "is_whole")]
    pub sweep_angle: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub inner_radius: f32,
}

impl Default for Ellipse {
    fn default() -> Self {
        Self {
            size: Size::default(),
            fill: Fill::default(),
            start_angle: 0.,
            sweep_angle: whole(),
            inner_radius: 0.,
        }
    }
}

fn whole() -> f32 {
    360.
}

fn is_whole(sweep: &f32) -> bool {
    *sweep == 360.
}

fn is_zero(value: &f32) -> bool {
    *value == 0.
}

impl NodeKind for Ellipse {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        let shape = Shape::Ellipse {
            rect: Rect::sized(size),
            start: self.start_angle,
            sweep: self.sweep_angle,
            inner: self.inner_radius,
        };
        painter.fill_shape(shape, &self.fill.fill);
    }
}
