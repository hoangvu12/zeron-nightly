//! `Delete`: nodes gone, with their descendants.

use super::{Draft, EditError};
use crate::model::Node;

impl Draft {
    /// Deletes the nodes `ids` and their descendants: all of them, or none
    /// when one isn't there. A node inside another one deleted goes with it.
    pub fn delete(&mut self, ids: &[String]) -> Result<(), EditError> {
        if let Some(missing) = ids.iter().find(|id| self.design.find(id).is_none()) {
            return Err(EditError::NotFound(missing.clone()));
        }
        for id in ids {
            if self.design.find(id).is_some() {
                let (_, _, node) = self.take(id)?;
                self.forget(&node);
            }
        }
        Ok(())
    }

    /// Notes that `node` and its descendants are gone: no longer changed
    /// by this call, nor warned about.
    pub(super) fn forget(&mut self, node: &Node) {
        let mut gone = Vec::new();
        collect_ids(node, &mut gone);
        self.warnings.retain(|w| w.node.as_ref().is_none_or(|node| !gone.contains(node)));
        for id in &gone {
            self.changed.remove(id);
        }
    }
}

fn collect_ids(node: &Node, ids: &mut Vec<String>) {
    ids.push(node.id.clone());
    for child in node.kind.get().children() {
        collect_ids(child, ids);
    }
}
