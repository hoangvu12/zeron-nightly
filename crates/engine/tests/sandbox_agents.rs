//! The agents a composer offers for a chat on a place (spec #37, "Composer
//! agents"; #46), at the engine's RPCs: the device keeping the provider
//! answers `ListHarnesses` / `ListModels` for a place itself, from what the
//! template had the last time it ran, else from its own list. Asking never
//! creates or wakes a sandbox. An agent the sandbox doesn't have is refused
//! there with "<Agent> isn't on this template", the message kept to send
//! again, and never swapped for another.
//!
//! The sandbox is the fake provider's second real engine; the edge is
//! scripted.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::Path;
use std::time::{Duration, Instant};

use fake_provider::{FakeProvider, engine_config, start_engine};
use scripted_edge::{ScriptedEdge, USER};
use serde_json::{Value, json};
use zeron_doc::{MessageRole, MessageStatus, SessionCommandPayload};
use zeron_engine::EngineRuntime;
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_rpc::{RpcClient, memory_client, methods};

const ORG: &str = "org_home";
const TEMPLATE: &str = "zeron-dev";

async fn host_device(dir: &Path, edge: &ScriptedEdge) -> EngineRuntime {
    std::fs::create_dir_all(dir).unwrap();
    let session = json!({
        "refreshToken": edge.seed_session(ORG),
        "user": { "id": USER, "email": "u@example.com" },
        "orgId": ORG,
    });
    std::fs::write(dir.join("session.json"), session.to_string()).unwrap();
    start_engine(&engine_config(dir, &edge.url, None)).await
}

async fn create_on_place(rpc: &RpcClient, chat: &str, template: &str) -> Value {
    rpc.call(
        methods::MUTATE,
        json!({
            "op": "createChat",
            "chatId": chat,
            "place": { "provider": "fake", "template": template },
        }),
    )
    .await
    .expect("createChat on a place")
}

async fn send(rpc: &RpcClient, chat: &str, cwd: &str, harness: HarnessId, message_id: &str) {
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: RunRequest {
            mcp: None,
            prompt: "Add a landing page for reportly".into(),
            harness: Some(harness),
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: cwd.into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            access: Default::default(),
            plan: false,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            env: Default::default(),
            extra_mcp: Vec::new(),
        },
        message_id: message_id.into(),
    })
    .unwrap();
    rpc.call(
        methods::QUEUE_COMMAND,
        json!({ "chatId": chat, "command": command }),
    )
    .await
    .expect("queue the message");
}

