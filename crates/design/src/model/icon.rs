use serde::{Deserialize, Serialize};

use super::{Color, Fill, NodeKind, Paint, Size};
use crate::render::{Painter, Rect};

/// An Iconify icon (`icon`, as `set:name`) drawn into its box in its fill's
/// colour, at a `weight` (100 to 900, 400 the set's regular) where the set
/// has weights.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Icon {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
    #[serde(default)]
    pub icon: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
}

impl Icon {
    /// The colour it draws in: its first solid fill, or black.
    pub fn color(&self) -> Color {
        self.fill
            .fill
            .first()
            .map(|paint| match *paint {
                Paint::Solid(color) => color,
            })
            .unwrap_or(Color { r: 0, g: 0, b: 0, a: 255 })
    }
}

impl NodeKind for Icon {
    fn size(&self) -> (f32, f32) {
        (self.size.width, self.size.height)
    }

    fn paint(&self, painter: &mut dyn Painter) {
        painter.icon(&self.icon, self.weight, self.color(), Rect::sized(self.size()));
    }
}
