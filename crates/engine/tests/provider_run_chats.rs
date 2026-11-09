//! Provider-run chats end to end in one engine, through its public request
//! surface (`Mutate createChat`, queued commands), against the fake Claude
//! Code session server and a stub `claude` CLI
//! (docs/design/provider-run-chats.md; ticket #3).
//!
//! Only externally visible results are asserted: what the fake server
//! received, the chat row and document, and the transcript.

use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::{HarnessId, ProviderSession, RunRequest, SandboxLevel, ToolCall};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn, in_ms,
    is_text_delta,
};
use zeron_rpc::methods;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const FIRST_PROMPT: &str = "Run `uname -a` and `cat /tmp/zeron-init-marker 2>&1`, then reply DONE";
const SECOND_PROMPT: &str = "Reply with the single word ZULU.";

struct Rig {
    _tmp: tempfile::TempDir,
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

async fn rig_with(enabled: bool, prepare: impl FnOnce(&ClaudeLogin, &StubClaude)) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    let login = ClaudeLogin::create(
        tmp.path().join("claude-login"),
        &FakeAccount::a(),
        "token-a-1",
    );
    let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
    prepare(&login, &stub);
    let registry = HarnessRegistry::new();
    if enabled {
        registry.register(Arc::new(ClaudeCloudHarness::new(ClaudeCloudConfig {
            base_url: fake.base_url().to_owned(),
            login: ClaudeConfigDir::at(login.dir()),
            claude_executable: Some(stub.path().to_owned()),
        })
        .expect("harness builds")));
    }
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::ClaudeCodeCloud,
        None,
    )
    .expect("engine core assembles");
    let client = zeron_rpc::memory_client(core.rpc_service());
    Rig {
        _tmp: tmp,
        fake,
        login,
        stub,
        core,
        client,
    }
}

async fn rig() -> Rig {
    rig_with(true, |_, _| {}).await
}

impl Rig {
    /// The composer's `createChat` for "Anthropic cloud", no project.
    async fn create(&self) -> Result<serde_json::Value, zeron_rpc::RpcError> {
        self.client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": self.core.device_id,
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write"},
                    "prompt": FIRST_PROMPT,
                    "messageId": FIRST,
                }),
            )
            .await
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

    /// A follow-up, as the composer queues one.
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

    fn posted_events(&self) -> Vec<zeron_provider_fake::RecordedRequest> {
        self.fake
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/events"))
            .collect()
    }
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
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

fn account_a_session(session_id: &str) -> ProviderSession {
    let a = FakeAccount::a();
    ProviderSession {
        provider: "claude".into(),
        session_id: session_id.into(),
        account_uuid: a.account_uuid,
        account_email: a.email,
        org_id: a.org_id,
    }
}

