//! Archive, delete and revive (ticket #10; docs/design/provider-run-chats.md
//! §6, "Archive and delete").
//!
//! Real engines through what the person does: the sidebar's
//! `DiscoverProviderSessions`, archive and delete as the app writes them to
//! the registry (and `Mutate deleteChat`), and messages as the composer
//! queues them. Two engines share the workspace through the in-memory
//! registry room where devices interact. Assertions are on the chat rows,
//! the transcript, what the chat shows (`providerState`), and what the fake
//! Claude Code session server was asked.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_doc::{MessagePart, MessageRole, SessionCommandPayload, SessionMessageEntry};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::provider_cloud::ProviderChatState;
use zeron_proto::{Chat, HarnessId, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const ELSEWHERE_PROMPT: &str = "Reply with the single word ZULU.";
const RESUMED_PROMPT: &str = "Picking this up again on my phone.";

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

    /// An engine for `device` with Claude cloud sessions on, reading the
    /// list on every call.
    fn engine(&self, device: &str) -> Host {
        use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
        let dir = self.dir(device);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("device-id"), device).unwrap();
        ClaudeCloudSessions::open(&dir)
            .save(ClaudeCloudSettings {
                enabled: true,
                ..ClaudeCloudSettings::default()
            })
            .expect("setting saves");
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
        core.set_provider_list_interval(Duration::ZERO);
        let client = zeron_rpc::memory_client(core.rpc_service());
        Host { core, client }
    }

    /// A session someone started on claude.ai, its first turn played out.
    async fn started_elsewhere(&self, title: &str) -> String {
        self.fake.queue_turn(Turn::text_zulu());
        let sid = self.fake.start_elsewhere(title, ELSEWHERE_PROMPT);
        wait_for("the turn at the provider", || !self.fake.turn_playing(&sid)).await;
        sid
    }

    /// The session quiet for ten days: outside the list's window.
    fn old(&self, sid: &str) {
        let ten_days_ago = (chrono::Utc::now() - chrono::Duration::days(10))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        self.fake.set_times(sid, &ten_days_ago, &ten_days_ago);
    }

    /// The session's last activity `by` from now, by the provider's clock.
    fn later(&self, sid: &str, by: chrono::Duration) {
        let created = (chrono::Utc::now() - chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let at = (chrono::Utc::now() + by).to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        self.fake.set_times(sid, &created, &at);
    }

    /// The session resumed on the phone: active again, with a new turn.
    async fn resume_elsewhere(&self, sid: &str) {
        self.fake.queue_turn(Turn::text_zulu());
        self.fake.resume_elsewhere(sid, RESUMED_PROMPT);
        wait_for("the resumed turn at the provider", || {
            !self.fake.turn_playing(sid)
        })
        .await;
    }
}

struct Host {
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

impl Host {
    /// The sidebar's call, once the registry is current: a device only acts
    /// on the list then (one just joined to the room may be behind).
    async fn discover(&self) -> Value {
        wait_for("the registry current", || {
            self.core.workspace.registry_current()
        })
        .await;
        self.client
            .call(methods::DISCOVER_PROVIDER_SESSIONS, json!({ "visible": true }))
            .await
            .expect("DiscoverProviderSessions")
    }

    /// Discover `sid` and wait for its history; returns its chat id.
    async fn import(&self, sid: &str) -> String {
        self.discover().await;
        let chat = self.chat_of(sid).expect("a chat for the session").id;
        wait_for("the history mirrored", || {
            self.entries(&chat)
                .iter()
                .any(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU")
        })
        .await;
        chat
    }

    fn chat(&self, chat: &str) -> Option<Chat> {
        self.core.workspace.chat(chat).ok().flatten()
    }

    fn chat_of(&self, sid: &str) -> Option<Chat> {
        self.core
            .workspace
            .read_chats()
            .unwrap()
            .into_iter()
            .find(|c| {
                c.provider_session
                    .as_ref()
                    .is_some_and(|s| s.session_id == sid)
            })
    }

    fn deleted(&self, chat: &str) -> bool {
        self.core.workspace.chat_deleted(chat)
    }

    fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
    }

