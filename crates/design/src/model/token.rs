//! Tokens and Themes: named values (a colour, number or string) that any
//! property refers to as `$name` instead of a literal, each with a value
//! per Theme where it varies.
//!
//! A Token is written `{ "type": "color", "value": "#4F46E5" }`, or with
//! themed values, `{ "type": "color", "value": [{ "value": "#FFFFFF",
//! "theme": { "mode": "light" } }, …] }`. The Design's Themes are its axes
//! and each axis's values (`{ "mode": ["light", "dark"] }`), the first
//! value of each being the one a node is shown in unless it, or a node
//! above it, sets its own (`theme: { "mode": "dark" }`).

use serde_json::{Map, Value};

use super::Color;

/// A Theme as a node is shown in: a value for each axis.
pub type Theme = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Color,
    Number,
    String,
}

impl TokenKind {
    const NAMES: &[(&str, TokenKind)] =
        &[("color", TokenKind::Color), ("number", TokenKind::Number), ("string", TokenKind::String)];

    /// Whether `value` is one of this kind's values.
    fn check(self, value: &Value) -> Result<(), String> {
        match (self, value) {
            (TokenKind::Color, Value::String(color)) => Color::parse(color).map(drop),
            (TokenKind::Number, Value::Number(_)) | (TokenKind::String, Value::String(_)) => Ok(()),
            (kind, other) => Err(format!("a {} Token's value can't be {other}", kind.name())),
        }
    }

    fn name(self) -> &'static str {
        TokenKind::NAMES.iter().find(|(_, kind)| *kind == self).map(|(name, _)| *name).unwrap_or_default()
    }
}

/// A Token, read from what the Design holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// Its values, each with the Theme it's for (none for a value that
    /// doesn't vary).
    pub values: Vec<(Value, Theme)>,
}

impl Token {
    /// The Token `name` written as `token`, or why it can't be one.
    pub fn read(name: &str, token: &Value) -> Result<Self, String> {
        let example = r##"{ type: "color", value: "#4F46E5" }"##;
        let Value::Object(token) = token else {
            return Err(format!(
                "Token \"{name}\" is {token}, but a Token has a type and a value, like {example}"
            ));
        };
        let kind = match token.get("type").and_then(Value::as_str) {
            Some(kind) => TokenKind::NAMES.iter().find(|(name, _)| *name == kind).map(|(_, kind)| *kind),
            None => None,
        };
        let Some(kind) = kind else {
            return Err(format!(
                "Token \"{name}\" needs a type, color, number or string, like {example}"
            ));
        };
        let error = |e: String| format!("Token \"{name}\": {e}");
        let values = match token.get("value") {
            None | Some(Value::Null) => return Err(error("it has no value".into())),
            Some(Value::Array(themed)) => themed
                .iter()
                .map(|themed| {
                    let value = themed.get("value").cloned().unwrap_or_default();
                    let theme = match themed.get("theme") {
                        Some(Value::Object(theme)) if theme.values().all(Value::is_string) => theme.clone(),
                        _ => {
                            return Err(error(format!(
                                "each of its themed values is {{ value, theme: {{ axis: \"value\" }} }}, not {themed}"
                            )));
                        }
                    };
                    kind.check(&value).map_err(error)?;
                    Ok((value, theme))
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(value) => {
                kind.check(value).map_err(error)?;
                vec![(value.clone(), Theme::new())]
            }
        };
        if values.is_empty() {
            return Err(error("it has no value".into()));
        }
        Ok(Self { kind, values })
    }

    /// Its value in `theme`: the one for a Theme that `theme` matches on
    /// every axis it names, naming the most (the later on a tie); else its
    /// first.
    pub fn value_in(&self, theme: &Theme) -> &Value {
        let matching = self
            .values
            .iter()
            .filter(|(_, wanted)| wanted.iter().all(|(axis, value)| theme.get(axis) == Some(value)));
        let mut best: Option<&(Value, Theme)> = None;
        for candidate in matching {
            if best.is_none_or(|best| candidate.1.len() >= best.1.len()) {
                best = Some(candidate);
            }
        }
        &best.unwrap_or(&self.values[0]).0
    }
}

/// Whether `name` can name a Token.
pub fn check_name(name: &str) -> Result<(), String> {
    if let Some(bare) = name.strip_prefix('$') {
        return Err(format!(
            "Token names don't start with \"$\": name it \"{bare}\", and refer to it as \"${bare}\""
        ));
    }
    if name.is_empty() || !name.chars().all(name_char) {
        return Err(format!(
            "\"{name}\" can't name a Token: use letters, digits, \"-\", \"_\", \".\" and \"/\""
        ));
    }
    Ok(())
}

fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')
}

/// The Token `text` refers to, when it's `$name`.
pub fn reference(text: &str) -> Option<&str> {
    let name = text.strip_prefix('$')?;
    (!name.is_empty() && name.chars().all(name_char)).then_some(name)
}

/// Whether `value` refers to a Token anywhere in it.
pub fn refers(value: &Value) -> bool {
    match value {
        Value::String(text) => reference(text).is_some(),
        Value::Array(items) => items.iter().any(refers),
        Value::Object(props) => props.values().any(refers),
        _ => false,
    }
}

/// The Theme a Design shows its nodes in: each axis's first value.
pub fn default_theme(themes: &Map<String, Value>) -> Theme {
    themes
        .iter()
        .filter_map(|(axis, values)| Some((axis.clone(), values.as_array()?.first()?.clone())))
        .collect()
}
