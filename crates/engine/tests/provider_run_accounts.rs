//! More Claude accounts (ticket #14, docs/design/provider-run-chats.md §3):
//! "Add account" signs a new folder in through the official CLI and that
//! account's sessions show and run on the device; a switched device login
//! makes the old account's chats say this device can't host them, without
//! turning the feature off, and Zeron offers to add the old account back.
//!
//! A real engine through its public surface, against the fake Claude Code
//! session server (which keeps each token's account's sessions apart) and a
//! stub `claude` whose `auth login` plays the person signing in. Assertions
//! are on what is visible: the settings status, the sidebar's list, the chat
//! rows and transcripts, and what the fake server received.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const THIRD: &str = "3c9d7e2f-1a4b-4c8d-9e0f-a1b2c3d4e5f6";
const ZULU: &str = "Reply with the single word ZULU.";

/// Each fake account's own token, and the account the server files it under.
const TOKEN_A: &str = "token-a";
const TOKEN_B: &str = "token-b";

struct Device {
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
    harness: Arc<ClaudeCloudHarness>,
    core: EngineCore,
    client: zeron_rpc::RpcClient,
    _tmp: tempfile::TempDir,
}

/// One device, its own login signed in as account A, with Claude cloud
/// sessions on.
async fn device() -> Device {
    use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    fake.own_token(TOKEN_A, "a");
    fake.own_token(TOKEN_B, "b");
    let login = ClaudeLogin::create(tmp.path().join("claude-login"), &FakeAccount::a(), TOKEN_A);
    let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
    let harness = Arc::new(
        ClaudeCloudHarness::new(ClaudeCloudConfig {
            base_url: fake.base_url().to_owned(),
            login: ClaudeConfigDir::at(login.dir()),
            claude_executable: Some(stub.path().to_owned()),
        })
        .expect("harness builds"),
    );
    let registry = HarnessRegistry::new();
    registry.register(harness.clone());
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), "dev-pc").unwrap();
    ClaudeCloudSessions::open(&data)
        .save(ClaudeCloudSettings {
            enabled: true,
            ..ClaudeCloudSettings::default()
        })
        .expect("setting saves");
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
        .expect("engine core assembles");
    core.set_provider_list_interval(Duration::ZERO);
    let client = zeron_rpc::memory_client(core.rpc_service());
    Device {
        fake,
        login,
        stub,
        harness,
        core,
        client,
        _tmp: tmp,
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

impl Device {
    async fn call(&self, method: &str, params: Value) -> Value {
        self.client.call(method, params).await.expect(method)
    }

    /// The settings page's status.
    async fn status(&self) -> Value {
        self.call(methods::GET_CLAUDE_CLOUD_SESSIONS, json!({}))
            .await
    }

    /// The sidebar's call.
    async fn sync(&self) -> Value {
        self.call(methods::SYNC_PROVIDER_SESSIONS, json!({})).await
    }

    fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
    }

    fn replied_zulu(&self, chat: &str) -> bool {
        self.entries(chat).last().is_some_and(|e| {
            e.role == MessageRole::Assistant
                && e.status == Some(MessageStatus::Complete)
                && text_of(e).trim() == "ZULU"
        })
    }

    /// "Anthropic cloud", no project, from this device's composer.
    async fn create(&self) -> (String, String) {
        let created = self
            .call(
                methods::MUTATE,
                json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": "dev-pc",
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write", "access": "full-access"},
                    "prompt": ZULU,
                    "messageId": FIRST,
                }),
            )
            .await;
        (
            created["chatId"].as_str().unwrap().to_owned(),
            created["cwd"].as_str().unwrap().to_owned(),
        )
    }

    fn send(&self, chat: &str, cwd: &str, message_id: &str) {
        let request = RunRequest {
            mcp: None,
            prompt: ZULU.into(),
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

    /// The person messages under `uuid` stored in `session`.
    fn stored(&self, session: &str, uuid: &str) -> usize {
        self.fake
            .stored_events(session)
            .iter()
            .filter(|e| e.payload["type"] == "user" && e.payload["uuid"] == uuid)
            .count()
    }

    fn disabled(&self) -> Option<String> {
        use zeron_harness::provider_cloud::ProviderCloud;
        self.harness.status().borrow().disabled.clone()
    }
}

/// The sidebar entry for session `sid`.
fn listed<'a>(reply: &'a Value, sid: &str) -> Option<&'a Value> {
    reply["sessions"]
        .as_array()?
        .iter()
        .find(|s| s["sessionId"] == sid)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_added_account_signs_in_through_the_cli_and_its_sessions_appear() {
    let pc = device().await;
    pc.fake.queue_turn(Turn::text_zulu());
    let mine = pc.fake.start_elsewhere_as("a", "A's session", ZULU);
    pc.fake.queue_turn(Turn::text_zulu());
    let theirs = pc.fake.start_elsewhere_as("b", "B's session", ZULU);
    wait_for("both turns at the provider", || {
        !pc.fake.turn_playing(&mine) && !pc.fake.turn_playing(&theirs)
    })
    .await;

    // Only the device's own login: only its account's sessions.
    let before = pc.sync().await;
    assert!(listed(&before, &mine).is_some(), "{before:#}");
    assert!(listed(&before, &theirs).is_none(), "{before:#}");

    // "Add account": the official CLI signs a new folder in (the person
    // finishes in the browser as B).
    pc.stub.sign_in_as(&FakeAccount::b(), TOKEN_B);
    let status = pc.call(methods::ADD_CLAUDE_CLOUD_ACCOUNT, json!({})).await;
    let accounts = status["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2, "{status:#}");
    assert_eq!(accounts[0]["email"], FakeAccount::a().email);
    assert_eq!(accounts[1]["email"], FakeAccount::b().email);
    assert_eq!(accounts[1]["enabled"], true);
    let login = pc
        .stub
        .calls()
        .into_iter()
        .find(|c| c.args == "auth login")
        .expect("`claude auth login` ran");
    assert!(
        login.config_dir.contains("claude-accounts"),
        "a folder of its own in the data dir: {login:?}"
    );

    // B's session shows, as B's, with its history.
    let after = pc.sync().await;
    let shown = listed(&after, &theirs).expect("B's session in the sidebar");
    assert!(listed(&after, &mine).is_some());
    let chat_id = shown["chatId"].as_str().unwrap().to_owned();
    let chat = pc
        .core
        .workspace
        .chat(&chat_id)
        .unwrap()
        .expect("a chat row");
    let session = chat.provider_session.expect("providerSession");
    assert_eq!(session.account_uuid, FakeAccount::b().account_uuid);
    assert_eq!(session.account_email, FakeAccount::b().email);
    wait_for("B's history mirrored", || pc.replied_zulu(&chat_id)).await;

    // Switched off on this device: its sessions are read no more.
    let id = accounts[1]["id"].as_str().unwrap();
    pc.call(
        methods::SET_CLAUDE_CLOUD_SESSIONS,
        json!({"account": {"id": id, "enabled": false}}),
    )
    .await;
    let off = pc.sync().await;
    assert!(listed(&off, &theirs).is_none(), "{off:#}");
    assert!(listed(&off, &mine).is_some());

    // Signing in as an account already here adds nothing.
    pc.stub.sign_in_as(&FakeAccount::a(), TOKEN_A);
    let again = pc
        .client
        .call(methods::ADD_CLAUDE_CLOUD_ACCOUNT, json!({}))
        .await;
    assert!(again.is_err(), "{again:?}");
    assert_eq!(pc.status().await["accounts"].as_array().unwrap().len(), 2);
    pc.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_switched_device_login_stops_the_old_accounts_chats_and_offers_to_add_it() {
    let pc = device().await;
    pc.fake.queue_turn(Turn::text_zulu());
    let (chat, cwd) = pc.create().await;
    let session = pc.fake.created_sessions().last().unwrap().clone();
    wait_for("the first reply", || pc.replied_zulu(&chat)).await;
    assert!(pc.status().await["offer"].is_null());

    // The device's Claude login is switched to account B.
    pc.login.set_account(&FakeAccount::b());
    pc.login
        .set_token(TOKEN_B, zeron_provider_fake::in_ms(3_600_000));
    pc.send(&chat, &cwd, SECOND);
    wait_for("hostUnavailable", || {
        pc.core
            .workspace
            .chat_host_unavailable(&chat)
            .is_some_and(|u| u.device_id == "dev-pc")
    })
    .await;
    assert_eq!(pc.stored(&session, SECOND), 0, "nothing went as account B");
    assert!(
        pc.disabled().is_none(),
        "a mismatch never turns the feature off"
    );
    // B's own sessions still list (the feature works for B).
    pc.fake.queue_turn(Turn::text_zulu());
    let bs = pc.fake.start_elsewhere_as("b", "B's session", ZULU);
    wait_for("B's turn", || !pc.fake.turn_playing(&bs)).await;
    let reply = pc.sync().await;
    assert!(listed(&reply, &bs).is_some(), "{reply:#}");

    // Zeron offers to keep A's chats working here, in the sidebar's reply
    // and on the settings page.
    assert_eq!(reply["accountOffer"], FakeAccount::a().email);
    assert_eq!(pc.status().await["offer"], FakeAccount::a().email);

    // Accepting signs A in to a folder of its own; A's chat runs here again
    // and the marker clears once A's login works.
    pc.stub.sign_in_as(&FakeAccount::a(), TOKEN_A);
    let status = pc.call(methods::ADD_CLAUDE_CLOUD_ACCOUNT, json!({})).await;
    assert!(status["offer"].is_null(), "{status:#}");
    pc.fake.queue_turn(Turn::text_zulu());
    pc.fake.queue_turn(Turn::text_zulu());
    pc.send(&chat, &cwd, THIRD);
    wait_for("A's chat answering again", || {
        pc.stored(&session, THIRD) == 1 && pc.replied_zulu(&chat)
    })
    .await;
    wait_for("the marker cleared", || {
        pc.core.workspace.chat_host_unavailable(&chat).is_none()
    })
    .await;
    assert!(pc.disabled().is_none());
    pc.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn declining_the_offer_keeps_it_away() {
    let pc = device().await;
    assert!(pc.status().await["offer"].is_null());
    pc.login.set_account(&FakeAccount::b());
    assert_eq!(pc.status().await["offer"], FakeAccount::a().email);
    let status = pc
        .call(
            methods::SET_CLAUDE_CLOUD_SESSIONS,
            json!({"dismissOffer": true}),
        )
        .await;
    assert!(status["offer"].is_null(), "{status:#}");
    assert!(pc.status().await["offer"].is_null());
    // Each later switch offers the account switched away from.
    pc.login.set_account(&FakeAccount::a());
    assert_eq!(pc.status().await["offer"], FakeAccount::b().email);
    pc.login.set_account(&FakeAccount::b());
    assert_eq!(pc.status().await["offer"], FakeAccount::a().email);
    pc.core.shutdown().await;
}
