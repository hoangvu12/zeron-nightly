//! Places a chat runs on (spec #37, docs/design/cloud-byo.md): the sandbox
//! providers this device's engine keeps, their accounts' templates, and how
//! far a chat's sandbox got while it starts (the chat's progress line).
//!
//! ```text
//! shared  sandboxes            { providers = { { id, name, icon }… } }
//! shared  sandbox_steps        { [chat] = { state, step?, template?, since, error? } }:
//!                              `state` starting | ready | failed, `step` the
//!                              one it's on (or that failed), `since` when
//!                              that began, in Unix seconds
//! call    sandboxes.templates  provider → { { id, name }… } (Blank isn't one)
//! invoke  sandboxes.follow     chat: keep its steps in `sandbox_steps`
//! invoke  sandboxes.unfollow   chat
//! call    sandboxes.run_again  chat: a failed step, and the ones after it
//! ```
//!
//! Every call is the engine's (`ListSandboxProviders`, `ListSandboxTemplates`,
//! `WatchSandboxSteps`, `RunSandboxStepsAgain`); a failure rejects with an
//! outcome word, or the engine's sentence.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use zgx_desktop::WindowContext;
use zgx_script::{ScriptHost, Shared, Value};

use crate::auth::text;
use crate::engine;
use crate::plugins::json_value;

#[derive(Default)]
struct State {
    /// Each window's `sandboxes` and `sandbox_steps`.
    shared: Vec<(Rc<Shared>, Rc<Shared>)>,
    providers: Json,
    /// The chats followed, and their steps as last heard.
    steps: BTreeMap<String, Json>,
    watches: BTreeMap<String, engine::rpc::Watch>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { providers: json!([]), ..State::default() });
}

fn publish() {
    let (shared, providers, steps) = STATE.with(|s| {
        let s = s.borrow();
        let steps: serde_json::Map<String, Json> =
            s.steps.iter().filter_map(|(chat, raw)| Some((chat.clone(), steps_json(raw)?))).collect();
        (s.shared.clone(), providers_json(&s.providers), Json::Object(steps))
    });
    let (providers, steps) = (json_value(&json!({ "providers": providers })), json_value(&steps));
    for (p, st) in shared {
        p.set(providers.clone());
        st.set(steps.clone());
    }
}

/// The providers as the picker lists them.
fn providers_json(listed: &Json) -> Json {
    let providers: Vec<Json> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = p["id"].as_str().filter(|id| !id.is_empty())?;
            Some(json!({
                "id": id,
                "name": p["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id),
                "icon": p["icon"].as_str().unwrap_or_default(),
            }))
        })
        .collect();
    Json::Array(providers)
}

/// A chat's steps as its progress line reads them (`SandboxSteps`), or none
/// for a chat not on a place this engine keeps.
fn steps_json(steps: &Json) -> Option<Json> {
    let state = steps["state"].as_str()?;
    let mut out = json!({
        "state": state,
        "since": steps["since"].as_i64().unwrap_or_default() as f64 / 1000.0,
    });
    if let Some(step) = steps["step"].as_str() {
        out["step"] = Json::from(step);
    }
    if let Some(template) = steps["template"].as_str().filter(|t| !t.is_empty()) {
        out["template"] = Json::from(template);
    }
    if let Some(error) = steps["error"].as_str().filter(|e| !e.is_empty()) {
        out["error"] = Json::from(error);
    }
    Some(out)
}

