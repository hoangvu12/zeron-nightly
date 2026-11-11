//! Changing a Design: one call's edits go to a [`Draft`], a copy of the
//! Design, which is kept whole when the call succeeds and dropped when it
//! fails, so a failed call changes nothing.
//!
//! Each edit is a method here (the snippet sandbox's `Insert` is
//! [`Draft::insert`]); the draft notes what each one changed, for the
//! call's result and for whoever follows the Design.

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

use crate::model::{Design, Kind, Node};

/// Characters a generated node id is made of.
const ID_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ID_LEN: usize = 5;

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
}

/// What a call's draft ends as, once it succeeded.
pub struct Edited {
    pub design: Design,
    /// Each node the call made, by name; a name made twice in one call is
    /// listed as `Name 2`, `Name 3` and so on.
    pub names: Map<String, Value>,
    pub changed: BTreeSet<String>,
    pub screenshots: Vec<String>,
}

impl Draft {
    pub fn new(design: Design) -> Self {
        let mut ids = HashSet::new();
        design.walk(&mut |node| {
            ids.insert(node.id.clone());
        });
        Self {
            design,
            ids,
            created: Vec::new(),
            changed: BTreeSet::new(),
            screenshots: Vec::new(),
        }
    }

    pub fn design(&self) -> &Design {
        &self.design
    }

    /// Adds `node` (a node's properties, with any `children` inline) as the
    /// last child of `parent`, or on the Canvas without one. Every node it
    /// makes gets a new id, and a name (its type's, capitalised) when it has
    /// none. Returns the new node's id.
    pub fn insert(&mut self, parent: Option<&str>, mut node: Value) -> Result<String, EditError> {
        let mut made = Vec::new();
        self.prepare(&mut node, &mut made)?;
        let node: Node = serde_json::from_value(node)
            .map_err(|e| EditError::Invalid(format!("Insert: {e}")))?;
        let siblings = match parent {
            None => &mut self.design.children,
            Some(parent) => {
                let parent_node = self
                    .design
                    .find_mut(parent)
                    .ok_or_else(|| EditError::NotFound(parent.into()))?;
                let kind = parent_node.kind.name();
                parent_node.kind.get_mut().children_mut().ok_or_else(|| {
                    EditError::Invalid(format!("\"{parent}\" is a {kind}, which holds no children"))
                })?
            }
        };
        let id = node.id.clone();
        siblings.push(node);
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
        }
    }

    /// Gives `node` and its inline children new ids and default names,
    /// noting each as (name, id).
    fn prepare(&mut self, node: &mut Value, made: &mut Vec<(String, String)>) -> Result<(), EditError> {
        let Value::Object(props) = node else {
            return Err(EditError::Invalid(format!("Insert takes a node object, not {node}")));
        };
        let Some(kind) = props.get("type").and_then(Value::as_str) else {
            return Err(EditError::Invalid(format!(
                "Insert needs a node with a \"type\" ({})",
                Kind::NAMES.join(", ")
            )));
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

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
