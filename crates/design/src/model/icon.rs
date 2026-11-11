use serde::{Deserialize, Serialize};

use super::{Color, Fill, NodeKind, PaintKind, Size, Sizing};
use crate::assets::AssetKey;
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
    /// The colour it draws in: its first solid fill drawn, at that fill's
    /// opacity, or black.
    pub fn color(&self) -> Color {
        self.fill
            .fill
            .iter()
            .filter(|paint| paint.enabled)
            .find_map(|paint| match paint.kind {
                PaintKind::Solid(color) => Some(Color {
                    a: (f32::from(color.a) * paint.opacity.clamp(0., 1.)).round() as u8,
                    ..color
                }),
                _ => None,
            })
            .unwrap_or(Color { r: 0, g: 0, b: 0, a: 255 })
    }
}

impl NodeKind for Icon {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn assets(&self) -> Vec<AssetKey> {
        crate::icons::name(&self.icon, self.weight).map(AssetKey::icon).into_iter().collect()
    }

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        painter.icon(&self.icon, self.weight, self.color(), Rect::sized(size));
    }
}
