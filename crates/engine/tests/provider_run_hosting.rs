//! Hosting provider-run chats across devices (ticket #12,
//! docs/design/provider-run-chats.md §4): takeover on use, the claim, and
//! demotion, with two or three real engines.
//!
//! The engines share a registry room (the in-memory registry server) and
//! their chats' documents (an in-memory bridge standing in for the chat
//! rooms), and talk to one fake Claude Code session server, each with its
//! own Claude login (so the server tells their requests apart by token). A
//! fake relay says which devices are online. Only externally visible results
//! are asserted: which device hosts the chat, what the fake server received,
//! the transcript, and the states the chat shows.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use zeron_engine::{DocHost, EngineCore, HarnessRegistry, HostPresence, WorkspaceHost};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::provider_cloud::{HostUnavailableReason, ProviderChatState};
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;
use zeron_sync::registry::mock_server::MockRegistryServer;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const THIRD: &str = "3c9d7e2f-1a4b-4c8d-9e0f-a1b2c3d4e5f6";
const FIRST_PROMPT: &str = "Run `uname -a` and `cat /tmp/zeron-init-marker 2>&1`, then reply DONE";

// ---- the fake relay ----

/// Which devices the relay says are online (all, until a test says not).
#[derive(Default)]
struct FakeRelay {
    offline: Mutex<HashSet<String>>,
}

impl FakeRelay {
    fn set_offline(&self, device: &str, offline: bool) {
        let mut set = self.offline.lock().unwrap();
        if offline {
            set.insert(device.to_owned());
        } else {
            set.remove(device);
        }
    }
}

impl HostPresence for FakeRelay {
    fn online<'a>(&'a self, device_id: &'a str) -> BoxFuture<'a, bool> {
        let online = !self.offline.lock().unwrap().contains(device_id);
        Box::pin(async move { online })
    }
}

// ---- the chat-room bridge ----

/// Exchanges every provider-run chat's document updates between devices
/// that are both connected, like the chat rooms do.
type Bridged = Vec<(String, DocHost, WorkspaceHost)>;

struct DocBridge {
    cut: Arc<Mutex<HashSet<String>>>,
    devices: Arc<Mutex<Bridged>>,
    task: tokio::task::JoinHandle<()>,
}

impl DocBridge {
    fn start(devices: Bridged) -> Self {
        let cut: Arc<Mutex<HashSet<String>>> = Arc::default();
        let devices = Arc::new(Mutex::new(devices));
        let (off, all) = (cut.clone(), devices.clone());
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let devices = all.lock().unwrap().clone();
                let mut chats: HashSet<String> = HashSet::new();
                for (_, _, workspace) in &devices {
                    for chat in workspace.read_chats().unwrap_or_default() {
                        if chat.provider_session.is_some() {
                            chats.insert(chat.id);
                        }
                    }
                }
                let cut = off.lock().unwrap().clone();
                for chat in &chats {
                    for (i, (a_id, a, _)) in devices.iter().enumerate() {
                        for (b_id, b, _) in devices.iter().skip(i + 1) {
                            if cut.contains(a_id) || cut.contains(b_id) {
                                continue;
                            }
                            let (Ok(a), Ok(b)) = (a.open(chat), b.open(chat)) else {
                                continue;
                            };
                            exchange(a.doc().doc(), b.doc().doc());
                        }
                    }
                }
            }
        });
        Self { cut, devices, task }
    }

    /// `device` restarted: bridge its new engine instead.
    fn replace(&self, device: &str, doc_host: DocHost, workspace: WorkspaceHost) {
        for entry in self.devices.lock().unwrap().iter_mut() {
            if entry.0 == device {
                *entry = (device.to_owned(), doc_host.clone(), workspace.clone());
            }
        }
    }

    fn set_connected(&self, device: &str, connected: bool) {
        let mut cut = self.cut.lock().unwrap();
        if connected {
            cut.remove(device);
        } else {
            cut.insert(device.to_owned());
        }
    }
}

impl Drop for DocBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn exchange(a: &loro::LoroDoc, b: &loro::LoroDoc) {
    if a.oplog_vv() == b.oplog_vv() {
        return;
    }
    if let Ok(bytes) = a.export(loro::ExportMode::updates(&b.oplog_vv())) {
        let _ = b.import(&bytes);
    }
    if let Ok(bytes) = b.export(loro::ExportMode::updates(&a.oplog_vv())) {
        let _ = a.import(&bytes);
    }
}

// ---- devices ----

