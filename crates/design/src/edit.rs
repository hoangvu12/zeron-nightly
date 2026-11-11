//! Changing a Design: one call's edits go to a [`Draft`], a copy of the
//! Design, which is kept whole when the call succeeds and dropped when it
//! fails, so a failed call changes nothing.
//!
//! Each edit is a method here (the snippet sandbox's `Insert` is
//! [`Draft::insert`]); the draft notes what each one changed, for the
//! call's result and for whoever follows the Design.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::icons::Icons;
use crate::model::{Design, Kind, Node};

/// Characters a generated node id is made of.
const ID_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ID_LEN: usize = 5;

/// How deep nodes nest at most. Walking and drawing recurse per level, and
/// a file nested past about 60 levels is past serde_json's limit to read.
pub const MAX_DEPTH: usize = 48;

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("no node has the id \"{0}\"")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

/// One call's working copy of a Design.
pub struct Draft {
    design: Design,
    ids: HashSet<String>,
    /// Nodes made by this call, as (name, id), in order.
    created: Vec<(String, String)>,
    /// Nodes this call made or changed.
    changed: BTreeSet<String>,
    /// Nodes whose screenshots the call asked for, in order.
    screenshots: Vec<String>,
    /// Where icon names are checked; without, any name is taken.
    icons: Option<Arc<Icons>>,
    /// What the agent should know about its edits that didn't fail them.
    warnings: Vec<String>,
}

/// What a call's draft ends as, once it succeeded.
pub struct Edited {
    pub design: Design,
    /// Each node the call made, by name; a name made twice in one call is
    /// listed as `Name 2`, `Name 3` and so on.
    pub names: Map<String, Value>,
    pub changed: BTreeSet<String>,
    pub screenshots: Vec<String>,
    pub warnings: Vec<String>,
}

impl Draft {
    pub fn new(design: Design) -> Self {
        let mut ids = HashSet::new();
        design.walk(&mut |node| {
            ids.insert(node.id.clone());
            // Nodes kept inside a node of an unknown type have ids too.
            if !node.kind.is_known() {
                kept_ids(&Value::Object(node.extra.clone()), &mut ids);
            }
        });
        Self {
            design,
            ids,
            created: Vec::new(),
            changed: BTreeSet::new(),
            screenshots: Vec::new(),
            icons: None,
            warnings: Vec::new(),
        }
    }

    /// Checks the icons the call places against `icons`, fetching each.
    pub fn with_icons(mut self, icons: Arc<Icons>) -> Self {
        self.icons = Some(icons);
        self
    }

    /// Adds `node` (a node's properties, with any `children` inline) as the
    /// last child of `parent`, or on the Canvas without one. Every node it
    /// makes gets a new id, and a name (its type's, capitalised) when it has
    /// none. A property the model doesn't know is kept, with a warning.
    /// Returns the new node's id.
    pub fn insert(&mut self, parent: Option<&str>, mut node: Value) -> Result<String, EditError> {
        let depth = match parent {
            None => 0,
            Some(parent) => self
                .design
                .depth_of(parent)
                .ok_or_else(|| EditError::NotFound(parent.into()))?,
        };
        if depth + nesting(&node) > MAX_DEPTH {
            return Err(EditError::Invalid(format!(
                "Insert: nodes nest at most {MAX_DEPTH} deep"
            )));
        }
        let mut made = Vec::new();
        self.prepare(&mut node, &mut made)?;
        let node: Node = serde_json::from_value(node)
            .map_err(|e| EditError::Invalid(format!("Insert: {e}")))?;
        self.check(&node).map_err(|e| EditError::Invalid(format!("Insert: {e}")))?;
        let mut warnings = Vec::new();
        warn_unknown(&node, &mut warnings);
        let siblings = match parent {
            None => &mut self.design.children,
            Some(parent) => {
                let parent_node = self
                    .design
                    .find_mut(parent)
                    .ok_or_else(|| EditError::NotFound(parent.into()))?;
                let kind = parent_node.kind.name().to_owned();
                parent_node.kind.get_mut().children_mut().ok_or_else(|| {
                    EditError::Invalid(format!("\"{parent}\" is a {kind}, which holds no children"))
                })?
            }
        };
        let id = node.id.clone();
        siblings.push(node);
        self.warnings.extend(warnings);
        for (name, id) in made {
            self.changed.insert(id.clone());
            self.created.push((name, id));
        }
        Ok(id)
    }

