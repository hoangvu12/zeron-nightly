//! Each node's CSS, as declarations: where it sits and how big it is (by
//! its parent's layout and its own sizes), what its Frame lays out, and
//! how it looks.

use serde_json::{Map, Value};

use super::tokens;
use crate::layout::LaidOut;
use crate::model::{
    Align, Color, Direction, EffectKind, Justify, Kind, Node, Paint, PaintKind, Sizing, StrokeAlign, TextAlign,
    TextGrowth, VerticalAlign,
};
use crate::model::{GradientShape, ImageMode};
use crate::render::{Look, Outline};

/// What a node sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parent {
    /// Nothing: a Screen on the page.
    Page,
    /// A Frame that places its children where they say.
    Free,
    /// A Frame that lays them out in a row (or a column).
    Flex { row: bool },
}

impl Parent {
    /// What `node` is to its children.
    pub fn of(node: &Node) -> Parent {
        match node.kind.get().layout().map(|layout| layout.layout) {
            Some(Direction::Horizontal) => Parent::Flex { row: true },
            Some(Direction::Vertical) => Parent::Flex { row: false },
            _ => Parent::Free,
        }
    }
}

/// A node as an element: its tag, its declarations, and for an SVG its
/// attributes and content.
pub struct Styled {
    pub tag: &'static str,
    pub declarations: Vec<(String, String)>,
    pub svg_attributes: String,
    pub inner: String,
}

/// `node`'s element, in `parent`, as `laid_out` placed it; `refs` are its
/// properties that name Tokens, as written.
pub fn node(
    node: &Node,
    parent: Parent,
    laid_out: &LaidOut,
    refs: &Map<String, Value>,
    image_href: &dyn Fn(&str) -> String,
) -> Styled {
    let placed = laid_out.get(&node.id).map(|placed| placed.bounds);
    let size = placed.map_or((0., 0.), |bounds| (bounds.width, bounds.height));
    let kind = node.kind.get();
    let look = kind.look(size);
    let plain_ellipse = matches!(&node.kind, Kind::Ellipse(e) if e.sweep_angle.abs() >= 360. && e.inner_radius <= 0.);
    let svg = look
        .as_ref()
        .is_some_and(|look| !matches!(look.outline, Outline::Rounded { .. }) && !plain_ellipse);
    let tag = match (&node.kind, svg) {
        (_, true) => "svg",
        (Kind::Text(_), _) => "p",
        _ => "div",
    };
    let mut out = Styled { tag, declarations: Vec::new(), svg_attributes: String::new(), inner: String::new() };
    let d = &mut out.declarations;
    let mut set = |property: &str, value: String| d.push((property.to_owned(), value));

    // Where it sits.
    match parent {
        Parent::Page => set("position", "relative".into()),
        Parent::Free => {
            set("position", "absolute".into());
            set("left", px(node.x));
            set("top", px(node.y));
        }
        Parent::Flex { .. } if node.absolute => {
            set("position", "absolute".into());
            set("left", px(node.x));
            set("top", px(node.y));
        }
        // A Frame, so what's absolute inside it is placed from it.
        Parent::Flex { .. } if kind.layout().is_some() => set("position", "relative".into()),
        Parent::Flex { .. } => {}
    }

    // How big it is.
    let (width, height) = kind.size();
    let shrinks = matches!(&node.kind, Kind::Text(_)) || kind.layout().is_some_and(|l| l.layout != Direction::None);
    let in_flex = match parent {
        Parent::Flex { row } if !node.absolute => Some(row),
        _ => None,
    };
    for (property, sizing, laid, main) in [
        ("width", width, size.0, in_flex == Some(true)),
        ("height", height, size.1, in_flex == Some(false)),
    ] {
        match sizing {
            Sizing::Fixed(length) => {
                set(property, sized(refs, property, length));
                if in_flex.is_some() {
                    set("flex-shrink", "0".into());
                }
            }
            Sizing::FillContainer if in_flex.is_some() && main => {
                set("flex", "1 1 0".into());
                set(if property == "width" { "min-width" } else { "min-height" }, "0".into());
            }
            Sizing::FillContainer if in_flex.is_some() => set("align-self", "stretch".into()),
            Sizing::FillContainer if parent == Parent::Page => set(property, px(laid)),
            Sizing::FillContainer => set(property, "100%".into()),
            Sizing::FitContent if shrinks && parent != Parent::Page => {}
            Sizing::FitContent => set(property, px(laid)),
        }
    }
    dedupe(d);

    // What it lays out.
    if let Some(layout) = kind.layout() {
        if layout.layout != Direction::None {
            let mut set = |property: &str, value: String| d.push((property.to_owned(), value));
            set("display", "flex".into());
            set("flex-direction", if layout.layout == Direction::Horizontal { "row" } else { "column" }.into());
            if layout.gap != 0. || refs.contains_key("gap") {
                set("gap", sized(refs, "gap", layout.gap));
            }
            let p = layout.padding;
            if [p.top, p.right, p.bottom, p.left].iter().any(|side| *side != 0.) {
                set("padding", format!("{} {} {} {}", px(p.top), px(p.right), px(p.bottom), px(p.left)));
            }
            set("justify-content", justify(layout.justify_content).into());
            set("align-items", align(layout.align_items).into());
        }
    }
    if kind.clips() {
        d.push(("overflow".into(), "hidden".into()));
    }

    // How it looks.
    if let Kind::Text(text) = &node.kind {
        text_style(d, text, refs);
        if let Some(look) = &look {
            shadows(d, look, true);
        }
    } else if let Kind::Icon(icon) = &node.kind {
        d.push(("color".into(), color(icon.color(), 1.)));
    } else if let Some(look) = &look {
        if svg {
            svg_shape(&mut out, look, size);
        } else {
            let d = &mut out.declarations;
            if let Some(background) = background(look.fill, refs, image_href) {
                d.extend(background);
            }
            if plain_ellipse {
                d.push(("border-radius".into(), "50%".into()));
            } else if let Outline::Rounded { radii, .. } = look.outline
                && radii.iter().any(|radius| *radius > 0.)
            {
                let value = if radii.iter().all(|r| *r == radii[0]) {
                    sized(refs, "cornerRadius", radii[0])
                } else {
                    radii.map(px).join(" ")
                };
                d.push(("border-radius".into(), value));
            }
            stroke(d, look);
            shadows(d, look, false);
        }
    }
    let d = &mut out.declarations;
    let blur = look.as_ref().map(|look| look.effects.effect.iter().filter(|e| e.enabled));
    for effect in blur.into_iter().flatten() {
        match effect.kind {
            EffectKind::LayerBlur(radius) => d.push(("filter".into(), format!("blur({})", px(radius / 2.)))),
            EffectKind::BackgroundBlur(radius) => {
                d.push(("backdrop-filter".into(), format!("blur({})", px(radius / 2.))));
            }
            _ => {}
        }
    }
    if node.opacity < 1. {
        d.push(("opacity".into(), number(node.opacity)));
    }
    let mut transform = Vec::new();
    if node.rotation != 0. {
        transform.push(format!("rotate({}deg)", number(-node.rotation)));
    }
    if node.flip_x || node.flip_y {
        transform.push(format!("translate({}, {})", px(if node.flip_x { size.0 } else { 0. }), px(if node.flip_y { size.1 } else { 0. })));
        transform.push(format!("scale({}, {})", if node.flip_x { -1 } else { 1 }, if node.flip_y { -1 } else { 1 }));
    }
    if !transform.is_empty() {
        d.push(("transform-origin".into(), "0 0".into()));
        d.push(("transform".into(), transform.join(" ")));
    }
    out
}

