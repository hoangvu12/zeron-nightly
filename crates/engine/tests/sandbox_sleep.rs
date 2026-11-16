//! Sleep and wake (spec #37 ticket #43, docs/design/cloud-byo.md "Waking"),
//! at the engine's RPCs with the fake provider: a sandbox idle (or its chat
//! archived) winds its engine down and stops; the host device stands in for
//! it in its device room and wakes it only for a queued send, from whichever
//! client sent it; it extends the time limit while a chat is busy.
//!
//! The edge is scripted: device rooms (with the 4409 a second host gets, and
//! nudges queued and replayed as production's front worker leaves them:
//! deleted as they're sent), sign-in refresh and a shared registry. It has
//! no chat rooms, so a send reaches a sandbox by the sender's relay escort.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/places.rs"]
mod places;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fake_provider::FakeProvider;
use places::{
    add_project, create_on_place, eventually, host_device, origin, project, ready, send, steps,
    steps_until,
};
use scripted_edge::ScriptedEdge;
use serde_json::{Value, json};
use zeron_doc::{MessageRole, MessageStatus, SessionCommandPayload};
use zeron_engine::EngineRuntime;
use zeron_engine::sandboxes::SleepTimings;
use zeron_proto::{AccessMode, HarnessId, RunRequest, SandboxLevel};
use zeron_rpc::{RpcClient, memory_client, methods};

const MINUTE: Duration = Duration::from_secs(60);

/// Quick to sleep: idle 2 s, looked at four times a second.
fn drowsy() -> SleepTimings {
    SleepTimings {
        idle: Duration::from_secs(2),
        time_limit: Duration::from_secs(30 * 60),
        check_every: Duration::from_millis(250),
    }
}

/// Awake unless something puts it to sleep (archiving, its time limit).
fn wakeful() -> SleepTimings {
    SleepTimings {
        idle: Duration::from_secs(60 * 60),
        ..drowsy()
    }
}

struct World {
    edge: ScriptedEdge,
    dirs: tempfile::TempDir,
    host: EngineRuntime,
    rpc: RpcClient,
    provider: Arc<FakeProvider>,
    chat: String,
    device: String,
    cwd: String,
}

/// A chat on the fake cloud whose first message (sent from the host
/// device) ran on its sandbox. It stays awake until a test lets it sleep
/// (a message reaches a sandbox some seconds after it's sent).
async fn world(chat: &str) -> World {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    host.core().sandboxes.set_sleep_timings(wakeful());
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;
    let created = create_on_place(&rpc, chat, "fake").await;
    let device = created["deviceId"].as_str().unwrap().to_owned();
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    host.core().workspace.rename_chat(chat, "Pre-titled").unwrap();
    send(&rpc, chat, &format!("{chat}-first"), &cwd, "hello sandbox").await;
    ready(&rpc, chat).await;
    replied(&provider, chat, &format!("{chat}-first"), MINUTE).await;
    World {
        edge,
        dirs,
        host,
        rpc,
        provider,
        chat: chat.to_owned(),
        device,
        cwd,
    }
}

