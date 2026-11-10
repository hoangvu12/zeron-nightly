//! Sessions from claude.ai in the sidebar (ticket #9;
//! docs/design/provider-run-chats.md §6, "Session list").
//!
//! Real engines through their public surface (`DiscoverProviderSessions`, as
//! the sidebar calls it), against the fake Claude Code session server. Two
//! engines share the workspace through the in-memory registry room, as in
//! `workspace_sync.rs`. Assertions are on what is visible: the chat rows,
//! the transcript, what the sidebar is told, and what the fake server was
//! asked.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_doc::{MessagePart, MessageRole, SessionMessageEntry};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::{AccessMode, Chat, HarnessId};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const ELSEWHERE_PROMPT: &str = "Reply with the single word ZULU.";

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

    /// An engine for `device` with Claude cloud sessions on.
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
}

struct Host {
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

impl Host {
    /// The sidebar's call.
    async fn discover(&self, visible: bool) -> Value {
        self.client
            .call(
                methods::DISCOVER_PROVIDER_SESSIONS,
                json!({ "visible": visible }),
            )
            .await
            .expect("DiscoverProviderSessions")
    }

    fn chat(&self, chat: &str) -> Option<Chat> {
        self.core.workspace.chat(chat).ok().flatten()
    }

    /// The chat of provider session `sid`, as this device's registry has it.
    fn chat_of(&self, sid: &str) -> Option<Chat> {
        self.provider_chats().into_iter().find(|c| {
            c.provider_session
                .as_ref()
                .is_some_and(|s| s.session_id == sid)
        })
    }

    fn provider_chats(&self) -> Vec<Chat> {
        self.core
            .workspace
            .read_chats()
            .unwrap()
            .into_iter()
            .filter(|c| c.provider_session.is_some())
            .collect()
    }

    fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
    }
}

/// The sidebar entry for session `sid` in a `DiscoverProviderSessions` reply.
fn listed<'a>(reply: &'a Value, sid: &str) -> Option<&'a Value> {
    reply["sessions"]
        .as_array()?
        .iter()
        .find(|s| s["sessionId"] == sid)
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

