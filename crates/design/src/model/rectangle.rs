use serde::{Deserialize, Serialize};

use super::{Corners, Effects, Fill, NodeKind, Size, Sizing, Stroke};
use crate::render::{Look, Outline, Rect};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rectangle {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub corners: Corners,
    #[serde(flatten)]
    pub fill: Fill,
    #[serde(flatten)]
    pub stroke: Stroke,
    #[serde(flatten)]
    pub effects: Effects,
}

impl NodeKind for Rectangle {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn look(&self, size: (f32, f32)) -> Option<Look<'_>> {
        Some(Look {
            outline: Outline::rounded(Rect::sized(size), self.corners.radii),
            fill: &self.fill.fill,
            stroke: self.stroke.stroke.as_ref(),
            effects: &self.effects,
        })
    }
}
