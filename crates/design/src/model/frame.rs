use serde::{Deserialize, Serialize};

use super::{Corners, Effects, Fill, Layout, Node, NodeKind, Size, Sizing, Stroke};
use crate::render::{Look, Outline, Rect};

/// A container node, holding its children where they place themselves or
/// laying them out in a row or column. Its size fits its content unless set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub layout: Layout,
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
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::FitContent)
    }

    fn layout(&self) -> Option<&Layout> {
        Some(&self.layout)
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

    fn look(&self, size: (f32, f32)) -> Option<Look<'_>> {
        Some(Look {
            outline: Outline::rounded(Rect::sized(size), self.corners.radii),
            fill: &self.fill.fill,
            stroke: self.stroke.stroke.as_ref(),
            effects: &self.effects,
        })
    }
}
