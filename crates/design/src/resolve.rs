//! The Design as drawn: each Instance expanded from its Component, each
//! property that refers to a Token given the Token's value in the node's
//! Theme, and nodes not enabled left out. Layout and drawing read only
//! this ([`drawn`]); `Get` reads it in part, on request ([`Resolve`]).
//!
//! An expanded Instance keeps its own id, and the nodes inside it take the
//! Component's ids under it, slash-separated (`card/title`, and
//! `card/button/label` inside a nested Instance), which is how edits and
//! `Get` reach them.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::model::{Design, Kind, Node, Theme, Token, token};

/// How many Instances deep an Instance can be, inside its Components.
const MAX_NESTING: usize = 16;

/// What to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolve {
    pub instances: bool,
    pub tokens: bool,
    /// Leave out the nodes not enabled.
    pub enabled_only: bool,
}

impl Resolve {
    /// Everything, as the Design is drawn.
    pub const DRAWN: Resolve = Resolve { instances: true, tokens: true, enabled_only: true };
}

/// Something a Design asks that can't be drawn as asked, about a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub node: String,
    pub message: String,
}

/// The Design as drawn.
pub fn drawn(design: &Design) -> Design {
    resolved(design, Resolve::DRAWN).0
}

/// `design` with what `how` asks resolved, and what couldn't be.
pub fn resolved(design: &Design, how: Resolve) -> (Design, Vec<Problem>) {
    if !asks(&design.children) {
        return (design.clone(), Vec::new());
    }
    let mut components = HashMap::new();
    design.walk(&mut |node| {
        if node.is_component() {
            components.insert(node.id.clone(), node.clone());
        }
    });
    let tokens = design
        .tokens
        .iter()
        .filter_map(|(name, token)| Some((name.clone(), Token::read(name, token).ok()?)))
        .collect();
    let mut resolver = Resolver { how, components, tokens, problems: Vec::new() };
    let theme = token::default_theme(&design.themes);
    let children = design
        .children
        .iter()
        .filter_map(|node| resolver.node(node.clone(), &theme, &mut Vec::new()))
        .collect();
    let design = Design { children, ..design.without_children() };
    (design, resolver.problems)
}

/// Whether any of `nodes` is an Instance, refers to a Token or isn't
/// enabled: whether resolving changes anything.
fn asks(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| {
        matches!(node.kind, Kind::Instance(_)) || !node.refs.is_empty() || !node.enabled || asks(node.kind.get().children())
    })
}

struct Resolver {
    how: Resolve,
    /// The Design's Components by id.
    components: HashMap<String, Node>,
    tokens: HashMap<String, Token>,
    problems: Vec<Problem>,
}

impl Resolver {
    /// `node` resolved in `theme` (its parent's), inside the Components
    /// in `nesting`; none when it's left out.
    fn node(&mut self, mut node: Node, theme: &Theme, nesting: &mut Vec<String>) -> Option<Node> {
        if self.how.enabled_only && !node.enabled {
            return None;
        }
        let mut expanded = None;
        if self.how.instances && matches!(node.kind, Kind::Instance(_)) {
            (node, expanded) = self.expand(node, nesting);
        } else if node.is_component() {
            // So an Instance of a Component inside it is caught at once.
            expanded = Some(node.id.clone());
        }
        // After expanding, so the Component's own Theme counts.
        let mut theme = theme.clone();
        theme.extend(node.theme.clone());
        if self.how.tokens && !node.refs.is_empty() {
            self.substitute(&mut node, &theme);
        }
        if let Some(children) = node.kind.get_mut().children_mut() {
            let taken = std::mem::take(children);
            nesting.extend(expanded.clone());
            let resolved = taken.into_iter().filter_map(|child| self.node(child, &theme, nesting)).collect();
            if expanded.is_some() {
                nesting.pop();
            }
            *node.kind.get_mut().children_mut().expect("it held children") = resolved;
        }
        Some(node)
    }

