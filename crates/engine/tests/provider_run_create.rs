//! A create whose outcome is unknown (ticket #11;
//! docs/design/provider-run-chats.md §4, "Creating a session").
//!
//! Creating a session isn't idempotent, so Zeron never retries it. Each
//! create carries the tag `zeron:<first prompt uuid>`; when its answer is
//! lost, the engine looks for that tag in the session list and adopts the
//! session it finds, or says "Creation uncertain" and creates nothing more.
//! Only the person creates again.
//!
//! Through the engine's public surface (`Mutate createChat`, as the
//! composer sends it), against the fake Claude Code session server.

use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{MessagePart, MessageRole, SessionMessageEntry};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::HarnessId;
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, Fault, SeedSession, StubClaude, Turn,
};
use zeron_rpc::{RpcError, capability_errors, methods};

const PROMPT: &str = "Reply with the single word ZULU.";

struct Rig {
    _tmp: tempfile::TempDir,
    fake: FakeSessionServer,
    _login: ClaudeLogin,
    _stub: StubClaude,
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

async fn rig() -> Rig {
    use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    let login = ClaudeLogin::create(
        tmp.path().join("claude-login"),
        &FakeAccount::a(),
        "token-a-1",
    );
    let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeCloudHarness::new(ClaudeCloudConfig {
            base_url: fake.base_url().to_owned(),
            login: ClaudeConfigDir::at(login.dir()),
            claude_executable: Some(stub.path().to_owned()),
        })
        .expect("harness builds"),
    ));
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    ClaudeCloudSessions::open(&data)
        .save(ClaudeCloudSettings {
            enabled: true,
            ..ClaudeCloudSettings::default()
        })
        .expect("setting saves");
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
        .expect("engine core assembles");
    let client = zeron_rpc::memory_client(core.rpc_service());
    Rig {
        _tmp: tmp,
        fake,
        _login: login,
        _stub: stub,
        core,
        client,
    }
}

impl Rig {
    /// The composer's `createChat` for "Anthropic cloud": no chat id (the
    /// engine names the chat after its session).
    async fn create(&self, message_id: &str) -> Result<serde_json::Value, RpcError> {
        self.client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "deviceId": self.core.device_id,
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write", "access": "full-access"},
                    "prompt": PROMPT,
                    "messageId": message_id,
                }),
            )
            .await
    }

    fn creates(&self) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/code/sessions")
            .count()
    }

    fn provider_chats(&self) -> usize {
        self.core
            .workspace
            .read_chats()
            .unwrap()
            .iter()
            .filter(|c| c.provider_session.is_some())
            .count()
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

fn uncertain(result: Result<serde_json::Value, RpcError>) -> bool {
    matches!(result, Err(RpcError::Capability(code))
        if code == capability_errors::PROVIDER_CREATION_UNCERTAIN)
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

#[tokio::test(flavor = "multi_thread")]
async fn a_create_carries_the_tag_of_its_first_prompt() {
    let rig = rig().await;
    rig.fake.queue_turn(Turn::text_zulu());
    let message = uuid::Uuid::new_v4().to_string();
    rig.create(&message).await.expect("createChat");
    let create = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/v1/code/sessions")
        .unwrap();
    assert_eq!(create.body["tags"], serde_json::json!([format!("zeron:{message}")]));
    assert_eq!(create.body["events"][0]["payload"]["uuid"], message);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_answer_adopts_the_session_it_created() {
    let rig = rig().await;
    rig.fake.queue_turn(Turn::text_zulu());
    rig.fake.lose_create_answers(1);
    let message = uuid::Uuid::new_v4().to_string();

    let created = rig.create(&message).await.expect("the created session is adopted");

    // One session, created once: the lost answer was never retried.
    assert_eq!(rig.fake.created_sessions().len(), 1);
    assert_eq!(rig.creates(), 1);
    let sid = &rig.fake.created_sessions()[0];
    assert_eq!(created["providerSession"]["sessionId"], sid.as_str());
    let chat = created["chatId"].as_str().unwrap().to_owned();
    assert_eq!(
        chat,
        zeron_proto::provider_cloud::chat_id("claude", sid),
        "named after the session it found"
    );
    // The chat mirrors the session; its first message isn't sent again.
    wait_for("the reply mirrored", || {
        let entries = rig.entries(&chat);
        entries
            .iter()
            .any(|e| e.role == MessageRole::User && text_of(e) == PROMPT)
            && entries
                .iter()
                .any(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU")
    })
    .await;
    let users = rig
        .fake
        .stored_events(sid)
        .iter()
        .filter(|e| e.payload["type"] == "user" && e.source == "client")
        .count();
    assert_eq!(users, 1, "the first prompt reached the session once");
    assert_eq!(rig.provider_chats(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_session_found_is_creation_uncertain_and_creates_nothing_more() {
    let rig = rig().await;
    // The create fails at the server before anything is made, in a way the
    // client can't tell apart from a lost answer.
    rig.fake.inject(Fault {
        method: Some("POST"),
        path_contains: "/v1/code/sessions".into(),
        status: 502,
        times: 1,
        retry_after: None,
    });
    let message = uuid::Uuid::new_v4().to_string();

    assert!(uncertain(rig.create(&message).await));

    assert!(rig.fake.created_sessions().is_empty());
    assert_eq!(rig.creates(), 1, "never retried");
    assert_eq!(rig.provider_chats(), 0);
    // The list was read again, after the create, to look for its tag.
    assert!(rig.fake.list_reads().len() >= 2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.creates(), 1, "nothing tries again later either");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn create_again_creates_exactly_one_new_session() {
    let rig = rig().await;
    rig.fake.inject(Fault {
        method: Some("POST"),
        path_contains: "/v1/code/sessions".into(),
        status: 502,
        times: 1,
        retry_after: None,
    });
    assert!(uncertain(rig.create(&uuid::Uuid::new_v4().to_string()).await));

    // The person presses "Create again": a new create, with a new message.
    rig.fake.queue_turn(Turn::text_zulu());
    let created = rig
        .create(&uuid::Uuid::new_v4().to_string())
        .await
        .expect("created again");
    assert_eq!(rig.fake.created_sessions().len(), 1);
    assert_eq!(rig.creates(), 2);
    assert_eq!(rig.provider_chats(), 1);
    assert!(created["chatId"].as_str().is_some());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_kept_tags_a_lost_answer_is_creation_uncertain() {
    let rig = rig().await;
    rig.fake.keep_tags(false);
    rig.fake.queue_turn(Turn::text_zulu());
    rig.fake.lose_create_answers(1);

    assert!(uncertain(rig.create(&uuid::Uuid::new_v4().to_string()).await));

    // The session exists, but Zeron can't tell it's this create's: it
    // adopts nothing and makes no other.
    assert_eq!(rig.fake.created_sessions().len(), 1);
    assert_eq!(rig.creates(), 1);
    assert_eq!(rig.provider_chats(), 0);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_create_is_a_plain_failure() {
    let rig = rig().await;
    rig.fake.inject(Fault {
        method: Some("POST"),
        path_contains: "/v1/code/sessions".into(),
        status: 400,
        times: 1,
        retry_after: None,
    });
    let result = rig.create(&uuid::Uuid::new_v4().to_string()).await;
    assert!(matches!(result, Err(RpcError::Failed(_))), "{result:?}");
    assert!(rig.fake.list_reads().len() <= 1, "no tag lookup after a refusal");
    assert_eq!(rig.creates(), 1);
    rig.core.shutdown().await;
}
