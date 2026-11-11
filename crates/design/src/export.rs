//! Exporting a Design's Screens as HTML, for a coding agent to start an
//! implementation from: one file, styled with plain CSS or Tailwind, from
//! the Design as drawn ([`crate::resolve::drawn`]).
//!
//! A Frame that lays its children out is a flexbox, and any other child
//! sits where it's placed, absolutely; where CSS can't size a node as the
//! layout pass does (a Frame without a layout that fits its children), it
//! takes the size the layout gave it. Fills, strokes, effects and text
//! become their CSS; a shape other than a rounded rectangle, and an icon,
//! is an inline SVG. Each node's style is a list of CSS declarations
//! ([`css`]), written as a rule of its own class or as Tailwind classes
//! ([`tailwind`]). Tokens are CSS custom properties per Theme
//! ([`tokens`]), which a property that names a Token uses.

use std::fmt::Write as _;

use serde_json::Value;

use crate::assets::Assets;
use crate::fonts::Fonts;
use crate::layout::LaidOut;
use crate::model::{Design, Kind, Node};
use crate::resolve::{self, Resolve};

mod css;
mod tailwind;
pub mod tokens;

/// How an HTML export is styled and wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlOptions {
    pub tailwind: bool,
    /// A whole page (doctype, head and body), or just the Screens with
    /// their styles.
    pub scaffold: bool,
    /// Each element's layer name as `data-name`.
    pub layer_names: bool,
}

/// What an HTML export reads besides the Design.
pub struct Sources<'a> {
    pub fonts: &'a Fonts,
    /// The icons' SVG.
    pub assets: &'a dyn Assets,
    /// Where the page finds the image `url` names: a path relative to the
    /// page for a file beside the Design, or the URL itself.
    pub image_href: &'a dyn Fn(&str) -> String,
}

/// The Screens `ids` (or, without any, every top-level node that isn't a
/// Component) of `design` as one HTML file.
pub fn html(design: &Design, ids: &[String], options: HtmlOptions, sources: &Sources) -> Result<String, String> {
    let drawn = resolve::drawn(design);
    // The nodes as written but for their Instances, to see which of their
    // properties name a Token.
    let written = resolve::resolved(design, Resolve { instances: true, tokens: false, enabled_only: true }).0;
    let typesetter = sources.fonts.typesetter(&drawn);
    let laid_out = LaidOut::of(&drawn, &typesetter);
    let roots: Vec<&Node> = if ids.is_empty() {
        drawn.children.iter().filter(|node| !node.is_component()).collect()
    } else {
        ids.iter()
            .map(|id| drawn.find(id).ok_or_else(|| format!("no node has the id \"{id}\" to export")))
            .collect::<Result<_, _>>()?
    };
    let mut page = Page {
        options,
        sources,
        laid_out: &laid_out,
        written: &written,
        rules: String::new(),
        body: String::new(),
        families: Vec::new(),
    };
    for root in &roots {
        page.element(root, css::Parent::Page, 2);
    }
    Ok(page.finish(&drawn))
}

struct Page<'a> {
    options: HtmlOptions,
    sources: &'a Sources<'a>,
    laid_out: &'a LaidOut,
    written: &'a Design,
    /// Each node's rule, for plain CSS.
    rules: String,
    body: String,
    /// The font families and weights the text uses, for Google Fonts.
    families: Vec<(String, u16)>,
}

