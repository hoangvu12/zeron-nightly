//! Changing a Design: one call's edits go to a [`Draft`], a copy of the
//! Design, which is kept whole when the call succeeds and dropped when it
//! fails, so a failed call changes nothing.
//!
//! Each edit is a method on the draft (the snippet sandbox's `Insert` is
//! [`Draft::insert`]), in a file of its own but for Insert; the draft notes
//! what each one changed, for the call's result and for whoever follows
//! the Design. An edit that fails leaves the draft as it was, as a snippet
//! may catch its error and carry on.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::assets::Cache;
use crate::model::{Design, Kind, Node, warn};
use crate::printed::Printed;

mod copy;
mod delete;
mod moved;
mod replace;
mod update;
mod warning;

pub use warning::Warning;

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
    /// What the snippet printed, for the call's result.
    printed: Printed,
    /// What the agent should know about its edits that didn't fail them.
    warnings: Vec<Warning>,
    /// Where the assets nodes draw with are checked; without, any is taken.
    assets: Option<Arc<Cache>>,
}

/// What a call's draft ends as, once it succeeded.
pub struct Edited {
    pub design: Design,
    /// Each node the call made, by name; a name made twice in one call is
    /// listed as `Name 2`, `Name 3` and so on.
    pub names: Map<String, Value>,
    pub changed: BTreeSet<String>,
    pub screenshots: Vec<String>,
    /// The lines the call printed.
    pub printed: Vec<String>,
    pub warnings: Vec<Warning>,
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
            printed: Printed::default(),
            warnings: Vec::new(),
            assets: None,
        }
    }

    /// Checks the assets the call's nodes draw with (their icons) against
    /// `assets`, fetching each.
    pub fn with_assets(mut self, assets: Arc<Cache>) -> Self {
        self.assets = Some(assets);
        self
    }

    /// The Design as this call has it so far.
    pub fn design(&self) -> &Design {
        &self.design
    }

    /// Adds a line to the call's result.
    pub fn print(&mut self, line: String) {
        self.printed.push(line);
    }

    /// Adds `node` (a node's properties, with any `children` inline) as the
    /// last child of `parent`, or on the Canvas without one. Every node it
    /// makes gets a new id, and a name (its type's, capitalised) when it has
    /// none. A property the model doesn't know is kept, with a warning.
    /// Returns the new node's id.
    pub fn insert(&mut self, parent: Option<&str>, node: Value) -> Result<String, EditError> {
        let depth = self.depth_inside(parent)?;
        self.children_of(parent)?;
        let built = self.build(node, depth, "Insert")?;
        Ok(self.attach(parent, None, built))
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

    /// Warns the agent about `node` (and its `property`, when it's about
    /// one) without failing the edit. A later edit in the call that sets
    /// that property, or deletes the node, takes the warning back.
    pub fn warn(&mut self, node: &str, property: Option<&str>, message: String) {
        self.warnings.push(Warning {
            node: Some(node.to_owned()),
            property: property.map(str::to_owned),
            message,
        });
    }

    pub fn finish(self) -> Edited {
        let mut names = Map::new();
        // A node made and deleted in one call was never there.
        let created = self.created.into_iter().filter(|(_, id)| self.design.find(id).is_some());
        for (name, id) in created {
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
            printed: self.printed.into_lines(),
            warnings: warning::finished(self.warnings),
        }
    }

    /// Takes back the warnings about `node`'s `property`.
    fn unwarn(&mut self, node: &str, property: &str) {
        self.warnings
            .retain(|w| !(w.node.as_deref() == Some(node) && w.property.as_deref() == Some(property)));
    }

    /// How deep `parent` is (0 for the Canvas): one less than its children.
    fn depth_inside(&self, parent: Option<&str>) -> Result<usize, EditError> {
        match parent {
            None => Ok(0),
            Some(parent) => self
                .design
                .depth_of(parent)
                .ok_or_else(|| EditError::NotFound(parent.into())),
        }
    }

    /// The children of `parent`, or the Canvas's nodes without one.
    fn children_of(&mut self, parent: Option<&str>) -> Result<&mut Vec<Node>, EditError> {
        match parent {
            None => Ok(&mut self.design.children),
            Some(parent) => {
                let parent_node = self
                    .design
                    .find_mut(parent)
                    .ok_or_else(|| EditError::NotFound(parent.into()))?;
                let kind = parent_node.kind.name().to_owned();
                parent_node.kind.get_mut().children_mut().ok_or_else(|| {
                    EditError::Invalid(format!("\"{parent}\" is a {kind}, which holds no children"))
                })
            }
        }
    }

    /// `node` as Insert takes it, made into a node inside one `depth` deep:
    /// new ids, default names, and warnings for what the model doesn't
    /// know. Changes nothing in the Design.
    fn build(&mut self, mut node: Value, depth: usize, verb: &str) -> Result<Built, EditError> {
        if depth + nesting(&node) > MAX_DEPTH {
            return Err(EditError::Invalid(format!(
                "{verb}: nodes nest at most {MAX_DEPTH} deep"
            )));
        }
        let mut made = Vec::new();
        self.prepare(&mut node, &mut made, verb)?;
        // Unsupported values, as the model read them; then unknown properties.
        let (node, unsupported) = warn::collect(|| serde_json::from_value::<Node>(node));
        let node = node.map_err(|e| EditError::Invalid(format!("{verb}: {e}")))?;
        let mut warnings = unsupported_warnings(&node, verb, unsupported);
        self.check(&node, &mut warnings)
            .map_err(|e| EditError::Invalid(format!("{verb}: {e}")))?;
        warn_unknown(&node, &mut warnings);
        Ok(Built { node, made, warnings })
    }

    /// Refuses what `node` or its descendants can't be drawn with, as
    /// [`Draft::check_one`]: for a node an edit made.
    fn check(&self, node: &Node, warnings: &mut Vec<Warning>) -> Result<(), String> {
        self.check_one(node, warnings)?;
        node.kind
            .get()
            .children()
            .iter()
            .try_for_each(|child| self.check(child, warnings))
    }

    /// Refuses what `node` itself can't be drawn with: an icon Iconify
    /// doesn't have (fetching it now, so it's cached to draw), a path that
    /// isn't path data, a polygon's side count; and warns of a weight on an
    /// icon without weights. Every edit that makes a node or sets its
    /// properties checks it here (Update and Copy only the nodes they set).
    fn check_one(&self, node: &Node, warnings: &mut Vec<Warning>) -> Result<(), String> {
        match &node.kind {
            Kind::Icon(icon) => {
                if let Some(assets) = &self.assets {
                    let svg = assets.icons().svg(&icon.icon, icon.weight).map_err(|e| e.to_string())?;
                    if icon.weight.is_some() && !crate::icons::takes_weight(&icon.icon, &svg) {
                        warnings.push(Warning {
                            node: Some(node.id.clone()),
                            property: Some("weight".into()),
                            message: format!("\"{}\" ({}) has no weights; its weight does nothing", node.name, icon.icon),
                        });
                    }
                }
            }
            Kind::Path(path) => path.problem().map_or(Ok(()), Err)?,
            Kind::Polygon(polygon) => polygon.problem().map_or(Ok(()), Err)?,
            _ => {}
        }
        Ok(())
    }

    /// Puts a built node among `parent`'s children (already checked to
    /// hold children), at `index` or last. Returns its id.
    fn attach(&mut self, parent: Option<&str>, index: Option<usize>, built: Built) -> String {
        let Built { node, made, warnings } = built;
        let id = node.id.clone();
        let siblings = self.children_of(parent).expect("the parent was checked");
        let index = index.unwrap_or(siblings.len()).min(siblings.len());
        siblings.insert(index, node);
        self.warnings.extend(warnings);
        for (name, id) in made {
            self.changed.insert(id.clone());
            self.created.push((name, id));
        }
        id
    }

    /// Takes the node `id` out of the Design, with where it was: its
    /// parent's id (none on the Canvas) and its index there.
    fn take(&mut self, id: &str) -> Result<(Option<String>, usize, Node), EditError> {
        let (parent, index) = self
            .design
            .position_of(id)
            .ok_or_else(|| EditError::NotFound(id.into()))?;
        let node = self.children_of(parent.as_deref())?.remove(index);
        Ok((parent, index, node))
    }

    /// Gives `node` and its inline children new ids and default names,
    /// noting each as (name, id).
    fn prepare(&mut self, node: &mut Value, made: &mut Vec<(String, String)>, verb: &str) -> Result<(), EditError> {
        let Value::Object(props) = node else {
            return Err(EditError::Invalid(format!("{verb} takes a node object, not {node}")));
        };
        let kind = match props.get("type").and_then(Value::as_str) {
            Some(kind) if Kind::NAMES.contains(&kind) => kind,
            kind => {
                return Err(EditError::Invalid(format!(
                    "{verb} needs a node whose \"type\" is one of {}{}",
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
                self.prepare(child, made, verb)?;
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

/// A node Insert, Replace or Copy made, not yet in the Design.
struct Built {
    node: Node,
    /// Its nodes, as (name, id), for the call's map of new ids.
    made: Vec<(String, String)>,
    warnings: Vec<Warning>,
}

/// The model's warnings about `node`'s values it doesn't support, as an
/// edit `verb` made them.
pub(super) fn unsupported_warnings(node: &Node, verb: &str, messages: Vec<String>) -> Vec<Warning> {
    messages
        .into_iter()
        .map(|message| Warning {
            node: Some(node.id.clone()),
            property: None,
            message: format!("{verb} \"{}\": {message}", node.name),
        })
        .collect()
}

/// A warning for each property `node` and its children have that the model
/// doesn't know (kept, but doing nothing): most often a typo.
fn warn_unknown(node: &Node, warnings: &mut Vec<Warning>) {
    for property in node.unknown_properties() {
        warnings.push(Warning::unknown(node, property));
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

/// How many levels `node` and its descendants take.
fn height(node: &Node) -> usize {
    1 + node.kind.get().children().iter().map(height).max().unwrap_or(0)
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Whether the node `id` is among `node`'s descendants.
fn holds(node: &Node, id: &str) -> bool {
    node.kind.get().children().iter().any(|child| child.id == id || holds(child, id))
}
