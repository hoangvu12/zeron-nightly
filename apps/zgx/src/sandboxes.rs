//! Places a chat runs on (spec #37, docs/design/cloud-byo.md): the sandbox
//! providers this device's engine keeps, their accounts' templates, and how
//! far a chat's sandbox got while it starts (the chat's progress line).
//!
//! ```text
//! shared  sandboxes            { providers = { { id, name, icon }… } }
//! shared  sandbox_steps        { [chat] = { state, step?, template?, since, error?,
//!                              provider?, disk_full } }: `state` starting |
//!                              ready | failed | gone | sleeping | asleep |
//!                              waking, `step` the one it's on (or that
//!                              failed), `since` when that began, in Unix
//!                              seconds
//! call    sandboxes.templates  provider → { { id, name }… } (Blank isn't one)
//! invoke  sandboxes.follow     chat: keep its steps in `sandbox_steps`; first
//!                              asks the provider whether it's still there,
//!                              never waking it (again once [`ASK_AGAIN`] has
//!                              passed)
//! invoke  sandboxes.unfollow   chat
//! call    sandboxes.run_again  chat: a failed step, and the ones after it
//! call    sandboxes.fresh      chat: Start fresh, a new sandbox with what the
//!                              chat pushed
//! call    sandboxes.wake       chat: Wake a sandbox asleep (#43)
//! ```
//!
//! Every call is the engine's (`ListSandboxProviders`, `ListSandboxTemplates`,
//! `CheckSandbox`, `WatchSandboxSteps`, `RunSandboxStepsAgain`,
//! `StartSandboxFresh`, `WakeSandbox`); a failure rejects with an outcome
//! word, or the engine's sentence.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

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
    /// When each chat's provider was last asked whether it's still there.
    asked: BTreeMap<String, Instant>,
}

/// How long an answer from the provider stands before opening the chat asks
/// again (a reload within it doesn't).
const ASK_AGAIN: Duration = Duration::from_secs(60);

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
    if let Some(provider) = steps["provider"].as_str().filter(|p| !p.is_empty()) {
        out["provider"] = Json::from(provider);
    }
    out["disk_full"] = Json::Bool(steps["diskFull"].as_bool() == Some(true));
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

/// Ask the provider whether `chat`'s sandbox is still there (deleted outside
/// Zeron, its disk full), never waking it; what it says is the chat's steps
/// until the watch says more.
fn check(chat: String) {
    STATE.with(|s| s.borrow_mut().asked.insert(chat.clone(), Instant::now()));
    engine::get().spawn(async move {
        let Ok(checked) = call("CheckSandbox", json!({ "chatId": chat })).await else {
            return;
        };
        if checked.is_object() && STATE.with(|s| s.borrow().steps.contains_key(&chat)) {
            STATE.with(|s| s.borrow_mut().steps.insert(chat, checked));
            publish();
        }
    });
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
        if chat.is_empty() {
            return;
        }
        let stale = STATE.with(|s| s.borrow().asked.get(&chat).is_none_or(|at| at.elapsed() >= ASK_AGAIN));
        STATE.with(|s| {
            s.borrow_mut().steps.entry(chat.clone()).or_insert(Json::Null);
        });
        if stale {
            check(chat.clone());
        }
        if STATE.with(|s| s.borrow().watches.contains_key(&chat)) {
            return;
        }
        if let Some(rpc) = engine::get().current() {
            let watch = follow(&rpc, &chat);
            STATE.with(|s| s.borrow_mut().watches.insert(chat, watch));
        }
    });
    host.async_action("sandboxes.fresh", |chat| {
        let chat = text(&chat);
        async move {
            crate::plugins::preview::refuse("sandboxes.fresh")?;
            if chat.is_empty() {
                return Err("invalid".into());
            }
            call("StartSandboxFresh", json!({ "chatId": chat })).await?;
            Ok(Value::Bool(true))
        }
    });
    host.async_action("sandboxes.wake", |chat| {
        let chat = text(&chat);
        async move {
            crate::plugins::preview::refuse("sandboxes.wake")?;
            if chat.is_empty() {
                return Err("invalid".into());
            }
            call("WakeSandbox", json!({ "chatId": chat })).await?;
            Ok(Value::Bool(true))
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
            Some(json!({ "state": "starting", "step": "installZeron", "template": "zeron-dev", "since": 1_700_000_012.0,
                         "provider": "docker", "disk_full": false }))
        );
        let failed = json!({ "state": "failed", "step": "getCode", "since": 1000, "error": "Couldn't clone" });
        assert_eq!(
            super::steps_json(&failed),
            Some(json!({ "state": "failed", "step": "getCode", "since": 1.0, "error": "Couldn't clone", "disk_full": false }))
        );
        assert_eq!(super::steps_json(&json!(null)), None);
    }

    /// What its alerts read (#44): deleted outside Zeron, a full disk; and
    /// asleep, or a wake that failed (#43).
    #[test]
    fn gone_a_full_disk_asleep_and_a_failed_wake_read_as_the_deck_takes_them() {
        let gone = json!({ "provider": "docker", "state": "gone", "since": 0,
                           "error": "The sandbox was deleted outside Zeron.", "diskFull": false });
        let read = super::steps_json(&gone).unwrap();
        assert_eq!(read["state"], "gone");
        assert_eq!(read["provider"], "docker");
        let full = json!({ "provider": "fake", "state": "failed", "step": "getCode", "since": 0, "diskFull": true });
        assert_eq!(super::steps_json(&full).unwrap()["disk_full"], true);
        let asleep = json!({ "provider": "docker", "state": "asleep", "step": null, "since": 0 });
        let read = super::steps_json(&asleep).unwrap();
        assert_eq!(read["state"], "asleep");
        assert!(read.get("step").is_none());
        let woke = json!({ "provider": "docker", "state": "failed", "step": "wake", "since": 0, "error": "It didn't start." });
        assert_eq!(super::steps_json(&woke).unwrap()["step"], "wake");
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