fn rfc3339(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

async fn bridge(hosts: &[&Host]) -> zeron_sync::registry::mock_server::MockRegistryServer {
    let server = zeron_sync::registry::mock_server::MockRegistryServer::start().await;
    for host in hosts {
        host.core.workspace.connect_registry_url(&server.url());
    }
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_started_elsewhere_appears_with_its_full_history() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Started on claude.ai").await;
    let a = world.engine("dev-a");

    let reply = a.discover(true).await;
    let shown = listed(&reply, &sid).expect("the session is in the sidebar");
    assert_eq!(shown["title"], "Started on claude.ai");
    let chat_id = shown["chatId"].as_str().unwrap().to_owned();

    let chat = a.chat(&chat_id).expect("a chat row for the session");
    assert_eq!(chat.device_id, "dev-a", "the importing device hosts it");
    assert_eq!(chat.title.as_deref(), Some("Started on claude.ai"));
    assert_eq!(
        chat.cwd.as_deref(),
        Some(format!("claude-cloud://{sid}").as_str())
    );
    assert!(
        chat.space_id.is_none(),
        "a repo-less session lists under Chats"
    );
    let config = chat.config.expect("config");
    assert_eq!(config.harness, HarnessId::ClaudeCodeCloud);
    assert_eq!(
        config.access,
        AccessMode::Supervised,
        "an imported session's prompts are asked, not answered for the person"
    );
    let session = chat.provider_session.expect("providerSession");
    assert_eq!(session.session_id, sid);
    assert_eq!(session.account_uuid, FakeAccount::a().account_uuid);

    wait_for("the history mirrored", || {
        let entries = a.entries(&chat_id);
        entries
            .iter()
            .any(|e| e.role == MessageRole::User && text_of(e) == ELSEWHERE_PROMPT)
            && entries
                .iter()
                .any(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU")
    })
    .await;
    // Importing sent nothing to the session.
    assert!(
        !world
            .fake
            .requests()
            .iter()
            .any(|r| r.method == "POST" && r.path.ends_with("/events"))
    );
    // The chat document carries the session too (survives a revive).
    let doc = a.core.doc_host.open(&chat_id).unwrap();
    assert_eq!(
        doc.doc().provider_run_session().map(|s| s.session_id),
        Some(sid.clone())
    );
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn old_sessions_stay_out_and_a_resumed_old_one_comes_in_across_pages() {
    let world = World::new().await;
    let now = chrono::Utc::now();
    let old = world.started_elsewhere("Old and quiet").await;
    let resumed = world.started_elsewhere("Old but resumed").await;
    let ten_days_ago = rfc3339(now - chrono::Duration::days(10));
    world.fake.set_times(&old, &ten_days_ago, &ten_days_ago);
    world.fake.set_times(
        &resumed,
        &ten_days_ago,
        &rfc3339(now - chrono::Duration::hours(1)),
    );
    // A full first page of newer, archived sessions puts both on page two.
    for n in 0..20 {
        world.fake.seed_session(SeedSession {
            id: format!("cse_archived{n:02}"),
            environment_id: "env_fake0001".into(),
            status: "archived".into(),
            last_event_at: rfc3339(now - chrono::Duration::minutes(n + 1)),
        });
    }
    let a = world.engine("dev-a");

    let reply = a.discover(true).await;
    assert!(listed(&reply, &resumed).is_some(), "{reply:#}");
    assert!(listed(&reply, &old).is_none(), "{reply:#}");
    assert!(a.chat_of(&resumed).is_some());
    assert!(a.chat_of(&old).is_none());
    assert!(
        a.provider_chats().iter().all(|c| !c
            .provider_session
            .as_ref()
            .unwrap()
            .session_id
            .starts_with("cse_archived")),
        "archived sessions get no chat"
    );
    // The list was read from page one, following its cursor, and stopped
    // once it reached sessions older than 7 days.
    let reads = world.fake.list_reads();
    assert_eq!(reads.len(), 2, "{reads:#?}");
    assert!(reads[0].query_param("cursor").is_none());
    assert!(reads[1].query_param("cursor").is_some());
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_discovering_one_session_end_with_one_chat() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Seen by both").await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    // Both import before either has heard of the other's row.
    let (from_a, from_b) = tokio::join!(a.discover(true), b.discover(true));
    assert_eq!(
        listed(&from_a, &sid).unwrap()["chatId"],
        listed(&from_b, &sid).unwrap()["chatId"],
        "the same chat id on every device"
    );
    let _room = bridge(&[&a, &b]).await;
    wait_for("one chat with one host on both devices", || {
        match (a.chat_of(&sid), b.chat_of(&sid)) {
            (Some(on_a), Some(on_b)) => {
                on_a.device_id == on_b.device_id
                    && a.provider_chats().len() == 1
                    && b.provider_chats().len() == 1
            }
            _ => false,
        }
    })
    .await;
    // A later read on either device changes nothing.
    a.core.set_provider_list_interval(Duration::ZERO);
    b.core.set_provider_list_interval(Duration::ZERO);
    let host = a.chat_of(&sid).unwrap().device_id;
    b.discover(true).await;
    a.discover(true).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.provider_chats().len(), 1);
    assert_eq!(b.chat_of(&sid).unwrap().device_id, host);
    a.core.shutdown().await;
    b.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_registry_cannot_move_a_chat_or_overwrite_its_fields() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Listed title").await;
    let a = world.engine("dev-a");
    let b = world.engine("dev-b");
    let room = bridge(&[&a]).await;
    // A has the chat, and the person renamed it.
    let chat_id = listed(&a.discover(true).await, &sid).unwrap()["chatId"]
        .as_str()
        .unwrap()
        .to_owned();
    a.core.workspace.rename_chat(&chat_id, "Mine").unwrap();
    wait_for("A's row at the relay", || {
        room.row("chats", &chat_id).is_some()
    })
    .await;

    // B, just started and not yet synced (its relay isn't answering), knows
    // nothing of the chat. It discovers the same session and writes its row.
    b.core
        .workspace
        .connect_registry_url("ws://127.0.0.1:9/unreachable");
    b.discover(true).await;
    assert_eq!(b.chat(&chat_id).unwrap().device_id, "dev-b");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        b.entries(&chat_id).is_empty(),
        "a stale device doesn't start mirroring the chat"
    );
    b.core.workspace.connect_registry_url(&room.url());

    wait_for("both devices agree with A's writes", || {
        [&a, &b].iter().all(|host| {
            host.chat(&chat_id).is_some_and(|chat| {
                chat.device_id == "dev-a" && chat.title.as_deref() == Some("Mine")
            })
        })
    })
    .await;
    a.core.shutdown().await;
    b.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn title_status_and_activity_follow_the_list_without_touching_the_row() {
    let world = World::new().await;
    let sid = world.started_elsewhere("First title").await;
    let a = world.engine("dev-a");
    a.core.set_provider_list_interval(Duration::ZERO);
    let reply = a.discover(true).await;
    let first = listed(&reply, &sid).unwrap().clone();
    assert_eq!(first["activity"], "idle");
    let chat_id = first["chatId"].as_str().unwrap().to_owned();
    let row = a.chat(&chat_id).unwrap();

    // On claude.ai: renamed, then a turn that waits on a question.
    world.fake.set_title(&sid, "Renamed on claude.ai");
    world.fake.queue_turn(Turn::prompts());
    world.fake.post_as_client(
        &sid,
        json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": "",
            "type": "user",
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": "Ask me something"},
        }),
    );
    let mut now = Value::Null;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while now["activity"] != "waitingForYou" {
        assert!(tokio::time::Instant::now() < deadline, "{now:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        now = listed(&a.discover(true).await, &sid).unwrap().clone();
    }
    assert_eq!(now["title"], "Renamed on claude.ai");
    assert!(
        now["lastEventAt"].as_str().unwrap() > first["lastEventAt"].as_str().unwrap(),
        "the last activity moves on: {first:#} → {now:#}"
    );
    // The row keeps what it had: discovery wrote none of it.
    let after = a.chat(&chat_id).unwrap();
    assert_eq!(after.title, row.title);
    assert_eq!(after.title.as_deref(), Some("First title"));
    assert_eq!(after.last_seen_at, row.last_seen_at);
    assert_eq!(after.device_id, row.device_id);
    a.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_is_read_only_while_visible_and_at_most_once_a_minute() {
    let world = World::new().await;
    let sid = world.started_elsewhere("Throttled").await;
    let a = world.engine("dev-a");

    // Hidden: nothing is read, and the engine never reads on its own.
    let hidden = a.discover(false).await;
    assert!(hidden["sessions"].as_array().is_none_or(Vec::is_empty));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(world.fake.list_reads().is_empty());

    // Visible: one read, however often (and however concurrently) asked
    // within the minute; later calls answer from it.
    let (first, second) = tokio::join!(a.discover(true), a.discover(true));
    let third = a.discover(true).await;
    assert_eq!(world.fake.list_reads().len(), 1);
    assert!(listed(&first, &sid).is_some());
    assert_eq!(first, second);
    assert_eq!(first, third);
    // Hidden again still answers from the last read, reading nothing.
    assert_eq!(a.discover(false).await, first);
    assert_eq!(world.fake.list_reads().len(), 1);
    a.core.shutdown().await;
}