/// Create, then wait for the first turn to be mirrored. Returns (chat, cwd).
async fn create_and_mirror(rig: &Rig) -> (String, String) {
    rig.fake.queue_turn(Turn::bash_tool());
    let created = rig.create().await.expect("createChat");
    let chat = created["chatId"].as_str().unwrap().to_owned();
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    wait_for("the first turn mirrored", || {
        !rig.assistants(&chat, MessageStatus::Complete).is_empty()
    })
    .await;
    (chat, cwd)
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_chat_creates_one_session_and_mirrors_its_first_turn() {
    let rig = rig().await;
    rig.fake.queue_turn(Turn::bash_tool());
    let created = rig.create().await.expect("createChat");

    // Exactly one session, created with the first prompt under its id.
    let sessions = rig.fake.created_sessions();
    assert_eq!(sessions.len(), 1);
    let session_id = &sessions[0];
    let create = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/v1/code/sessions")
        .unwrap();
    assert_eq!(create.body["events"][0]["payload"]["uuid"], FIRST);
    assert_eq!(
        create.body["events"][0]["payload"]["message"]["content"],
        FIRST_PROMPT
    );
    assert_eq!(create.body["environment_id"], "env_fake0001");

    // The chat is named by the session, not by the client's id.
    let chat = created["chatId"].as_str().unwrap();
    assert_ne!(chat, "client-picked-id");
    assert_eq!(
        chat,
        zeron_proto::provider_cloud::chat_id("claude", session_id)
    );
    assert_eq!(uuid::Uuid::parse_str(chat).unwrap().get_version_num(), 5);
    let cwd = format!("claude-cloud://{session_id}");
    assert_eq!(created["cwd"], cwd.as_str());

    let row = rig.core.workspace.chat(chat).unwrap().expect("chat row");
    assert_eq!(row.provider_session, Some(account_a_session(session_id)));
    assert_eq!(row.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(row.device_id, rig.core.device_id);
    assert_eq!(row.space_id, None);
    assert_eq!(
        row.config.as_ref().map(|c| c.harness),
        Some(HarnessId::ClaudeCodeCloud)
    );
    let doc = rig.core.doc_host.open(chat).unwrap();
    assert_eq!(
        doc.doc().provider_run_session(),
        Some(account_a_session(session_id))
    );

    // The first turn renders as a local Claude Code chat's would.
    wait_for("the first turn mirrored", || {
        !rig.assistants(chat, MessageStatus::Complete).is_empty()
    })
    .await;
    let entries = rig.entries(chat);
    let user = entries
        .iter()
        .find(|e| e.role == MessageRole::User)
        .expect("user entry");
    assert_eq!(user.id, FIRST);
    assert_eq!(text_of(user), FIRST_PROMPT);
    let reply = &rig.assistants(chat, MessageStatus::Complete)[0];
    assert!(
        reply.parts.iter().any(|p| matches!(p,
            MessagePart::Tool { call: ToolCall::Exec { command }, resolved: true, .. }
                if command == "uname -a; cat /tmp/zeron-init-marker 2>&1")),
        "{:?}",
        reply.parts
    );
    assert!(
        text_of(reply).contains("`uname -a` returned:"),
        "{}",
        text_of(reply)
    );

    // The first prompt rode the create request: it isn't sent again, and no
    // local resume id is recorded for the chat.
    assert!(rig.posted_events().is_empty());
    let row = rig.core.workspace.chat(chat).unwrap().unwrap();
    assert_eq!(row.harness_session_id, None);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_stream_live_and_follow_ups_reach_the_session_honestly() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;

    // The token is read from the folder for every request.
    rig.login.set_token("token-a-2", in_ms(3_600_000));
    let credentials = std::fs::read(rig.login.credentials_file()).unwrap();

    let (turn, gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(turn);
    rig.send(&chat, &cwd, SECOND, SECOND_PROMPT);

    // The delta is in the transcript while the turn is still running.
    gate.reached().await;
    wait_for("the streamed text", || {
        rig.assistants(&chat, MessageStatus::Streaming)
            .iter()
            .any(|e| text_of(e).contains("ZULU"))
    })
    .await;
    assert_eq!(rig.assistants(&chat, MessageStatus::Complete).len(), 1);
    gate.release();
    wait_for("the second turn", || {
        rig.assistants(&chat, MessageStatus::Complete).len() == 2
    })
    .await;
    let second = rig
        .assistants(&chat, MessageStatus::Complete)
        .pop()
        .unwrap();
    assert_eq!(text_of(&second).trim(), "ZULU");

    // It reached the session once, under the message's id.
    let sent = rig.posted_events();
    assert_eq!(sent.len(), 1);
    let payload = &sent[0].body["events"][0]["payload"];
    assert_eq!(payload["uuid"], SECOND);
    assert_eq!(payload["type"], "user");
    assert_eq!(payload["message"]["content"], SECOND_PROMPT);
    assert_eq!(sent[0].header("authorization"), Some("Bearer token-a-2"));
    let user = rig
        .entries(&chat)
        .into_iter()
        .find(|e| e.id == SECOND)
        .expect("second message");
    assert_eq!(text_of(&user), SECOND_PROMPT);

    // Zeron's own identity on every request, never a client platform, and
    // only the session endpoints.
    let ua = format!("zeron/{}", env!("CARGO_PKG_VERSION"));
    let requests = rig.fake.requests();
    assert!(requests.len() >= 4, "{requests:?}");
    for request in &requests {
        assert_eq!(
            request.header("user-agent"),
            Some(ua.as_str()),
            "{} {}",
            request.method,
            request.path
        );
        assert_eq!(
            request.header("anthropic-client-platform"),
            None,
            "{}",
            request.path
        );
        assert!(
            request.path.starts_with("/v1/code/sessions"),
            "{}",
            request.path
        );
    }
    // Zeron only reads the login: the file is as the CLI (here, the test) left it.
    assert_eq!(
        std::fs::read(rig.login.credentials_file()).unwrap(),
        credentials
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_for_another_account_is_refused() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;

    // Zeron's account switcher (say) signs the folder in as someone else.
    rig.login.set_account(&FakeAccount::b());
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    wait_for("the refusal in the transcript", || {
        serde_json::to_string(&rig.entries(&chat))
            .unwrap()
            .contains("different account")
    })
    .await;
    assert!(
        rig.posted_events().is_empty(),
        "nothing was sent as another account"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_near_expiry_is_renewed_through_the_cli() {
    let rig = rig_with(true, |login, stub| {
        login.set_token("token-a-old", in_ms(60_000));
        stub.renew_with(&ClaudeLogin::credentials_json(
            "token-a-fresh",
            in_ms(3_600_000),
        ));
    })
    .await;
    let (_chat, _cwd) = create_and_mirror(&rig).await;
    let calls = rig.stub.calls();
    assert!(!calls.is_empty());
    assert!(calls.iter().all(|c| c.args == "auth status --json"));
    assert_eq!(std::path::Path::new(&calls[0].config_dir), rig.login.dir());
    for request in rig.fake.requests() {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer token-a-fresh"),
            "{}",
            request.path
        );
    }
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_run_chats_are_off_unless_enabled() {
    let rig = rig_with(false, |_, _| {}).await;
    let error = rig.create().await.expect_err("refused while off");
    assert!(error.to_string().contains("off on this device"), "{error}");
    assert!(rig.fake.requests().is_empty());
    assert!(rig.core.workspace.read_chats().unwrap().is_empty());
    rig.core.shutdown().await;
}
