//! Messages to a provider-run chat reach its session exactly once, however
//! their delivery went (ticket #13; docs/design/provider-run-chats.md §4,
//! "Commands"): a command whose delivery is uncertain (the device crashed
//! after sending, or the acknowledgement was lost) is sent again only under
//! its original uuid, after the session's history is checked for it, and a
//! repeat the session acknowledges as a duplicate starts no turn.
//!
//! Real engines through their public surface (`createChat`, queued
//! commands), restarted on the same data folder, against the fake Claude
//! Code session server. Assertions are on what is visible: what the server
//! stored, the transcript, the command's outcome and the chat's status.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionCommandStatus,
    SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::{HarnessId, RunRequest, SandboxLevel, SessionStatus};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const FIRST_PROMPT: &str = "Run `uname -a` and `cat /tmp/zeron-init-marker 2>&1`, then reply DONE";
const SECOND_PROMPT: &str = "Reply with the single word ZULU.";

/// What outlives an engine: the fake provider, the Claude login, the stub
/// CLI and the data folders.
struct World {
    tmp: tempfile::TempDir,
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
}

impl World {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeSessionServer::start().await;
        fake.seed_session(SeedSession::environment("env_fake0001"));
        let login = ClaudeLogin::create(
            tmp.path().join("claude-login"),
            &FakeAccount::a(),
            "token-a-1",
        );
        let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
        Self {
            tmp,
            fake,
            login,
            stub,
        }
    }

    fn dir(&self, device: &str) -> PathBuf {
        self.tmp.path().join(device)
    }

    /// An engine for `device`, on its own data folder (the same one again
    /// after a restart), with Claude cloud sessions on.
    fn engine(&self, device: &str) -> Host {
        let dir = self.dir(device);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("device-id"), device).unwrap();
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(
            ClaudeCloudHarness::new(ClaudeCloudConfig {
                base_url: self.fake.base_url().to_owned(),
                login: ClaudeConfigDir::at(self.login.dir()),
                claude_executable: Some(self.stub.path().to_owned()),
            })
            .expect("harness builds"),
        ));
        {
            use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
            ClaudeCloudSessions::open(&dir)
                .save(ClaudeCloudSettings {
                    enabled: true,
                    ..ClaudeCloudSettings::default()
                })
                .expect("setting saves");
        }
        let core = EngineCore::assemble(&dir, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
            .expect("engine core assembles");
        let client = zeron_rpc::memory_client(core.rpc_service());
        Host { core, client }
    }

    fn session(&self) -> String {
        self.fake.created_sessions()[0].clone()
    }

    /// The person's messages stored in the session under `uuid`.
    fn stored(&self, uuid: &str) -> usize {
        self.fake
            .stored_events(&self.session())
            .iter()
            .filter(|e| e.payload["type"] == "user" && e.payload["uuid"] == uuid)
            .count()
    }

    /// Requests the session got for `uuid`: event posts carrying it.
    fn posts_of(&self, uuid: &str) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/events"))
            .filter(|r| {
                r.body["events"]
                    .as_array()
                    .is_some_and(|events| events.iter().any(|e| e["payload"]["uuid"] == uuid))
            })
            .count()
    }
}

