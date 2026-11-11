//! `Update`: setting properties on a node already in the Design.

use serde_json::{Map, Value};

use super::{Draft, EditError, Warning, unsupported_warnings};
use crate::model::{Kind, Node};

/// What Update can't change: a node's identity.
const FIXED: &[&str] = &["id", "type", "ref"];

impl Draft {
    /// Sets `props` on the node `id`, each as Insert would take it; `null`
    /// unsets one. Its id, type and ref can't change, nor can its children
    /// (Insert, Move, Delete and Replace change those), but a node as `Get`
    /// read it updates as it is, so what Get reads can be changed and sent
    /// back. A node inside an Instance (`card/title`) takes them as that
    /// Instance's override.
    pub fn update(&mut self, id: &str, props: Value) -> Result<(), EditError> {
        let props = properties(props, "Update")?;
        if super::inside::is_inside(id) {
            return self.update_inside(id, props);
        }
        let node = self.design.find_mut(id).ok_or_else(|| EditError::NotFound(id.into()))?;
        let unsupported = set(node, &props, "Update")?;
        let node = self.design.find(id).expect("the node was just updated");
        let mut warnings = unsupported_warnings(node, "Update", unsupported);
        warnings.extend(set_warnings(node, &props));
        for key in props.keys() {
            self.unwarn(id, key);
        }
        self.warnings.extend(warnings);
        self.changed.insert(id.to_owned());
        Ok(())
    }
}

/// `props` as an object of properties to set.
pub(super) fn properties(props: Value, verb: &str) -> Result<Map<String, Value>, EditError> {
    match props {
        Value::Object(props) => Ok(props),
        Value::Null => Ok(Map::new()),
        other => Err(EditError::Invalid(format!(
            "{verb} takes the properties to set as an object, not {other}"
        ))),
    }
}

/// Sets `props` on `node`, as [`Node::set_props`] does, once it's checked
/// they leave what Update can't change as it is. Returns the model's
/// warnings about values it doesn't support.
pub(super) fn set(node: &mut Node, props: &Map<String, Value>, verb: &str) -> Result<Vec<String>, EditError> {
    if !node.kind.is_known() {
        return Err(EditError::Invalid(format!(
            "\"{}\" is a {} node, which Zeron doesn't know: it takes no edits",
            node.id,
            node.kind.name()
        )));
    }
    let mut settable = Map::new();
    for (key, value) in props {
        if FIXED.contains(&key.as_str()) {
            let current = match key.as_str() {
                "id" => Some(Value::String(node.id.clone())),
                "type" => Some(Value::String(node.kind.name().to_owned())),
                _ => match &node.kind {
                    Kind::Instance(instance) => Some(Value::String(instance.component.clone())),
                    _ => node.extra.get(key).cloned(),
                },
            };
            if current.as_ref() != Some(value) {
                return Err(EditError::Invalid(format!(
                    "{verb} can't change a node's \"{key}\" (Replace puts a new node in its place)"
                )));
            }
        } else if key == "children" {
            let current = serde_json::to_value(node.kind.get().children()).expect("nodes serialize");
            if !read_from(value, &current) {
                return Err(EditError::Invalid(format!(
                    "{verb} doesn't change children: Insert, Move, Delete or Replace them"
                )));
            }
        } else {
            settable.insert(key.clone(), value.clone());
        }
    }
    node.set_props(&settable).map_err(|e| EditError::Invalid(format!("{verb}: {e}")))
}

/// Whether `given` is `current` as `Get` reads it: the same, but that
/// nodes past the depth read have no `children`, and numbers may have come
/// back through JavaScript as whole numbers.
fn read_from(given: &Value, current: &Value) -> bool {
    match (given, current) {
        (Value::Object(given), Value::Object(current)) => {
            let props = |node: &Map<String, Value>| node.keys().filter(|key| *key != "children").count();
            props(given) == props(current)
                && given.iter().all(|(key, value)| match current.get(key) {
                    Some(current) => read_from(value, current),
                    None => key == "children" && value.as_array().is_some_and(Vec::is_empty),
                })
        }
        (Value::Array(given), Value::Array(current)) => {
            given.len() == current.len() && given.iter().zip(current).all(|(given, current)| read_from(given, current))
        }
        (Value::Number(given), Value::Number(current)) => given.as_f64() == current.as_f64(),
        _ => given == current,
    }
}

/// A warning for each of `props` set on `node` that the model doesn't know.
pub(super) fn set_warnings(node: &Node, props: &Map<String, Value>) -> Vec<Warning> {
    node.unknown_properties()
        .filter(|property| props.contains_key(*property))
        .map(|property| Warning::unknown(node, property))
        .collect()
}
