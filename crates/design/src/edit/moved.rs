//! `Move`: a node to a new parent and index.

use super::{Draft, EditError, MAX_DEPTH, height};

impl Draft {
    /// Moves the node `id` with its descendants into `parent` (onto the
    /// Canvas without one) at `index` among its children, or last. It keeps
    /// its x and y, now from its new parent.
    pub fn move_to(&mut self, id: &str, parent: Option<&str>, index: Option<usize>) -> Result<(), EditError> {
        let node = self.design.find(id).ok_or_else(|| EditError::NotFound(id.into()))?;
        let node_height = height(node);
        if let Some(parent) = parent
            && (parent == id || super::holds(node, parent))
        {
            return Err(EditError::Invalid(format!(
                "Move: \"{parent}\" is inside \"{id}\", which can't move inside itself"
            )));
        }
        let depth = self.depth_inside(parent)?;
        if depth + node_height > MAX_DEPTH {
            return Err(EditError::Invalid(format!("Move: nodes nest at most {MAX_DEPTH} deep")));
        }
        let (old_parent, old_index) = self.design.position_of(id).expect("the node was found");
        let siblings = self.children_of(parent)?;
        let room = siblings.len() - usize::from(old_parent.as_deref() == parent);
        if let Some(index) = index
            && index > room
        {
            return Err(EditError::Invalid(format!(
                "Move: the index is {index}, but it can be at most {room} there"
            )));
        }
        let node = self
            .children_of(old_parent.as_deref())
            .expect("its parent holds it")
            .remove(old_index);
        let siblings = self.children_of(parent).expect("the parent was checked");
        siblings.insert(index.unwrap_or(siblings.len()), node);
        self.changed.insert(id.to_owned());
        Ok(())
    }
}
