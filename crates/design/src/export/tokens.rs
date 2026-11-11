//! A Design's Tokens as CSS custom properties: each Token's value in the
//! Theme nodes are shown in by default on `:root`, and its value in each
//! other Theme under that Theme's data attributes (`[data-mode="dark"]`),
//! which an element shown in that Theme carries.

use std::fmt::Write as _;

use serde_json::Value;

use crate::model::{Design, Theme, Token, token};

/// The CSS of `design`'s Tokens, empty without any.
pub fn css(design: &Design) -> String {
    let tokens: Vec<(&String, Token)> = design
        .tokens
        .iter()
        .filter_map(|(name, written)| Some((name, Token::read(name, written).ok()?)))
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    let default = token::default_theme(&design.themes);
    let mut css = String::from(":root {\n");
    for (name, token) in &tokens {
        let _ = writeln!(css, "  --{}: {};", ident(name), value_css(token.value_in(&default)));
    }
    css.push_str("}\n");
    // Each other Theme a value is for, in the order first named.
    let mut themes: Vec<&Theme> = Vec::new();
    for (_, token) in &tokens {
        for (_, theme) in &token.values {
            if !theme.is_empty() && !themes.contains(&theme) {
                themes.push(theme);
            }
        }
    }
    for theme in themes {
        let selector: String = theme
            .iter()
            .map(|(axis, value)| {
                let value = value.as_str().unwrap_or_default();
                format!("[data-{}={}]", ident(axis), super::css::string(value))
            })
            .collect();
        let _ = writeln!(css, "{selector} {{");
        // Only the Tokens with a value for this Theme, so an element that
        // sets one axis keeps what its parent set of the others.
        for (name, token) in &tokens {
            if let Some((value, _)) = token.values.iter().rev().find(|(_, for_theme)| for_theme == theme) {
                let _ = writeln!(css, "  --{}: {};", ident(name), value_css(value));
            }
        }
        css.push_str("}\n");
    }
    css
}

/// A Token value as CSS: a colour as written, a number as is, a string quoted.
fn value_css(value: &Value) -> String {
    match value {
        Value::String(text) if text.starts_with('#') => text.clone(),
        Value::String(text) => super::css::string(text),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// `name` as a CSS identifier: what one can't hold as `-`.
pub fn ident(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}
