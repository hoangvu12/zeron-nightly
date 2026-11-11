use serde::{Deserialize, Serialize};

use super::{Fill, NodeKind, Size, Sizing};
use crate::render::{Painter, Rect, Shape};

/// SVG path data (`geometry`) whose `viewBox` is stretched onto its box;
/// without one, the geometry's own bounds are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Path {
    #[serde(flatten)]
    pub size: Size,
    #[serde(flatten)]
    pub fill: Fill,
    #[serde(default)]
    pub geometry: String,
    /// `[x, y, width, height]`, as an SVG's `viewBox`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_box: Option<[f32; 4]>,
    #[serde(default, skip_serializing_if = "FillRule::is_nonzero")]
    pub fill_rule: FillRule,
}

/// Which points a path that crosses itself fills, as SVG's `fill-rule`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FillRule {
    #[default]
    Nonzero,
    Evenodd,
}

impl FillRule {
    fn is_nonzero(&self) -> bool {
        *self == FillRule::Nonzero
    }
}

impl Path {
    /// Why the path can't be drawn as written, if it can't.
    pub fn problem(&self) -> Option<String> {
        if !crate::render::is_path_data(&self.geometry) {
            return Some(format!(
                "\"{}\" is not SVG path data (as in \"M0 0 L10 0 L5 10 Z\")",
                self.geometry
            ));
        }
        match self.view_box {
            Some([_, _, width, height]) if !(width > 0. && height > 0.) => {
                Some("a viewBox is [x, y, width, height], with a width and height over 0".into())
            }
            _ => None,
        }
    }
}

impl NodeKind for Path {
    fn size(&self) -> (Sizing, Sizing) {
        self.size.or(Sizing::Fixed(0.))
    }

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        let shape = Shape::Path {
            geometry: &self.geometry,
            view_box: self.view_box.map(|[x, y, width, height]| Rect { x, y, width, height }),
            rect: Rect::sized(size),
            fill_rule: self.fill_rule,
        };
        painter.fill_shape(shape, &self.fill.fill);
    }
}
