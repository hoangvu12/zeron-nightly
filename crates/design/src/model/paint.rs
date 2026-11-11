//! Paint: what fills a node's area. A `fill` is one paint or a list of them
//! drawn in order; a solid colour is written as its hex string.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

/// The fill property group, flattened into the types that have a fill.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "one_or_many",
        deserialize_with = "paints"
    )]
    pub fill: Vec<Paint>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Paint {
    Solid(Color),
}

impl Paint {
    fn to_value(self) -> Value {
        match self {
            Paint::Solid(color) => json!(color.to_hex()),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(hex) => Color::parse(hex).map(Paint::Solid),
            Value::Object(paint) => match paint.get("type").and_then(Value::as_str) {
                Some("color") => match paint.get("color") {
                    Some(Value::String(hex)) => Color::parse(hex).map(Paint::Solid),
                    _ => Err("a color paint needs a \"color\" hex string".into()),
                },
                Some(other) => Err(format!("unsupported paint type \"{other}\" (supported: color)")),
                None => Err("a paint needs a \"type\"".into()),
            },
            _ => Err(format!("a paint is a hex colour or a paint object, not {value}")),
        }
    }
}

fn one_or_many<S: Serializer>(paints: &[Paint], serializer: S) -> Result<S::Ok, S::Error> {
    match paints {
        [paint] => paint.to_value().serialize(serializer),
        paints => paints
            .iter()
            .map(|paint| paint.to_value())
            .collect::<Vec<_>>()
            .serialize(serializer),
    }
}

fn paints<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Paint>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Array(paints) => paints
            .iter()
            .map(|paint| Paint::from_value(paint).map_err(D::Error::custom))
            .collect(),
        Value::Null => Ok(Vec::new()),
        paint => Paint::from_value(&paint).map(|paint| vec![paint]).map_err(D::Error::custom),
    }
}

/// An sRGB colour with alpha, written `#RRGGBB` or `#RRGGBBAA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// `#RGB`, `#RRGGBB` or `#RRGGBBAA`.
    pub fn parse(hex: &str) -> Result<Self, String> {
        let invalid = || format!("\"{hex}\" is not a colour (#RGB, #RRGGBB or #RRGGBBAA)");
        let digits = hex.strip_prefix('#').ok_or_else(invalid)?;
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).map_err(|_| invalid());
        match digits.len() {
            3 => {
                let nibble = |at: usize| {
                    u8::from_str_radix(&digits[at..at + 1], 16)
                        .map(|n| n * 17)
                        .map_err(|_| invalid())
                };
                Ok(Self { r: nibble(0)?, g: nibble(1)?, b: nibble(2)?, a: 255 })
            }
            6 => Ok(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: 255 }),
            8 => Ok(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }),
            _ => Err(invalid()),
        }
    }

    pub fn to_hex(self) -> String {
        let Self { r, g, b, a } = self;
        if a == 255 {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }
}
