use serde::{Deserialize, Serialize};

use super::{Fill, NodeKind, Size, Sizing};
use crate::render::{Painter, Rect};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rectangle {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
}

impl NodeKind for Rectangle {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        painter.fill_rect(Rect::sized(size), &self.fill.fill);
    }
}
