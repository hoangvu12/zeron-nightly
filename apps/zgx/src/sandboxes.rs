//! Chats on sandboxes (spec #37), as their composer shows them: how far each
//! followed chat's sandbox got, from the engine that keeps its provider
//! (`CheckSandbox`, then `WatchSandboxSteps`), and Start fresh. The engine
//! runs every step; this only carries its answers.
//!
//! What scripts see, `sandboxes`: chat id → its sandbox, for each chat
//! followed whose sandbox this device keeps:
//!   { state = "starting" | "ready" | "failed" | "gone" | "sleeping"
//!     | "asleep" | "waking", provider, provider_name?, step?, error?,
//!     disk_full }
//!
//!   sandboxes.follow(chat)   follow a chat's sandbox (an action; opening a
//!                            chat). Asks the provider whether it's still
//!                            there, never waking it, then hears each change.
//!   sandboxes.fresh(chat)    Start fresh: a new sandbox, with the chat's
//!                            pushed work. Rejects with an outcome word.
//!   sandboxes.wake(chat)     Wake a sandbox asleep (`WakeSandbox`): it
//!                            starts again, its engine with it.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap},
    rc::Rc,
    time::{Duration, Instant},
};

use serde_json::{Value as Json, json};
use zgx_desktop::WindowContext;
use zgx_script::{ScriptHost, Shared, Value};

use crate::auth::text;
use crate::engine::{self, rpc::Watch};

struct Sandboxes {
    shared: RefCell<Vec<Shared>>,
    /// The chats followed, and what's known of each one's sandbox.
    followed: RefCell<BTreeSet<String>>,
    steps: RefCell<BTreeMap<String, Json>>,
    watches: RefCell<HashMap<String, Watch>>,
    /// Provider id → its name, as the engine lists them.
    names: RefCell<HashMap<String, String>>,
    /// When each chat was last asked about: opened again after
    /// [`ASK_AGAIN`], it's asked again (a reload within it isn't).
    asked: RefCell<HashMap<String, Instant>>,
    listening: Cell<bool>,
}

/// How long an answer stands before opening the chat asks again.
const ASK_AGAIN: Duration = Duration::from_secs(60);

thread_local! {
    static SANDBOXES: Rc<Sandboxes> = Rc::new(Sandboxes {
        shared: RefCell::default(),
        followed: RefCell::default(),
        steps: RefCell::default(),
        watches: RefCell::default(),
        names: RefCell::default(),
        asked: RefCell::default(),
        listening: Cell::new(false),
    });
}

pub fn install(_cx: &mut WindowContext, host: &ScriptHost) {
    let sandboxes = SANDBOXES.with(Rc::clone);
    sandboxes.shared.borrow_mut().push(host.shared("sandboxes", sandboxes.value()));
    if !sandboxes.listening.replace(true) {
        // A new connection: every followed chat asked again (its watches
        // ended with the old one).
        let s = sandboxes.clone();
        engine::get().on_connect(move |_| {
            let chats: Vec<String> = s.followed.borrow().iter().cloned().collect();
            for chat in chats {
                s.clone().ask(chat);
            }
        });
    }
    let s = sandboxes.clone();
    host.action("sandboxes.follow", move |chat| {
        let chat = text(&chat);
        if chat.is_empty() {
            return;
        }
        let new = s.followed.borrow_mut().insert(chat.clone());
        let stale = s.asked.borrow().get(&chat).is_none_or(|at| at.elapsed() >= ASK_AGAIN);
        if new || stale {
            s.clone().ask(chat);
        }
    });
    host.async_action("sandboxes.fresh", move |chat| {
        let chat = text(&chat);
        async move {
            crate::plugins::preview::refuse("sandboxes.fresh")?;
            if chat.is_empty() {
                return Err("invalid".to_string());
            }
            call("StartSandboxFresh", json!({ "chatId": chat })).await?;
            Ok(Value::Bool(true))
        }
    });
    host.async_action("sandboxes.wake", move |chat| {
        let chat = text(&chat);
        async move {
            crate::plugins::preview::refuse("sandboxes.wake")?;
            if chat.is_empty() {
                return Err("invalid".to_string());
            }
            call("WakeSandbox", json!({ "chatId": chat })).await?;
            Ok(Value::Bool(true))
        }
    });
}

