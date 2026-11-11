//! Reading a Design back, as the snippet API's `Get` does: nodes as the
//! file holds them, to a depth, so what's read inserts as the same nodes;
//! or each node of a subtree with where the layout put it. On request,
//! with Tokens resolved and Instances expanded ([`Options`]); a node inside
//! an Instance (`card/title`) is read expanded either way.

use std::borrow::Cow;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::edit::EditError;
use crate::layout::LaidOut;
use crate::model::{Design, Node};
use crate::resolve::{self, Resolve};

/// What `Get` resolves, as the snippet asks.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    /// Each property that refers to a Token as the Token's value in the
    /// node's Theme.
    #[serde(default)]
    pub resolve_tokens: bool,
    /// Each Instance as its Component's nodes, with its overrides.
    #[serde(default)]
    pub expand_instances: bool,
}

/// The Design as `Get` reads the node `id` (or the Canvas) with `options`.
pub fn view<'a>(design: &'a Design, id: Option<&str>, options: &Options) -> Cow<'a, Design> {
    // A slash path names a node inside an Instance.
    let inside_instance = id.is_some_and(|id| id.contains('/'));
    if !(options.resolve_tokens || options.expand_instances || inside_instance) {
        return Cow::Borrowed(design);
    }
    let how = Resolve {
        instances: options.expand_instances || inside_instance,
        tokens: options.resolve_tokens,
        enabled_only: false,
    };
    Cow::Owned(resolve::resolved(design, how).0)
}

/// `Get`'s depth as the snippet gives it: a whole number of levels, or
/// `Infinity` for all of them.
pub fn depth(depth: f64) -> Result<Option<usize>, EditError> {
    if depth == f64::INFINITY {
        Ok(None)
    } else if depth >= 0. {
        Ok(Some(depth as usize))
    } else {
        Err(EditError::Invalid("Get's depth is a number, 0 or more".into()))
    }
}

/// The node `id` (or, without one, the Canvas's nodes, as a list) with its
/// descendants `depth` levels down, or all of them without a depth.
pub fn nodes(design: &Design, id: Option<&str>, depth: Option<usize>) -> Result<Value, EditError> {
    match id {
        Some(id) => {
            let node = design.find(id).ok_or_else(|| EditError::NotFound(id.into()))?;
            Ok(node_value(node, depth))
        }
        None => Ok(Value::Array(design.children.iter().map(|node| node_value(node, depth)).collect())),
    }
}

fn node_value(node: &Node, depth: Option<usize>) -> Value {
    let mut value = serde_json::to_value(node).expect("a node serializes");
    cut_below(&mut value, depth);
    value
}

/// Drops `children` past `depth` levels down.
fn cut_below(node: &mut Value, depth: Option<usize>) {
    let Some(object) = node.as_object_mut() else {
        return;
    };
    match depth {
        Some(0) => {
            object.remove("children");
        }
        _ => {
            if let Some(Value::Array(children)) = object.get_mut("children") {
                for child in children {
                    cut_below(child, depth.map(|depth| depth - 1));
                }
            }
        }
    }
}

/// Each node of the subtree at `id` (or of the whole Canvas), parents
/// before their children, as `{ node, info }`: the node without its
/// children, and its bounds on the Canvas, depth below where the visit
/// started, index among its siblings, parent's id and layout problems.
pub fn visit(design: &Design, laid_out: &LaidOut, id: Option<&str>) -> Result<Vec<Value>, EditError> {
    let mut visited = Vec::new();
    let mut visitor = Visitor { laid_out, visited: &mut visited };
    match id {
        Some(id) => {
            let (node, parent, index) =
                locate(&design.children, None, id).ok_or_else(|| EditError::NotFound(id.into()))?;
            visitor.visit(node, parent, index, 0);
        }
        None => {
            for (index, node) in design.children.iter().enumerate() {
                visitor.visit(node, None, index, 0);
            }
        }
    }
    Ok(visited)
}

struct Visitor<'a> {
    laid_out: &'a LaidOut,
    visited: &'a mut Vec<Value>,
}

impl Visitor<'_> {
    fn visit(&mut self, node: &Node, parent: Option<&str>, index: usize, depth: usize) {
        let (bounds, problems): (Value, Vec<String>) = match self.laid_out.get(&node.id) {
            Some(placed) => (
                json!({
                    "x": rounded(placed.bounds.x),
                    "y": rounded(placed.bounds.y),
                    "width": rounded(placed.bounds.width),
                    "height": rounded(placed.bounds.height),
                }),
                placed.problems.iter().map(ToString::to_string).collect(),
            ),
            None => (Value::Null, Vec::new()),
        };
        self.visited.push(json!({
            "node": node_value(node, Some(0)),
            "info": {
                "bounds": bounds,
                "depth": depth,
                "index": index,
                "parent": parent,
                "problems": problems,
            },
        }));
        for (index, child) in node.kind.get().children().iter().enumerate() {
            self.visit(child, Some(&node.id), index, depth + 1);
        }
    }
}

/// The node `id` among `nodes`, with its parent's id and its index.
fn locate<'a>(nodes: &'a [Node], parent: Option<&'a str>, id: &str) -> Option<(&'a Node, Option<&'a str>, usize)> {
    nodes.iter().enumerate().find_map(|(index, node)| {
        if node.id == id {
            Some((node, parent, index))
        } else {
            locate(node.kind.get().children(), Some(&node.id), id)
        }
    })
}

/// To a hundredth of a pixel, so f32's noise doesn't reach the agent.
fn rounded(pixels: f32) -> f64 {
    (f64::from(pixels) * 100.).round() / 100.
}
