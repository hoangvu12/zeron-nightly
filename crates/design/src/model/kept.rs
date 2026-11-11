//! Reading and writing a [`Node`] so that what the model doesn't know is
//! kept: a node of a type it knows keeps its unknown properties in
//! [`Node::extra`], and one of a type it doesn't becomes [`Opaque`], all
//! its properties kept.
//!
//! A property is known when the model writes it back, or when the model
//! reads a different value of it differently (a default it doesn't write,
//! like `x: 0`). No list of property names to keep up to date: a property a
//! type gains is known as soon as its type reads it.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::{Kind, Node, Opaque, paint, warn};

/// The properties every node has.
#[derive(Deserialize, PartialEq)]
struct Common {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    absolute: bool,
    #[serde(default)]
    rotation: f32,
    #[serde(default, rename = "flipX")]
    flip_x: bool,
    #[serde(default, rename = "flipY")]
    flip_y: bool,
    #[serde(default = "opaque", deserialize_with = "opacity")]
    opacity: f32,
}

fn opaque() -> f32 {
    1.
}

/// Kept within 0 to 1, with a warning past them.
fn opacity<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    let mut props = Map::new();
    props.insert("opacity".into(), Value::deserialize(deserializer)?);
    paint::opacity(&props).map_err(D::Error::custom)
}

#[derive(Serialize)]
struct Written<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    name: &'a str,
    #[serde(skip_serializing_if = "is_none_or_zero")]
    x: Option<f32>,
    #[serde(skip_serializing_if = "is_none_or_zero")]
    y: Option<f32>,
    #[serde(skip_serializing_if = "is_none_or_zero")]
    rotation: Option<f32>,
    #[serde(rename = "flipX", skip_serializing_if = "std::ops::Not::not")]
    flip_x: bool,
    #[serde(rename = "flipY", skip_serializing_if = "std::ops::Not::not")]
    flip_y: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    opacity: Option<f32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    absolute: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    kind: Option<&'a Kind>,
    #[serde(flatten)]
    extra: &'a Map<String, Value>,
}

fn is_none_or_zero(value: &Option<f32>) -> bool {
    value.is_none_or(|value| value == 0.)
}

impl Node {
    /// What's written: all of an [`Opaque`] node but its id is in its
    /// `extra`, as it was read.
    fn written(&self) -> Written<'_> {
        let known = self.kind.is_known();
        Written {
            id: &self.id,
            name: if known { &self.name } else { "" },
            x: known.then_some(self.x),
            y: known.then_some(self.y),
            rotation: known.then_some(self.rotation),
            flip_x: known && self.flip_x,
            flip_y: known && self.flip_y,
            opacity: (known && self.opacity != 1.).then_some(self.opacity),
            absolute: known && self.absolute,
            kind: known.then_some(&self.kind),
            extra: &self.extra,
        }
    }

    /// A node from its JSON object, keeping what the model doesn't know.
    pub fn from_props(props: Map<String, Value>) -> Result<Self, String> {
        let type_name = match props.get("type") {
            Some(Value::String(name)) => name.clone(),
            _ => return Err("a node needs a \"type\"".into()),
        };
        if !Kind::NAMES.contains(&type_name.as_str()) {
            // Read as little as can be: whatever a newer Zeron put in it,
            // only its id must be a string for it to load.
            let mut extra = props;
            let id = match extra.remove("id") {
                Some(Value::String(id)) => id,
                _ => return Err(format!("a \"{type_name}\" node needs an \"id\"")),
            };
            let name = extra.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            let at = |key: &str| extra.get(key).and_then(Value::as_f64).unwrap_or_default() as f32;
            let (x, y) = (at("x"), at("y"));
            return Ok(Self {
                id,
                name,
                x,
                y,
                rotation: 0.,
                flip_x: false,
                flip_y: false,
                opacity: 1.,
                absolute: false,
                kind: Kind::Opaque(Opaque { type_name }),
                extra,
            });
        }
        let read = |props: &Map<String, Value>| -> Result<(Common, Kind), String> {
            let value = Value::Object(props.clone());
            let common = Common::deserialize(&value).map_err(|e| e.to_string())?;
            let kind = Kind::deserialize(&value).map_err(|e| e.to_string())?;
            Ok((common, kind))
        };
        let (common, kind) = read(&props)?;
        let mut node = Self {
            id: common.id,
            name: common.name,
            x: common.x,
            y: common.y,
            rotation: common.rotation,
            flip_x: common.flip_x,
            flip_y: common.flip_y,
            opacity: common.opacity,
            absolute: common.absolute,
            kind,
            extra: Map::new(),
        };
        let Value::Object(written) = serde_json::to_value(node.written()).map_err(|e| e.to_string())?
        else {
            unreachable!("a node is written as an object");
        };
        // Probed without its children, so a probe reads one node, not its
        // whole subtree.
        let mut shallow = props.clone();
        shallow.remove("children");
        let mut reference = None;
        for (key, value) in &props {
            if written.contains_key(key) {
                continue;
            }
            let unknown = if key == "children" {
                node.kind.get_mut().children_mut().is_none()
            } else {
                // Quietly: the node's warnings came with its first read.
                let quiet = |props: &Map<String, Value>| warn::collect(|| read(props)).0;
                let reference = match &reference {
                    Some(reference) => reference,
                    None => reference.insert(quiet(&shallow)?),
                };
                others_than(value).into_iter().all(|other| {
                    let mut probe = shallow.clone();
                    probe.insert(key.clone(), other);
                    quiet(&probe).is_ok_and(|read| read == *reference)
                })
            };
            if unknown {
                node.extra.insert(key.clone(), value.clone());
            }
        }
        Ok(node)
    }

    /// The keys of [`Node::extra`] the model doesn't read: unknown
    /// properties of a known type (none for an [`Opaque`] node).
    pub fn unknown_properties(&self) -> impl Iterator<Item = &str> {
        let extra = if self.kind.is_known() { Some(&self.extra) } else { None };
        extra.into_iter().flat_map(|extra| extra.keys().map(String::as_str))
    }
}

/// Values of about the same shape as `value`, one of which a model reading
/// `value` would read differently. Several for a number, as a model may
/// keep a number within bounds (an opacity of 2 and of 3 both read as 1, a
/// radius of -5 and of -4 as 0), and for an object, which a model may drop.
fn others_than(value: &Value) -> Vec<Value> {
    match value {
        Value::Null => vec![json!(1)],
        Value::Bool(b) => vec![json!(!b)],
        Value::Number(n) => {
            let n = n.as_f64().unwrap_or_default();
            vec![json!(n.abs() + 1.), json!(if n == 0. { 1. } else { 0. })]
        }
        Value::String(s) => vec![json!(format!("{s}~"))],
        Value::Array(items) => {
            let mut items = items.clone();
            items.push(Value::Null);
            vec![Value::Array(items)]
        }
        Value::Object(_) => vec![Value::Null, json!(1)],
    }
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.written().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Object(props) => Node::from_props(props).map_err(D::Error::custom),
            other => Err(D::Error::custom(format!("a node is an object, not {other}"))),
        }
    }
}