    /// What the chat shows about its session.
    fn state(&self, chat: &str) -> Option<ProviderChatState> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().provider_chat_state())
    }

    /// Archive or unarchive in Zeron, as the app writes it.
    fn set_archived(&self, chat: &str, archived: bool) {
        assert!(self.core.workspace.set_chat_archived(chat, archived).unwrap());
    }

    /// A message, as the composer queues one.
    fn send(&self, chat: &str, sid: &str, prompt: &str) {
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
                        cwd: format!("claude-cloud://{sid}"),
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
                    message_id: uuid::Uuid::new_v4().to_string(),
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

fn archived_on_claude(sid: &str) -> ProviderChatState {
    ProviderChatState::ArchivedOnProvider {
        url: format!("https://claude.ai/code/{sid}"),
    }
}

async fn bridge(hosts: &[&Host]) -> zeron_sync::registry::mock_server::MockRegistryServer {
    let server = zeron_sync::registry::mock_server::MockRegistryServer::start().await;
    for host in hosts {
        host.core.workspace.connect_registry_url(&server.url());
    }
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn archiving_on_claude_deletes_the_chat_and_discovery_leaves_it_deleted() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Done with this").await;
    let a = world.engine("dev-a");
    let chat = a.import(&sid).await;

    world.fake.archive(&sid);
    a.discover().await;
    assert!(a.chat(&chat).is_none(), "the chat is gone");
    assert!(a.deleted(&chat));

    // Later reads don't bring it back, and Zeron archived nothing itself.
    a.discover().await;
    a.discover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.chat(&chat).is_none());
    assert!(world.fake.archive_requests().is_empty(), "{:?}", world.fake.archive_requests());
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn archiving_in_zeron_leaves_the_session_active_and_unarchiving_continues_it() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Archive me in Zeron").await;
    let a = world.engine("dev-a");
    let chat = a.import(&sid).await;

    a.set_archived(&chat, true);
    a.discover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(world.fake.archive_requests().is_empty());
    assert_eq!(world.fake.session_status(&sid).as_deref(), Some("active"));
    assert!(a.chat(&chat).is_some_and(|c| c.archived), "still there, archived");

    a.set_archived(&chat, false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.state(&chat), None, "nothing to say: the session is active");
    world.fake.queue_turn(Turn::text_zulu());
    a.send(&chat, &sid, "Carry on");
    wait_for("the reply to the next message", || {
        a.entries(&chat)
            .iter()
            .filter(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU")
            .count()
            == 2
    })
    .await;
    assert!(
        world
            .fake
            .stored_events(&sid)
            .iter()
            .any(|e| e.payload["message"]["content"] == "Carry on"),
        "the message reached the session"
    );
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_in_zeron_archives_the_session() {
    let world = World::new().await;
    let by_app = world.started_elsewhere("Deleted in the app").await;
    let by_rpc = world.started_elsewhere("Deleted over the engine").await;
    let a = world.engine("dev-a");
    a.discover().await;
    let app_chat = a.chat_of(&by_app).unwrap().id;
    let rpc_chat = a.chat_of(&by_rpc).unwrap().id;

    // The app deletes by writing the tombstone to the registry.
    a.core.workspace.delete_chat(&app_chat).unwrap();
    // Another client asks the engine.
    a.client
        .call(
            methods::MUTATE,
            json!({ "op": "deleteChat", "chatId": rpc_chat }),
        )
        .await
        .expect("deleteChat");

    wait_for("both sessions archived", || {
        world.fake.session_status(&by_app).as_deref() == Some("archived")
            && world.fake.session_status(&by_rpc).as_deref() == Some("archived")
    })
    .await;
    // Once each, and discovery doesn't bring them back.
    a.discover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut asked = world.fake.archive_requests();
    asked.sort();
    let mut expected = vec![by_app.clone(), by_rpc.clone()];
    expected.sort();
    assert_eq!(asked, expected);
    assert!(a.chat(&app_chat).is_none() && a.chat(&rpc_chat).is_none());
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_an_old_chat_away_from_its_host_archives_the_session() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Host was away").await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    let room = bridge(&[&a, &b]).await;
    let chat = a.import(&sid).await;
    wait_for("B knows the chat", || b.chat(&chat).is_some()).await;
    // Quiet for ten days: past the list's window.
    world.old(&sid);
    // A, the host, is off; the person deletes the chat on B.
    a.core.shutdown().await;
    b.core.workspace.delete_chat(&chat).unwrap();
    wait_for("the tombstone at the relay", || {
        room.row("chats", &chat).is_some_and(|r| r.deleted)
    })
    .await;

    // B knew the chat: it archives the session itself, reading it alone (it
    // is outside the list's window).
    wait_for("the session archived", || {
        world.fake.session_status(&sid).as_deref() == Some("archived")
    })
    .await;
    assert!(b.chat(&chat).is_none(), "and it stays deleted");
    b.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archive_a_delete_owes_survives_a_restart() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Archive me later").await;
    let a = world.engine("dev-a");
    let chat = a.import(&sid).await;
    world.old(&sid);
    // The archive fails while A runs.
    world.fake.fail_archives(usize::MAX);
    a.core.workspace.delete_chat(&chat).unwrap();
    wait_for("the archive tried", || !world.fake.archive_requests().is_empty()).await;
    assert_eq!(world.fake.session_status(&sid).as_deref(), Some("active"));
    a.core.shutdown().await;
    drop(a);

    // After a restart (no list read: the session is outside its window
    // anyway), the archive is tried again and done.
    world.fake.fail_archives(0);
    let again = world.engine("dev-a");
    wait_for("the session archived after the restart", || {
        world.fake.session_status(&sid).as_deref() == Some("archived")
    })
    .await;
    assert!(again.chat(&chat).is_none());
    assert!(world.fake.list_reads().len() <= 1, "no list read needed");
    again.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_unarchived_while_its_host_was_away_is_kept_and_says_so() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Unarchived elsewhere").await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    let room = bridge(&[&a, &b]).await;
    let chat = a.import(&sid).await;
    a.set_archived(&chat, true);
    wait_for("B sees it archived", || b.chat(&chat).is_some_and(|c| c.archived)).await;
    // A, the host, is off. The session is archived on claude.ai, then the
    // person unarchives the chat on B.
    a.core.shutdown().await;
    drop(a);
    world.fake.archive(&sid);
    b.set_archived(&chat, false);
    wait_for("the unarchive at the relay", || {
        room.row("chats", &chat)
            .is_some_and(|r| r.fields.get("archived") == Some(&json!(false)))
    })
    .await;

    // A comes back and reads the list: the chat stays, and says why it
    // can't take messages.
    let a = world.engine("dev-a");
    a.core.workspace.connect_registry_url(&room.url());
    wait_for("A sees it unarchived", || a.chat(&chat).is_some_and(|c| !c.archived)).await;
    a.discover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.chat(&chat).is_some(), "kept");
    assert_eq!(a.state(&chat), Some(archived_on_claude(&sid)));
    a.core.shutdown().await;
    b.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_active_again_after_its_delete_comes_back_with_its_history() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Come back").await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    let _room = bridge(&[&a, &b]).await;
    let chat = a.import(&sid).await;

    // Deleted in Zeron: its session is archived.
    a.core.workspace.delete_chat(&chat).unwrap();
    wait_for("the session archived", || {
        world.fake.session_status(&sid).as_deref() == Some("archived")
    })
    .await;
    wait_for("B knows of the delete", || b.deleted(&chat)).await;
    let archives_before = world.fake.archive_requests().len();

    // Then resumed on the phone, well after the delete (later than the
    // clocks' disagreement a revive allows for).
    world.resume_elsewhere(&sid).await;
    world.later(&sid, chrono::Duration::minutes(5));
    b.discover().await;
    let back = b.chat(&chat).expect("the chat is back");
    assert_eq!(back.device_id, "dev-b", "hosted by the device that brought it back");
    assert_eq!(
        back.provider_session.as_ref().map(|s| s.session_id.as_str()),
        Some(sid.as_str())
    );
    wait_for("its whole history, the resumed turn included", || {
        let entries = b.entries(&chat);
        let user = |text: &str| {
            entries
                .iter()
                .any(|e| e.role == MessageRole::User && text_of(e) == text)
        };
        user(ELSEWHERE_PROMPT)
            && user(RESUMED_PROMPT)
            && entries
                .iter()
                .filter(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU")
                .count()
                == 2
    })
    .await;
    wait_for("A sees it back too", || a.chat(&chat).is_some()).await;
    // Bringing it back archived nothing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(world.fake.archive_requests().len(), archives_before);
    assert_eq!(world.fake.session_status(&sid).as_deref(), Some("active"));
    a.core.shutdown().await;
    b.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_just_after_a_delete_does_not_revive_it() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Within the clocks' slack").await;
    let a = world.engine("dev-a");
    let chat = a.import(&sid).await;
    world.fake.fail_archives(usize::MAX);
    a.core.workspace.delete_chat(&chat).unwrap();
    wait_for("the archive tried", || !world.fake.archive_requests().is_empty()).await;
    // Resumed a moment later: by the provider's clock, maybe before the
    // delete by this device's.
    world.resume_elsewhere(&sid).await;
    a.discover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.chat(&chat).is_none(), "not revived on activity this close");
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unarchiving_a_chat_whose_session_was_archived_says_so_with_a_link() {
    let world = World::new().await;
    let seen = world.started_elsewhere("Seen archived").await;
    let unseen = world.started_elsewhere("Unarchived first").await;
    let a = world.engine("dev-a");
    a.discover().await;
    let seen_chat = a.chat_of(&seen).unwrap().id;
    let unseen_chat = a.chat_of(&unseen).unwrap().id;
    a.set_archived(&seen_chat, true);
    a.set_archived(&unseen_chat, true);

    // Both archived on claude.ai. A list read sees the first while its chat
    // is archived in Zeron; the person unarchives the second before any.
    world.fake.archive(&seen);
    world.fake.archive(&unseen);
    a.set_archived(&unseen_chat, false);
    wait_for("the unarchived chat says its session is archived", || {
        a.state(&unseen_chat) == Some(archived_on_claude(&unseen))
    })
    .await;
    a.discover().await;
    a.set_archived(&seen_chat, false);

    for (chat, sid) in [(&seen_chat, &seen), (&unseen_chat, &unseen)] {
        assert!(a.chat(chat).is_some_and(|c| !c.archived), "kept, unarchived");
        assert_eq!(a.state(chat), Some(archived_on_claude(sid)));
    }
    // Later reads keep them (the person was told), and Zeron archived
    // nothing itself.
    a.discover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.chat(&seen_chat).is_some() && a.chat(&unseen_chat).is_some());
    assert!(world.fake.archive_requests().is_empty());
    let state = a.state(&seen_chat).unwrap();
    assert_eq!(state.label(), "Archived on claude.ai");
    assert_eq!(state.link(), Some(format!("https://claude.ai/code/{seen}").as_str()));
    a.core.shutdown().await;
}