fn text_style(d: &mut Vec<(String, String)>, text: &crate::model::Text, refs: &Map<String, Value>) {
    let mut set = |property: &str, value: String| d.push((property.to_owned(), value));
    let color_value = match whole_ref(refs, "fill") {
        Some(name) => format!("var(--{})", tokens::ident(name)),
        None => first_solid(&text.fill.fill).map_or("transparent".into(), |(c, o)| color(c, o)),
    };
    set("color", color_value);
    let family = match whole_ref(refs, "fontFamily") {
        Some(name) => format!("var(--{})", tokens::ident(name)),
        None => format!("\"{}\"", text.family()),
    };
    set("font-family", format!("{family}, sans-serif"));
    set("font-size", sized(refs, "fontSize", text.font_size()));
    if text.weight().0 != 400 {
        set("font-weight", text.weight().0.to_string());
    }
    // Unset, the font's own, as a page whose base sets one (Tailwind's) wouldn't.
    set("line-height", text.line_height.map_or("normal".into(), number));
    if text.letter_spacing != 0. {
        set("letter-spacing", px(text.letter_spacing));
    }
    match text.text_align {
        TextAlign::Left => {}
        TextAlign::Center => set("text-align", "center".into()),
        TextAlign::Right => set("text-align", "right".into()),
        TextAlign::Justify => set("text-align", "justify".into()),
    }
    let decorations: Vec<&str> =
        [(text.underline, "underline"), (text.strikethrough, "line-through")].iter().filter(|(on, _)| *on).map(|(_, d)| *d).collect();
    if !decorations.is_empty() {
        set("text-decoration", decorations.join(" "));
    }
    match text.growth() {
        TextGrowth::Auto => set("white-space", "pre".into()),
        TextGrowth::FixedWidth => set("white-space", "pre-wrap".into()),
        TextGrowth::FixedWidthHeight => {
            set("white-space", "pre-wrap".into());
            if text.text_align_vertical != VerticalAlign::Top {
                set("display", "flex".into());
                set("flex-direction", "column".into());
                let at = if text.text_align_vertical == VerticalAlign::Middle { "center" } else { "flex-end" };
                set("justify-content", at.into());
            }
        }
    }
}

