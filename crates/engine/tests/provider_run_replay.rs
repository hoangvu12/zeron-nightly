//! Provider-run chats survive restarts, dropped runs and other hosts without
//! losing or duplicating anything (ticket #8; docs/design/provider-run-chats.md
//! §5, "Mirrored entries", "Replay watermark", "Deduplication").
//!
//! Real engines through their public surface (`createChat`, queued commands,
//! `EngineCore::resume_provider_chat`), restarted on the same data folder,
//! against the fake Claude Code session server. Two engines share the
//! workspace through the in-memory registry room (as in `workspace_sync.rs`)
//! and their chat documents by exchanging Loro updates, as the relay does.
//! Assertions are on what is visible: the transcript, what the fake server
//! received and stored, and the chat document's mirror state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, PROMPTS_QUESTION, SeedSession, StubClaude, Turn,
    is_text_delta,
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
        let core = EngineCore::assemble(&dir, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
            .expect("engine core assembles");
        let client = zeron_rpc::memory_client(core.rpc_service());
        Host { core, client }
    }

    fn session(&self) -> String {
        self.fake.created_sessions()[0].clone()
    }

    /// Stored person's messages with this text, across the whole session.
    fn stored_messages(&self, text: &str) -> usize {
        self.fake
            .stored_events(&self.session())
            .iter()
            .filter(|e| e.payload["type"] == "user" && e.payload["message"]["content"] == text)
            .count()
    }

    fn last_seq(&self) -> u64 {
        self.fake
            .stored_events(&self.session())
            .iter()
            .map(|e| e.sequence_num)
            .max()
            .unwrap_or(0)
    }
}

struct Host {
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

impl Host {
    async fn create(&self, access: &str) -> (String, String) {
        let created = self
            .client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": self.core.device_id,
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write", "access": access},
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

    fn assistants(&self, chat: &str, status: MessageStatus) -> Vec<SessionMessageEntry> {
        self.entries(chat)
            .into_iter()
            .filter(|e| e.role == MessageRole::Assistant && e.status == Some(status))
            .collect()
    }

    fn settled(&self, chat: &str) -> bool {
        let entries = self.entries(chat);
        !entries.is_empty()
            && entries
                .iter()
                .all(|e| e.status != Some(MessageStatus::Streaming))
    }

    fn watermark(&self, chat: &str) -> u64 {
        self.core
            .doc_host
            .open(chat)
            .unwrap()
            .doc()
            .provider_watermark()
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
            .expect("queue follow-up");
    }

    /// The question the chat waits on: (request id, questions).
    fn open_input(&self, chat: &str) -> Option<(String, Vec<zeron_proto::UserInputQuestion>)> {
        self.entries(chat)
            .iter()
            .flat_map(|e| e.parts.clone())
            .find_map(|p| match p {
                MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } => Some((request_id, questions)),
                _ => None,
            })
    }

    fn answer(&self, chat: &str, request_id: &str, question_id: &str, label: &str) {
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::RespondInput {
                    request_id: request_id.into(),
                    answers: vec![zeron_proto::UserInputAnswer {
                        question_id: question_id.into(),
                        labels: vec![label.into()],
                    }],
                },
            )
            .expect("queue answer");
    }

    /// This engine's copy of the chat document, as Loro updates.
    fn updates(&self, chat: &str) -> Vec<u8> {
        self.core
            .doc_host
            .open(chat)
            .unwrap()
            .doc()
            .doc()
            .export(loro::ExportMode::all_updates())
            .unwrap()
    }