struct Device {
    id: String,
    kind: Kind,
    data: std::path::PathBuf,
    login: ClaudeLogin,
    core: EngineCore,
    client: zeron_rpc::RpcClient,
    stub: StubClaude,
    _tmp: tempfile::TempDir,
}

/// A device's engine on its data folder, with its harnesses and the fake
/// relay.
fn engine(
    fake: &FakeSessionServer,
    relay: &Arc<FakeRelay>,
    data: &std::path::Path,
    login: &ClaudeLogin,
    stub: &StubClaude,
    kind: Kind,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    if let Kind::Cloud(_) = kind {
        registry.register(Arc::new(
            ClaudeCloudHarness::new(ClaudeCloudConfig {
                base_url: fake.base_url().to_owned(),
                login: ClaudeConfigDir::at(login.dir()),
                claude_executable: Some(stub.path().to_owned()),
            })
            .expect("harness builds"),
        ));
    }
    let core = EngineCore::assemble(data, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
        .expect("engine core assembles");
    core.provider_hosting.set_presence(relay.clone());
    core
}

/// What a device can do with provider-run chats.
#[derive(Clone, Copy)]
enum Kind {
    /// Claude cloud sessions on, signed in as this account.
    Cloud(fn() -> FakeAccount),
    /// An older build (or the feature off): no `claude-code-cloud`.
    Old,
}

struct Fleet {
    fake: FakeSessionServer,
    room: MockRegistryServer,
    relay: Arc<FakeRelay>,
    bridge: DocBridge,
    devices: Vec<Device>,
}

async fn fleet(devices: &[(&str, &str, Kind)]) -> Fleet {
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    let room = MockRegistryServer::start().await;
    let relay = Arc::new(FakeRelay::default());
    let mut out = Vec::new();
    for (id, name, kind) in devices {
        let tmp = tempfile::tempdir().unwrap();
        let account = match kind {
            Kind::Cloud(account) => account(),
            Kind::Old => FakeAccount::a(),
        };
        let login = ClaudeLogin::create(tmp.path().join("claude-login"), &account, &token(id));
        let stub = StubClaude::install(tmp.path().join("bin"), &account);
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("device-id"), id).unwrap();
        if let Kind::Cloud(_) = kind {
            // "Claude cloud sessions" on, so the engine keeps the harness.
            use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
            ClaudeCloudSessions::open(&data)
                .save(ClaudeCloudSettings {
                    enabled: true,
                    ..ClaudeCloudSettings::default()
                })
                .expect("setting saves");
        }
        let core = engine(&fake, &relay, &data, &login, &stub, *kind);
        core.workspace.connect_registry_url(&room.url());
        let client = zeron_rpc::memory_client(core.rpc_service());
        client
            .call(
                methods::MUTATE,
                serde_json::json!({"op": "renameDevice", "deviceId": id, "name": name}),
            )
            .await
            .expect("rename device");
        out.push(Device {
            id: (*id).to_owned(),
            kind: *kind,
            data,
            login,
            core,
            client,
            stub,
            _tmp: tmp,
        });
    }
    let bridge = DocBridge::start(
        out.iter()
            .map(|d| {
                (
                    d.id.clone(),
                    d.core.doc_host.clone(),
                    d.core.workspace.clone(),
                )
            })
            .collect(),
    );
    let fleet = Fleet {
        fake,
        room,
        relay,
        bridge,
        devices: out,
    };
    // Every device knows every other (and what it can do).
    for device in &fleet.devices {
        wait_for("every device row", || {
            device
                .core
                .workspace
                .read_devices()
                .unwrap_or_default()
                .len()
                == fleet.devices.len()
        })
        .await;
    }
    fleet
}