impl Sandboxes {
    /// Ask whether `chat`'s sandbox is still there, then hear its changes.
    fn ask(self: Rc<Self>, chat: String) {
        self.asked.borrow_mut().insert(chat.clone(), Instant::now());
        engine::get().spawn(async move {
            let Ok(checked) = call("CheckSandbox", json!({ "chatId": chat })).await else {
                return;
            };
            if !checked.is_object() {
                // Not on a sandbox this device keeps.
                self.watches.borrow_mut().remove(&chat);
                if self.steps.borrow_mut().remove(&chat).is_some() {
                    self.publish();
                }
                return;
            }
            self.learn_names(&checked).await;
            self.set(&chat, checked);
            let Some(rpc) = engine::get().connection(Duration::ZERO).await else {
                return;
            };
            let weak = Rc::downgrade(&self);
            let id = chat.clone();
            let watch = rpc.watch("WatchSandboxSteps", json!({ "chatId": chat }), move |item| {
                if let Some(me) = weak.upgrade()
                    && item.is_object()
                {
                    me.set(&id, item);
                }
            });
            self.watches.borrow_mut().insert(chat, watch);
        });
    }

    /// The provider's name, asked once.
    async fn learn_names(&self, steps: &Json) {
        let Some(provider) = steps["provider"].as_str() else {
            return;
        };
        if self.names.borrow().contains_key(provider) {
            return;
        }
        if let Ok(listed) = call("ListSandboxProviders", json!({})).await {
            let mut names = self.names.borrow_mut();
            for p in listed.as_array().into_iter().flatten() {
                if let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) {
                    names.insert(id.to_string(), name.to_string());
                }
            }
        }
    }

    fn set(&self, chat: &str, steps: Json) {
        let changed = self.steps.borrow().get(chat) != Some(&steps);
        if changed {
            self.steps.borrow_mut().insert(chat.to_string(), steps);
            self.publish();
        }
    }

    fn publish(&self) {
        let value = self.value();
        for shared in self.shared.borrow().iter() {
            shared.set(value.clone());
        }
    }

    fn value(&self) -> Value {
        let names = self.names.borrow();
        Value::record(
            self.steps
                .borrow()
                .iter()
                .map(|(chat, s)| (chat.clone(), steps_value(s, &names))),
        )
    }
}

/// A sandbox's steps as the scripts read them.
fn steps_value(s: &Json, names: &HashMap<String, String>) -> Value {
    let provider = s["provider"].as_str().unwrap_or_default();
    let opt = |v: &Json| v.as_str().filter(|t| !t.is_empty()).map_or(Value::Nil, Value::from);
    Value::record([
        ("state", Value::from(s["state"].as_str().unwrap_or("starting"))),
        ("provider", Value::from(provider)),
        ("provider_name", names.get(provider).map_or(Value::Nil, |n| Value::from(n.as_str()))),
        ("step", opt(&s["step"])),
        ("error", opt(&s["error"])),
        ("disk_full", Value::Bool(s["diskFull"].as_bool() == Some(true))),
    ])
}

async fn call(method: &str, params: Json) -> Result<Json, String> {
    engine::get().call(method, params).await.map_err(|error| {
        if error.to_ascii_lowercase().contains("unknown method") {
            "outdated".to_string()
        } else {
            eprintln!("zeron-zgx: sandboxes: {method} failed: {error}");
            engine::outcome(&error).to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_read_as_the_scripts_take_them() {
        let names = HashMap::from([("docker".to_string(), "Docker Sandbox".to_string())]);
        let gone = steps_value(
            &json!({ "chatId": "c", "provider": "docker", "deviceId": "d", "state": "gone",
                     "step": null, "since": 1, "error": "The sandbox was deleted outside Zeron.",
                     "diskFull": false, "steps": [] }),
            &names,
        );
        let Value::Map(_) = &gone else { panic!("a record") };
        assert_eq!(gone.field("state").map(text).as_deref(), Some("gone"));
        assert_eq!(gone.field("provider_name").map(text).as_deref(), Some("Docker Sandbox"));
        assert!(matches!(gone.field("disk_full"), Some(Value::Bool(false))));
        let full = steps_value(
            &json!({ "provider": "fake", "state": "failed", "step": "getCode",
                     "error": "The sandbox's disk is full.", "diskFull": true }),
            &names,
        );
        assert!(matches!(full.field("disk_full"), Some(Value::Bool(true))));
        assert_eq!(full.field("step").map(text).as_deref(), Some("getCode"));
        assert!(matches!(full.field("provider_name"), None | Some(Value::Nil)));
        // Asleep, and a wake that failed, as the engine says them (#43).
        let asleep = steps_value(&json!({ "provider": "docker", "state": "asleep", "step": null }), &names);
        assert_eq!(asleep.field("state").map(text).as_deref(), Some("asleep"));
        assert!(matches!(asleep.field("step"), None | Some(Value::Nil)));
        let woke = steps_value(
            &json!({ "provider": "docker", "state": "failed", "step": "wake", "error": "It didn't start." }),
            &names,
        );
        assert_eq!(woke.field("step").map(text).as_deref(), Some("wake"));
    }
}
