//! The layout pass: every node's bounds on the Canvas, from its sizes
//! ([`Sizing`]) and its parent's [`Layout`], and the problems an agent
//! should hear of. Drawing and `Get` both read it, so the bounds an agent
//! reads are the ones drawn.
//!
//! A Frame with a layout puts its children (but [`Node::absolute`] ones) in
//! a row or column inside its padding, ignoring their x and y: fixed and
//! fit-content children take their size, fill-container ones share what's
//! left, and the rest is spread by `justifyContent` along the row and
//! `alignItems` across it. Any other child sits at its x and y.
//!
//! Widths are settled before heights, so a node's height can follow the
//! width it gets: text wraps at it ([`crate::model::NodeKind::measure`]).

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::model::{Align, Design, Direction, Justify, Layout, Node, Sizing};
use crate::render::Rect;
use crate::skia::Typesetter;

/// What a fill-container child counts for in a fit-content parent when it
/// has no content of its own and none of its siblings gives a size.
pub const FILL_FALLBACK: f32 = 100.;

/// Within this much, an edge counts as inside its clip.
const EPSILON: f32 = 0.01;

/// A Design laid out: each node's bounds and problems, by id.
#[derive(Debug, Default)]
pub struct LaidOut {
    nodes: HashMap<String, Placed>,
}

/// Where one node was put, and what's wrong with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    /// On the Canvas, not from its parent.
    pub bounds: Rect,
    pub problems: Vec<Problem>,
}

/// Something about a node's layout an agent should fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Part of it is cut off by a Frame that clips.
    PartiallyClipped,
    /// All of it is cut off by a Frame that clips.
    FullyClipped,
    /// Its size fits its content, and its children all fill it along that
    /// side, so neither can size the other.
    FitAroundFill,
    /// Its text is taller than its height, so it overflows.
    TextOverflow,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Problem::PartiallyClipped => "partially clipped",
            Problem::FullyClipped => "fully clipped",
            Problem::FitAroundFill => {
                "fit content around children that all fill container: sized from their own content, \
                 or 100 px each without any"
            }
            Problem::TextOverflow => "text taller than its height, overflowing it",
        })
    }
}

impl LaidOut {
    /// Lays out the whole Design, its text set by `typesetter` (made for
    /// it): each Canvas node at its x and y.
    pub fn of(design: &Design, typesetter: &Typesetter) -> Self {
        let mut pass = Pass {
            typesetter,
            measured: HashMap::new(),
            fit_around_fill: HashSet::new(),
            placed: HashMap::new(),
        };
        for node in &design.children {
            let size = pass.measure(node, None);
            pass.place(node, [node.x, node.y], size, None);
        }
        Self { nodes: pass.placed }
    }

    /// Where the node `id` was put.
    pub fn get(&self, id: &str) -> Option<&Placed> {
        self.nodes.get(id)
    }
}

/// One run of the layout pass, measuring each node once for each width it
/// is given.
struct Pass<'a> {
    typesetter: &'a Typesetter,
    /// Each node's size where its parent leaves it to the node, by id and
    /// the width it was given (as bits).
    measured: HashMap<(&'a str, Option<u32>), [f32; 2]>,
    /// Nodes found fitting children that all fill them.
    fit_around_fill: HashSet<&'a str>,
    placed: HashMap<String, Placed>,
}

/// A node's sizes, as `[width, height]`.
fn sizing(node: &Node) -> [Sizing; 2] {
    let (width, height) = node.kind.get().size();
    [width, height]
}

/// How `node` lays its children out, and the axis it runs along (0 for a
/// row, 1 for a column) if it does.
fn arrangement(node: &Node) -> (Layout, Option<usize>) {
    let layout = node.kind.get().layout().copied().unwrap_or_default();
    let main = match layout.layout {
        Direction::None => None,
        Direction::Horizontal => Some(0),
        Direction::Vertical => Some(1),
    };
    (layout, main)
}

/// Whether `child` is laid out in its parent's row or column.
fn in_flow(child: &Node, main: Option<usize>) -> bool {
    main.is_some() && !child.absolute
}

fn largest(extents: impl IntoIterator<Item = f32>) -> f32 {
    extents.into_iter().fold(0., f32::max)
}

impl<'a> Pass<'a> {
    /// `node`'s size when its parent doesn't decide it: fixed, or fitting
    /// its content (fill container too, with no parent's room to fill).
    /// `width` is the width its parent gives it when it fills it; its
    /// height then fits its content at that width.
    fn measure(&mut self, node: &'a Node, width: Option<f32>) -> [f32; 2] {
        let sizing = sizing(node);
        let width = match sizing[0] {
            Sizing::Fixed(pixels) => Some(pixels),
            Sizing::FitContent => None,
            Sizing::FillContainer => width,
        };
        let key = (node.id.as_str(), width.map(f32::to_bits));
        if let Some(size) = self.measured.get(&key) {
            return *size;
        }
        let content = if width.is_some() && matches!(sizing[1], Sizing::Fixed(_)) {
            [0.; 2]
        } else {
            self.content(node, sizing, width)
        };
        let height = match sizing[1] {
            Sizing::Fixed(pixels) => pixels,
            Sizing::FitContent | Sizing::FillContainer => content[1],
        };
        let size = [width.unwrap_or(content[0]), height];
        self.measured.insert(key, size);
        size
    }

