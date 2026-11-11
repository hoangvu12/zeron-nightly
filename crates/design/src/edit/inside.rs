//! Edits to a node inside an Instance, named by its slash path
//! (`card/title`): they become the Instance's overrides, as the node
//! follows its Component. Update sets properties there and Replace puts a
//! new node in its place; deleting or moving one is refused (an Instance
//! has its Component's nodes), and disabling it hides it instead.

use serde_json::{Map, Value};

use super::update::{set, set_warnings};
use super::{Draft, EditError, unsupported_warnings};
use crate::model::{Design, Kind, Node};
use crate::resolve::{self, Resolve};

/// Whether `id` names a node inside an Instance.
pub(super) fn is_inside(id: &str) -> bool {
    id.contains('/')
}

/// The Design with its Instances expanded, as slash paths name their nodes.
pub(super) fn expanded(design: &Design) -> Design {
    let how = Resolve { instances: true, tokens: false, enabled_only: false };
    resolve::resolved(design, how).0
}

impl Draft {
    /// Update on the node `path` inside an Instance.
    pub(super) fn update_inside(&mut self, path: &str, props: Map<String, Value>) -> Result<(), EditError> {
        let mut node = self.inside(path)?;
        // Checked as an Update of the node as it's drawn, then kept as the
        // Instance's override.
        let unsupported = set(&mut node, &props, "Update")?;
        let mut warnings = unsupported_warnings(&node, "Update", unsupported);
        self.check_one(&node, &mut warnings)
            .map_err(|e| EditError::Invalid(format!("Update: {e}")))?;
        let (instance, key) = path.split_once('/').expect("a path has a slash");
        let descendants = self.overrides(instance)?;
        resolve::merge_override(descendants, key, &Value::Object(settable(&props)));
        warnings.extend(set_warnings(&node, &props));
        for key in props.keys() {
            self.unwarn(path, key);
        }
        self.warnings.extend(warnings);
        self.changed.insert(instance.to_owned());
        Ok(())
    }

    /// Replace on the node `path` inside an Instance: the new node takes
    /// its place, and its path. Returns the path.
    pub(super) fn replace_inside(&mut self, path: &str, node: Value) -> Result<String, EditError> {
        let expanded = expanded(&self.design);
        let depth = expanded.depth_of(path).ok_or_else(|| EditError::NotFound(path.into()))?;
        let built = self.build(node, depth - 1, "Replace")?;
        let (instance, key) = path.split_once('/').expect("a path has a slash");
        let Value::Object(mut written) = serde_json::to_value(&built.node).expect("a node serializes") else {
            unreachable!("a node is written as an object");
        };
        written.remove("id");
        let root = built.node.id.clone();
        self.overrides(instance)?.insert(key.to_owned(), Value::Object(written));
        // Its nodes are under the path, as the Instance draws them.
        let under = |id: &str| if id == root { path.to_owned() } else { format!("{path}/{id}") };
        for (name, id) in built.made {
            self.created.push((name, under(&id)));
        }
        self.warnings.extend(built.warnings.into_iter().map(|mut warning| {
            warning.node = warning.node.as_deref().map(under);
            warning
        }));
        self.changed.insert(instance.to_owned());
        Ok(path.to_owned())
    }

    /// Refuses `verb` on the node `id` when it's inside an Instance.
    pub(super) fn outside(&self, id: Option<&str>, verb: &str) -> Result<(), EditError> {
        match id {
            Some(id) if is_inside(id) => Err(EditError::Invalid(format!(
                "{verb} can't change what's in \"{id}\": it's inside an Instance, which has its Component's \
                 nodes. Change the Component, or Update or Replace the node in this Instance only"
            ))),
            _ => Ok(()),
        }
    }

    /// The node `path` inside an Instance, as the Instance draws it.
    fn inside(&self, path: &str) -> Result<Node, EditError> {
        expanded(&self.design)
            .find(path)
            .cloned()
            .ok_or_else(|| EditError::NotFound(path.into()))
    }

    /// The overrides of the Instance `id`.
    fn overrides(&mut self, id: &str) -> Result<&mut Map<String, Value>, EditError> {
        match self.design.find_mut(id).map(|node| &mut node.kind) {
            Some(Kind::Instance(instance)) => Ok(&mut instance.descendants),
            _ => Err(EditError::NotFound(id.into())),
        }
    }
}

/// `props` as an Instance's override keeps them: without what Update
/// leaves as it is (its id, type and ref, and children), which `set`
/// checked are.
fn settable(props: &Map<String, Value>) -> Map<String, Value> {
    props
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "type" | "ref" | "children"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