    /// Merge another device's copy of the chat document in (the relay).
    fn merge(&self, chat: &str, updates: &[u8]) {
        let handle = self.core.doc_host.open(chat).unwrap();
        handle.doc().doc().import(updates).unwrap();
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

fn users_saying(entries: &[SessionMessageEntry], text: &str) -> usize {
    entries
        .iter()
        .filter(|e| e.role == MessageRole::User && text_of(e) == text)
        .count()
}

fn shape(entries: &[SessionMessageEntry]) -> Vec<(MessageRole, Option<MessageStatus>, String)> {
    entries
        .iter()
        .map(|e| (e.role, e.status, text_of(e)))
        .collect()
}

/// Create on `host` and wait for the first turn to be mirrored.
async fn create_and_mirror(world: &World, host: &Host) -> (String, String) {
    world.fake.queue_turn(Turn::bash_tool());
    let (chat, cwd) = host.create("full-access").await;
    wait_for("the first turn mirrored", || {
        !host.assistants(&chat, MessageStatus::Complete).is_empty() && host.settled(&chat)
    })
    .await;
    (chat, cwd)
}

async fn bridge(a: &Host, b: &Host) -> zeron_sync::registry::mock_server::MockRegistryServer {
    let server = zeron_sync::registry::mock_server::MockRegistryServer::start().await;
    a.core.workspace.connect_registry_url(&server.url());
    b.core.workspace.connect_registry_url(&server.url());
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn events_that_show_nothing_still_advance_the_watermark() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    let (chat, _cwd) = create_and_mirror(&world, &host).await;
    // The session's last stored events (its `result`, its bookkeeping) write
    // no entry of their own, and still count as mirrored.
    wait_for("the watermark at the session's last event", || {
        host.watermark(&chat) == world.last_seq()
    })
    .await;
    assert!(world.last_seq() > 0);
    // Each entry mirrored from the session carries the event it came from.
    let doc = host.core.doc_host.open(&chat).unwrap();
    let stamped: Vec<_> = doc
        .doc()
        .provider_entries()
        .into_iter()
        .filter(|e| e.provider_uuid.is_some())
        .collect();
    assert_eq!(stamped.len(), 2, "{stamped:?}");
    assert_eq!(stamped[0].id, FIRST);
    assert_eq!(stamped[0].provider_uuid.as_deref(), Some(FIRST));
    let stored = world.fake.stored_events(&world.session());
    for entry in &stamped {
        let seq = entry.provider_seq.expect("sequence number");
        assert!(
            stored
                .iter()
                .any(|e| e.sequence_num == seq
                    && Some(e.event_id.as_str()) == entry.provider_uuid.as_deref()),
            "{entry:?}"
        );
    }
    host.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_mid_turn_ends_with_the_whole_turn_once() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    let (chat, cwd) = create_and_mirror(&world, &host).await;

    let (held, gate) = Turn::text_zulu().hold_after(is_text_delta);
    world.fake.queue_turn(held);
    host.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    gate.reached().await;
    wait_for("the streamed text", || {
        host.assistants(&chat, MessageStatus::Streaming)
            .iter()
            .any(|e| text_of(e).contains("ZULU"))
    })
    .await;

    // Zeron quits mid-turn; the session finishes it with every device off.
    host.core.shutdown().await;
    drop(host);
    gate.release();
    let sid = world.session();
    wait_for("the turn finished at the provider", || {
        !world.fake.turn_playing(&sid)
    })
    .await;

    // Back up: the transcript catches up on its own, with no message sent.
    let host = world.engine("dev-a");
    wait_for("the cut-off turn written whole", || {
        host.settled(&chat)
            && host.watermark(&chat) == world.last_seq()
            && host
                .assistants(&chat, MessageStatus::Complete)
                .iter()
                .any(|e| text_of(e).trim() == "ZULU")
    })
    .await;
    let entries = host.entries(&chat);
    let shape = shape(&entries);
    // No gaps, no duplicates: each message once, one reply each, the cut-off
    // reply no longer aborted.
    assert_eq!(users_saying(&entries, FIRST_PROMPT), 1, "{shape:#?}");
    assert_eq!(users_saying(&entries, SECOND_PROMPT), 1, "{shape:#?}");
    let replies: Vec<_> = entries
        .iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .collect();
    assert_eq!(replies.len(), 2, "{shape:#?}");
    assert!(
        replies.iter().all(|e| e.status == Some(MessageStatus::Complete)),
        "{shape:#?}"
    );
    assert_eq!(text_of(replies[1]).trim(), "ZULU", "{shape:#?}");
    let second = entries.iter().position(|e| e.id == SECOND).unwrap();
    assert!(entries[second + 1..].iter().any(|e| e.role == MessageRole::Assistant));
    // The session got each message exactly once.
    assert_eq!(world.stored_messages(SECOND_PROMPT), 1);
    assert_eq!(world.stored_messages(FIRST_PROMPT), 1);

    // And the chat goes on from there.
    world.fake.queue_turn(Turn::text_zulu());
    host.send(&chat, &cwd, "3c9d7e2f-1a4b-4c8d-9e0f-a1b2c3d4e5f6", "Once more.");
    wait_for("the next reply", || {
        host.settled(&chat) && host.assistants(&chat, MessageStatus::Complete).len() == 3
    })
    .await;
    assert_eq!(users_saying(&host.entries(&chat), SECOND_PROMPT), 1);
    host.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_left_open_by_a_restart_is_asked_again_and_answerable() {
    let world = World::new().await;
    let host = world.engine("dev-a");
    world.fake.queue_turn(Turn::prompts());
    let (chat, _cwd) = host.create("supervised").await;
    wait_for("the permission question", || host.open_input(&chat).is_some()).await;
    let sid = world.session();

    host.core.shutdown().await;
    drop(host);
    assert_eq!(world.fake.open_prompts(&sid).len(), 1);

    // Back up: the open question shows again, and answering it works.
    let host = world.engine("dev-a");
    wait_for("the question asked again", || host.open_input(&chat).is_some()).await;
    let (request, questions) = host.open_input(&chat).unwrap();
    assert!(
        questions[0].question.contains("Read Wiki Structure"),
        "{questions:?}"
    );
    host.answer(&chat, &request, &questions[0].id, "Yes");
    wait_for("the next question", || {
        host.open_input(&chat)
            .is_some_and(|(_, q)| q[0].question == PROMPTS_QUESTION)
    })
    .await;
    let (request, questions) = host.open_input(&chat).unwrap();
    host.answer(&chat, &request, &questions[0].id, "blue");
    wait_for("the turn completed", || {
        host.settled(&chat) && !world.fake.turn_playing(&sid)
    })
    .await;

    let prompts = world.fake.prompts(&sid);
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        world.fake.answer(&sid, &prompts[0]).unwrap()["behavior"],
        "allow"
    );
    assert_eq!(
        world.fake.answer(&sid, &prompts[1]).unwrap()["updatedInput"]["answers"]
            [PROMPTS_QUESTION],
        "blue"
    );
    // One question panel per prompt in the transcript, none left open.
    assert!(host.open_input(&chat).is_none());
    let entries = host.entries(&chat);
    assert_eq!(users_saying(&entries, FIRST_PROMPT), 1, "{:#?}", shape(&entries));
    host.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_copies_of_a_provider_event_converge_to_one_on_every_device_with_no_send() {
    let world = World::new().await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    let _room = bridge(&a, &b).await;
    let (chat, _cwd) = create_and_mirror(&world, &a).await;
    b.merge(&chat, &a.updates(&chat));
    wait_for("the chat on B", || users_saying(&b.entries(&chat), FIRST_PROMPT) == 1).await;

    // Two devices, both briefly acting as host, each mirror the same person's
    // message (the same provider event) into their own copy.
    let doc_a = a.core.doc_host.open(&chat).unwrap();
    let doc_b = b.core.doc_host.open(&chat).unwrap();
    let original = doc_a
        .doc()
        .provider_entries()
        .into_iter()
        .find(|e| e.provider_uuid.as_deref() == Some(FIRST))
        .expect("the first message carries its event");
    let seq = original.provider_seq.unwrap();
    let copy = |id: &str| SessionMessageEntry {
        id: id.into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text: FIRST_PROMPT.into(),
        }],
        created_at: 0,
        device_id: "stale-host".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        duration_ms: None,
    };
    doc_a
        .doc()
        .push_provider_message(&copy("copy-on-a"), FIRST, seq)
        .unwrap();
    doc_b
        .doc()
        .push_provider_message(&copy("copy-on-b"), FIRST, seq)
        .unwrap();
    let (from_a, from_b) = (a.updates(&chat), b.updates(&chat));
    let shown = |host: &Host| {
        host.entries(&chat)
            .into_iter()
            .filter(|e| e.role == MessageRole::User && text_of(e) == FIRST_PROMPT)
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };
    let kept = |host: &Host| {
        host.core
            .doc_host
            .open(&chat)
            .unwrap()
            .doc()
            .provider_entries()
            .into_iter()
            .filter(|e| e.provider_uuid.as_deref() == Some(FIRST))
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };

    // B, not the host, gets A's copy: all three are there, one shows.
    // (Which is the lowest Loro element id's, not necessarily the first
    // written: the same choice on every device.)
    b.merge(&chat, &from_a);
    assert_eq!(kept(&b).len(), 3);
    let chosen = shown(&b);
    assert_eq!(chosen.len(), 1, "{chosen:?}");

    // A, the host, gets B's copy while the chat is idle: it sweeps on the
    // remote change, with no message sent and no run.
    a.merge(&chat, &from_b);
    wait_for("the host's sweep", || kept(&a).len() == 1).await;
    assert_eq!(kept(&a), chosen);
    assert_eq!(shown(&a), chosen);
    assert!(
        !world
            .fake
            .requests()
            .iter()
            .any(|r| r.method == "POST" && r.path.ends_with("/events")),
        "nothing was sent"
    );
    // The sweep reaches B: the same copy everywhere.
    b.merge(&chat, &a.updates(&chat));
    assert_eq!(kept(&b), chosen);
    assert_eq!(shown(&b), chosen);
    a.core.shutdown().await;
    b.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_host_resumes_the_chat_and_catches_up_on_what_it_missed() {
    let world = World::new().await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    let _room = bridge(&a, &b).await;
    let (chat, _cwd) = create_and_mirror(&world, &a).await;
    wait_for("the chat row on B", || {
        b.core.workspace.chat(&chat).ok().flatten().is_some()
    })
    .await;
    b.merge(&chat, &a.updates(&chat));

    // A goes away; meanwhile someone works on claude.ai.
    a.core.shutdown().await;
    let sid = world.session();
    world.fake.queue_turn(Turn::text_zulu());
    world.fake.post_as_client(
        &sid,
        serde_json::json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": "",
            "type": "user",
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": "Typed on claude.ai"},
        }),
    );
    wait_for("the turn at the provider", || !world.fake.turn_playing(&sid)).await;