    /// The size `node`'s content needs at `width` (when known): its own
    /// (text's), or its children's, inside its padding when it lays them
    /// out, and to reach its farthest one when it doesn't.
    fn content(&mut self, node: &'a Node, own: [Sizing; 2], width: Option<f32>) -> [f32; 2] {
        if let Some(size) = node.kind.get().measure(self.typesetter, width) {
            return size;
        }
        let (layout, main) = arrangement(node);
        let flow: Vec<&'a Node> = node
            .kind
            .get()
            .children()
            .iter()
            .filter(|child| main.is_none() || in_flow(child, main))
            .collect();
        let measured: Vec<[f32; 2]> = self
            .widths(&layout, main, width, &flow)
            .into_iter()
            .zip(&flow)
            .map(|(width, child)| self.measure(child, width))
            .collect();
        let mut size = [0.; 2];
        for axis in [0, 1] {
            // Children that all fill a fit-content side leave it nothing to
            // fit: each counts as its own content, or a fallback.
            let all_fill =
                !flow.is_empty() && flow.iter().all(|child| sizing(child)[axis] == Sizing::FillContainer);
            if all_fill && own[axis] == Sizing::FitContent {
                self.fit_around_fill.insert(&node.id);
            }
            let mut extents = Vec::with_capacity(flow.len());
            for (child, measured) in flow.iter().zip(&measured) {
                let mut extent = measured[axis];
                if all_fill && extent <= 0. {
                    extent = FILL_FALLBACK;
                }
                if main.is_none() {
                    extent += [child.x, child.y][axis];
                }
                extents.push(extent);
            }
            size[axis] = match main {
                None => largest(extents),
                Some(main) => {
                    let padding = padding(&layout)[axis];
                    let children = if axis == main {
                        let gaps = layout.gap * extents.len().saturating_sub(1) as f32;
                        extents.iter().sum::<f32>() + gaps
                    } else {
                        largest(extents)
                    };
                    children + padding[0] + padding[1]
                }
            };
        }
        size
    }

