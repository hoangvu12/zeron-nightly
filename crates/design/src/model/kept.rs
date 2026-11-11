//! Reading and writing a [`Node`] so that what the model doesn't know is
//! kept: a node of a type it knows keeps its unknown properties in
//! [`Node::extra`], and one of a type it doesn't becomes [`Opaque`], all
//! its properties kept.
//!
//! A property is known when the model writes it back, or when the model
//! reads a different value of it differently (a default it doesn't write,
//! like `x: 0`). No list of property names to keep up to date: a property a
//! type gains is known as soon as its type reads it.
//!
//! A property that refers to a Token (`$name`, anywhere in its value) is
//! kept as written in [`Node::refs`], for the model to read once resolved.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::{Kind, Node, Opaque, paint, token, warn};

/// The properties every node has, by name.
pub(super) const COMMON: &[&str] = &[
    "id", "name", "x", "y", "absolute", "rotation", "flipX", "flipY", "opacity", "inProgress", "enabled", "theme",
];

/// Properties never taken as referring to Tokens: what a node is, and
/// what's resolved along with what it holds.
const NOT_REFS: &[&str] = &["id", "type", "name", "children", "descendants", "ref"];

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
    #[serde(default, rename = "inProgress")]
    in_progress: bool,
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    theme: Map<String, Value>,
}

fn enabled() -> bool {
    true
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
    #[serde(rename = "inProgress", skip_serializing_if = "std::ops::Not::not")]
    in_progress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    theme: Option<&'a Map<String, Value>>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    kind: Option<&'a Kind>,
    #[serde(flatten)]
    refs: &'a Map<String, Value>,
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
            in_progress: known && self.in_progress,
            enabled: (known && !self.enabled).then_some(false),
            theme: (known && !self.theme.is_empty()).then_some(&self.theme),
            kind: known.then_some(&self.kind),
            refs: &self.refs,
            extra: &self.extra,
        }
    }

    /// A node from its JSON object, keeping what the model doesn't know.
    pub fn from_props(mut props: Map<String, Value>) -> Result<Self, String> {
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
                in_progress: false,
                enabled: true,
                theme: Map::new(),
                kind: Kind::Opaque(Opaque { type_name }),
                refs: Map::new(),
                extra,
            });
        }
        let refs: Map<String, Value> = props
            .iter()
            .filter(|(key, value)| !NOT_REFS.contains(&key.as_str()) && token::refers(value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        props.retain(|key, _| !refs.contains_key(key));
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
            in_progress: common.in_progress,
            enabled: common.enabled,
            theme: common.theme,
            kind,
            refs,
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

    /// Sets `props` on the node as written (`null` unsets one), its
    /// children kept, so a property it kept unknown or referring to a Token
    /// is replaced, not written twice. Returns the model's warnings about
    /// values it doesn't support; on an error, the node is as it was.
    pub fn set_props(&mut self, props: &Map<String, Value>) -> Result<Vec<String>, String> {
        // Written without its children, so a node's whole subtree isn't.
        let children = self.kind.get_mut().children_mut().map(std::mem::take);
        let set = self.with_props(props).map(|(node, warnings)| {
            *self = node;
            warnings
        });
        if let (Some(children), Some(slot)) = (children, self.kind.get_mut().children_mut()) {
            *slot = children;
        }
        set
    }

    fn with_props(&self, props: &Map<String, Value>) -> Result<(Node, Vec<String>), String> {
        let Value::Object(mut written) = serde_json::to_value(self).map_err(|e| e.to_string())? else {
            unreachable!("a node is written as an object");
        };
        for (key, value) in props {
            if value.is_null() {
                written.remove(key);
            } else {
                written.insert(key.clone(), value.clone());
            }
        }
        let (node, warnings) = warn::collect(|| Node::from_props(written));
        Ok((node?, warnings))
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
