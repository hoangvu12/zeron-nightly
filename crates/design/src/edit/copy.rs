//! `Copy`: a node and its descendants again, with new ids.

use std::collections::HashMap;

use serde_json::Value;

use super::update::{properties, set, set_warnings};
use super::{Built, Draft, EditError, MAX_DEPTH, height, unsupported_warnings};
use crate::model::Node;

impl Draft {
    /// Copies the node `id` with its descendants as the last child of
    /// `parent`, or onto the Canvas without one. The copy and every node
    /// in it get new ids. `props` are set on the copy as Update sets them,
    /// but for its `descendants`: an object from the id of a node inside
    /// `id` to properties set on that node's copy. Returns the copy's id.
    pub fn copy(&mut self, id: &str, parent: Option<&str>, props: Value) -> Result<String, EditError> {
        let mut props = properties(props, "Copy")?;
        let descendants = match props.remove("descendants") {
            None | Some(Value::Null) => Default::default(),
            Some(descendants) => properties(descendants, "Copy's descendants")?,
        };
        let mut copy = self.design.find(id).ok_or_else(|| EditError::NotFound(id.into()))?.clone();
        let depth = self.depth_inside(parent)?;
        self.children_of(parent)?;
        if depth + height(&copy) > MAX_DEPTH {
            return Err(EditError::Invalid(format!("Copy: nodes nest at most {MAX_DEPTH} deep")));
        }
        if let Some(unknown) = first_unknown(&copy) {
            return Err(EditError::Invalid(format!(
                "Copy: \"{}\" is a {} node, which Zeron doesn't know, so it can't be copied",
                unknown.id,
                unknown.kind.name()
            )));
        }
        let mut warnings = Vec::new();
        for (descendant, props) in descendants {
            let props = properties(props, "Copy's descendants")?;
            let inside = find_below(&mut copy, &descendant).ok_or_else(|| {
                EditError::Invalid(format!("Copy: no node inside \"{id}\" has the id \"{descendant}\""))
            })?;
            let unsupported = set(inside, &props, "Copy")?;
            warnings.extend(unsupported_warnings(inside, "Copy", unsupported));
            warnings.extend(set_warnings(inside, &props));
        }
        let unsupported = set(&mut copy, &props, "Copy")?;
        warnings.extend(unsupported_warnings(&copy, "Copy", unsupported));
        warnings.extend(set_warnings(&copy, &props));

        let mut renamed = HashMap::new();
        self.renew_ids(&mut copy, &mut renamed);
        for warning in &mut warnings {
            warning.node = warning.node.as_ref().map(|old| renamed[old].clone());
        }
        let built = Built {
            made: vec![(copy.name.clone(), copy.id.clone())],
            node: copy,
            warnings,
        };
        let id = self.attach(parent, None, built);
        self.changed.extend(renamed.into_values());
        Ok(id)
    }

    /// Gives `node` and its descendants new ids, noting each old id's new one.
    fn renew_ids(&mut self, node: &mut Node, renamed: &mut HashMap<String, String>) {
        let id = self.new_id();
        renamed.insert(std::mem::replace(&mut node.id, id.clone()), id);
        if let Some(children) = node.kind.get_mut().children_mut() {
            for child in children {
                self.renew_ids(child, renamed);
            }
        }
    }
}

/// The node `id` among `node`'s descendants.
fn find_below<'a>(node: &'a mut Node, id: &str) -> Option<&'a mut Node> {
    for child in node.kind.get_mut().children_mut()? {
        if child.id == id {
            return Some(child);
        }
        if let Some(found) = find_below(child, id) {
            return Some(found);
        }
    }
    None
}

/// The first node of a type the model doesn't know in `node`'s subtree:
/// its ids are kept as written, so it can't take new ones.
fn first_unknown(node: &Node) -> Option<&Node> {
    if !node.kind.is_known() {
        return Some(node);
    }
    node.kind.get().children().iter().find_map(first_unknown)
}