async fn ready(rpc: &RpcClient, chat: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let steps = rpc
            .call(methods::SANDBOX_STEPS, json!({ "chatId": chat }))
            .await
            .unwrap();
        assert_ne!(steps["state"], "failed", "{steps:#}");
        if steps["state"] == "ready" {
            return;
        }
        assert!(Instant::now() < deadline, "{steps:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The ids a `ListHarnesses` answer lists.
fn ids(listed: &Value) -> Vec<String> {
    listed
        .as_array()
        .expect("a list of agents")
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn agents_on(rpc: &RpcClient, target: &str) -> Value {
    tokio::time::timeout(
        Duration::from_secs(5),
        rpc.call(
            methods::LIST_HARNESSES,
            json!({ "targetDeviceId": target, "includeRegistry": true }),
        ),
    )
    .await
    .expect("answered at once, not by a sandbox")
    .expect("ListHarnesses")
}

async fn models_on(rpc: &RpcClient, target: Option<&str>, harness: &str) -> Value {
    let mut params = json!({ "harness": harness });
    if let Some(target) = target {
        params["targetDeviceId"] = json!(target);
    }
    tokio::time::timeout(Duration::from_secs(30), rpc.call(methods::LIST_MODELS, params))
        .await
        .expect("answered")
        .expect("ListModels")
}

/// The chat's transcript in its sandbox.
fn transcript(provider: &FakeProvider, sandbox: &str, chat: &str) -> Vec<zeron_doc::SessionMessageEntry> {
    provider
        .with_engine(sandbox, |core| {
            core.doc_host
                .open(chat)
                .ok()
                .and_then(|h| h.doc().read_entries().ok())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_template_that_never_ran_offers_this_devices_agents_and_makes_no_sandbox() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());

    let place = zeron_proto::places::place_target("fake", Some(TEMPLATE));
    let mine = rpc
        .call(methods::LIST_HARNESSES, json!({ "includeRegistry": true }))
        .await
        .unwrap();
    assert_eq!(ids(&agents_on(&rpc, &place).await), ids(&mine));
    assert_eq!(
        models_on(&rpc, Some(&place), "mock").await,
        models_on(&rpc, None, "mock").await
    );

    // A template not named is the provider's default: the same.
    let default = zeron_proto::places::place_target("fake", None);
    assert_eq!(ids(&agents_on(&rpc, &default).await), ids(&mine));

    // Opening a composer made nothing and woke nothing.
    assert!(provider.calls().is_empty(), "{:#?}", provider.calls());
    assert!(provider.sandboxes().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_template_offers_the_agents_it_had_when_it_last_ran() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let host = host_device(&dirs.path().join("host"), &edge).await;
    // The template has only the mock agent.
    let provider = FakeProvider::cloud_with_agents(&dirs.path().join("cloud"), &[HarnessId::Mock]);
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    let place = zeron_proto::places::place_target("fake", Some(TEMPLATE));

    let created = create_on_place(&rpc, "chat-a", TEMPLATE).await;
    let device = created["deviceId"].as_str().unwrap().to_owned();
    ready(&rpc, "chat-a").await;

    // Once it has run, a new chat on that template offers what it had.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let offered = ids(&agents_on(&rpc, &place).await);
        if offered == ["mock"] {
            break;
        }
        assert!(Instant::now() < deadline, "still offered: {offered:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let theirs = provider
        .with_engine("chat-a", |core| core.registry.descriptors())
        .unwrap();
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    let models = models_on(&rpc, Some(&place), "mock").await;
    assert!(
        models.as_array().is_some_and(|m| !m.is_empty()),
        "{models:#}"
    );

    // Another template hasn't run: it still offers this device's agents.
    let other = zeron_proto::places::place_target("fake", Some("python"));
    let mine = rpc
        .call(methods::LIST_HARNESSES, json!({ "includeRegistry": true }))
        .await
        .unwrap();
    assert_eq!(ids(&agents_on(&rpc, &other).await), ids(&mine));

    // The chat's sandbox goes to sleep. Its composer still lists the
    // template's agents, and asking neither wakes it nor reaches it.
    provider.sleep("chat-a").await;
    let before = provider.calls();
    assert_eq!(ids(&agents_on(&rpc, &device).await), ["mock"]);
    assert_eq!(models_on(&rpc, Some(&device), "mock").await, models);
    assert_eq!(provider.calls(), before);

    // What a template had is kept across a restart of this device.
    let kept = zeron_engine::sandboxes::Sandboxes::open(&dirs.path().join("host"));
    let listed = kept.catalog("fake", Some(TEMPLATE)).expect("kept on disk");
    assert_eq!(
        listed.harnesses.iter().map(|d| d.id).collect::<Vec<_>>(),
        [HarnessId::Mock]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_not_on_the_template_says_so_keeps_the_message_and_is_never_swapped() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud_with_agents(&dirs.path().join("cloud"), &[HarnessId::Mock]);
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());

    let created = create_on_place(&rpc, "chat-b", TEMPLATE).await;
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    host.core()
        .workspace
        .rename_chat("chat-b", "Pre-titled")
        .unwrap();
    // (Once it's up on its template's agents: the fake shapes its engine
    // after boot, and a message waiting from before could be taken first.)
    ready(&rpc, "chat-b").await;
    // The user picked Codex; the template doesn't have it.
    send(&rpc, "chat-b", &cwd, HarnessId::Codex, "m-1").await;

    // The sandbox refuses it with a reason a client can show, and the
    // message waits to be sent again.
    let deadline = Instant::now() + Duration::from_secs(60);
    let refused = loop {
        let waiting = provider
            .with_engine("chat-b", |core| core.doc_host.waiting_messages("chat-b").ok())
            .flatten()
            .unwrap_or_default();
        if let Some(message) = waiting
            .into_iter()
            .find(|w| w.message_id.as_deref() == Some("m-1") && w.failure.is_some())
        {
            break message;
        }
        if Instant::now() >= deadline {
            let commands = provider
                .with_engine("chat-b", |core| {
                    core.doc_host
                        .open("chat-b")
                        .ok()
                        .and_then(|h| h.doc().read_commands().ok())
                        .unwrap_or_default()
                        .iter()
                        .map(|c| format!("{} {:?} {:?}", c.id, c.status, c.resolution))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            panic!(
                "m-1 was never refused. Commands in the sandbox: {commands:#?}; transcript: {:#?}",
                transcript(&provider, "chat-b", "chat-b")
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let failure = refused.failure.unwrap();
    assert_eq!(
        zeron_proto::places::agent_missing(&failure),
        Some((HarnessId::Codex, "Codex isn't on this template"))
    );
    assert_eq!(refused.prompt, "Add a landing page for reportly");

    // Nothing ran in its place: no other agent answered it.
    assert!(
        transcript(&provider, "chat-b", "chat-b").is_empty(),
        "{:#?}",
        transcript(&provider, "chat-b", "chat-b")
    );

    // The user switches agent and sends it again: it runs, once.
    send(&rpc, "chat-b", &cwd, HarnessId::Mock, "m-1").await;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let entries = transcript(&provider, "chat-b", "chat-b");
        let answered = entries
            .iter()
            .any(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete));
        if answered {
            let asked: Vec<_> = entries
                .iter()
                .filter(|e| e.role == MessageRole::User)
                .map(|e| e.id.clone())
                .collect();
            assert_eq!(asked, ["m-1"]);
            break;
        }
        assert!(Instant::now() < deadline, "{entries:#?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Sent again, it no longer waits as refused.
    let waiting = provider
        .with_engine("chat-b", |core| core.doc_host.waiting_messages("chat-b").ok())
        .flatten()
        .unwrap_or_default();
    assert!(
        waiting.iter().all(|w| w.failure.is_none()),
        "{waiting:#?}"
    );
}