    /// Asks for screenshots of these nodes with the call's result.
    pub fn screenshot(&mut self, ids: Vec<String>) -> Result<(), EditError> {
        for id in ids {
            if self.design.find(&id).is_none() {
                return Err(EditError::NotFound(id));
            }
            self.screenshots.push(id);
        }
        Ok(())
    }

    pub fn finish(self) -> Edited {
        let mut names = Map::new();
        for (name, id) in self.created {
            let mut key = name.clone();
            let mut n = 1;
            while names.contains_key(&key) {
                n += 1;
                key = format!("{name} {n}");
            }
            names.insert(key, Value::String(id));
        }
        Edited {
            design: self.design,
            names,
            changed: self.changed,
            screenshots: self.screenshots,
            warnings: self.warnings,
        }
    }

    /// Refuses what `node` or its children can't be drawn with: an icon
    /// Iconify doesn't have (fetching it now, so it's cached to draw), a
    /// path that isn't path data, a polygon's side count.
    fn check(&self, node: &Node) -> Result<(), String> {
        match &node.kind {
            Kind::Icon(icon) => {
                if let Some(icons) = &self.icons {
                    icons.svg(&icon.icon, icon.weight).map_err(|e| e.to_string())?;
                }
            }
            Kind::Path(path) => path.problem().map_or(Ok(()), Err)?,
            Kind::Polygon(polygon) => polygon.problem().map_or(Ok(()), Err)?,
            _ => {}
        }
        node.kind.get().children().iter().try_for_each(|child| self.check(child))
    }

    /// Gives `node` and its inline children new ids and default names,
    /// noting each as (name, id).
    fn prepare(&mut self, node: &mut Value, made: &mut Vec<(String, String)>) -> Result<(), EditError> {
        let Value::Object(props) = node else {
            return Err(EditError::Invalid(format!("Insert takes a node object, not {node}")));
        };
        let kind = match props.get("type").and_then(Value::as_str) {
            Some(kind) if Kind::NAMES.contains(&kind) => kind,
            kind => {
                return Err(EditError::Invalid(format!(
                    "Insert needs a node whose \"type\" is one of {}{}",
                    Kind::NAMES.join(", "),
                    kind.map(|kind| format!(" (not \"{kind}\")")).unwrap_or_default(),
                )));
            }
        };
        let name = match props.get("name").and_then(Value::as_str).map(str::trim) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => capitalised(kind),
        };
        let id = self.new_id();
        props.insert("id".into(), Value::String(id.clone()));
        props.insert("name".into(), Value::String(name.clone()));
        made.push((name, id));
        if let Some(Value::Array(children)) = props.get_mut("children") {
            for child in children {
                self.prepare(child, made)?;
            }
        }
        Ok(())
    }

    fn new_id(&mut self) -> String {
        loop {
            let bytes = uuid::Uuid::new_v4().into_bytes();
            let id: String = bytes[..ID_LEN]
                .iter()
                .map(|b| ID_ALPHABET[*b as usize % ID_ALPHABET.len()] as char)
                .collect();
            if self.ids.insert(id.clone()) {
                return id;
            }
        }
    }
}

/// A warning for each property `node` and its children have that the model
/// doesn't know (kept, but doing nothing): most often a typo.
fn warn_unknown(node: &Node, warnings: &mut Vec<String>) {
    for property in node.unknown_properties() {
        warnings.push(format!(
            "\"{}\" ({}) has a property \"{property}\" Zeron doesn't know; it's kept, but does nothing",
            node.name,
            node.kind.name()
        ));
    }
    for child in node.kind.get().children() {
        warn_unknown(child, warnings);
    }
}

/// Every `id` string in `value`, however deep.
fn kept_ids(value: &Value, ids: &mut HashSet<String>) {
    match value {
        Value::Object(props) => {
            if let Some(Value::String(id)) = props.get("id") {
                ids.insert(id.clone());
            }
            props.values().for_each(|value| kept_ids(value, ids));
        }
        Value::Array(items) => items.iter().for_each(|value| kept_ids(value, ids)),
        _ => {}
    }
}

/// How many levels `node` and its inline children take.
fn nesting(node: &Value) -> usize {
    1 + match node.get("children") {
        Some(Value::Array(children)) => children.iter().map(nesting).max().unwrap_or(0),
        _ => 0,
    }
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