fn token(device: &str) -> String {
    format!("token-{device}")
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

impl Fleet {
    fn device(&self, id: &str) -> &Device {
        self.devices.iter().find(|d| d.id == id).expect("device")
    }

    /// The person loses `id` (asleep, no network): the relay says offline,
    /// and its registry and chat rooms are cut.
    fn go_offline(&self, id: &str) {
        self.relay.set_offline(id, true);
        self.bridge.set_connected(id, false);
        self.device(id).core.workspace.disconnect_edge();
    }

    /// `id` is back: online, its rooms rejoined.
    fn come_back(&self, id: &str) {
        self.relay.set_offline(id, false);
        self.bridge.set_connected(id, true);
        self.device(id)
            .core
            .workspace
            .connect_registry_url(&self.room.url());
    }

    /// Quit `id`'s engine and start it again on the same data, joining the
    /// registry room at `registry_url` (its chat rooms stay as they were).
    async fn restart(&mut self, id: &str, registry_url: &str) {
        let at = self.devices.iter().position(|d| d.id == id).unwrap();
        let Device {
            id,
            kind,
            data,
            login,
            core,
            client,
            stub,
            _tmp,
        } = self.devices.swap_remove(at);
        core.shutdown().await;
        drop((core, client));
        let core = engine(&self.fake, &self.relay, &data, &login, &stub, kind);
        core.workspace.connect_registry_url(registry_url);
        let client = zeron_rpc::memory_client(core.rpc_service());
        self.bridge
            .replace(&id, core.doc_host.clone(), core.workspace.clone());
        self.devices.push(Device {
            id,
            kind,
            data,
            login,
            core,
            client,
            stub,
            _tmp,
        });
    }

    /// Every event payload sent to the session with `device`'s token.
    fn posted_by(&self, device: &str) -> Vec<serde_json::Value> {
        let bearer = format!("Bearer {}", token(device));
        self.fake
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/events"))
            .filter(|r| r.header("authorization") == Some(bearer.as_str()))
            .flat_map(|r| r.body["events"].as_array().cloned().unwrap_or_default())
            .map(|e| e["payload"].clone())
            .collect()
    }

    /// How many requests of any kind `device` made.
    fn requests_by(&self, device: &str) -> usize {
        let bearer = format!("Bearer {}", token(device));
        self.fake
            .requests()
            .into_iter()
            .filter(|r| r.header("authorization") == Some(bearer.as_str()))
            .count()
    }

    /// The person messages under `id` stored in the session, by uuid.
    fn stored_messages(&self, session: &str, uuid: &str) -> usize {
        self.fake
            .stored_events(session)
            .iter()
            .filter(|e| e.payload["type"] == "user" && e.payload["uuid"] == uuid)
            .count()
    }

    async fn shutdown(self) {
        for device in &self.devices {
            device.core.shutdown().await;
        }
    }
}

impl Device {
    /// "Anthropic cloud", no project, from this device's composer.
    async fn create(&self) -> (String, String) {
        let created = self
            .client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": self.id,
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
        self.entries(chat)
            .iter()
            .all(|e| e.status != Some(MessageStatus::Streaming))
    }

    /// The chat's host as this device's registry has it.
    fn host_of(&self, chat: &str) -> Option<String> {
        self.core
            .workspace
            .chat(chat)
            .ok()
            .flatten()
            .map(|c| c.device_id)
    }

    fn hosts(&self, chat: &str) -> bool {
        self.host_of(chat).as_deref() == Some(self.id.as_str())
    }

    /// The person sends a message from this device's composer.
    fn send(&self, chat: &str, cwd: &str, message_id: &str, prompt: &str) {
        let request = RunRequest {
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
        };
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Run {
                    request,
                    message_id: message_id.into(),
                },
            )
            .expect("queue message");
    }

    /// How many uses of provider-run chats this device has decided about.
    fn decisions(&self) -> u64 {
        *self.core.provider_hosting.watch_decisions().borrow()
    }

    /// Wait until this device has decided about `n` more uses than `from`.
    async fn decided(&self, from: u64, n: u64) {
        let rx = self.core.provider_hosting.watch_decisions();
        wait_for("the use decided", || *rx.borrow() >= from + n).await;
    }

    /// The person opens the chat on this device.
    async fn open(&self, chat: &str) {
        self.client
            .call(methods::FOCUS_CHAT, serde_json::json!({"chatId": chat}))
            .await
            .expect("focus chat");
    }

    fn notes(&self, chat: &str) -> Vec<String> {
        self.entries(chat)
            .iter()
            .filter(|e| e.role == MessageRole::System)
            .map(text_of)
            .filter(|t| t.starts_with("Continued here"))
            .collect()
    }

    fn state(&self, chat: &str) -> Option<ProviderChatState> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().provider_chat_state())
    }
}

/// `owner` creates a chat and its first turn is mirrored there and seen by
/// every other device.
async fn chat_on(fleet: &Fleet, owner: &str) -> (String, String, String) {
    fleet.fake.queue_turn(Turn::bash_tool());
    let device = fleet.device(owner);
    let (chat, cwd) = device.create().await;
    let session = fleet.fake.created_sessions().last().unwrap().clone();
    for other in &fleet.devices {
        wait_for("the first turn everywhere", || {
            other.replies(&chat) == 1 && other.host_of(&chat).as_deref() == Some(owner)
        })
        .await;
    }
    wait_for("the first turn settled", || device.settled(&chat)).await;
    (chat, cwd, session)
}