    /// The width each of `children` gets from a parent `width` wide (once
    /// that's known) when it fills it: the rest of it from the child's x
    /// without a layout, its inside across a column, and a share of what
    /// the others leave along a row.
    fn widths(
        &mut self,
        layout: &Layout,
        main: Option<usize>,
        width: Option<f32>,
        children: &[&'a Node],
    ) -> Vec<Option<f32>> {
        let Some(width) = width else {
            return vec![None; children.len()];
        };
        let [[left, right], _] = padding(layout);
        let inner = (width - left - right).max(0.);
        match main {
            None => children.iter().map(|child| Some((width - child.x).max(0.))).collect(),
            Some(1) => vec![Some(inner); children.len()],
            Some(_) => {
                let fills = children.iter().filter(|child| sizing(child)[0] == Sizing::FillContainer).count();
                let mut taken = layout.gap * children.len().saturating_sub(1) as f32;
                for child in children {
                    if sizing(child)[0] != Sizing::FillContainer {
                        taken += self.measure(child, None)[0];
                    }
                }
                vec![Some((inner - taken).max(0.) / fills.max(1) as f32); children.len()]
            }
        }
    }

    /// Places `node` at `origin` on the Canvas at `size`, then its children;
    /// `visible` is the part of the Canvas the Frames above it don't clip.
    fn place(&mut self, node: &'a Node, origin: [f32; 2], size: [f32; 2], visible: Option<Rect>) {
        let bounds = Rect {
            x: origin[0],
            y: origin[1],
            width: size[0],
            height: size[1],
        };
        let mut problems: Vec<Problem> = visible.and_then(|visible| clipped(bounds, visible)).into_iter().collect();
        if self.fit_around_fill.contains(&node.id.as_str()) {
            problems.push(Problem::FitAroundFill);
        }
        if let Some([_, content]) = node.kind.get().measure(self.typesetter, Some(size[0]))
            && content > size[1] + EPSILON
        {
            problems.push(Problem::TextOverflow);
        }
        self.placed.insert(node.id.clone(), Placed { bounds, problems });

        let kind = node.kind.get();
        let visible = match (kind.clips(), visible) {
            (true, Some(visible)) => Some(intersection(visible, bounds)),
            (true, None) => Some(bounds),
            (false, visible) => visible,
        };
        let (layout, main) = arrangement(node);
        let mut flow = Vec::new();
        for child in kind.children() {
            if in_flow(child, main) {
                flow.push(child);
                continue;
            }
            // At its x and y, filling the rest of its parent from there.
            let at = [child.x, child.y];
            let child_sizing = sizing(child);
            let width = match child_sizing[0] {
                Sizing::FillContainer => (size[0] - at[0]).max(0.),
                _ => self.measure(child, None)[0],
            };
            let height = match child_sizing[1] {
                Sizing::FillContainer => (size[1] - at[1]).max(0.),
                _ => self.measure(child, Some(width))[1],
            };
            let child_size = [width, height];
            self.place(child, [origin[0] + child.x, origin[1] + child.y], child_size, visible);
        }
        if let Some(main) = main {
            self.flow(&layout, main, origin, size, &flow, visible);
        }
    }

    /// Lays `children` out along `main` inside the padding of a node at
    /// `origin` and `size`.
    fn flow(
        &mut self,
        layout: &Layout,
        main: usize,
        origin: [f32; 2],
        size: [f32; 2],
        children: &[&'a Node],
        visible: Option<Rect>,
    ) {
        if children.is_empty() {
            return;
        }
        let cross = 1 - main;
        let padding = padding(layout);
        let inner_origin = [0, 1].map(|axis| origin[axis] + padding[axis][0]);
        let inner = [0, 1].map(|axis| (size[axis] - padding[axis][0] - padding[axis][1]).max(0.));

        // Widths first, then heights at them: along the row, fills share
        // what the others leave; across it, they take the inside.
        let gaps = layout.gap * (children.len() - 1) as f32;
        let mut sizes = vec![[0.; 2]; children.len()];
        for axis in [0, 1] {
            // Which of them fill along the row.
            let mut fills = Vec::new();
            for (i, child) in children.iter().enumerate() {
                sizes[i][axis] = match sizing(child)[axis] {
                    Sizing::FillContainer if axis == main => {
                        fills.push(i);
                        0.
                    }
                    Sizing::FillContainer => inner[axis],
                    _ if axis == 0 => self.measure(child, None)[0],
                    _ => self.measure(child, Some(sizes[i][0]))[1],
                };
            }
            if !fills.is_empty() {
                let taken: f32 = sizes.iter().map(|size| size[main]).sum::<f32>() + gaps;
                let share = (inner[main] - taken).max(0.) / fills.len() as f32;
                for &fill in &fills {
                    sizes[fill][main] = share;
                }
            }
        }
        let used: f32 = sizes.iter().map(|size| size[main]).sum::<f32>() + gaps;
        let left = inner[main] - used;
        let n = children.len() as f32;
        let (mut at, between) = match layout.justify_content {
            Justify::Start => (0., layout.gap),
            Justify::Center => (left / 2., layout.gap),
            Justify::End => (left, layout.gap),
            Justify::SpaceBetween if children.len() > 1 && left > 0. => (0., layout.gap + left / (n - 1.)),
            Justify::SpaceAround if left > 0. => (left / n / 2., layout.gap + left / n),
            // Overflowing, as CSS does: from the start, or around the middle.
            Justify::SpaceBetween => (0., layout.gap),
            Justify::SpaceAround => (left / 2., layout.gap),
        };
        for (child, child_size) in children.iter().zip(sizes) {
            let free = inner[cross] - child_size[cross];
            let mut position = inner_origin;
            position[main] += at;
            position[cross] += match layout.align_items {
                Align::Start => 0.,
                Align::Center => free / 2.,
                Align::End => free,
            };
            self.place(child, position, child_size, visible);
            at += child_size[main] + between;
        }
    }
}

/// Padding as `[[left, right], [top, bottom]]`, by axis.
fn padding(layout: &Layout) -> [[f32; 2]; 2] {
    let padding = layout.padding;
    [[padding.left, padding.right], [padding.top, padding.bottom]]
}

/// How much of `bounds` is cut off outside `visible`, if any. A `visible`
/// with no area is clips that don't overlap: nothing shows inside them.
fn clipped(bounds: Rect, visible: Rect) -> Option<Problem> {
    let (right, bottom) = (bounds.x + bounds.width, bounds.y + bounds.height);
    let (visible_right, visible_bottom) = (visible.x + visible.width, visible.y + visible.height);
    // A node with no area is out only past the edges, not on them.
    let outside = if bounds.width <= 0. || bounds.height <= 0. {
        right < visible.x || bounds.x > visible_right || bottom < visible.y || bounds.y > visible_bottom
    } else {
        right <= visible.x || bounds.x >= visible_right || bottom <= visible.y || bounds.y >= visible_bottom
    };
    if visible.width <= 0. || visible.height <= 0. || outside {
        Some(Problem::FullyClipped)
    } else if bounds.x < visible.x - EPSILON
        || bounds.y < visible.y - EPSILON
        || right > visible_right + EPSILON
        || bottom > visible_bottom + EPSILON
    {
        Some(Problem::PartiallyClipped)
    } else {
        None
    }
}

/// The overlap of two rectangles, empty (at `a`'s corner) when none.
fn intersection(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    Rect {
        x,
        y,
        width: (right - x).max(0.),
        height: (bottom - y).max(0.),
    }
}
