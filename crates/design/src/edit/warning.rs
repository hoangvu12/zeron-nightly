//! Warnings: what the agent should know about a call's edits that didn't
//! fail them (a property the model doesn't know, a value it can't use),
//! returned with the call so the agent can fix them in its next one.

use serde::Serialize;

use crate::model::Node;

/// Most warnings a call returns; past this, one more says how many were left out.
const MAX_WARNINGS: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    /// The node it's about, by id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The property it's about, when it's about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    pub message: String,
}

impl Warning {
    /// `node` has `property`, which the model doesn't know.
    pub(super) fn unknown(node: &Node, property: &str) -> Self {
        Self {
            node: Some(node.id.clone()),
            property: Some(property.to_owned()),
            message: format!(
                "\"{}\" ({}) has a property \"{property}\" Zeron doesn't know; it's kept, but does nothing",
                node.name,
                node.kind.name()
            ),
        }
    }
}

/// A call's warnings as it returns them: each once, at most [`MAX_WARNINGS`].
pub(super) fn finished(warnings: Vec<Warning>) -> Vec<Warning> {
    let mut kept: Vec<Warning> = Vec::new();
    for warning in warnings {
        if !kept.contains(&warning) {
            kept.push(warning);
        }
    }
    if kept.len() > MAX_WARNINGS {
        let left_out = kept.len() - MAX_WARNINGS;
        kept.truncate(MAX_WARNINGS);
        kept.push(Warning {
            node: None,
            property: None,
            message: format!("{left_out} more warnings like these were left out"),
        });
    }
    kept
}