/// `look`'s fills as a CSS background, the last drawn on top.
fn background(
    paints: &[Paint],
    refs: &Map<String, Value>,
    image_href: &dyn Fn(&str) -> String,
) -> Option<Vec<(String, String)>> {
    let paints: Vec<&Paint> = paints.iter().filter(|paint| paint.enabled).collect();
    if paints.is_empty() {
        return None;
    }
    if let [paint] = paints.as_slice()
        && let PaintKind::Solid(c) = paint.kind
    {
        let value = match whole_ref(refs, "fill") {
            Some(name) => format!("var(--{})", tokens::ident(name)),
            None => color(c, paint.opacity),
        };
        return Some(vec![("background-color".into(), value)]);
    }
    let layers: Vec<String> = paints
        .iter()
        .rev()
        .filter_map(|paint| match &paint.kind {
            PaintKind::Solid(c) => {
                let c = color(*c, paint.opacity);
                Some(format!("linear-gradient({c}, {c})"))
            }
            PaintKind::Gradient(gradient) => {
                let stops: Vec<String> = gradient
                    .stops
                    .iter()
                    .map(|stop| format!("{} {}%", color(stop.color, paint.opacity), number(stop.position * 100.)))
                    .collect();
                let at = format!("{}% {}%", number(gradient.center.0 * 100.), number(gradient.center.1 * 100.));
                Some(match gradient.shape {
                    // Upwards at 0, turning counter-clockwise; CSS turns clockwise.
                    GradientShape::Linear => {
                        format!("linear-gradient({}deg, {})", number((-gradient.rotation).rem_euclid(360.)), stops.join(", "))
                    }
                    GradientShape::Radial => format!(
                        "radial-gradient({}% {}% at {at}, {})",
                        number(gradient.size.0 * 50.),
                        number(gradient.size.1 * 50.),
                        stops.join(", ")
                    ),
                    GradientShape::Angular => format!(
                        "conic-gradient(from {}deg at {at}, {})",
                        number((-gradient.rotation).rem_euclid(360.)),
                        stops.join(", ")
                    ),
                })
            }
            PaintKind::Image(image) => {
                let size = match image.mode {
                    ImageMode::Cover => "cover",
                    ImageMode::Contain => "contain",
                    ImageMode::Stretch => "100% 100%",
                };
                Some(format!("url(\"{}\") center / {size} no-repeat", image_href(&image.url)))
            }
            PaintKind::Unsupported(_) => None,
        })
        .collect();
    (!layers.is_empty()).then(|| vec![("background".into(), layers.join(", "))])
}

/// A stroke as CSS: one width all round as a ring of box shadows (which
/// follow the corners, inside, outside or across the edge), otherwise as
/// borders.
fn stroke(d: &mut Vec<(String, String)>, look: &Look) {
    let Some(stroke) = look.stroke else {
        return;
    };
    let Some((c, opacity)) = first_solid(&stroke.fill) else {
        return;
    };
    let c = color(c, opacity);
    let t = stroke.thickness;
    let uniform = t.top == t.right && t.top == t.bottom && t.top == t.left;
    if uniform && stroke.dashes.is_empty() {
        let width = t.top;
        let rings = match stroke.align {
            StrokeAlign::Inside => vec![format!("inset 0 0 0 {} {c}", px(width))],
            StrokeAlign::Outside => vec![format!("0 0 0 {} {c}", px(width))],
            StrokeAlign::Center => {
                vec![format!("inset 0 0 0 {} {c}", px(width / 2.)), format!("0 0 0 {} {c}", px(width / 2.))]
            }
        };
        add_shadows(d, rings);
    } else {
        let style = if stroke.dashes.is_empty() { "solid" } else { "dashed" };
        d.push(("border-style".into(), style.into()));
        d.push(("border-color".into(), c));
        d.push(("border-width".into(), format!("{} {} {} {}", px(t.top), px(t.right), px(t.bottom), px(t.left))));
    }
}

