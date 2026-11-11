//! Auto-layout's properties: how a node is sized ([`Sizing`]) and how a
//! Frame lays its children out ([`Layout`]). The pass that applies them is
//! [`crate::layout`].

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// One side of a node's size: `120`, `"fit_content"` or `"fill_container"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sizing {
    /// Pixels at 1x.
    Fixed(f32),
    /// As big as its children need.
    FitContent,
    /// The room its parent has for it.
    FillContainer,
}

const FIT_CONTENT: &str = "fit_content";
const FILL_CONTAINER: &str = "fill_container";

impl Serialize for Sizing {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Sizing::Fixed(pixels) => pixels.serialize(serializer),
            Sizing::FitContent => FIT_CONTENT.serialize(serializer),
            Sizing::FillContainer => FILL_CONTAINER.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Sizing {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Number(n) => match n.as_f64() {
                Some(pixels) if pixels >= 0. && pixels.is_finite() => Ok(Sizing::Fixed(pixels as f32)),
                _ => Err(D::Error::custom(format!("a size is not negative, not {n}"))),
            },
            Value::String(s) if s == FIT_CONTENT => Ok(Sizing::FitContent),
            Value::String(s) if s == FILL_CONTAINER => Ok(Sizing::FillContainer),
            other => Err(D::Error::custom(format!(
                "a size is a number of pixels, \"{FIT_CONTENT}\" or \"{FILL_CONTAINER}\", not {other}"
            ))),
        }
    }
}

/// The layout property group, flattened into Frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    #[serde(default, skip_serializing_if = "is_default")]
    pub layout: Direction,
    /// Between each child and the next.
    #[serde(default, skip_serializing_if = "is_default")]
    pub gap: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub padding: Padding,
    /// Where children sit along the layout's direction.
    #[serde(default, skip_serializing_if = "is_default")]
    pub justify_content: Justify,
    /// Where children sit across it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub align_items: Align,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Children sit at their own x and y.
    #[default]
    None,
    /// In a row, left to right.
    Horizontal,
    /// In a column, top to bottom.
    Vertical,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    /// The first and last child at the ends, the room left between them.
    SpaceBetween,
    /// The room left split evenly around each child.
    SpaceAround,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// Room inside a Frame's edges, written as one value for every side, two
/// (top and bottom, left and right) or four (top, right, bottom, left).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Padding {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Serialize for Padding {
    /// In the shortest form that says it.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Self { top, right, bottom, left } = *self;
        if top == right && right == bottom && bottom == left {
            top.serialize(serializer)
        } else if top == bottom && right == left {
            [top, right].serialize(serializer)
        } else {
            [top, right, bottom, left].serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Padding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let invalid = || D::Error::custom(format!("padding is one, two or four numbers, not {value}"));
        let sides: Vec<f32> = match &value {
            Value::Number(_) => vec![value.as_f64().ok_or_else(invalid)? as f32],
            Value::Array(sides) => sides
                .iter()
                .map(|side| side.as_f64().map(|side| side as f32).ok_or_else(invalid))
                .collect::<Result<_, _>>()?,
            _ => return Err(invalid()),
        };
        match sides[..] {
            [all] => Ok(Self { top: all, right: all, bottom: all, left: all }),
            [vertical, horizontal] => Ok(Self {
                top: vertical,
                right: horizontal,
                bottom: vertical,
                left: horizontal,
            }),
            [top, right, bottom, left] => Ok(Self { top, right, bottom, left }),
            _ => Err(invalid()),
        }
    }
}
