use serde::{Deserialize, Serialize};

use super::{Fill, Layout, Node, NodeKind, Size, Sizing};
use crate::render::{Painter, Rect};

/// A container node, holding its children where they place themselves or
/// laying them out in a row or column. Its size fits its content unless set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub layout: Layout,
    #[serde(flatten)]
    pub fill: Fill,
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

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        painter.fill_rect(Rect::sized(size), &self.fill.fill);
    }
}
