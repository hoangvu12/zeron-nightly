//! Provider-run chats whose Claude login needs renewing or a sign-in,
//! through the engine's public request surface, against the fake session
//! server and the stub `claude`.
//!
//! Asserted from outside only: what the fake server received, how many times
//! the stub ran, the chat document's `providerState` (what the chat shows),
//! the transcript, the harness's published kill switch, and the login files.

use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::Harness;
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::provider_cloud::ProviderChatState;
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const THIRD: &str = "9a4c2e7b-1d3f-4b6a-8e5c-7f9d1b3a5c2e";

struct Rig {
    _tmp: tempfile::TempDir,
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
    harness: Arc<ClaudeCloudHarness>,
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

impl Rig {
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
        let core = EngineCore::assemble(
            &tmp.path().join("data"),
            Arc::new(registry),
            HarnessId::ClaudeCodeCloud,
            None,
        )
        .expect("engine core assembles");
        let client = zeron_rpc::memory_client(core.rpc_service());
        Self {
            _tmp: tmp,
            fake,
            login,
            stub,
            harness,
            core,
            client,
        }
    }

    /// Create the chat and wait for its first turn. Returns (chat, cwd).
    async fn create_and_mirror(&self) -> (String, String) {
        self.fake.queue_turn(Turn::bash_tool());
        let created = self
            .client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": self.core.device_id,
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write"},
                    "prompt": "Run uname -a",
                    "messageId": FIRST,
                }),
            )
            .await
            .expect("createChat");
        let chat = created["chatId"].as_str().unwrap().to_owned();
        let cwd = created["cwd"].as_str().unwrap().to_owned();
        wait_for("the first turn", || self.completed(&chat) == 1).await;
        (chat, cwd)
    }

    fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
    }

    fn completed(&self, chat: &str) -> usize {
        self.entries(chat)
            .iter()
            .filter(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
            .count()
    }

    fn transcript(&self, chat: &str) -> String {
        serde_json::to_string(&self.entries(chat)).unwrap()
    }

    /// What the chat shows (its document's `providerState`).
    fn state(&self, chat: &str) -> Option<ProviderChatState> {
        self.core
            .doc_host
            .open(chat)
            .ok()?
            .doc()
            .provider_chat_state()
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

    fn posted_events(&self) -> Vec<zeron_provider_fake::RecordedRequest> {
        self.fake
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/events"))
            .collect()
    }

    fn disabled(&self) -> Option<String> {
        self.harness
            .provider_cloud()
            .unwrap()
            .status()
            .borrow()
            .disabled
            .clone()
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

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_token_is_renewed_by_the_cli_and_the_chat_carries_on() {
    let rig = Rig::new().await;
    let (chat, cwd) = rig.create_and_mirror().await;
    let creds_before = std::fs::read(rig.login.credentials_file()).unwrap();

    // The token dies at the provider; the CLI holds a fresh one.
    rig.fake.reject_token("token-a-1");
    rig.stub.renew_with(&ClaudeLogin::credentials_json(
        "token-a-2",
        zeron_provider_fake::in_ms(3_600_000),
    ));
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the second turn", || rig.completed(&chat) == 2).await;

    let reply = rig
        .entries(&chat)
        .into_iter()
        .rfind(|e| e.role == MessageRole::Assistant)
        .unwrap();
    assert_eq!(text_of(&reply).trim(), "ZULU");
    assert_eq!(rig.stub.call_count(), 1);
    let sent = rig.posted_events();
    let auths: Vec<_> = sent.iter().map(|r| r.header("authorization")).collect();
    assert_eq!(
        auths,
        vec![Some("Bearer token-a-1"), Some("Bearer token-a-2")],
        "refused once, then sent with the renewed token"
    );
    assert_eq!(rig.state(&chat), None);
    assert_eq!(rig.disabled(), None);
    // Only the CLI (the stub) wrote the login.
    assert_ne!(
        std::fs::read(rig.login.credentials_file()).unwrap(),
        creds_before
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_after_the_renewal_shows_sign_in_again_and_turns_the_feature_off() {
    let rig = Rig::new().await;
    let (chat, cwd) = rig.create_and_mirror().await;
    assert_eq!(rig.state(&chat), None);
    let login_before = std::fs::read(rig.login.credentials_file()).unwrap();

    rig.fake.reject_token("token-a-1");
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("Sign in again on the chat", || {
        rig.state(&chat)
            == Some(ProviderChatState::SignInAgain {
                email: FakeAccount::a().email,
            })
    })
    .await;
    wait_for("the error in the transcript", || {
        rig.transcript(&chat).contains("Sign in again")
    })
    .await;
    assert_eq!(rig.stub.call_count(), 1, "one renewal");
    assert_eq!(rig.posted_events().len(), 2, "the send and one retry");
    let reason = rig.disabled().expect("the feature turned itself off");
    assert!(reason.contains("refused the login"), "{reason}");
    // Zeron never wrote the login.
    assert_eq!(
        std::fs::read(rig.login.credentials_file()).unwrap(),
        login_before
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_without_the_chats_login_shows_sign_in_as_until_it_is_back() {
    let rig = Rig::new().await;
    let (chat, cwd) = rig.create_and_mirror().await;

    // Zeron's account switcher (say) signs the folder in as someone else.
    rig.login.set_account(&FakeAccount::b());
    rig.send(&chat, &cwd, SECOND, "hello?");
    wait_for("Sign in as a on the chat", || {
        rig.state(&chat)
            == Some(ProviderChatState::SignInAs {
                email: FakeAccount::a().email,
            })
    })
    .await;
    assert!(rig.posted_events().is_empty(), "nothing sent as b");
    assert_eq!(rig.disabled(), None, "a mismatch never trips the switch");
    assert_eq!(rig.stub.call_count(), 0);

    // The account's login is back: the next message works and the state clears.
    rig.login.set_account(&FakeAccount::a());
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, THIRD, "Reply with the single word ZULU.");
    wait_for("the state cleared", || rig.state(&chat).is_none()).await;
    wait_for("the reply", || {
        rig.entries(&chat).iter().any(|e| {
            e.role == MessageRole::Assistant
                && e.status == Some(MessageStatus::Complete)
                && text_of(e).trim() == "ZULU"
        })
    })
    .await;
    rig.core.shutdown().await;
}
