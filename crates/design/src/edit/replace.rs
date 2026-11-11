//! `Replace`: a new node in an old one's place.

use serde_json::Value;

use super::{Draft, EditError};

impl Draft {
    /// Puts `node` (as Insert takes it, with new ids) where the node `id`
    /// is among its siblings, deleting `id` and its descendants. Returns
    /// the new node's id. A node inside an Instance (`card/title`) is
    /// replaced in that Instance only, the new node taking its path.
    pub fn replace(&mut self, id: &str, node: Value) -> Result<String, EditError> {
        if super::inside::is_inside(id) {
            return self.replace_inside(id, node);
        }
        let depth = self.design.depth_of(id).ok_or_else(|| EditError::NotFound(id.into()))?;
        let built = self.build(node, depth - 1, "Replace")?;
        let (parent, index, old) = self.take(id)?;
        self.forget(&old);
        Ok(self.attach(parent.as_deref(), Some(index), built))
    }
}