struct Host {
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

impl Host {
    async fn create(&self) -> (String, String) {
        let created = self
            .client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": self.core.device_id,
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write", "access": "full-access"},
                    "prompt": FIRST_PROMPT,
                    "messageId": FIRST,
                }),
            )
            .await
            .expect("createChat");
        (
            created["chatId"].as_str().unwrap().to_owned(),
            created["cwd"].as_str().unwrap().to_owned(),
        )
    }

    fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
    }

    fn replies(&self, chat: &str) -> usize {
        self.entries(chat)
            .iter()
            .filter(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
            .count()
    }

    fn settled(&self, chat: &str) -> bool {
        let entries = self.entries(chat);
        !entries.is_empty()
            && entries
                .iter()
                .all(|e| e.status != Some(MessageStatus::Streaming))
    }

    fn idle(&self, chat: &str) -> bool {
        self.core
            .sessions
            .session_status(chat)
            .is_none_or(|s| s.status == SessionStatus::Idle)
    }

    /// The outcome of the command carrying message `message_id`, newest.
    fn outcome(&self, chat: &str, message_id: &str) -> Option<SessionCommandStatus> {
        self.core
            .doc_host
            .open(chat)
            .ok()?
            .doc()
            .read_commands()
            .ok()?
            .into_iter()
            .rev()
            .find(|c| {
                matches!(&c.payload, SessionCommandPayload::Run { message_id: id, .. }
                    if id == message_id)
            })
            .map(|c| c.status)
    }

    fn send(&self, chat: &str, cwd: &str, message_id: &str, prompt: &str) {
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Run {
                    request: RunRequest {
                        mcp: None,
                        prompt: prompt.into(),
                        harness: None,
                        model: None,
                        reasoning: None,
                        model_options: Default::default(),
                        cwd: cwd.into(),
                        sandbox: SandboxLevel::WorkspaceWrite,
                        auto_approve: false,
                        access: Default::default(),
                        plan: false,
                        attachments: Vec::new(),
                        worktree: None,
                        resume: None,
                        env: Default::default(),
                        extra_mcp: Vec::new(),
                    },
                    message_id: message_id.into(),
                },
            )
            .expect("queue message");
    }
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn text_of(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn shape(
    entries: &[SessionMessageEntry],
) -> Vec<(String, MessageRole, Option<MessageStatus>, String)> {
    entries
        .iter()
        .map(|e| (e.id.clone(), e.role, e.status, text_of(e)))
        .collect()
}

/// Create on `host` and wait for the first turn to be mirrored.
async fn create_and_mirror(world: &World, host: &Host) -> (String, String) {
    world.fake.queue_turn(Turn::bash_tool());
    let (chat, cwd) = host.create().await;
    wait_for("the first turn mirrored", || {
        host.replies(&chat) == 1 && host.settled(&chat) && host.idle(&chat)
    })
    .await;
    (chat, cwd)
}

/// The device dies right after sending: the session stored the message, the
/// answer never came back. Back up, it sends the message again under the
/// same uuid (having checked the history), the session has it once, the
/// reply shows, and the send counts as made, not failed.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_right_after_sending_delivers_the_message_once_after_the_restart() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    let (chat, cwd) = create_and_mirror(&world, &host).await;
    let uuid = zeron_proto::provider_cloud::event_uuid(SECOND);

    world.fake.queue_turn(Turn::text_zulu());
    let lost = world.fake.hold_next_ack();
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    wait_for("the message stored", || world.stored(&uuid) == 1).await;
    host.core.shutdown().await;
    drop(host);
    lost.release();

    let host = world.engine("dev-a");
    wait_for("the reply after the restart", || {
        host.settled(&chat)
            && host
                .entries(&chat)
                .last()
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
    })
    .await;
    wait_for("the send made", || {
        host.outcome(&chat, SECOND) == Some(SessionCommandStatus::Applied)
    })
    .await;
    wait_for("the chat idle", || host.idle(&chat)).await;
    let entries = host.entries(&chat);
    assert_eq!(world.stored(&uuid), 1, "{:#?}", shape(&entries));
    assert_eq!(
        entries
            .iter()
            .filter(|e| text_of(e) == SECOND_PROMPT)
            .count(),
        1,
        "{:#?}",
        shape(&entries)
    );
    // Nothing went under another uuid.
    let people: usize = world
        .fake
        .stored_events(&world.session())
        .iter()
        .filter(|e| e.payload["type"] == "user" && e.payload["message"]["content"] == SECOND_PROMPT)
        .count();
    assert_eq!(people, 1);
    host.core.shutdown().await;
}