const PC: &str = "dev-pc";
const LAPTOP: &str = "dev-laptop";
const VPS: &str = "dev-vps";

fn cloud_a() -> Kind {
    Kind::Cloud(FakeAccount::a)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_host_is_taken_over_on_use_and_the_chat_continues() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let laptop = fleet.device(LAPTOP);

    fleet.go_offline(PC);
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");

    // The laptop claims the chat, and continues it once the room says so.
    wait_for("the laptop hosting and replying", || {
        laptop.hosts(&chat) && laptop.replies(&chat) == 2 && laptop.settled(&chat)
    })
    .await;
    assert_eq!(fleet.stored_messages(&session, SECOND), 1);
    assert!(
        fleet
            .posted_by(LAPTOP)
            .iter()
            .any(|p| p["uuid"] == SECOND && p["type"] == "user")
    );
    assert_eq!(laptop.notes(&chat), vec!["Continued here, PC was offline."]);
    // The rest of the transcript caught up from where the PC got to: the
    // first turn shows once.
    let entries = laptop.entries(&chat);
    let first: Vec<_> = entries.iter().filter(|e| e.id == FIRST).collect();
    assert_eq!(first.len(), 1, "{entries:#?}");
    assert_eq!(
        fleet.room.row("chats", &chat).unwrap().fields["deviceId"],
        LAPTOP
    );
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_eligible_host_online_keeps_the_chat_and_nothing_moves_it_in_the_background() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));

    // Used on the laptop while the PC is online: the PC runs it.
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.open(&chat).await;
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the reply, everywhere", || {
        laptop.replies(&chat) == 2 && pc.replies(&chat) == 2 && laptop.settled(&chat)
    })
    .await;
    assert_eq!(fleet.stored_messages(&session, SECOND), 1);
    assert!(fleet.posted_by(PC).iter().any(|p| p["uuid"] == SECOND));
    assert_eq!(fleet.requests_by(LAPTOP), 0, "the laptop never acted");
    assert_eq!(pc.host_of(&chat).as_deref(), Some(PC));
    assert_eq!(laptop.host_of(&chat).as_deref(), Some(PC));
    assert!(laptop.notes(&chat).is_empty());

    // The PC goes away, but nobody uses the chat: it stays where it is. (A
    // rename from the laptop makes every device's registry move on once, so
    // the hosts were looked at again.)
    fleet.go_offline(PC);
    laptop
        .core
        .workspace
        .rename_chat(&chat, "Renamed while the PC is away")
        .unwrap();
    wait_for("the rename in the room", || {
        fleet
            .room
            .row("chats", &chat)
            .is_some_and(|r| r.fields["title"] == "Renamed while the PC is away")
    })
    .await;
    assert_eq!(laptop.host_of(&chat).as_deref(), Some(PC));
    assert_eq!(
        fleet.room.row("chats", &chat).unwrap().fields["deviceId"],
        PC
    );
    assert_eq!(fleet.requests_by(LAPTOP), 0);
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_claiming_at_once_converge_and_the_session_hears_each_message_once() {
    let fleet = fleet(&[
        (PC, "PC", cloud_a()),
        (LAPTOP, "Laptop", cloud_a()),
        (VPS, "VPS", cloud_a()),
    ])
    .await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (laptop, vps) = (fleet.device(LAPTOP), fleet.device(VPS));

    fleet.go_offline(PC);
    fleet.fake.queue_turn(Turn::text_zulu());
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    vps.send(&chat, &cwd, THIRD, "Again: the single word ZULU.");

    // One host, the same on every device and in the room.
    wait_for("one host everywhere", || {
        let host = laptop.host_of(&chat);
        host.is_some()
            && host.as_deref() != Some(PC)
            && vps.host_of(&chat) == host
            && fleet
                .room
                .row("chats", &chat)
                .is_some_and(|r| r.fields["deviceId"].as_str() == host.as_deref())
    })
    .await;
    let winner = laptop.host_of(&chat).unwrap();
    let (host, other) = if winner == LAPTOP {
        (laptop, vps)
    } else {
        (vps, laptop)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    wait_for("both messages sent", || {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stored {} {}; host {} run {} posted {}; other {} run {} posted {}; hosts {:?} {:?}; commands {:?}",
            fleet.stored_messages(&session, SECOND),
            fleet.stored_messages(&session, THIRD),
            host.id,
            host.core.sessions.has_run(&chat),
            fleet.posted_by(&host.id).len(),
            other.id,
            other.core.sessions.has_run(&chat),
            fleet.posted_by(&other.id).len(),
            host.host_of(&chat),
            other.host_of(&chat),
            host.core
                .doc_host
                .open(&chat)
                .unwrap()
                .doc()
                .read_commands()
                .unwrap()
                .iter()
                .map(|c| (c.issued_by.clone(), c.status, c.resolution.clone()))
                .collect::<Vec<_>>()
        );
        fleet.stored_messages(&session, SECOND) == 1
            && fleet.stored_messages(&session, THIRD) == 1
            && host.entries(&chat).iter().any(|e| e.id == SECOND)
            && host.entries(&chat).iter().any(|e| e.id == THIRD)
    })
    .await;
    // Each message reached the session exactly once.
    assert_eq!(fleet.stored_messages(&session, SECOND), 1);
    assert_eq!(fleet.stored_messages(&session, THIRD), 1);
    // The device that lost runs nothing.
    wait_for("the other device idle", || {
        !other.core.sessions.has_run(&chat)
    })
    .await;
    // The claimant that lost never started: one note, the winner's.
    wait_for("the note", || !host.notes(&chat).is_empty()).await;
    assert_eq!(host.notes(&chat).len(), 1, "{:?}", host.notes(&chat));
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_demoted_host_that_reconnects_stops_at_once_and_keeps_away() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));
    assert!(
        pc.core.sessions.has_run(&chat),
        "the PC mirrors the session"
    );

    fleet.go_offline(PC);
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the laptop hosting and replying", || {
        laptop.hosts(&chat) && laptop.replies(&chat) == 2 && laptop.settled(&chat)
    })
    .await;

    // The PC comes back: it learns it isn't host, and stops.
    fleet.come_back(PC);
    wait_for("the PC demoted", || {
        pc.host_of(&chat).as_deref() == Some(LAPTOP) && !pc.core.sessions.has_run(&chat)
    })
    .await;
    let pc_posts = fleet.posted_by(PC).len();

    // Used on the PC now: the laptop is online and eligible, so it keeps
    // the chat, and the PC sends nothing.
    fleet.fake.queue_turn(Turn::text_zulu());
    let decided = pc.decisions();
    pc.open(&chat).await;
    pc.send(&chat, &cwd, THIRD, "Again: the single word ZULU.");
    wait_for("the reply on the laptop", || {
        laptop.replies(&chat) >= 3 && laptop.settled(&chat)
    })
    .await;
    // Both uses on the PC were decided: neither moved the chat.
    pc.decided(decided, 2).await;
    assert_eq!(fleet.stored_messages(&session, THIRD), 1);
    assert!(fleet.posted_by(LAPTOP).iter().any(|p| p["uuid"] == THIRD));
    assert_eq!(fleet.posted_by(PC).len(), pc_posts);
    assert!(!pc.core.sessions.has_run(&chat));
    assert_eq!(
        fleet.room.row("chats", &chat).unwrap().fields["deviceId"],
        LAPTOP
    );
    assert_eq!(pc.host_of(&chat).as_deref(), Some(LAPTOP));
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_the_capability_is_taken_over_on_use() {
    let fleet = fleet(&[(PC, "Old PC", Kind::Old), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, LAPTOP).await;
    let (old, laptop) = (fleet.device(PC), fleet.device(LAPTOP));

    // An older build ends up host (written straight to the row, as an old
    // tool or an old claim-on-first-command could).
    assert!(
        old.core
            .workspace
            .claim_chat_host(&chat, chrono::Utc::now())
            .expect("move the chat")
    );
    wait_for("the old PC hosting", || {
        laptop.host_of(&chat).as_deref() == Some(PC) && !laptop.core.sessions.has_run(&chat)
    })
    .await;

    // Opened on the laptop: the old PC can't run it, so the laptop takes it.
    laptop.open(&chat).await;
    wait_for("the laptop hosting", || laptop.hosts(&chat)).await;
    wait_for("the note", || !laptop.notes(&chat).is_empty()).await;
    assert_eq!(
        laptop.notes(&chat),
        vec!["Continued here, Old PC can't run Claude Code cloud chats."]
    );
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the reply", || {
        laptop.replies(&chat) == 2 && laptop.settled(&chat)
    })
    .await;
    assert_eq!(fleet.stored_messages(&session, SECOND), 1);
    assert!(old.core.sessions.session_status(&chat).is_none());
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_signed_into_another_account_says_so_and_hands_the_chat_over() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));

    // The PC's Claude login is switched to another account; the next
    // message there finds it can't host the chat, and says so on the row.
    pc.login.set_account(&FakeAccount::b());
    pc.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("hostUnavailable on the laptop", || {
        laptop
            .core
            .workspace
            .chat_host_unavailable(&chat)
            .is_some_and(|u| u.device_id == PC && u.reason == HostUnavailableReason::Account)
    })
    .await;
    assert_eq!(fleet.stored_messages(&session, SECOND), 0);

    // Opened on the laptop: it takes the chat over and clears the flag, and
    // the message the PC couldn't send goes now (ticket #13).
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.open(&chat).await;
    wait_for("the laptop hosting", || laptop.hosts(&chat)).await;
    wait_for("the flag cleared", || {
        laptop.core.workspace.chat_host_unavailable(&chat).is_none()
            && pc.core.workspace.chat_host_unavailable(&chat).is_none()
    })
    .await;
    wait_for("the note", || !laptop.notes(&chat).is_empty()).await;
    assert_eq!(
        laptop.notes(&chat),
        vec!["Continued here, PC couldn't host this chat."]
    );
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.send(&chat, &cwd, THIRD, "Reply with the single word ZULU.");
    wait_for("the reply", || {
        fleet.stored_messages(&session, THIRD) == 1
            && !fleet.fake.turn_playing(&session)
            && laptop.settled(&chat)
            && laptop
                .entries(&chat)
                .last()
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
    })
    .await;
    assert_eq!(fleet.stored_messages(&session, THIRD), 1);
    assert_eq!(fleet.stored_messages(&session, SECOND), 1);
    assert!(fleet.posted_by(PC).is_empty(), "nothing went as account B");
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_no_device_able_to_host_the_chat_shows_sign_in_as() {
    let fleet = fleet(&[
        (PC, "PC", cloud_a()),
        (LAPTOP, "Laptop", Kind::Cloud(FakeAccount::b)),
    ])
    .await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));

    // The only device with the chat's login is off; the laptop has another
    // account's. The PC could still run it: the chat waits for it, and says
    // nothing about signing in.
    fleet.go_offline(PC);
    let decided = laptop.decisions();
    laptop.open(&chat).await;
    laptop.decided(decided, 1).await;
    assert_eq!(laptop.state(&chat), None);
    assert_eq!(laptop.host_of(&chat).as_deref(), Some(PC));

    // Back, but signed into another account: it says it can't host the
    // chat. Now no device can.
    fleet.come_back(PC);
    pc.login.set_account(&FakeAccount::b());
    pc.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("hostUnavailable on the laptop", || {
        laptop
            .core
            .workspace
            .chat_host_unavailable(&chat)
            .is_some_and(|u| u.device_id == PC)
    })
    .await;
    let decided = laptop.decisions();
    laptop.open(&chat).await;
    laptop.decided(decided, 1).await;
    wait_for("Sign in as", || {
        laptop.state(&chat)
            == Some(ProviderChatState::SignInAs {
                email: FakeAccount::a().email,
            })
    })
    .await;
    assert_eq!(laptop.host_of(&chat).as_deref(), Some(PC));
    assert_eq!(fleet.requests_by(LAPTOP), 0);
    assert_eq!(fleet.stored_messages(&session, SECOND), 0);
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_demoted_host_restarting_with_commands_waiting_sends_nothing() {
    let mut fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;

    // The PC goes away and the laptop takes the chat over.
    fleet.go_offline(PC);
    fleet.fake.queue_turn(Turn::text_zulu());
    fleet
        .device(LAPTOP)
        .send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the laptop hosting and replying", || {
        let laptop = fleet.device(LAPTOP);
        laptop.hosts(&chat) && laptop.replies(&chat) == 2 && laptop.settled(&chat)
    })
    .await;
    let pc_posts = fleet.posted_by(PC).len();

    // The PC restarts, its rows still naming it host, and its registry room
    // not answering yet. A message typed there waits.
    fleet.restart(PC, "ws://127.0.0.1:9").await;
    fleet.fake.queue_turn(Turn::text_zulu());
    let pc = fleet.device(PC);
    assert_eq!(pc.host_of(&chat).as_deref(), Some(PC), "stale rows");
    let decided = pc.decisions();
    pc.send(&chat, &cwd, THIRD, "Again: the single word ZULU.");
    pc.decided(decided, 1).await;
    assert!(!pc.core.sessions.has_run(&chat));

    // The room answers: the laptop hosts it, and runs the waiting message.
    fleet.come_back(PC);
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));
    wait_for("the PC's message answered on the laptop", || {
        fleet.stored_messages(&session, THIRD) == 1 && laptop.settled(&chat)
    })
    .await;
    assert!(fleet.posted_by(LAPTOP).iter().any(|p| p["uuid"] == THIRD));
    assert_eq!(fleet.posted_by(PC).len(), pc_posts, "the PC sent nothing");
    assert!(!pc.core.sessions.has_run(&chat));
    assert_eq!(pc.host_of(&chat).as_deref(), Some(LAPTOP));
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_refuses_forks_and_side_chats_of_a_provider_run_chat() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, _cwd, _session) = chat_on(&fleet, PC).await;
    let pc = fleet.device(PC);

    // As an older client asks: a fork of the chat, then a side chat of it.
    let fork = pc
        .client
        .call(
            methods::FORK_SIDE_CHAT,
            serde_json::json!({"chatId": "fork-1", "sourceChatId": chat}),
        )
        .await;
    assert!(fork.is_err(), "{fork:?}");
    let side = pc
        .client
        .call(
            methods::MUTATE,
            serde_json::json!({"op": "createChat", "chatId": "side-1", "deviceId": PC,
                "parentChatId": chat, "childKind": "side"}),
        )
        .await;
    assert!(side.is_err(), "{side:?}");
    for id in ["fork-1", "side-1"] {
        assert!(pc.core.workspace.chat(id).unwrap().is_none(), "{id}");
    }
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_run_chat_moves_only_by_a_claim() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, _cwd, _session) = chat_on(&fleet, PC).await;
    let pc = fleet.device(PC);

    // The SetChatHost tool (any client) can't move it.
    let moved = pc
        .client
        .call(
            methods::MUTATE,
            serde_json::json!({"op": "setChatHost", "chatId": chat, "deviceId": LAPTOP}),
        )
        .await;
    assert!(moved.is_err(), "{moved:?}");
    assert_eq!(pc.host_of(&chat).as_deref(), Some(PC));
    assert_eq!(
        fleet.room.row("chats", &chat).unwrap().fields["deviceId"],
        PC
    );
    fleet.shutdown().await;
}

