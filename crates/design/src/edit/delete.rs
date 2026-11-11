//! `Delete`: nodes gone, with their descendants.

use super::{Draft, EditError};
use crate::model::Node;

impl Draft {
    /// Deletes the node `id` and its descendants.
    pub fn delete(&mut self, id: &str) -> Result<(), EditError> {
        let (_, _, node) = self.take(id)?;
        self.forget(&node);
        Ok(())
    }

    /// Notes that `node` and its descendants are gone: deleted, if the
    /// Design had them, and no longer warned about.
    pub(super) fn forget(&mut self, node: &Node) {
        let mut gone = Vec::new();
        collect_ids(node, &mut gone);
        let made: Vec<&str> = self.created.iter().map(|(_, id)| id.as_str()).collect();
        let had: Vec<String> = gone.iter().filter(|id| !made.contains(&id.as_str())).cloned().collect();
        self.warnings.retain(|w| w.node.as_ref().is_none_or(|node| !gone.contains(node)));
        for id in &gone {
            self.changed.remove(id);
        }
        self.deleted.extend(had);
    }
}

fn collect_ids(node: &Node, ids: &mut Vec<String>) {
    ids.push(node.id.clone());
    for child in node.kind.get().children() {
        collect_ids(child, ids);
    }
}