/// The device dies while its send is on the way, before the session stored
/// it. Back up, the send is uncertain: the history doesn't have it, so it
/// goes again under its uuid, and the session gets it once.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_the_session_stored_the_message_sends_it_after_the_restart() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    let (chat, cwd) = create_and_mirror(&world, &host).await;
    let uuid = zeron_proto::provider_cloud::event_uuid(SECOND);

    world.fake.queue_turn(Turn::text_zulu());
    // The post reaches the server and never gets further.
    let _stuck = world.fake.hold_next_post();
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    wait_for("the send on its way", || world.posts_of(&uuid) == 1).await;
    host.core.shutdown().await;
    drop(host);
    assert_eq!(world.stored(&uuid), 0);

    let host = world.engine("dev-a");
    wait_for("the reply after the restart", || {
        host.settled(&chat)
            && host
                .entries(&chat)
                .last()
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
    })
    .await;
    wait_for("the chat idle", || host.idle(&chat)).await;
    let entries = host.entries(&chat);
    assert_eq!(world.stored(&uuid), 1, "{:#?}", shape(&entries));
    assert_eq!(
        entries
            .iter()
            .filter(|e| text_of(e) == SECOND_PROMPT)
            .count(),
        1,
        "{:#?}",
        shape(&entries)
    );
    // Checked in the history before it went again.
    let checked =
        world.fake.requests().iter().any(|r| {
            r.method == "GET" && r.path.ends_with("/events") && !r.path.ends_with("stream")
        });
    assert!(
        checked,
        "the history was read first: {:#?} {:?}",
        world
            .fake
            .requests()
            .iter()
            .map(|r| format!("{} {} {:?}", r.method, r.path, r.query))
            .collect::<Vec<_>>(),
        host.core
            .doc_host
            .open(&chat)
            .unwrap()
            .doc()
            .read_commands()
            .unwrap()
            .iter()
            .map(|c| (c.status, c.resolution.clone()))
            .collect::<Vec<_>>()
    );
    host.core.shutdown().await;
}

/// The same message sent again once its turn is over and mirrored (a retry
/// of a send that did arrive): the session acknowledges a duplicate and
/// starts nothing, so the chat doesn't wait for a turn that won't come.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_the_session_already_has_starts_no_turn() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    let (chat, cwd) = create_and_mirror(&world, &host).await;
    let uuid = zeron_proto::provider_cloud::event_uuid(SECOND);

    world.fake.queue_turn(Turn::text_zulu());
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    wait_for("the reply", || {
        host.replies(&chat) == 2 && host.settled(&chat) && host.idle(&chat)
    })
    .await;
    let entries_before = host.entries(&chat).len();

    // Sent again (a retry under the same id), with the run still parked...
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    // ...and again after a restart, with no run live.
    tokio::time::sleep(Duration::from_millis(300)).await;
    wait_for("idle after the repeat", || host.idle(&chat)).await;
    host.core.shutdown().await;
    drop(host);
    let host = world.engine("dev-a");
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    tokio::time::sleep(Duration::from_millis(500)).await;
    wait_for("idle after the repeat, restarted", || {
        host.idle(&chat) && host.settled(&chat)
    })
    .await;
    // The session has it once and played one turn for it.
    assert_eq!(world.stored(&uuid), 1);
    assert!(!world.fake.turn_playing(&world.session()));
    let entries = host.entries(&chat);
    assert_eq!(entries.len(), entries_before, "{:#?}", shape(&entries));
    assert_eq!(host.replies(&chat), 2);
    host.core.shutdown().await;
}

/// A posted message whose acknowledgement is lost (a 5xx after the session
/// stored it) is retried under its uuid: one copy in the session, one turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_acknowledgement_is_retried_under_the_same_uuid() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    let (chat, cwd) = create_and_mirror(&world, &host).await;
    let uuid = zeron_proto::provider_cloud::event_uuid(SECOND);

    world.fake.queue_turn(Turn::text_zulu());
    world.fake.lose_next_ack(503);
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    wait_for("the reply", || {
        host.replies(&chat) == 2 && host.settled(&chat) && host.idle(&chat)
    })
    .await;
    assert_eq!(world.stored(&uuid), 1);
    assert!(world.posts_of(&uuid) >= 2, "the send was retried");
    assert_eq!(
        host.outcome(&chat, SECOND),
        Some(SessionCommandStatus::Applied)
    );
    host.core.shutdown().await;
}
