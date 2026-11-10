//! Provider-run chats end to end in one engine, through its public request
//! surface (`Mutate createChat`, queued commands), against the fake Claude
//! Code session server and a stub `claude` CLI
//! (docs/design/provider-run-chats.md).
//!
//! Only externally visible results are asserted: what the fake server
//! received, the chat row and document, and the transcript. The controls
//! (ticket #5) go through the same surface: Steer, Interrupt and
//! RespondInput commands, and follow-ups whose settings changed.

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
    ClaudeLogin, FakeAccount, FakeSessionServer, PROMPTS_QUESTION, SeedSession, StubClaude, Turn,
    in_ms, is_text_delta,
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

/// The device's "Claude cloud sessions" setting, turned on before the engine
/// starts, so it keeps the fake-backed harness the rig registered.
fn claude_cloud_sessions_on(data_dir: &std::path::Path) {
    use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
    std::fs::create_dir_all(data_dir).unwrap();
    ClaudeCloudSessions::open(data_dir)
        .save(ClaudeCloudSettings {
            enabled: true,
            ..ClaudeCloudSettings::default()
        })
        .expect("setting saves");
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
        registry.register(Arc::new(
            ClaudeCloudHarness::new(ClaudeCloudConfig {
                base_url: fake.base_url().to_owned(),
                login: ClaudeConfigDir::at(login.dir()),
                claude_executable: Some(stub.path().to_owned()),
            })
            .expect("harness builds"),
        ));
        claude_cloud_sessions_on(&tmp.path().join("data"));
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
        self.create_with_access("full-access").await
    }

    async fn create_with_access(
        &self,
        access: &str,
    ) -> Result<serde_json::Value, zeron_rpc::RpcError> {
        self.client
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
        self.send_with(chat, cwd, message_id, prompt, |_| {});
    }

    /// A follow-up whose composer settings `adjust` changes.
    fn send_with(
        &self,
        chat: &str,
        cwd: &str,
        message_id: &str,
        prompt: &str,
        adjust: impl FnOnce(&mut RunRequest),
    ) {
        let mut request = RunRequest {
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
        adjust(&mut request);
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Run {
                    request,
                    message_id: message_id.into(),
                },
            )
            .expect("queue follow-up");
    }

    /// Every event payload sent to the session, in order.
    fn posted_payloads(&self) -> Vec<serde_json::Value> {
        self.posted_events()
            .iter()
            .flat_map(|r| r.body["events"].as_array().cloned().unwrap_or_default())
            .map(|e| e["payload"].clone())
            .collect()
    }

    /// The questions the chat waits on: (request id, questions).
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

    /// Answer the chat's open question with `label`, as the question panel does.
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
    let account_file = rig.login.dir().join(".claude.json");
    let account = std::fs::read(&account_file).unwrap();

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
    // Zeron only reads the login: both files are as the CLI (here, the test)
    // left them.
    assert_eq!(
        std::fs::read(rig.login.credentials_file()).unwrap(),
        credentials
    );
    assert_eq!(std::fs::read(&account_file).unwrap(), account);
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

// ---- Controls (ticket #5) ----

const THIRD: &str = "3c9d7e2f-1a4b-4c8d-9e0f-a1b2c3d4e5f6";
const FOURTH: &str = "7e6d5c4b-3a29-4180-9f7e-6d5c4b3a2918";

fn position(entries: &[SessionMessageEntry], pred: impl Fn(&SessionMessageEntry) -> bool) -> usize {
    entries.iter().position(pred).expect("entry present")
}

