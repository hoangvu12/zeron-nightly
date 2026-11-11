use serde::{Deserialize, Serialize};

use super::{Corners, Effects, Fill, Node, NodeKind, Size, Stroke};
use crate::render::{Look, Outline, Rect};

/// A container node, holding its children where they place themselves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clip: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl NodeKind for Frame {
    fn size(&self) -> (f32, f32) {
        (self.size.width, self.size.height)
    }

    fn children(&self) -> &[Node] {
        &self.children
    }

    fn children_mut(&mut self) -> Option<&mut Vec<Node>> {
        Some(&mut self.children)
    }

    fn clips(&self) -> bool {
        self.clip
    }

    fn look(&self) -> Option<Look<'_>> {
        Some(Look {
            outline: Outline::rounded(Rect::sized(self.size()), self.corners.radii),
            fill: &self.fill.fill,
            stroke: self.stroke.stroke.as_ref(),
            effects: &self.effects,
        })
    }
}