/// Two messages drained together right after a takeover reach the session
/// once each, and the replies show. (The second can be stored in the middle
/// of the first one's reply; the reply once got stamped with the message's
/// uuid and swept as its duplicate. See `provider_run_chats.rs`,
/// `a_message_stored_mid_reply_leaves_the_reply_in_the_transcript`.)
#[tokio::test(flavor = "multi_thread")]
async fn two_messages_drained_together_after_a_takeover_are_answered() {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    fleet.go_offline(PC);
    let laptop = fleet.device(LAPTOP);
    fleet.fake.queue_turn(Turn::text_zulu());
    fleet.fake.queue_turn(Turn::text_zulu());
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    laptop.send(&chat, &cwd, THIRD, "Again: the single word ZULU.");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    wait_for("answered", || {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            laptop
                .entries(&chat)
                .iter()
                .map(|e| (e.role, text_of(e).chars().take(12).collect::<String>()))
                .collect::<Vec<_>>()
        );
        fleet.stored_messages(&session, THIRD) == 1
            && !fleet.fake.turn_playing(&session)
            && laptop.settled(&chat)
            && laptop
                .entries(&chat)
                .iter()
                .rev()
                .find(|e| e.role != MessageRole::System)
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
    })
    .await;
    fleet.shutdown().await;
}

