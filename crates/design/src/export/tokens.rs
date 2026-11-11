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
        let _ = writeln!(css, "  --{}: {};", ident(name), super::token_value(token.value_in(&default)));
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
            .map(|(axis, value)| format!("[data-{}=\"{}\"]", ident(axis), value.as_str().unwrap_or_default()))
            .collect();
        let _ = writeln!(css, "{selector} {{");
        for (name, token) in &tokens {
            // The value it has there, over what the Theme doesn't name.
            let mut shown = default.clone();
            shown.extend(theme.iter().map(|(axis, value)| (axis.clone(), value.clone())));
            let value: &Value = token.value_in(&shown);
            let _ = writeln!(css, "  --{}: {};", ident(name), super::token_value(value));
        }
        css.push_str("}\n");
    }
    css
}

/// `name` as a CSS identifier: what one can't hold as `-`.
pub fn ident(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}
