//! Strokes: a line along a node's outline, painted with paints like a
//! fill, with a width per side, an alignment to the outline, and its cap,
//! join and dashes.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::Paint;
use super::paint::{paints_from, paints_to_value};
use super::warn::{at_least_zero, choice, number, warn};

/// The stroke property group, flattened into the types that have one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "write",
        deserialize_with = "read"
    )]
    pub stroke: Option<StrokeStyle>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeStyle {
    pub fill: Vec<Paint>,
    pub thickness: Sides,
    pub align: Align,
    pub join: Join,
    pub cap: Cap,
    /// Dash and gap lengths in turn; none for a solid line.
    pub dashes: Vec<f32>,
}

/// A width for each side of a node's box.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sides {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Sides {
    pub fn all(width: f32) -> Self {
        Self { top: width, right: width, bottom: width, left: width }
    }

    /// The width every side shares, if they do.
    pub fn uniform(&self) -> Option<f32> {
        (self.top == self.right && self.right == self.bottom && self.bottom == self.left)
            .then_some(self.top)
    }

    pub fn scaled(&self, factor: f32) -> Self {
        Self {
            top: self.top * factor,
            right: self.right * factor,
            bottom: self.bottom * factor,
            left: self.left * factor,
        }
    }
}

/// Where the line sits on the outline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Inside,
    Center,
    Outside,
}

const ALIGNS: &[(&str, Align)] = &[
    ("inside", Align::Inside),
    ("center", Align::Center),
    ("outside", Align::Outside),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Join {
    #[default]
    Miter,
    Round,
    Bevel,
}

const JOINS: &[(&str, Join)] = &[("miter", Join::Miter), ("round", Join::Round), ("bevel", Join::Bevel)];

/// How dashes end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cap {
    #[default]
    None,
    Round,
    Square,
}

const CAPS: &[(&str, Cap)] = &[("none", Cap::None), ("round", Cap::Round), ("square", Cap::Square)];

fn name_of<T: PartialEq + Copy>(options: &[(&'static str, T)], value: T) -> &'static str {
    options.iter().find(|(_, v)| *v == value).map(|(name, _)| *name).unwrap_or_default()
}

impl StrokeStyle {
    fn to_value(&self) -> Value {
        let mut props = Map::new();
        props.insert("fill".into(), paints_to_value(&self.fill));
        props.insert(
            "thickness".into(),
            match self.thickness.uniform() {
                Some(width) => json!(width),
                None => {
                    let Sides { top, right, bottom, left } = self.thickness;
                    json!({ "top": top, "right": right, "bottom": bottom, "left": left })
                }
            },
        );
        props.insert("align".into(), json!(name_of(ALIGNS, self.align)));
        if self.join != Join::default() {
            props.insert("join".into(), json!(name_of(JOINS, self.join)));
        }
        if self.cap != Cap::default() {
            props.insert("cap".into(), json!(name_of(CAPS, self.cap)));
        }
        if !self.dashes.is_empty() {
            props.insert("dashPattern".into(), json!(self.dashes));
        }
        Value::Object(props)
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Object(props) = value else {
            return Err(format!("a stroke is {{ fill, thickness, align, .. }}, not {value}"));
        };
        let thickness = match props.get("thickness") {
            None | Some(Value::Null) => Sides::all(1.),
            Some(Value::Number(_)) => Sides::all(at_least_zero(
                "a stroke's thickness",
                number(props, "thickness")?.unwrap_or(1.),
            )),
            Some(Value::Object(sides)) => {
                let side = |key: &str| -> Result<f32, String> {
                    Ok(at_least_zero("a stroke's thickness", number(sides, key)?.unwrap_or(0.)))
                };
                Sides { top: side("top")?, right: side("right")?, bottom: side("bottom")?, left: side("left")? }
            }
            Some(other) => {
                return Err(format!(
                    "a stroke's \"thickness\" is a number or {{ top, right, bottom, left }}, not {other}"
                ));
            }
        };
        let dashes = match props.get("dashPattern") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(lengths)) => lengths
                .iter()
                .map(|length| {
                    length
                        .as_f64()
                        .map(|l| at_least_zero("a dash length", l as f32))
                        .ok_or_else(|| format!("a dash pattern is a list of lengths, not {length}"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(other) => return Err(format!("a dash pattern is a list of lengths, not {other}")),
        };
        let dashes = if dashes.iter().sum::<f32>() > 0. { dashes } else { Vec::new() };
        if !dashes.is_empty() && thickness.uniform().is_none() {
            warn("dashes need one thickness on every side; this stroke is drawn solid");
        }
        Ok(Self {
            fill: props.get("fill").map(paints_from).transpose()?.unwrap_or_default(),
            thickness,
            align: choice(props, "align", "stroke align", ALIGNS)?.unwrap_or_default(),
            join: choice(props, "join", "stroke join", JOINS)?.unwrap_or_default(),
            cap: choice(props, "cap", "stroke cap", CAPS)?.unwrap_or_default(),
            dashes,
        })
    }
}

fn write<S: Serializer>(stroke: &Option<StrokeStyle>, serializer: S) -> Result<S::Ok, S::Error> {
    stroke.as_ref().map(StrokeStyle::to_value).serialize(serializer)
}

fn read<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<StrokeStyle>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        value => StrokeStyle::from_value(&value).map(Some).map_err(D::Error::custom),
    }
}
