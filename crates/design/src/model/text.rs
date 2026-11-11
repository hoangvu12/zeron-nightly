//! Text: a run of characters in one font, shaped and laid out by Skia's
//! paragraph layout ([`crate::skia::Typesetter`]) in the fonts the
//! Design's computer has ([`crate::fonts`]).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::{Fill, NodeKind, Size, Sizing};
use crate::assets::AssetKey;
use crate::render::Painter;
use crate::skia::Typesetter;

/// The family text without a `fontFamily` is set in: bundled, so it needs
/// no fetch ([`crate::fonts::BUNDLED_FAMILY`]).
pub const DEFAULT_FAMILY: &str = crate::fonts::BUNDLED_FAMILY;
pub const DEFAULT_FONT_SIZE: f32 = 14.;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Text {
    #[serde(flatten)]
    pub size: Size,
    /// Its characters' colour; text without one is invisible.
    #[serde(flatten)]
    pub fill: Fill,
    #[serde(default)]
    pub content: String,
    /// A Google Fonts family, or one in a font file beside the Design.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<FontSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<FontWeight>,
    /// Each line's height as a multiple of the font size; unset, the
    /// font's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    /// Pixels added after each character.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub letter_spacing: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub text_align: TextAlign,
    /// Where the lines sit in a box taller than they are.
    #[serde(default, skip_serializing_if = "is_default")]
    pub text_align_vertical: VerticalAlign,
    /// Unset, it follows the sizes set: a width wraps, a width and a
    /// height wrap in a fixed box, neither is one line ([`Text::growth`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_growth: Option<TextGrowth>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strikethrough: bool,
}

fn is_zero(value: &f32) -> bool {
    *value == 0.
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// How a text node's box follows its text. (Read in pen's kebab-case
/// too, as the first Designs with text wrote it.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextGrowth {
    /// As wide and tall as its text, each paragraph on one line.
    Auto,
    /// Wraps at its width and grows in height.
    #[serde(alias = "fixed-width")]
    FixedWidth,
    /// Wraps at its width; text past its height overflows it.
    #[serde(alias = "fixed-width-height")]
    FixedWidthHeight,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerticalAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// A font size in pixels, more than 0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FontSize(pub f32);

impl<'de> Deserialize<'de> for FontSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match f32::deserialize(deserializer)? {
            size if size > 0. && size.is_finite() => Ok(Self(size)),
            size => Err(D::Error::custom(format!("a font size is more than 0, not {size}"))),
        }
    }
}

/// A font weight from 1 to 1000 (400 regular, 700 bold), written as a
/// number; read from a number, a numeric string or `"normal"`/`"bold"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontWeight(pub u16);

impl FontWeight {
    pub const REGULAR: Self = Self(400);
}

impl Serialize for FontWeight {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FontWeight {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let weight = match &value {
            Value::Number(n) => n.as_f64(),
            Value::String(s) if s == "normal" => Some(400.),
            Value::String(s) if s == "bold" => Some(700.),
            Value::String(s) => s.parse().ok(),
            _ => None,
        };
        match weight {
            Some(weight) if (1. ..=1000.).contains(&weight) => Ok(Self(weight.round() as u16)),
            _ => Err(D::Error::custom(format!(
                "a font weight is a number from 1 to 1000 (400 regular, 700 bold), not {value}"
            ))),
        }
    }
}

impl Text {
    pub fn family(&self) -> &str {
        self.font_family.as_deref().unwrap_or(DEFAULT_FAMILY)
    }

    pub fn font_size(&self) -> f32 {
        self.font_size.map_or(DEFAULT_FONT_SIZE, |size| size.0)
    }

    pub fn weight(&self) -> FontWeight {
        self.font_weight.unwrap_or(FontWeight::REGULAR)
    }

    /// Its `textGrowth`, or when unset the one its sizes imply.
    pub fn growth(&self) -> TextGrowth {
        self.text_growth.unwrap_or(match (self.size.width, self.size.height) {
            (Some(_), Some(_)) => TextGrowth::FixedWidthHeight,
            (Some(_), None) => TextGrowth::FixedWidth,
            _ => TextGrowth::Auto,
        })
    }
}

impl NodeKind for Text {
    fn size(&self) -> (Sizing, Sizing) {
        let Size { width, height } = self.size;
        match self.growth() {
            TextGrowth::Auto => (Sizing::FitContent, Sizing::FitContent),
            TextGrowth::FixedWidth => (width.unwrap_or(Sizing::FitContent), Sizing::FitContent),
            TextGrowth::FixedWidthHeight => (
                width.unwrap_or(Sizing::FitContent),
                height.unwrap_or(Sizing::FitContent),
            ),
        }
    }

    /// Its families, but the bundled one, at its weight: those its
    /// characters may draw in.
    fn assets(&self) -> Vec<AssetKey> {
        let weight = crate::fonts::snapped(self.weight().0);
        crate::fonts::families(self)
            .iter()
            .filter(|family| !family.eq_ignore_ascii_case(crate::fonts::BUNDLED_FAMILY))
            .map(|family| AssetKey::font(family, weight))
            .collect()
    }

    fn measure(&self, typesetter: &Typesetter, width: Option<f32>) -> Option<[f32; 2]> {
        let wrap = match self.growth() {
            TextGrowth::Auto => None,
            TextGrowth::FixedWidth | TextGrowth::FixedWidthHeight => width,
        };
        Some(typesetter.measure(self, wrap))
    }

    fn paint(&self, size: (f32, f32), painter: &mut dyn Painter) {
        painter.text(self, size);
    }
}