impl Page<'_> {
    fn element(&mut self, node: &Node, parent: css::Parent, depth: usize) {
        let indent = "  ".repeat(depth);
        let refs = self.written.find(&node.id).map(|written| &written.refs);
        let empty = serde_json::Map::new();
        let styled = css::node(node, parent, self.laid_out, refs.unwrap_or(&empty), self.sources.image_href);
        let mut attributes = String::new();
        if self.options.tailwind {
            let _ = write!(attributes, " class=\"{}\"", attribute(&tailwind::classes(&styled.declarations)));
        } else {
            let class = class_of(&node.id);
            let _ = write!(attributes, " class=\"{class}\"");
            let _ = writeln!(self.rules, ".{class} {{");
            for (property, value) in &styled.declarations {
                let _ = writeln!(self.rules, "  {property}: {value};");
            }
            self.rules.push_str("}\n");
        }
        if self.options.layer_names && !node.name.is_empty() {
            let _ = write!(attributes, " data-name=\"{}\"", attribute(&node.name));
        }
        for (axis, value) in &node.theme {
            let value = value.as_str().unwrap_or_default();
            let _ = write!(attributes, " data-{}=\"{}\"", tokens::ident(axis), attribute(value));
        }
        let _ = write!(self.body, "{indent}<{}{attributes}", styled.tag);
        match &node.kind {
            Kind::Text(text) => {
                self.families.push((text.family().to_owned(), text.weight().0));
                let _ = writeln!(self.body, ">{}</{}>", escape(&text.content), styled.tag);
            }
            Kind::Icon(icon) => {
                let svg = crate::icons::name(&icon.icon, icon.weight)
                    .ok()
                    .and_then(|name| self.sources.assets.get(&crate::assets::AssetKey::icon(name)))
                    .map(|svg| sized_svg(&String::from_utf8_lossy(&svg)))
                    .unwrap_or_default();
                let _ = writeln!(self.body, ">{svg}</{}>", styled.tag);
            }
            _ if styled.tag == "svg" => {
                let _ = writeln!(self.body, "{}>{}</svg>", styled.svg_attributes, styled.inner);
            }
            kind => {
                let children = kind.get().children();
                if children.is_empty() {
                    let _ = writeln!(self.body, "></{}>", styled.tag);
                } else {
                    self.body.push_str(">\n");
                    let inside = css::Parent::of(node);
                    for child in children {
                        self.element(child, inside, depth + 1);
                    }
                    let _ = writeln!(self.body, "{indent}</{}>", styled.tag);
                }
            }
        }
    }

    fn finish(self, drawn: &Design) -> String {
        let mut head = String::new();
        if let Some(link) = google_fonts(&self.families) {
            let _ = writeln!(head, "  <link rel=\"preconnect\" href=\"https://fonts.gstatic.com\" crossorigin>");
            let _ = writeln!(head, "  <link rel=\"stylesheet\" href=\"{}\">", attribute(&link));
        }
        if self.options.tailwind {
            head.push_str("  <script src=\"https://cdn.tailwindcss.com\"></script>\n");
        }
        let mut style = tokens::css(drawn);
        if !self.options.tailwind {
            style.push_str("* { box-sizing: border-box; margin: 0; }\n");
        }
        if self.options.scaffold {
            style.push_str(
                "body { display: flex; flex-direction: column; align-items: flex-start; gap: 40px; padding: 40px; }\n",
            );
        }
        style.push_str(&self.rules);
        let _ = write!(head, "  <style>\n{}  </style>\n", indented(&style, 4));
        if !self.options.scaffold {
            return format!("{head}{}", self.body);
        }
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  \
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n{head}</head>\n\
             <body>\n{}</body>\n</html>\n",
            self.body
        )
    }
}

/// A Google Fonts stylesheet for `families`, each with the weights used.
fn google_fonts(families: &[(String, u16)]) -> Option<String> {
    let mut by_family: Vec<(&str, Vec<u16>)> = Vec::new();
    for (family, weight) in families {
        match by_family.iter_mut().find(|(name, _)| name == family) {
            Some((_, weights)) if !weights.contains(weight) => weights.push(*weight),
            Some(_) => {}
            None => by_family.push((family, vec![*weight])),
        }
    }
    if by_family.is_empty() {
        return None;
    }
    let mut url = String::from("https://fonts.googleapis.com/css2?");
    for (family, mut weights) in by_family {
        weights.sort_unstable();
        let weights: Vec<String> = weights.iter().map(u16::to_string).collect();
        let _ = write!(url, "family={}:wght@{}&", family.replace(' ', "+"), weights.join(";"));
    }
    url.push_str("display=swap");
    Some(url)
}

/// An icon's SVG filling its box: the root's own size (in em, Iconify's)
/// replaced.
fn sized_svg(svg: &str) -> String {
    let Some(start) = svg.find("<svg") else {
        return String::new();
    };
    let Some(end) = svg[start..].find('>').map(|end| start + end) else {
        return String::new();
    };
    let mut tag = String::new();
    let mut rest = &svg[start + 4..end];
    // Drop the root's width and height attributes.
    while let Some(at) = rest.find(|c: char| !c.is_whitespace()) {
        rest = &rest[at..];
        let name_end = rest.find(['=', ' ', '/']).unwrap_or(rest.len());
        let name = &rest[..name_end];
        let attribute_end = match rest[name_end..].strip_prefix('=') {
            Some(value) => {
                let quote = value.chars().next().unwrap_or('"');
                let close = value[1..].find(quote).map_or(value.len(), |at| at + 2);
                name_end + 1 + close
            }
            None => name_end.max(1),
        };
        if name != "width" && name != "height" {
            tag.push(' ');
            tag.push_str(&rest[..attribute_end]);
        }
        rest = &rest[attribute_end..];
    }
    format!("<svg width=\"100%\" height=\"100%\"{}>{}", tag.trim_end_matches(" /"), &svg[end + 1..])
}

/// The class a node's rule is for.
fn class_of(id: &str) -> String {
    format!("n-{}", id.replace(|c: char| !c.is_ascii_alphanumeric(), "-"))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn attribute(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

fn indented(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines().map(|line| format!("{pad}{line}\n")).collect()
}

/// A Token value as CSS: a colour as written, a number as is, a string quoted.
pub(crate) fn token_value(value: &Value) -> String {
    match value {
        Value::String(text) if text.starts_with('#') => text.clone(),
        Value::String(text) => format!("\"{}\"", text.replace('"', "\\\"")),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}
