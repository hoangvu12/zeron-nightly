//! A Design as its `.zdesign` file holds it: pretty-printed JSON in
//! pen.dev's shape, with the glossary's words (Tokens, not variables).
//!
//! A node is its common properties ([`Node`]) and its type's own
//! ([`Kind`]). Each type lives in a file of its own under `model/`, made of
//! property groups shared by the types that have them ([`Size`], [`Fill`]),
//! and joins [`Kind`] with one line in its `kinds!` list.
//!
//! What the model doesn't know is kept, not dropped ([`kept`]): a node's
//! unknown properties, a node of an unknown type ([`Opaque`]) and the
//! Design's unknown keys are written back as they were read.

mod corners;
mod effects;
mod ellipse;
mod frame;
mod icon;
mod image;
mod kept;
mod layout;
mod paint;
mod path;
mod polygon;
mod rectangle;
mod stroke;
mod text;
pub mod warn;

pub use ellipse::Ellipse;
pub use corners::Corners;
pub use effects::{Effect, EffectKind, Effects, Shadow};
pub use frame::Frame;
pub use icon::Icon;
pub use image::{ImageFill, ImageMode};
pub use layout::{Align, Direction, Justify, Layout, Padding, Sizing};
pub use paint::{BlendMode, Color, Fill, Gradient, GradientShape, Paint, PaintKind, Stop};
pub use path::{FillRule, Path};
pub use polygon::Polygon;
pub use rectangle::Rectangle;
pub use stroke::{Cap, Join, Sides, Stroke, StrokeAlign, StrokeStyle};
pub use text::{FontSize, FontWeight, Text, TextAlign, TextGrowth, VerticalAlign};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::assets::AssetKey;
use crate::render::{Look, Painter};
use crate::skia::Typesetter;

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
    /// Keys the model doesn't know, written back as they were read.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Design {
    fn default() -> Self {
        Self {
            version: VERSION.into(),
            children: Vec::new(),
            themes: Map::new(),
            tokens: Map::new(),
            extra: Map::new(),
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

    /// How deep the node is: 1 on the Canvas, 2 in a Screen, and so on.
    pub fn depth_of(&self, id: &str) -> Option<usize> {
        fn depth_in(nodes: &[Node], id: &str, depth: usize) -> Option<usize> {
            nodes.iter().find_map(|node| {
                (node.id == id)
                    .then_some(depth)
                    .or_else(|| depth_in(node.kind.get().children(), id, depth + 1))
            })
        }
        depth_in(&self.children, id, 1)
    }

    /// Where the node is: its parent's id (none on the Canvas) and its
    /// index among its siblings.
    pub fn position_of(&self, id: &str) -> Option<(Option<String>, usize)> {
        fn position_in(nodes: &[Node], parent: Option<&str>, id: &str) -> Option<(Option<String>, usize)> {
            nodes.iter().enumerate().find_map(|(index, node)| {
                (node.id == id)
                    .then(|| (parent.map(str::to_owned), index))
                    .or_else(|| position_in(node.kind.get().children(), Some(&node.id), id))
            })
        }
        position_in(&self.children, None, id)
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

/// One node: what every type has, its type's own properties, and what the
/// model doesn't know of it. Read and written as one JSON object ([`kept`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    /// Its top-left corner, from its parent's (or the Canvas's origin).
    pub x: f32,
    pub y: f32,
    /// Degrees counter-clockwise, about its top-left corner; it turns its
    /// children with it.
    pub rotation: f32,
    /// Mirrored left to right (and top to bottom) within its box, with its
    /// children.
    pub flip_x: bool,
    pub flip_y: bool,
    /// 0 to 1, for the node and its children as one.
    pub opacity: f32,
    /// Placed at its x and y even in a parent that lays its children out,
    /// and left out of that layout.
    pub absolute: bool,
    /// An agent is still building it (a Screen's mark while it works): the
    /// Design tab shows it as in progress.
    pub in_progress: bool,
    pub kind: Kind,
    /// Properties the model doesn't know (all but its id, for an
    /// [`Opaque`] node), written back as they were read.
    pub extra: Map<String, Value>,
}

/// What each node type answers, so walking, editing and drawing a Design
/// needn't know the types.
pub trait NodeKind {
    /// Its width and height as set, or its type's defaults.
    fn size(&self) -> (Sizing, Sizing);

    /// How it lays its children out, for the types that can.
    fn layout(&self) -> Option<&Layout> {
        None
    }

    /// Its children, for the types that hold them.
    fn children(&self) -> &[Node] {
        &[]
    }

    /// `None` for a type that can't hold children.
    fn children_mut(&mut self) -> Option<&mut Vec<Node>> {
        None
    }

    /// The assets it draws with ([`crate::assets`]): by default, the images
    /// its enabled fills draw.
    fn assets(&self) -> Vec<AssetKey> {
        let Some(look) = self.look((0., 0.)) else {
            return Vec::new();
        };
        look.fill
            .iter()
            .filter(|paint| paint.enabled)
            .filter_map(|paint| match &paint.kind {
                PaintKind::Image(image) => Some(AssetKey::image(&image.url)),
                _ => None,
            })
            .collect()
    }

    /// Whether its children are cut off at its bounds.
    fn clips(&self) -> bool {
        false
    }

    /// Its outline at its laid-out `size`, and the fills, stroke and
    /// effects drawn on it, for the types drawn that way.
    fn look(&self, _size: (f32, f32)) -> Option<Look<'_>> {
        None
    }

    /// Draws what the node draws itself beyond its [`look`](Self::look),
    /// not its children, at its laid-out `size` with its top-left at the
    /// painter's origin.
    fn paint(&self, _size: (f32, f32), _painter: &mut dyn Painter) {}

    /// The size of its own content, for a type whose content isn't its
    /// children (text), at `width` when its parent decides that.
    fn measure(&self, _typesetter: &Typesetter, _width: Option<f32>) -> Option<[f32; 2]> {
        None
    }
}

macro_rules! kinds {
    ($($(#[$doc:meta])* $variant:ident = $tag:literal,)*) => {
        /// A node's type (its `type` in the file) and that type's properties.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type")]
        pub enum Kind {
            $($(#[$doc])* #[serde(rename = $tag)] $variant($variant),)*
            /// A type the model doesn't know, kept as it was read.
            #[serde(skip)]
            Opaque(Opaque),
        }

        impl Kind {
            /// The `type` names the model knows, for messages.
            pub const NAMES: &[&str] = &[$($tag),*];

            pub fn get(&self) -> &dyn NodeKind {
                match self {
                    $(Kind::$variant(kind) => kind,)*
                    Kind::Opaque(kind) => kind,
                }
            }

            pub fn get_mut(&mut self) -> &mut dyn NodeKind {
                match self {
                    $(Kind::$variant(kind) => kind,)*
                    Kind::Opaque(kind) => kind,
                }
            }

            /// Whether it's a type the model knows (not [`Opaque`]).
            pub fn is_known(&self) -> bool {
                !matches!(self, Kind::Opaque(_))
            }

            /// Its `type`.
            pub fn name(&self) -> &str {
                match self {
                    $(Kind::$variant(_) => $tag,)*
                    Kind::Opaque(kind) => &kind.type_name,
                }
            }
        }
    };
}

kinds! {
    /// A container. On the Canvas itself, a Screen or a Component.
    Frame = "frame",
    Rectangle = "rectangle",
    Text = "text",
    Ellipse = "ellipse",
    Polygon = "polygon",
    Path = "path",
    Icon = "icon",
}

/// A node of a type the model doesn't know (one a newer Zeron wrote, say):
/// its properties stay in [`Node::extra`], it draws nothing and holds no
/// children an edit can reach.
#[derive(Debug, Clone, PartialEq)]
pub struct Opaque {
    pub type_name: String,
}

impl NodeKind for Opaque {
    fn size(&self) -> (Sizing, Sizing) {
        (Sizing::Fixed(0.), Sizing::Fixed(0.))
    }

    fn paint(&self, _size: (f32, f32), _painter: &mut dyn Painter) {}
}

/// A width and height, each fixed, fit content or fill container; unset,
/// each is its type's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Size {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Sizing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Sizing>,
}

impl Size {
    /// Each side as set, or `default`.
    pub fn or(self, default: Sizing) -> (Sizing, Sizing) {
        (self.width.unwrap_or(default), self.height.unwrap_or(default))
    }
}