    // B becomes the host and resumes the chat (what takeover calls).
    assert!(b.core.resume_provider_chat(&chat).await.expect("resumed"));
    wait_for("B caught up", || {
        b.settled(&chat) && b.watermark(&chat) == world.last_seq()
    })
    .await;
    let entries = b.entries(&chat);
    let shape = shape(&entries);
    assert_eq!(users_saying(&entries, FIRST_PROMPT), 1, "{shape:#?}");
    assert_eq!(users_saying(&entries, "Typed on claude.ai"), 1, "{shape:#?}");
    let typed = entries
        .iter()
        .position(|e| text_of(e) == "Typed on claude.ai")
        .unwrap();
    assert!(
        entries[typed + 1..]
            .iter()
            .any(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU"),
        "{shape:#?}"
    );
    // The first turn, which B already had, wasn't written again.
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.role == MessageRole::Assistant)
            .count(),
        2,
        "{shape:#?}"
    );
    // Nothing was sent to the session by resuming.
    assert!(
        !world
            .fake
            .requests()
            .iter()
            .any(|r| r.method == "POST" && r.path.ends_with("/events"))
    );
    // A second call while the run is live does nothing.
    assert!(!b.core.resume_provider_chat(&chat).await.unwrap());
    b.core.shutdown().await;
}
