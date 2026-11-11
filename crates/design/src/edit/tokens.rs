//! `SetTokens` and `GetTokens`: the Design's Tokens, and the Themes their
//! values are for.

use serde_json::{Map, Value, json};

use super::{Draft, EditError};
use crate::model::{Token, token};

impl Draft {
    /// Sets the Design's Tokens: `tokens` (name to Token, or `null` to
    /// remove one) merged into them, or in place of them all with
    /// `replace` (Themes and all). Each axis and value a themed value is
    /// for joins the Design's Themes. All or none: a Token that isn't one
    /// fails with why.
    pub fn set_tokens(&mut self, tokens: Value, replace: bool) -> Result<(), EditError> {
        let invalid = |e: String| EditError::Invalid(format!("SetTokens: {e}"));
        let Value::Object(tokens) = tokens else {
            return Err(invalid(format!(
                r##"it takes Tokens by name, like {{ primary: {{ type: "color", value: "#4F46E5" }} }}, not {tokens}"##
            )));
        };
        let mut read = Vec::new();
        for (name, written) in &tokens {
            token::check_name(name).map_err(invalid)?;
            if !written.is_null() {
                read.push(Token::read(name, written).map_err(invalid)?);
            }
        }
        if replace {
            // The Themes are the replaced Tokens' no longer.
            self.design.tokens.clear();
            self.design.themes.clear();
        }
        for (name, written) in tokens {
            if written.is_null() {
                self.design.tokens.remove(&name);
            } else {
                self.design.tokens.insert(name, written);
            }
        }
        for (axis, value) in read.iter().flat_map(|token| &token.values).flat_map(|(_, theme)| theme) {
            let values = self.design.themes.entry(axis.clone()).or_insert_with(|| json!([]));
            match values {
                Value::Array(values) if !values.contains(value) => values.push(value.clone()),
                Value::Array(_) => {}
                other => *other = json!([value]),
            }
        }
        Ok(())
    }

    /// The Design's Tokens as CSS custom properties, per Theme
    /// ([`crate::export::tokens`]).
    pub fn tokens_css(&self) -> String {
        crate::export::tokens::css(&self.design)
    }

    /// The Design's Tokens by name, and its Themes: each axis's values,
    /// the first the one nodes are shown in unless they set their own.
    pub fn tokens(&self) -> Value {
        let themes: &Map<String, Value> = &self.design.themes;
        json!({ "tokens": self.design.tokens, "themes": themes })
    }
}