/// The message ran on the sandbox's engine: its transcript holds it and a
/// finished reply after it.
async fn replied(provider: &FakeProvider, chat: &str, message_id: &str, within: Duration) {
    eventually(&format!("{message_id} ran on {chat}"), within, async || {
        let entries = provider
            .with_engine(chat, |core| {
                core.doc_host
                    .open(chat)
                    .ok()
                    .and_then(|h| h.doc().read_entries().ok())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let asked = entries
            .iter()
            .position(|e| e.role == MessageRole::User && e.id == message_id)?;
        entries[asked..]
            .iter()
            .any(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
            .then_some(())
    })
    .await;
}

impl World {
    fn timings(&self, timings: SleepTimings) {
        self.host.core().sandboxes.set_sleep_timings(timings);
    }
}

async fn asleep(w: &World) -> Value {
    steps_until(&w.rpc, &w.chat, MINUTE, |s| {
        assert!(s["state"] != "failed" && s["state"] != "gone", "{s:#}");
        s["state"] == "asleep"
    })
    .await
}

fn count(calls: &[String], call: &str) -> usize {
    calls.iter().filter(|c| *c == call).count()
}

/// The host device holds the sandbox's room for it.
async fn stood_in_for(w: &World) {
    eventually("the host device stands in", Duration::from_secs(20), async || {
        w.edge.host_connected(&w.device).then_some(())
    })
    .await;
}

/// Nothing woke it: still asleep, never started again.
async fn stays_asleep(w: &World, starts: usize, over: Duration) {
    let until = Instant::now() + over;
    while Instant::now() < until {
        let now = steps(&w.rpc, &w.chat).await;
        assert_eq!(now["state"], "asleep", "{now:#}");
        assert_eq!(
            count(&w.provider.calls(), &format!("start {}", w.chat)),
            starts,
            "{:#?}",
            w.provider.calls()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Awake again: ready, started once more, its engine back in its room.
async fn awake(w: &World, starts: usize) {
    steps_until(&w.rpc, &w.chat, MINUTE, |s| {
        assert!(s["state"] != "failed" && s["state"] != "gone", "{s:#}");
        s["state"] == "ready"
    })
    .await;
    assert_eq!(
        count(&w.provider.calls(), &format!("start {}", w.chat)),
        starts,
        "{:#?}",
        w.provider.calls()
    );
    assert!(w.provider.with_engine(&w.chat, |_| ()).is_some());
}

/// The stand-in stepped aside for the sandbox's engine and stays out: no
/// host joins the room again (each would 4409 the other).
async fn no_host_fight(w: &World) {
    let joins = w.edge.host_dials(&w.device).len();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(w.edge.host_dials(&w.device).len(), joins);
    assert!(w.edge.host_connected(&w.device));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_sandbox_sleeps_a_reader_doesnt_wake_it_and_a_send_does() {
    let w = world("chat-idle").await;
    w.timings(drowsy());

    // Idle: its engine winds down, then the provider stops it.
    asleep(&w).await;
    let calls = w.provider.calls();
    let quiesced = calls
        .iter()
        .position(|c| c == "run chat-idle: zeron-engine quiesce")
        .unwrap_or_else(|| panic!("{calls:#?}"));
    let stopped = calls
        .iter()
        .position(|c| c == "stop chat-idle")
        .unwrap_or_else(|| panic!("{calls:#?}"));
    assert!(quiesced < stopped, "{calls:#?}");
    assert!(w.provider.with_engine("chat-idle", |_| ()).is_none());
    stood_in_for(&w).await;
    w.timings(wakeful());

    // Someone opens the chat: the chat room nudges its host. A nudge with
    // no queued send leaves it asleep.
    assert_eq!(w.edge.post_nudge(&w.device, "chat-idle").await, 200);
    stays_asleep(&w, 1, Duration::from_secs(12)).await;

    // A send wakes it, and the message runs.
    send(&w.rpc, "chat-idle", "chat-idle-second", &w.cwd, "again").await;
    awake(&w, 2).await;
    replied(&w.provider, "chat-idle", "chat-idle-second", 2 * MINUTE).await;
    no_host_fight(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_from_a_client_that_never_talks_to_the_host_device_wakes_it() {
    let w = world("chat-far").await;
    // Another of the user's devices, which knows the chat from the registry.
    let laptop = host_device(&w.dirs.path().join("laptop"), &w.edge).await;
    let laptop_rpc = memory_client(laptop.core().rpc_service());
    eventually("the laptop knows the chat", MINUTE, async || {
        laptop
            .core()
            .workspace
            .chat("chat-far")
            .ok()
            .flatten()
            .filter(|row| row.device_id == w.device)
    })
    .await;

    w.timings(drowsy());
    asleep(&w).await;
    stood_in_for(&w).await;
    w.timings(wakeful());
    let host_device = w.host.core().device_id.clone();
    let asked_host = w.edge.client_dials(&host_device).len();

    send(&laptop_rpc, "chat-far", "chat-far-laptop", &w.cwd, "from afar").await;
    awake(&w, 2).await;
    replied(&w.provider, "chat-far", "chat-far-laptop", 2 * MINUTE).await;
    // The laptop's send reached the sandbox's room only: nobody dialed the
    // host device's own room for it.
    assert_eq!(
        w.edge.client_dials(&host_device).len(),
        asked_host,
        "the host device's room was dialed"
    );
    no_host_fight(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archiving_puts_it_to_sleep_unarchiving_doesnt_wake_it_and_wake_does() {
    let w = world("chat-shelf").await;
    let archive = |archived: bool| {
        w.rpc.call(
            methods::MUTATE,
            json!({ "op": "setChatArchived", "chatId": "chat-shelf", "archived": archived }),
        )
    };

    archive(true).await.expect("archive");
    asleep(&w).await;
    stood_in_for(&w).await;

    archive(false).await.expect("unarchive");
    stays_asleep(&w, 1, Duration::from_secs(3)).await;

    // Files, Terminal and Diffs offer Wake.
    w.rpc
        .call(methods::WAKE_SANDBOX, json!({ "chatId": "chat-shelf" }))
        .await
        .expect("Wake");
    awake(&w, 2).await;
    no_host_fight(&w).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_chat_keeps_its_sandbox_awake_and_a_stopped_sandbox_is_stood_in_for() {
    let w = world("chat-busy").await;
    // A turn waiting on the person (approve a command) keeps the chat busy.
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: RunRequest {
            mcp: None,
            prompt: "run the tests".into(),
            harness: Some(HarnessId::Mock),
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: w.cwd.clone(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            access: AccessMode::Supervised,
            plan: false,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            env: Default::default(),
            extra_mcp: Vec::new(),
        },
        message_id: "chat-busy-ask".into(),
    })
    .unwrap();
    w.rpc
        .call(
            methods::QUEUE_COMMAND,
            json!({ "chatId": "chat-busy", "command": command }),
        )
        .await
        .expect("queue");

    // Busy: its time limit is pushed out, and it stays awake past its idle
    // time.
    eventually("the time limit is extended", MINUTE, async || {
        w.provider
            .extends()
            .contains(&"chat-busy 1800".to_owned())
            .then_some(())
    })
    .await;
    w.timings(drowsy());
    tokio::time::sleep(Duration::from_secs(4)).await;
    let now = steps(&w.rpc, "chat-busy").await;
    assert_eq!(now["state"], "ready", "{now:#}");
    assert!(
        !w.provider.calls().contains(&"stop chat-busy".to_owned()),
        "{:#?}",
        w.provider.calls()
    );

    // The time limit runs out anyway (the host device was away): the
    // sandbox stops itself, and the host device stands in for it.
    w.provider.sleep("chat-busy").await;
    asleep(&w).await;
    stood_in_for(&w).await;
}

#[test]
fn sleep_timings_default_to_the_design() {
    let timings = SleepTimings::default();
    assert_eq!(timings.idle, Duration::from_secs(10 * 60));
    assert_eq!(timings.time_limit, Duration::from_secs(30 * 60));
}
