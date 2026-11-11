use serde::{Deserialize, Serialize};

use super::{Fill, Node, NodeKind, Size};
use crate::render::{Painter, Rect};

/// A container node, holding its children where they place themselves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
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

    fn paint(&self, painter: &mut dyn Painter) {
        painter.fill_rect(Rect::sized(self.size()), &self.fill.fill);
    }
}
