//! Save as template (spec #37): a chat's sandbox saved through its provider,
//! by the engine that keeps it (`SaveSandboxTemplate`), and the provider's
//! templates (`ListSandboxTemplates`). The engine wipes Zeron's identity
//! from the sandbox and saves it; this only carries the asks and answers.
//! (Named apart from the interface's own templates, `templates.*`.)
//!
//! What scripts see, `sandbox_templates`: chat id → `{ provider }`, for each
//! chat asked about that runs on a sandbox this device keeps.
//!
//!   sandbox_templates.check({ chat, … })   ask which of these chats run on
//!                                           a sandbox kept here (each once)
//!   sandbox_templates.save({ chat, name }) → { id, name }, or rejects with
//!                                           the engine's words
//!   sandbox_templates.list(provider)       → { { id, name } }

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use serde_json::{Value as Json, json};
use zgx_script::{ScriptHost, Shared, Value};

use crate::auth::text;
use crate::engine;

#[derive(Default)]
struct Kept {
    shared: RefCell<Vec<Shared>>,
    /// Chats asked about, and those on a sandbox kept here (→ provider).
    asked: RefCell<BTreeSet<String>>,
    on_sandbox: RefCell<BTreeMap<String, String>>,
}

thread_local! {
    static KEPT: Rc<Kept> = Rc::default();
}

pub fn install(host: &ScriptHost) {
    let kept = KEPT.with(Rc::clone);
    kept.shared.borrow_mut().push(host.shared("sandbox_templates", kept.value()));
    let k = kept.clone();
    host.action("sandbox_templates.check", move |chats| {
        let fresh: Vec<String> = chat_ids(&chats)
            .into_iter()
            .filter(|chat| k.asked.borrow_mut().insert(chat.clone()))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let k = k.clone();
        engine::get().spawn(async move {
            let mut changed = false;
            for chat in fresh {
                match engine::get().call("SandboxSteps", json!({ "chatId": chat })).await {
                    Ok(steps) => {
                        if let Some(provider) = on_sandbox(&steps) {
                            k.on_sandbox.borrow_mut().insert(chat, provider);
                            changed = true;
                        }
                    }
                    // Asked again next time (an engine not up yet).
                    Err(_) => {
                        k.asked.borrow_mut().remove(&chat);
                    }
                }
            }
            if changed {
                k.publish();
            }
        });
    });
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
    host.async_action("sandbox_templates.list", move |provider| {
        let provider = text(&provider);
        async move {
            let listed = engine::get()
                .call("ListSandboxTemplates", json!({ "provider": provider }))
                .await
                .map_err(|e| said(&e))?;
            let rows: Vec<Value> = listed.as_array().into_iter().flatten().filter_map(template_value).collect();
            Ok(Value::List(rows.into()))
        }
    });
}

impl Kept {
    fn value(&self) -> Value {
        Value::record(
            self.on_sandbox
                .borrow()
                .iter()
                .map(|(chat, provider)| (chat.clone(), Value::record([("provider", Value::from(provider.as_str()))]))),
        )
    }

    fn publish(&self) {
        let value = self.value();
        for shared in self.shared.borrow().iter() {
            shared.set(value.clone());
        }
    }
}

/// The chat ids a script passed: a list of them, or one.
fn chat_ids(value: &Value) -> Vec<String> {
    match value {
        Value::List(items) => items.iter().map(text).filter(|c| !c.is_empty()).collect(),
        one => Some(text(one)).filter(|c| !c.is_empty()).into_iter().collect(),
    }
}

/// `SandboxSteps`' answer: the provider, for a chat on a sandbox kept here
/// that's still there (`null`: not one).
fn on_sandbox(steps: &Json) -> Option<String> {
    if steps["state"].as_str() == Some("gone") {
        return None;
    }
    steps["provider"].as_str().filter(|p| !p.is_empty()).map(str::to_owned)
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
    fn a_chat_on_a_sandbox_kept_here_names_its_provider() {
        assert_eq!(on_sandbox(&json!({ "provider": "docker", "state": "ready" })).as_deref(), Some("docker"));
        assert_eq!(on_sandbox(&json!({ "provider": "docker", "state": "failed" })).as_deref(), Some("docker"));
        assert_eq!(on_sandbox(&json!({ "provider": "docker", "state": "gone" })), None);
        assert_eq!(on_sandbox(&Json::Null), None);
    }

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