/// Drop and inner shadows as box shadows, or, for text, text shadows.
fn shadows(d: &mut Vec<(String, String)>, look: &Look, text: bool) {
    let mut shadows = Vec::new();
    for effect in look.effects.effect.iter().filter(|effect| effect.enabled) {
        if let EffectKind::Shadow(shadow) = &effect.kind {
            let (x, y) = shadow.offset;
            let c = color(shadow.color, 1.);
            shadows.push(if text {
                format!("{} {} {} {c}", px(x), px(y), px(shadow.blur))
            } else {
                let inset = if shadow.inner { "inset " } else { "" };
                format!("{inset}{} {} {} {} {c}", px(x), px(y), px(shadow.blur), px(shadow.spread))
            });
        }
    }
    if text {
        if !shadows.is_empty() {
            d.push(("text-shadow".into(), shadows.join(", ")));
        }
    } else {
        add_shadows(d, shadows);
    }
}

fn add_shadows(d: &mut Vec<(String, String)>, shadows: Vec<String>) {
    if shadows.is_empty() {
        return;
    }
    match d.iter_mut().find(|(property, _)| property == "box-shadow") {
        Some((_, value)) => {
            value.push_str(", ");
            value.push_str(&shadows.join(", "));
        }
        None => d.push(("box-shadow".into(), shadows.join(", "))),
    }
}

/// A shape other than a rounded rectangle, as an SVG of its outline.
fn svg_shape(out: &mut Styled, look: &Look, (width, height): (f32, f32)) {
    let (path, even_odd) = crate::skia::svg_outline(&look.outline);
    let fill = first_solid(look.fill).map_or("none".into(), |(c, o)| color(c, o));
    let mut attributes = format!(" d=\"{path}\" fill=\"{fill}\"");
    if even_odd {
        attributes.push_str(" fill-rule=\"evenodd\"");
    }
    if let Some(stroke) = look.stroke
        && let Some((c, opacity)) = first_solid(&stroke.fill)
    {
        attributes.push_str(&format!(" stroke=\"{}\" stroke-width=\"{}\"", color(c, opacity), number(stroke.thickness.top)));
    }
    out.svg_attributes = format!(" viewBox=\"0 0 {} {}\" overflow=\"visible\"", number(width), number(height));
    out.inner = format!("<path{attributes}/>");
    shadows(&mut out.declarations, look, false);
    // An SVG draws its shadows around its box, not its shape.
    if let Some(at) = out.declarations.iter().position(|(property, _)| property == "box-shadow") {
        let (_, value) = out.declarations.remove(at);
        out.declarations.push(("filter".into(), format!("drop-shadow({value})")));
    }
}

/// The first enabled solid paint's colour, and its opacity.
fn first_solid(paints: &[Paint]) -> Option<(Color, f32)> {
    paints.iter().filter(|paint| paint.enabled).find_map(|paint| match paint.kind {
        PaintKind::Solid(c) => Some((c, paint.opacity)),
        _ => None,
    })
}

/// The Token `key` names, when the whole property is one reference.
fn whole_ref<'a>(refs: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    refs.get(key).and_then(Value::as_str).and_then(crate::model::token::reference)
}

/// A length, through the number Token the property names if it does.
fn sized(refs: &Map<String, Value>, key: &str, length: f32) -> String {
    match whole_ref(refs, key) {
        Some(name) => format!("calc(var(--{}) * 1px)", tokens::ident(name)),
        None => px(length),
    }
}

/// Later declarations of a property win; keep the last only.
fn dedupe(d: &mut Vec<(String, String)>) {
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<(String, String)> = d.drain(..).rev().filter(|(property, _)| seen.insert(property.clone())).collect();
    kept.reverse();
    *d = kept;
}

fn justify(justify: Justify) -> &'static str {
    match justify {
        Justify::Start => "flex-start",
        Justify::Center => "center",
        Justify::End => "flex-end",
        Justify::SpaceBetween => "space-between",
        Justify::SpaceAround => "space-around",
    }
}

fn align(align: Align) -> &'static str {
    match align {
        Align::Start => "flex-start",
        Align::Center => "center",
        Align::End => "flex-end",
    }
}

pub(super) fn color(Color { r, g, b, a }: Color, opacity: f32) -> String {
    let a = (f32::from(a) * opacity.clamp(0., 1.)).round() as u8;
    if a == 255 { format!("#{r:02X}{g:02X}{b:02X}") } else { format!("#{r:02X}{g:02X}{b:02X}{a:02X}") }
}

pub(super) fn px(length: f32) -> String {
    format!("{}px", number(length))
}

/// A number as short as it reads: to a hundredth, without a trailing zero.
pub(super) fn number(n: f32) -> String {
    let rounded = (f64::from(n) * 100.).round() / 100.;
    if rounded == rounded.trunc() { format!("{}", rounded as i64) } else { format!("{rounded}") }
}
