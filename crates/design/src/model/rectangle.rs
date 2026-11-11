use serde::{Deserialize, Serialize};

use super::{Fill, NodeKind, Size};
use crate::render::{Painter, Rect};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rectangle {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
}

impl NodeKind for Rectangle {
    fn size(&self) -> (f32, f32) {
        (self.size.width, self.size.height)
    }

    fn paint(&self, painter: &mut dyn Painter) {
        painter.fill_rect(Rect::sized(self.size()), &self.fill.fill);
    }
}
