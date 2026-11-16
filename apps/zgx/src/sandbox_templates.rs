//! Save as template (spec #37): a chat's sandbox saved through its provider
//! (`SaveSandboxTemplate`), by the host device that keeps it; the engine
//! routes the call there. The engine wipes Zeron's identity from the sandbox
//! and saves it; this only carries the ask and the answer. (Named apart from
//! the interface's own templates, `templates.*`. Listing a provider's
//! templates is src/sandboxes.rs's `sandboxes.templates`.)
//!
//!   sandbox_templates.save({ chat, name }) → { id, name }, or rejects with
//!                                           the engine's words (or an
//!                                           outcome word: offline, outdated)

use serde_json::{Value as Json, json};
use zgx_script::{ScriptHost, Value};

use crate::auth::text;
use crate::engine;

pub fn install(host: &ScriptHost) {
    host.async_action("sandbox_templates.save", move |arg| {
        let chat = arg.field("chat").map(text).unwrap_or_default();
        let name = arg.field("name").map(text).unwrap_or_default();
        async move {
            crate::plugins::preview::refuse("sandbox_templates.save")?;
            if chat.is_empty() {
                return Err("invalid".to_string());
            }
            let saved = engine::get()
                .call("SaveSandboxTemplate", json!({ "chatId": chat, "name": name }))
                .await
                .map_err(|e| said(&e))?;
            template_value(&saved["template"]).ok_or_else(|| "failed".to_string())
        }
    });
}

fn template_value(t: &Json) -> Option<Value> {
    let id = t["id"].as_str()?;
    let name = t["name"].as_str().unwrap_or(id);
    Some(Value::record([("id", Value::from(id)), ("name", Value::from(name))]))
}

/// What a failed call says: the engine's own words (they're written to be
/// read), or an outcome word when there are none.
fn said(error: &str) -> String {
    if error.to_ascii_lowercase().contains("unknown method") {
        return "outdated".into();
    }
    match engine::outcome(error) {
        "failed" if !error.trim().is_empty() => error.trim().to_string(),
        word => word.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_reads_as_the_scripts_take_it() {
        let t = template_value(&json!({ "id": "tmpl_1", "name": "reportly-dev" })).unwrap();
        assert_eq!(t.field("id").map(text).as_deref(), Some("tmpl_1"));
        assert_eq!(t.field("name").map(text).as_deref(), Some("reportly-dev"));
        let unnamed = template_value(&json!({ "id": "tmpl_2" })).unwrap();
        assert_eq!(unnamed.field("name").map(text).as_deref(), Some("tmpl_2"));
        assert!(template_value(&json!({ "name": "x" })).is_none());
    }

    #[test]
    fn the_engines_words_are_said_as_they_are() {
        let busy = "The chat is still working. Save it as a template once it's done.";
        assert_eq!(said(busy), busy);
        assert_eq!(said("offline"), "offline");
        assert_eq!(said("unknown method: SaveSandboxTemplate"), "outdated");
    }
}
