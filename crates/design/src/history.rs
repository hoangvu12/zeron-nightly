//! A Design's one undo stack (apps/zgx/docs/design/adr/0003): each step is
//! one change kept (an agent's call, or one of the person's edits), undone
//! latest first whoever made it.
//!
//! A step keeps only what its change replaced: the Canvas's order, the
//! top-level nodes it changed or removed, and the Themes and Tokens if it
//! changed them. As steps are undone latest first, what the step didn't
//! keep is as it was then.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::model::{Design, Node};

/// Most steps kept to undo; the oldest go first.
pub const MAX_STEPS: usize = 100;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

/// What one change replaced, to put back.
#[derive(Debug)]
struct Step {
    /// The change's description, for an Undo menu.
    description: String,
    /// The Canvas's nodes' ids, in order, before.
    order: Vec<String>,
    /// The top-level nodes the change changed or removed, as they were.
    nodes: HashMap<String, Node>,
    /// The rest of the Design, if the change changed it.
    rest: Option<Rest>,
}

#[derive(Debug, Clone, PartialEq)]
struct Rest {
    version: String,
    themes: Map<String, Value>,
    tokens: Map<String, Value>,
    extra: Map<String, Value>,
}

impl Rest {
    fn of(design: &Design) -> Self {
        Self {
            version: design.version.clone(),
            themes: design.themes.clone(),
            tokens: design.tokens.clone(),
            extra: design.extra.clone(),
        }
    }
}

impl History {
    /// Notes a change from `before` to `after` as one step, described as
    /// `description`; a change that changed nothing is no step. What was
    /// undone can't be redone after it.
    pub fn record(&mut self, description: &str, before: &Design, after: &Design) {
        if let Some(step) = Step::between(description, before, after) {
            self.undo.push(step);
            if self.undo.len() > MAX_STEPS {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    /// `current` with the latest step undone, and that step's description;
    /// none with nothing to undo.
    pub fn undo(&mut self, current: &Design) -> Option<(Design, String)> {
        let step = self.undo.pop()?;
        let (undone, redo) = step.applied(current);
        self.redo.push(redo);
        Some((undone, step.description))
    }

    /// `current` with the latest undone step done again.
    pub fn redo(&mut self, current: &Design) -> Option<(Design, String)> {
        let step = self.redo.pop()?;
        let (redone, undo) = step.applied(current);
        self.undo.push(undo);
        Some((redone, step.description))
    }
}

impl Step {
    fn between(description: &str, before: &Design, after: &Design) -> Option<Self> {
        let now: HashMap<&str, &Node> = after.children.iter().map(|node| (node.id.as_str(), node)).collect();
        let nodes: HashMap<String, Node> = before
            .children
            .iter()
            .filter(|node| now.get(node.id.as_str()) != Some(node))
            .map(|node| (node.id.clone(), node.clone()))
            .collect();
        let order: Vec<String> = before.children.iter().map(|node| node.id.clone()).collect();
        let rest = Some(Rest::of(before)).filter(|rest| *rest != Rest::of(after));
        let same_order = order.iter().eq(after.children.iter().map(|node| &node.id));
        if nodes.is_empty() && rest.is_none() && same_order {
            return None;
        }
        Some(Self {
            description: description.to_owned(),
            order,
            nodes,
            rest,
        })
    }

    /// `current` as it was before this step, and the step that would do it
    /// again.
    fn applied(&self, current: &Design) -> (Design, Step) {
        let now: HashMap<&str, &Node> = current.children.iter().map(|node| (node.id.as_str(), node)).collect();
        let rest = self.rest.clone().unwrap_or_else(|| Rest::of(current));
        let design = Design {
            version: rest.version,
            children: self
                .order
                .iter()
                .filter_map(|id| self.nodes.get(id).or_else(|| now.get(id.as_str()).copied()).cloned())
                .collect(),
            themes: rest.themes,
            tokens: rest.tokens,
            extra: rest.extra,
        };
        let again = Step::between(&self.description, current, &design).unwrap_or(Step {
            description: self.description.clone(),
            order: current.children.iter().map(|node| node.id.clone()).collect(),
            nodes: HashMap::new(),
            rest: None,
        });
        (design, again)
    }
}