fn templates_json(listed: &Json) -> Json {
    let templates: Vec<Json> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let id = t["id"].as_str().filter(|id| !id.is_empty())?;
            Some(json!({ "id": id, "name": t["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id) }))
        })
        .collect();
    Json::Array(templates)
}

fn follow(rpc: &engine::rpc::Rpc, chat: &str) -> engine::rpc::Watch {
    let heard = chat.to_owned();
    rpc.watch("WatchSandboxSteps", json!({ "chatId": chat }), move |item| {
        STATE.with(|s| s.borrow_mut().steps.insert(heard.clone(), item));
        publish();
    })
}

async fn call(method: &str, params: Json) -> Result<Json, String> {
    engine::get().call(method, params).await.map_err(|error| match engine::outcome(&error) {
        "failed" => {
            let words = error.trim().trim_start_matches("failed:").trim();
            if words.is_empty() { "failed".into() } else { words.to_string() }
        }
        word => word.to_string(),
    })
}

pub fn install(cx: &mut WindowContext, host: &ScriptHost) {
    let providers = Rc::new(host.shared("sandboxes", json_value(&json!({ "providers": [] }))));
    let steps = Rc::new(host.shared("sandbox_steps", json_value(&json!({}))));
    let first = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.shared.push((providers.clone(), steps.clone()));
        s.shared.len() == 1
    });
    let mine = Rc::downgrade(&providers);
    cx.on_closed(move || {
        STATE.with(|s| s.borrow_mut().shared.retain(|(p, _)| !std::ptr::eq(Rc::as_ptr(p), mine.as_ptr())));
    });
    publish();
    if first {
        // Each connection: the providers it keeps, once, and the chats
        // followed watched again on it.
        engine::get().on_connect(|rpc| {
            let chats: Vec<String> = STATE.with(|s| s.borrow().steps.keys().cloned().collect());
            let watches: BTreeMap<String, engine::rpc::Watch> =
                chats.iter().map(|chat| (chat.clone(), follow(rpc, chat))).collect();
            STATE.with(|s| s.borrow_mut().watches = watches);
            let rpc = rpc.clone();
            engine::get().spawn(async move {
                match rpc.call("ListSandboxProviders", json!({})).await {
                    Ok(listed) => {
                        STATE.with(|s| s.borrow_mut().providers = listed);
                        publish();
                    }
                    // (An engine from before places: none to offer.)
                    Err(error) => eprintln!("zeron-zgx: sandboxes: no providers: {error}"),
                }
            });
        });
    }
    host.async_action("sandboxes.templates", |provider| {
        let provider = text(&provider);
        async move {
            if provider.is_empty() {
                return Err("invalid".into());
            }
            let listed = call("ListSandboxTemplates", json!({ "provider": provider })).await?;
            Ok(json_value(&templates_json(&listed)))
        }
    });
    host.action("sandboxes.follow", |chat| {
        let chat = text(&chat);
        if chat.is_empty() || STATE.with(|s| s.borrow().watches.contains_key(&chat)) {
            return;
        }
        STATE.with(|s| {
            s.borrow_mut().steps.entry(chat.clone()).or_insert(Json::Null);
        });
        if let Some(rpc) = engine::get().current() {
            let watch = follow(&rpc, &chat);
            STATE.with(|s| s.borrow_mut().watches.insert(chat, watch));
        }
    });
    host.action("sandboxes.unfollow", |chat| {
        let chat = text(&chat);
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.watches.remove(&chat);
            s.steps.remove(&chat);
        });
        publish();
    });
    host.async_action("sandboxes.run_again", |chat| {
        let chat = text(&chat);
        async move {
            crate::plugins::preview::refuse("sandboxes.run_again")?;
            call("RunSandboxStepsAgain", json!({ "chatId": chat })).await?;
            Ok(Value::Nil)
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// The progress line reads the step it's on and since when (seconds, as
    /// the scripts' times are), and a failed step's error; a chat not on a
    /// place this engine keeps has none.
    #[test]
    fn a_sandbox_s_steps_read_as_its_progress_line() {
        let starting = json!({ "chatId": "c", "provider": "docker", "template": "zeron-dev", "deviceId": "d",
                               "state": "starting", "step": "installZeron", "since": 1_700_000_012_000_i64,
                               "error": null, "steps": [] });
        assert_eq!(
            super::steps_json(&starting),
            Some(json!({ "state": "starting", "step": "installZeron", "template": "zeron-dev", "since": 1_700_000_012.0 }))
        );
        let failed = json!({ "state": "failed", "step": "getCode", "since": 1000, "error": "Couldn't clone" });
        assert_eq!(
            super::steps_json(&failed),
            Some(json!({ "state": "failed", "step": "getCode", "since": 1.0, "error": "Couldn't clone" }))
        );
        assert_eq!(super::steps_json(&json!(null)), None);
    }

    /// One entry per provider, by its own name and icon; a provider with no
    /// id isn't offered.
    #[test]
    fn providers_are_offered_by_their_own_name_and_icon() {
        let listed = json!([
            { "id": "docker", "name": "Docker Sandbox", "icon": "docker", "ops": ["create"] },
            { "id": "boat", "icon": "boat" },
            { "name": "nameless" },
        ]);
        assert_eq!(
            super::providers_json(&listed),
            json!([
                { "id": "docker", "name": "Docker Sandbox", "icon": "docker" },
                { "id": "boat", "name": "boat", "icon": "boat" },
            ])
        );
        assert_eq!(
            super::templates_json(&json!([{ "id": "zeron-dev", "name": "zeron-dev" }, { "id": "" }])),
            json!([{ "id": "zeron-dev", "name": "zeron-dev" }])
        );
    }
}
