//! `Copy`: a node and its descendants again, with new ids; or, of a
//! Component, an Instance of it.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::inside::{expanded, is_inside};
use super::update::{properties, set, set_warnings};
use super::{Built, Draft, EditError, MAX_DEPTH, height, unsupported_warnings};
use crate::model::{Kind, Node};

impl Draft {
    /// Copies the node `id` with its descendants as the last child of
    /// `parent`, or onto the Canvas without one. The copy and every node
    /// in it get new ids, listed by name in the call's map as Insert's are.
    /// `props` are set on the copy as Update sets them, but for its
    /// `descendants`: an object from the id of a node inside `id` to
    /// properties set on that node's copy. Returns the copy's id.
    ///
    /// A copy of a Component is an Instance of it, `props` its overrides
    /// and `descendants` its descendants' (an Instance's copy adds them to
    /// its own). A node inside an Instance (`card/title`) is copied as the
    /// Instance draws it.
    pub fn copy(&mut self, id: &str, parent: Option<&str>, props: Value) -> Result<String, EditError> {
        self.outside(parent, "Copy")?;
        let mut props = properties(props, "Copy")?;
        let descendants = match props.remove("descendants") {
            None | Some(Value::Null) => Default::default(),
            Some(descendants) => properties(descendants, "Copy's descendants")?,
        };
        let source = match is_inside(id) {
            true => expanded(&self.design).find(id).cloned(),
            false => self.design.find(id).cloned(),
        };
        let mut copy = source.ok_or_else(|| EditError::NotFound(id.into()))?;
        if copy.is_component() && !is_inside(id) {
            return self.instance_of(&copy, parent, props, descendants);
        }
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
        if let Kind::Instance(instance) = &mut copy.kind {
            for (key, value) in &descendants {
                crate::resolve::merge_override(&mut instance.descendants, key, value);
            }
        }
        let descendants = if matches!(copy.kind, Kind::Instance(_)) { Map::new() } else { descendants };
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

        let (mut renamed, mut made) = (HashMap::new(), Vec::new());
        self.renew_ids(&mut copy, &mut renamed, &mut made);
        for warning in &mut warnings {
            warning.node = warning.node.as_ref().map(|old| renamed[old].clone());
        }
        let built = Built { node: copy, made, warnings };
        Ok(self.attach(parent, None, built))
    }

    /// A new Instance of `component`, where Copy puts it, with `props`
    /// overriding the Component's own and `descendants` its nodes'.
    fn instance_of(
        &mut self,
        component: &Node,
        parent: Option<&str>,
        mut props: Map<String, Value>,
        descendants: Map<String, Value>,
    ) -> Result<String, EditError> {
        props.insert("type".into(), "instance".into());
        props.insert("ref".into(), component.id.clone().into());
        props.entry("name").or_insert_with(|| component.name.clone().into());
        props.entry("x").or_insert_with(|| component.x.into());
        props.entry("y").or_insert_with(|| component.y.into());
        if !descendants.is_empty() {
            props.insert("descendants".into(), Value::Object(descendants));
        }
        let depth = self.depth_inside(parent)?;
        self.children_of(parent)?;
        // As deep as the Component's nodes go, once it's drawn.
        if depth + height(component) > MAX_DEPTH {
            return Err(EditError::Invalid(format!("Copy: nodes nest at most {MAX_DEPTH} deep")));
        }
        let built = self.build(Value::Object(props), depth, "Copy")?;
        Ok(self.attach(parent, None, built))
    }

    /// Gives `node` and its descendants new ids, noting each old id's new
    /// one, and each node as (name, new id) as Insert notes the nodes it makes.
    fn renew_ids(&mut self, node: &mut Node, renamed: &mut HashMap<String, String>, made: &mut Vec<(String, String)>) {
        let id = self.new_id();
        renamed.insert(std::mem::replace(&mut node.id, id.clone()), id.clone());
        made.push((node.name.clone(), id));
        if let Some(children) = node.kind.get_mut().children_mut() {
            for child in children {
                self.renew_ids(child, renamed, made);
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