    /// The Instance `node` as its Component, with its overrides; and the
    /// Component's id. As it is, when it can't be.
    fn expand(&mut self, node: Node, nesting: &[String]) -> (Node, Option<String>) {
        let Kind::Instance(instance) = &node.kind else {
            return (node, None);
        };
        let Some(component) = self.components.get(&instance.component) else {
            self.problem(&node.id, format!("its Component \"{}\" isn't in the Design", instance.component));
            return (node, None);
        };
        if nesting.contains(&instance.component) {
            self.problem(&node.id, format!("its Component \"{}\" holds an Instance of itself", instance.component));
            return (node, None);
        }
        if nesting.len() >= MAX_NESTING {
            self.problem(&node.id, format!("Instances nest at most {MAX_NESTING} deep inside their Components"));
            return (node, None);
        }
        let mut copy = component.clone();
        for child in copy.kind.get_mut().children_mut().into_iter().flatten() {
            prefix(child, &node.id);
        }
        if let Kind::Frame(frame) = &mut copy.kind {
            frame.reusable = false;
        }
        copy.id = node.id.clone();
        if !node.name.is_empty() {
            copy.name = node.name.clone();
        }
        (copy.x, copy.y, copy.rotation) = (node.x, node.y, node.rotation);
        (copy.flip_x, copy.flip_y, copy.opacity) = (node.flip_x, node.flip_y, node.opacity);
        (copy.absolute, copy.in_progress, copy.enabled) = (node.absolute, node.in_progress, node.enabled);
        copy.theme.extend(node.theme.clone());
        let mut overrides = instance.overrides.clone();
        overrides.extend(node.refs.clone());
        if let Err(e) = copy.set_props(&overrides) {
            self.problem(&node.id, format!("its overrides don't fit its Component: {e}"));
        }
        for key in overrides.keys().filter(|key| copy.unknown_properties().any(|unknown| unknown == *key)) {
            let message = format!("its Component has no property \"{key}\" to override; it's kept, but does nothing");
            self.problem(&node.id, message);
        }
        // By name first, so an override by id or path wins over one by name.
        let by_id = |key: &str| key.contains('/') || holds(&copy, &format!("{}/{key}", node.id));
        let (by_id, by_name): (Vec<_>, Vec<_>) = instance.descendants.iter().partition(|(key, _)| by_id(key));
        for (key, value) in by_name.into_iter().chain(by_id) {
            self.override_descendant(&mut copy, key, value);
        }
        (copy, Some(instance.component.clone()))
    }

    /// Overrides the node `key` names inside the expanded Instance `copy`
    /// with `value`.
    fn override_descendant(&mut self, copy: &mut Node, key: &str, value: &Value) {
        let instance = copy.id.clone();
        if let Some(target) = find_mut(copy, &format!("{instance}/{key}")) {
            let id = target.id.clone();
            if let Err(e) = apply(target, value) {
                self.problem(&id, e);
            }
            return;
        }
        // A path into a nested Instance goes on to it.
        if let Some((head, rest)) = key.split_once('/')
            && let Some(nested) = find_mut(copy, &format!("{instance}/{head}"))
            && let Kind::Instance(nested) = &mut nested.kind
        {
            merge_override(&mut nested.descendants, rest, value);
            return;
        }
        let named = named(copy, key);
        match named.as_slice() {
            [id] => {
                let id = id.clone();
                let target = find_mut(copy, &id).expect("it was just found");
                if let Err(e) = apply(target, value) {
                    self.problem(&id, e);
                }
            }
            [] => self.problem(&instance, format!("no node inside it has the id or name \"{key}\" to override")),
            _ => self.problem(
                &instance,
                format!("{} nodes inside it are named \"{key}\": override one by its id", named.len()),
            ),
        }
    }

