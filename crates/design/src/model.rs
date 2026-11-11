//! A Design as its `.zdesign` file holds it: pretty-printed JSON in
//! pen.dev's shape, with the glossary's words (Tokens, not variables).
//!
//! A node is its common properties ([`Node`]) and its type's own
//! ([`Kind`]). Each type lives in a file of its own under `model/`, made of
//! property groups shared by the types that have them ([`Size`], [`Fill`]),
//! and joins [`Kind`] with one line in its `kinds!` list.

mod frame;
mod paint;
mod rectangle;

pub use frame::Frame;
pub use paint::{Color, Fill, Paint};
pub use rectangle::Rectangle;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::render::Painter;

/// The format version new Designs are written with.
pub const VERSION: &str = "1.0";

/// One Design: its Canvas's top-level nodes (Screens and Components among
/// them), its Themes and its Tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Design {
    pub version: String,
    #[serde(default)]
    pub children: Vec<Node>,
    /// Theme axes and their values, kept as written until Themes are modelled.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub themes: Map<String, Value>,
    /// Tokens by name, kept as written until Tokens are modelled.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub tokens: Map<String, Value>,
}

impl Default for Design {
    fn default() -> Self {
        Self {
            version: VERSION.into(),
            children: Vec::new(),
            themes: Map::new(),
            tokens: Map::new(),
        }
    }
}

impl Design {
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// The file's text: pretty-printed, so a Design diffs well in Git.
    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("a Design serializes");
        json.push('\n');
        json
    }

    pub fn find(&self, id: &str) -> Option<&Node> {
        find_in(&self.children, id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        find_in_mut(&mut self.children, id)
    }

    /// Calls `visit` with every node, parents before their children.
    pub fn walk(&self, visit: &mut dyn FnMut(&Node)) {
        fn walk(nodes: &[Node], visit: &mut dyn FnMut(&Node)) {
            for node in nodes {
                visit(node);
                walk(node.kind.get().children(), visit);
            }
        }
        walk(&self.children, visit);
    }
}

fn find_in<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    nodes.iter().find_map(|node| {
        (node.id == id)
            .then_some(node)
            .or_else(|| find_in(node.kind.get().children(), id))
    })
}

fn find_in_mut<'a>(nodes: &'a mut [Node], id: &str) -> Option<&'a mut Node> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = node.kind.get_mut().children_mut().and_then(|c| find_in_mut(c, id)) {
            return Some(found);
        }
    }
    None
}

/// One node: what every type has, and its type's own properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its top-left corner, from its parent's (or the Canvas's origin).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub x: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub y: f32,
    #[serde(flatten)]
    pub kind: Kind,
}

fn is_zero(value: &f32) -> bool {
    *value == 0.
}

/// What each node type answers, so walking, editing and drawing a Design
/// needn't know the types.
pub trait NodeKind {
    fn size(&self) -> (f32, f32);

    /// Its children, for the types that hold them.
    fn children(&self) -> &[Node] {
        &[]
    }

    /// `None` for a type that can't hold children.
    fn children_mut(&mut self) -> Option<&mut Vec<Node>> {
        None
    }

    /// Whether its children are cut off at its bounds.
    fn clips(&self) -> bool {
        false
    }

    /// Draws the node itself, not its children, with its top-left at the
    /// painter's origin.
    fn paint(&self, painter: &mut dyn Painter);
}

macro_rules! kinds {
    ($($(#[$doc:meta])* $variant:ident = $tag:literal,)*) => {
        /// A node's type (its `type` in the file) and that type's properties.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type")]
        pub enum Kind {
            $($(#[$doc])* #[serde(rename = $tag)] $variant($variant),)*
        }

        impl Kind {
            /// The `type` names, for messages.
            pub const NAMES: &[&str] = &[$($tag),*];

            pub fn get(&self) -> &dyn NodeKind {
                match self {
                    $(Kind::$variant(kind) => kind,)*
                }
            }

            pub fn get_mut(&mut self) -> &mut dyn NodeKind {
                match self {
                    $(Kind::$variant(kind) => kind,)*
                }
            }

            pub fn name(&self) -> &'static str {
                match self {
                    $(Kind::$variant(_) => $tag,)*
                }
            }
        }
    };
}

kinds! {
    /// A container; a Screen when it's on the Canvas itself.
    Frame = "frame",
    Rectangle = "rectangle",
}

/// A fixed width and height, in pixels at 1x.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Size {
    #[serde(default)]
    pub width: f32,
    #[serde(default)]
    pub height: f32,
}