/// The host dies while its send is on the way (ticket #13), before the
/// session stored it or after: the device taking the chat over finds the
/// message's delivery uncertain, reads the session's history for it, and
/// sends it under its uuid only if it isn't there. The session has it once,
/// and the reply shows on the new host.
async fn host_dies_mid_send(stored_first: bool) {
    let fleet = fleet(&[(PC, "PC", cloud_a()), (LAPTOP, "Laptop", cloud_a())]).await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));
    let uuid = zeron_proto::provider_cloud::event_uuid(SECOND);

    fleet.fake.queue_turn(Turn::text_zulu());
    let _held = if stored_first {
        fleet.fake.hold_next_ack()
    } else {
        fleet.fake.hold_next_post()
    };
    pc.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the send on its way, and its entry everywhere", || {
        fleet.posted_by(PC).iter().any(|p| p["uuid"] == uuid)
            && laptop.entries(&chat).iter().any(|e| e.id == SECOND)
    })
    .await;
    fleet.go_offline(PC);
    pc.core.shutdown().await;

    laptop.open(&chat).await;
    wait_for("the reply on the laptop", || {
        laptop.hosts(&chat)
            && laptop.settled(&chat)
            && laptop
                .entries(&chat)
                .iter()
                .rev()
                .find(|e| e.role != MessageRole::System)
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
    })
    .await;
    assert_eq!(fleet.stored_messages(&session, &uuid), 1);
    let entries = laptop.entries(&chat);
    assert_eq!(entries.iter().filter(|e| e.id == SECOND).count(), 1);
    fleet.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_dying_before_its_send_was_stored_is_covered_by_the_new_host() {
    host_dies_mid_send(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_dying_before_its_send_was_acknowledged_is_covered_by_the_new_host() {
    host_dies_mid_send(true).await;
}

/// Ticket #13: a message sent while the chat's host is offline, from a
/// device that can't take the chat over (it has another account's login),
/// shows "Queued, waiting for an eligible device", reaches nobody, and is
/// delivered once the host is back.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_no_eligible_device_can_take_is_queued_until_the_host_returns() {
    let fleet = fleet(&[
        (PC, "PC", cloud_a()),
        (LAPTOP, "Laptop", Kind::Cloud(FakeAccount::b)),
    ])
    .await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));

    fleet.go_offline(PC);
    laptop.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("Queued", || {
        laptop.state(&chat) == Some(ProviderChatState::Queued)
    })
    .await;
    assert_eq!(
        ProviderChatState::Queued.label(),
        "Queued, waiting for an eligible device"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fleet.stored_messages(&session, SECOND), 0);
    assert_eq!(fleet.requests_by(LAPTOP), 0);
    assert_eq!(laptop.host_of(&chat).as_deref(), Some(PC));

    fleet.fake.queue_turn(Turn::text_zulu());
    fleet.come_back(PC);
    wait_for("the reply, and the queue cleared everywhere", || {
        fleet.stored_messages(&session, SECOND) == 1
            && pc.replies(&chat) == 2
            && pc.settled(&chat)
            && pc.state(&chat).is_none()
            && laptop.state(&chat).is_none()
    })
    .await;
    assert_eq!(fleet.requests_by(LAPTOP), 0);
    fleet.shutdown().await;
}