    /// Gives `node`'s properties that refer to Tokens their values in `theme`.
    fn substitute(&mut self, node: &mut Node, theme: &Theme) {
        let refs = std::mem::take(&mut node.refs);
        let resolved: Map<String, Value> = refs
            .iter()
            .map(|(key, value)| (key.clone(), self.value(value, theme, &node.id)))
            .collect();
        node.refs = refs;
        match node.set_props(&resolved) {
            Ok(unsupported) => {
                for message in unsupported {
                    self.problem(&node.id, format!("with its Tokens' values, {message}"));
                }
                let unknown: Vec<String> = node
                    .unknown_properties()
                    .filter(|key| resolved.contains_key(*key))
                    .map(str::to_owned)
                    .collect();
                for key in unknown {
                    let message = format!("has a property \"{key}\" Zeron doesn't know; it's kept, but does nothing");
                    self.problem(&node.id, message);
                }
            }
            Err(e) => self.problem(&node.id, format!("with its Tokens' values, {e}")),
        }
    }

    fn value(&mut self, value: &Value, theme: &Theme, node: &str) -> Value {
        match value {
            Value::String(text) => match token::reference(text) {
                Some(name) => match self.tokens.get(name) {
                    Some(token) => token.value_in(theme).clone(),
                    None => {
                        self.problem(node, format!("\"{text}\" refers to a Token the Design doesn't have"));
                        value.clone()
                    }
                },
                None => value.clone(),
            },
            Value::Array(items) => Value::Array(items.iter().map(|item| self.value(item, theme, node)).collect()),
            Value::Object(props) => {
                Value::Object(props.iter().map(|(key, value)| (key.clone(), self.value(value, theme, node))).collect())
            }
            other => other.clone(),
        }
    }

    fn problem(&mut self, node: &str, message: String) {
        let problem = Problem { node: node.to_owned(), message };
        if !self.problems.contains(&problem) {
            self.problems.push(problem);
        }
    }
}

/// Sets `value` on `target`: properties to set, or a node (with a `type`)
/// to put in its place, keeping its id.
fn apply(target: &mut Node, value: &Value) -> Result<(), String> {
    let Value::Object(props) = value else {
        return Err(format!("an override is an object of properties or a node, not {value}"));
    };
    if props.contains_key("type") {
        let mut props = props.clone();
        props.insert("id".into(), Value::String(target.id.clone()));
        let mut node: Node = serde_json::from_value(Value::Object(props)).map_err(|e| e.to_string())?;
        let id = node.id.clone();
        for child in node.kind.get_mut().children_mut().into_iter().flatten() {
            prefix(child, &id);
        }
        *target = node;
        return Ok(());
    }
    let mut props = props.clone();
    props.remove("children");
    target.set_props(&props).map(drop)
}

/// Adds `value` to the overrides `descendants`, over what's there for `key`:
/// properties merge into properties, anything else replaces it.
pub fn merge_override(descendants: &mut Map<String, Value>, key: &str, value: &Value) {
    match (descendants.get_mut(key), value) {
        (Some(Value::Object(there)), Value::Object(props)) if !props.contains_key("type") => {
            for (prop, value) in props {
                if value.is_null() {
                    there.remove(prop);
                } else {
                    there.insert(prop.clone(), value.clone());
                }
            }
        }
        _ => {
            descendants.insert(key.to_owned(), value.clone());
        }
    }
}

/// Puts `node` and its descendants under `prefix` (`prefix/id`).
fn prefix(node: &mut Node, prefix_with: &str) {
    node.id = format!("{prefix_with}/{}", node.id);
    for child in node.kind.get_mut().children_mut().into_iter().flatten() {
        prefix(child, prefix_with);
    }
}

fn find_mut<'a>(node: &'a mut Node, id: &str) -> Option<&'a mut Node> {
    if node.id == id {
        return Some(node);
    }
    node.kind.get_mut().children_mut()?.iter_mut().find_map(|child| find_mut(child, id))
}

/// Whether the node `id` is `node` or inside it.
fn holds(node: &Node, id: &str) -> bool {
    node.id == id || node.kind.get().children().iter().any(|child| holds(child, id))
}

/// The ids of the nodes inside `node` named `name`.
fn named(node: &Node, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    for child in node.kind.get().children() {
        if child.name == name {
            found.push(child.id.clone());
        }
        found.extend(named(child, name));
    }
    found
}
