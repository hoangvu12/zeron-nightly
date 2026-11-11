use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::{NodeKind, Sizing, kept};
use crate::render::Painter;

/// An Instance: a copy of a Component (`ref`, the Component's id) that
/// follows it. Its `descendants` override the nodes inside it, each keyed
/// by the node's id in the Component, by a slash path into a nested
/// Instance (`button/label`) or by a name only one of them has: an object
/// of properties sets them, a node (with a `type`) replaces it outright.
/// Any other property overrides the Component's own.
///
/// It's drawn expanded ([`crate::resolve`]), so as stored it has no size
/// and draws nothing itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Instance {
    #[serde(rename = "ref")]
    pub component: String,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub descendants: Map<String, Value>,
    /// The Component's own properties it overrides.
    #[serde(flatten)]
    pub overrides: Map<String, Value>,
}

impl<'de> Deserialize<'de> for Instance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut props = Map::<String, Value>::deserialize(deserializer)?;
        let component = match props.remove("ref") {
            Some(Value::String(component)) if !component.is_empty() => component,
            _ => return Err(D::Error::custom("an instance needs a \"ref\": its Component's id")),
        };
        let descendants = match props.remove("descendants") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(descendants)) => descendants,
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "an instance's \"descendants\" is an object from node to properties, not {other}"
                )));
            }
        };
        // What every node has, and its children (an Instance takes none of
        // its own), aren't overrides.
        props.retain(|key, _| key != "type" && key != "children" && !kept::COMMON.contains(&key.as_str()));
        Ok(Self { component, descendants, overrides: props })
    }
}

impl NodeKind for Instance {
    fn size(&self) -> (Sizing, Sizing) {
        (Sizing::Fixed(0.), Sizing::Fixed(0.))
    }

    fn paint(&self, _size: (f32, f32), _painter: &mut dyn Painter) {}
}