/// Ticket #13: a queued message that outlives its expiry is a failed send:
/// the host, back, expires it unsent, and the chat no longer says queued.
#[tokio::test(flavor = "multi_thread")]
async fn a_queued_message_that_expires_fails_and_is_never_sent() {
    let fleet = fleet(&[
        (PC, "PC", cloud_a()),
        (LAPTOP, "Laptop", Kind::Cloud(FakeAccount::b)),
    ])
    .await;
    let (chat, cwd, session) = chat_on(&fleet, PC).await;
    let (pc, laptop) = (fleet.device(PC), fleet.device(LAPTOP));

    fleet.go_offline(PC);
    let now = chrono::Utc::now().timestamp_millis();
    let command = zeron_doc::SessionCommandEntry {
        id: "queued-command".into(),
        payload: SessionCommandPayload::Run {
            request: RunRequest {
                mcp: None,
                prompt: "Reply with the single word ZULU.".into(),
                harness: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: cwd.clone(),
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
            message_id: SECOND.into(),
        },
        issued_by: LAPTOP.into(),
        issued_at: now,
        based_on: None,
        expires_at: Some(now + 1_000),
        status: zeron_doc::SessionCommandStatus::Pending,
        resolution: None,
    };
    laptop
        .core
        .doc_host
        .open(&chat)
        .unwrap()
        .doc()
        .queue_command(&command)
        .unwrap();
    laptop.open(&chat).await;
    wait_for("Queued", || {
        laptop.state(&chat) == Some(ProviderChatState::Queued)
    })
    .await;

    tokio::time::sleep(Duration::from_millis(1_500)).await;
    fleet.come_back(PC);
    let status = |device: &Device| {
        device
            .core
            .doc_host
            .open(&chat)
            .ok()
            .and_then(|h| h.doc().read_commands().ok())
            .and_then(|commands| {
                commands
                    .into_iter()
                    .find(|c| c.id == "queued-command")
                    .map(|c| c.status)
            })
    };
    wait_for("expired, everywhere, and no longer queued", || {
        status(pc) == Some(zeron_doc::SessionCommandStatus::Expired)
            && status(laptop) == Some(zeron_doc::SessionCommandStatus::Expired)
            && laptop.state(&chat).is_none()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fleet.stored_messages(&session, SECOND), 0);
    assert!(!laptop.entries(&chat).iter().any(|e| e.id == SECOND));
    fleet.shutdown().await;
}