fn no_streaming(rig: &Rig, chat: &str) -> bool {
    rig.entries(chat)
        .iter()
        .all(|e| e.status != Some(MessageStatus::Streaming))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_steer_mid_turn_reaches_the_session_at_once() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;
    let sid = rig.fake.created_sessions()[0].clone();

    let (held, gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    gate.reached().await;
    // The person sees the reply streaming, then steers.
    wait_for("the streamed text", || {
        rig.assistants(&chat, MessageStatus::Streaming)
            .iter()
            .any(|e| text_of(e).contains("ZULU"))
    })
    .await;
    rig.core
        .doc_host
        .queue_command(
            &chat,
            SessionCommandPayload::Steer {
                prompt: "Say it once more.".into(),
                message_id: Some(THIRD.into()),
            },
        )
        .unwrap();
    // It reaches the session while the turn it steers is still running.
    wait_for("the steer sent", || {
        rig.posted_payloads().iter().any(|p| p["uuid"] == THIRD)
    })
    .await;
    assert!(rig.fake.turn_playing(&sid));
    gate.release();

    wait_for("the steered turn over", || {
        no_streaming(&rig, &chat)
            && rig
                .entries(&chat)
                .last()
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
            && rig.entries(&chat).iter().any(|e| e.id == THIRD)
    })
    .await;
    let entries = rig.entries(&chat);
    let second = position(&entries, |e| e.id == SECOND);
    let third = position(&entries, |e| e.id == THIRD);
    assert_eq!(text_of(&entries[third]), "Say it once more.");
    assert!(second < third);
    // The reply before the steer and the one after it, each its own entry.
    let order: Vec<(String, MessageRole, Option<MessageStatus>, String)> = entries
        .iter()
        .map(|e| (e.id.clone(), e.role, e.status, text_of(e)))
        .collect();
    assert!(
        entries[second + 1..third]
            .iter()
            .any(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU")),
        "{order:#?}"
    );
    assert_eq!(text_of(entries.last().unwrap()).trim(), "ZULU");
    rig.core.shutdown().await;
}

/// A message sent while the reply is about to stream lands, in the session,
/// in the middle of that reply (after its live text, before the stored
/// message). The reply it splits keeps its entry: the person's message is not
/// the event that wrote it, so the duplicate sweep never takes the reply for
/// a copy of the message. (The takeover flake of #12: two messages drained
/// together, the reply then missing from the transcript.)
#[tokio::test(flavor = "multi_thread")]
async fn a_message_stored_mid_reply_leaves_the_reply_in_the_transcript() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;
    let sid = rig.fake.created_sessions()[0].clone();

    // The turn waits before its live text, and again right after it.
    let (held, before_text) = Turn::text_zulu().hold_after(
        |step| matches!(step, zeron_provider_fake::Step::Stored(p) if p["subtype"] == "status"),
    );
    let (held, after_text) = held.hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    before_text.reached().await;

    // The next message reaches the server, which stores it only once the
    // reply's text has streamed (the run reads that text after its send
    // returns, ahead of the message's own event).
    let post = rig.fake.hold_next_post();
    rig.send(&chat, &cwd, THIRD, "Again: the single word ZULU.");
    wait_for("the message on its way", || {
        rig.posted_payloads().iter().any(|p| p["uuid"] == THIRD)
    })
    .await;
    before_text.release();
    after_text.reached().await;
    post.release();
    wait_for("the message stored", || {
        rig.fake
            .stored_events(&sid)
            .iter()
            .any(|e| e.payload["uuid"] == THIRD)
    })
    .await;
    // The person's message's marker is through before the reply goes on.
    tokio::time::sleep(Duration::from_millis(300)).await;
    after_text.release();

    wait_for("both replies", || {
        !rig.fake.turn_playing(&sid)
            && no_streaming(&rig, &chat)
            && rig
                .entries(&chat)
                .last()
                .is_some_and(|e| e.role == MessageRole::Assistant && text_of(e).contains("ZULU"))
    })
    .await;
    let entries = rig.entries(&chat);
    let order: Vec<(String, MessageRole, Option<MessageStatus>, String)> = entries
        .iter()
        .map(|e| (e.id.clone(), e.role, e.status, text_of(e)))
        .collect();
    let second = position(&entries, |e| e.id == SECOND);
    // Both turns' replies are there (a turn queued behind another continues
    // its entry).
    let replies: usize = entries[second..]
        .iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .map(|e| text_of(e).matches("ZULU").count())
        .sum();
    assert_eq!(replies, 2, "{order:#?}");
    assert_eq!(
        entries.iter().filter(|e| e.id == THIRD).count(),
        1,
        "{order:#?}"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_interrupts_the_sessions_turn_under_the_stop_commands_id() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;
    let sid = rig.fake.created_sessions()[0].clone();

    let (held, _gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    rig.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    _gate.reached().await;
    wait_for("the streamed text", || {
        rig.assistants(&chat, MessageStatus::Streaming)
            .iter()
            .any(|e| text_of(e).contains("ZULU"))
    })
    .await;

    let stop = rig
        .core
        .doc_host
        .queue_command(&chat, SessionCommandPayload::Interrupt {})
        .unwrap();
    wait_for("the stopped turn settled", || {
        no_streaming(&rig, &chat) && !rig.assistants(&chat, MessageStatus::Aborted).is_empty()
    })
    .await;
    let interrupts: Vec<_> = rig
        .posted_payloads()
        .into_iter()
        .filter(|p| p["type"] == "control_request")
        .collect();
    assert_eq!(interrupts.len(), 1, "{interrupts:?}");
    let uuid = zeron_proto::provider_cloud::event_uuid(&stop);
    assert_eq!(interrupts[0]["uuid"], uuid.as_str());
    assert_eq!(interrupts[0]["request"]["subtype"], "interrupt");
    assert!(!rig.fake.turn_playing(&sid));
    let last = rig.fake.stored_events(&sid).pop().unwrap();
    assert_eq!(last.payload["terminal_reason"], "aborted_streaming");

    // The chat goes on: the next message runs a new turn in the session.
    rig.fake.queue_turn(Turn::text_zulu());
    rig.send(&chat, &cwd, FOURTH, "Again, ZULU.");
    wait_for("the next turn", || {
        rig.entries(&chat).iter().any(|e| e.id == FOURTH)
            && no_streaming(&rig, &chat)
            && rig.entries(&chat).last().is_some_and(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
    })
    .await;
    assert!(rig.posted_payloads().iter().any(|p| p["uuid"] == FOURTH));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_answers_from_the_chat_reach_the_session() {
    let rig = rig().await;
    rig.fake.queue_turn(Turn::prompts());
    let created = rig
        .create_with_access("supervised")
        .await
        .expect("createChat");
    let chat = created["chatId"].as_str().unwrap().to_owned();
    let sid = rig.fake.created_sessions()[0].clone();

    // The MCP tool's permission prompt, in the chat's question panel.
    wait_for("the permission question", || {
        rig.open_input(&chat).is_some()
    })
    .await;
    let (request, questions) = rig.open_input(&chat).unwrap();
    assert!(
        questions[0].question.contains("Read Wiki Structure"),
        "{questions:?}"
    );
    rig.answer(&chat, &request, &questions[0].id, "Yes");

    // Then AskUserQuestion.
    wait_for("the question", || {
        rig.open_input(&chat)
            .is_some_and(|(_, q)| q[0].question == PROMPTS_QUESTION)
    })
    .await;
    let (request, questions) = rig.open_input(&chat).unwrap();
    rig.answer(&chat, &request, &questions[0].id, "blue");

    wait_for("the turn completed", || {
        no_streaming(&rig, &chat) && !rig.assistants(&chat, MessageStatus::Complete).is_empty()
    })
    .await;
    let prompts = rig.fake.prompts(&sid);
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        rig.fake.answer(&sid, &prompts[0]).unwrap()["behavior"],
        "allow"
    );
    assert_eq!(
        rig.fake.answer(&sid, &prompts[1]).unwrap()["updatedInput"]["answers"][PROMPTS_QUESTION],
        "blue"
    );
    let answers: Vec<_> = rig
        .posted_payloads()
        .into_iter()
        .filter(|p| p["type"] == "control_response")
        .collect();
    assert_eq!(answers.len(), 2);
    for (answer, request_id) in answers.iter().zip(&prompts) {
        assert_eq!(
            answer["uuid"],
            zeron_proto::provider_cloud::answer_uuid(request_id).as_str()
        );
    }
    assert!(rig.open_input(&chat).is_none());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn model_and_plan_changes_go_ahead_of_the_message_as_control_requests() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;
    let sid = rig.fake.created_sessions()[0].clone();

    rig.fake.queue_turn(Turn::text_zulu());
    rig.send_with(&chat, &cwd, SECOND, "Plan it", |r| {
        r.model = Some("claude-sonnet-5".into());
        r.plan = true;
    });
    wait_for("the reply", || {
        rig.assistants(&chat, MessageStatus::Complete).len() == 2
    })
    .await;
    let sent = rig.posted_payloads();
    let kinds: Vec<(&str, &str)> = sent
        .iter()
        .map(|p| {
            (
                p["type"].as_str().unwrap(),
                p["request"]["subtype"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("control_request", "set_model"),
            ("control_request", "set_permission_mode"),
            ("user", ""),
        ]
    );
    assert_eq!(
        sent[0]["uuid"],
        zeron_proto::provider_cloud::control_uuid(SECOND, "set_model").as_str()
    );
    assert_eq!(sent[2]["uuid"], SECOND);
    assert_eq!(
        rig.fake.model(&sid),
        Some(serde_json::json!("claude-sonnet-5"))
    );
    assert_eq!(rig.fake.permission_mode(&sid).as_deref(), Some("plan"));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_typed_on_claude_ai_shows_as_the_persons() {
    let rig = rig().await;
    let (chat, _cwd) = create_and_mirror(&rig).await;
    let sid = rig.fake.created_sessions()[0].clone();

    rig.fake.queue_turn(Turn::text_zulu());
    rig.fake.post_as_client(
        &sid,
        serde_json::json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": "",
            "type": "user",
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": "Typed on claude.ai"},
        }),
    );
    wait_for("the message and its reply", || {
        rig.assistants(&chat, MessageStatus::Complete).len() == 2 && no_streaming(&rig, &chat)
    })
    .await;
    let entries = rig.entries(&chat);
    let typed = position(&entries, |e| {
        e.role == MessageRole::User && text_of(e) == "Typed on claude.ai"
    });
    assert!(
        entries[typed + 1..]
            .iter()
            .any(|e| e.role == MessageRole::Assistant && text_of(e).trim() == "ZULU")
    );
    // The first prompt (sent from here) shows once.
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.role == MessageRole::User && text_of(e) == FIRST_PROMPT)
            .count(),
        1
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutting_the_engine_down_mid_turn_leaves_the_session_working() {
    let rig = rig().await;
    let (chat, cwd) = create_and_mirror(&rig).await;
    let sid = rig.fake.created_sessions()[0].clone();

    let (held, gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    rig.send(&chat, &cwd, SECOND, SECOND_PROMPT);
    gate.reached().await;
    wait_for("the streamed text", || {
        rig.assistants(&chat, MessageStatus::Streaming)
            .iter()
            .any(|e| text_of(e).contains("ZULU"))
    })
    .await;

    // Quitting Zeron is not a Stop: nothing goes to the session.
    rig.core.shutdown().await;
    let controls: Vec<_> = rig
        .posted_payloads()
        .into_iter()
        .filter(|p| p["type"] == "control_request")
        .collect();
    assert!(controls.is_empty(), "{controls:?}");
    // The session works on with every device off, and finishes the turn.
    assert!(rig.fake.turn_playing(&sid));
    gate.release();
    wait_for("the turn finished at the provider", || {
        !rig.fake.turn_playing(&sid)
    })
    .await;
    let last = rig.fake.stored_events(&sid).pop().unwrap();
    assert_eq!(last.payload["subtype"], "success");
}
