//! `claude-code-cloud` against the fake session server: create, mirror a
//! recorded turn, send a follow-up, honest identity on every request, token
//! renewal through the stub `claude`, and the account check. Then the
//! controls (ticket #5): steering mid-turn, stop, prompt answers (and no
//! answer for a prompt already answered), model / permission / plan changes,
//! and re-executed commands deduped by their uuids.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_harness::provider_cloud::{CreateSession, RunBinding};
use zeron_harness::{CancellationToken, Harness, HarnessError, RunControls, SteerMessage};
use zeron_proto::provider_cloud::{answer_uuid, control_uuid};
use zeron_proto::{
    AccessMode, AgentEvent, DoneStatus, HarnessId, ProviderSession, RunRequest, SandboxLevel,
    ToolCall, UserInputAnswer, UserInputQuestion,
};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, PROMPTS_QUESTION, RecordedRequest, SeedSession,
    StubClaude, Turn, in_ms, is_text_delta,
};

struct Rig {
    _dir: tempfile::TempDir,
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
    harness: ClaudeCloudHarness,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    let login = ClaudeLogin::create(dir.path().join("login"), &FakeAccount::a(), "token-a-1");
    let stub = StubClaude::install(dir.path().join("bin"), &FakeAccount::a());
    let harness = ClaudeCloudHarness::new(ClaudeCloudConfig {
        base_url: fake.base_url().to_owned(),
        login: ClaudeConfigDir::at(login.dir()),
        claude_executable: Some(stub.path().to_owned()),
    });
    Rig {
        _dir: dir,
        fake,
        login,
        stub,
        harness,
    }
}

fn request(cwd: &str, prompt: &str) -> RunRequest {
    RunRequest {
        mcp: None,
        prompt: prompt.into(),
        harness: Some(HarnessId::ClaudeCodeCloud),
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        access: Default::default(),
        plan: false,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
        env: Default::default(),
        extra_mcp: Vec::new(),
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steering) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        realtime: None,
        execution_lease: None,
        request_input: Box::new(|_| oneshot::channel().1),
        steering,
        interrupt: token.clone(),
        turn: Default::default(),
    };
    (controls, steer_tx, token)
}

/// Events up to and including the next `Done`.
async fn turn(
    stream: &mut BoxStream<'static, Result<AgentEvent, HarnessError>>,
) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("turn in time")
            .expect("stream open")
            .expect("no harness error");
        let done = matches!(event, AgentEvent::Done { .. });
        out.push(event);
        if done {
            return out;
        }
    }
}

fn text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn create(rig: &Rig, prompt: &str, uuid: &str) -> ProviderSession {
    create_playing(rig, Turn::bash_tool(), prompt, uuid).await
}

async fn create_playing(rig: &Rig, first: Turn, prompt: &str, uuid: &str) -> ProviderSession {
    rig.fake.queue_turn(first);
    rig.harness
        .provider_cloud()
        .unwrap()
        .create_session(CreateSession {
            prompt: prompt.into(),
            prompt_uuid: uuid.into(),
            model: None,
            title: None,
        })
        .await
        .expect("session created")
}

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";

#[tokio::test]
async fn creates_mirrors_and_follows_up_with_an_honest_identity() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    assert_eq!(
        rig.fake.created_sessions(),
        vec![session.session_id.clone()]
    );
    assert_eq!(session.provider, "claude");
    assert_eq!(session.account_uuid, FakeAccount::a().account_uuid);
    assert_eq!(session.account_email, FakeAccount::a().email);
    assert_eq!(session.org_id, FakeAccount::a().org_id);
    let created = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/v1/code/sessions")
        .unwrap();
    assert_eq!(created.body["environment_id"], "env_fake0001");
    assert_eq!(created.body["events"][0]["payload"]["uuid"], FIRST);
    assert_eq!(
        created.body["events"][0]["payload"]["message"]["content"],
        "Run uname -a"
    );

    let cwd = zeron_proto::provider_cloud::claude_cwd(&session.session_id);
    rig.harness.provider_cloud().unwrap().bind_run(RunBinding {
        session: session.clone(),
        prompt_uuid: Some(FIRST.into()),
    });
    let (controls, steer, _interrupt) = controls();
    let mut stream = rig
        .harness
        .run(request(&cwd, "Run uname -a"), controls)
        .await
        .unwrap();
    let first = turn(&mut stream).await;
    match first
        .iter()
        .find(|e| matches!(e, AgentEvent::SessionStarted { .. }))
    {
        Some(AgentEvent::SessionStarted {
            harness,
            cwd: started_in,
            session_id,
            ..
        }) => {
            assert_eq!(*harness, HarnessId::ClaudeCodeCloud);
            assert_eq!(started_in, &cwd);
            assert!(session_id.is_empty(), "no local resume id");
        }
        _ => panic!("no SessionStarted: {first:?}"),
    }
    assert!(first.iter().any(|e| matches!(e,
        AgentEvent::ToolCall { call: ToolCall::Exec { command }, .. }
            if command == "uname -a; cat /tmp/zeron-init-marker 2>&1")));
    assert!(
        first
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { .. }))
    );
    assert!(
        text(&first).starts_with("`uname -a` returned:"),
        "{}",
        text(&first)
    );
    assert!(matches!(
        first.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            session_id: None,
            ..
        })
    ));
    // The first prompt rode the create request: not sent again.
    assert!(
        !rig.fake
            .requests()
            .iter()
            .any(|r| r.method == "POST" && r.path.ends_with("/events"))
    );

    rig.fake.queue_turn(Turn::text_zulu());
    steer
        .send(SteerMessage {
            prompt: "Reply with the single word ZULU.".into(),
            message_id: Some(SECOND.into()),
            attachments: Vec::new(),
            config: None,
        })
        .await
        .unwrap();
    let second = turn(&mut stream).await;
    assert!(matches!(second.first(), Some(AgentEvent::Steered { .. })));
    assert_eq!(text(&second), "ZULU");
    let sent: Vec<_> = rig
        .fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path.ends_with("/events"))
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].body["events"][0]["payload"]["uuid"], SECOND);
    assert_eq!(sent[0].body["events"][0]["payload"]["type"], "user");

    let ua = format!("zeron/{}", env!("CARGO_PKG_VERSION"));
    for request in rig.fake.requests() {
        assert_eq!(
            request.header("user-agent"),
            Some(ua.as_str()),
            "{} {}",
            request.method,
            request.path
        );
        assert_eq!(request.header("anthropic-client-platform"), None);
        assert_eq!(request.header("authorization"), Some("Bearer token-a-1"));
    }
    // Zeron never touches the environment endpoints.
    assert!(
        rig.fake
            .requests()
            .iter()
            .all(|r| r.path.starts_with("/v1/code/sessions"))
    );
}

#[tokio::test]
async fn a_token_near_expiry_is_renewed_by_the_cli_before_use() {
    let rig = rig().await;
    rig.login.set_token("token-a-old", in_ms(60_000));
    rig.stub.renew_with(&ClaudeLogin::credentials_json(
        "token-a-fresh",
        in_ms(3_600_000),
    ));
    let session = create(&rig, "Run uname -a", FIRST).await;
    let calls = rig.stub.calls();
    assert_eq!(calls[0].args, "auth status --json");
    assert_eq!(
        std::path::Path::new(&calls[0].config_dir),
        rig.login.dir(),
        "the renewal runs in the login's folder"
    );
    assert!(!session.session_id.is_empty());
    for request in rig.fake.requests() {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer token-a-fresh")
        );
    }
}

#[tokio::test]
async fn a_login_for_another_account_is_refused() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    let before = rig.fake.requests().len();
    rig.login.set_account(&FakeAccount::b());
    rig.harness.provider_cloud().unwrap().bind_run(RunBinding {
        session: session.clone(),
        prompt_uuid: Some(SECOND.into()),
    });
    let cwd = zeron_proto::provider_cloud::claude_cwd(&session.session_id);
    let (controls, _steer, _interrupt) = controls();
    match rig.harness.run(request(&cwd, "hello"), controls).await {
        Err(HarnessError::NotSignedIn(message)) => {
            assert!(message.contains("different account"), "{message}")
        }
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("ran with another account's login"),
    }
    assert_eq!(
        rig.fake.requests().len(),
        before,
        "nothing reached the server"
    );
}

#[tokio::test]
async fn a_run_for_an_unbound_session_refuses() {
    let rig = rig().await;
    let cwd = zeron_proto::provider_cloud::claude_cwd("cse_unknown");
    assert!(
        rig.harness
            .run(request(&cwd, "hello"), controls().0)
            .await
            .is_err()
    );
    assert!(
        rig.harness
            .run(request("/home/me/project", "hello"), controls().0)
            .await
            .is_err()
    );
    assert!(rig.fake.requests().is_empty());
}

// ---- Controls (ticket #5) ----

const THIRD: &str = "3c9d7e2f-1a4b-4c8d-9e0f-a1b2c3d4e5f6";
const FOURTH: &str = "7e6d5c4b-3a29-4180-9f7e-6d5c4b3a2918";
const STOP: &str = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";

type Stream = BoxStream<'static, Result<AgentEvent, HarnessError>>;

/// Questions the run asked, each with the sender that answers it.
type Asked = Arc<Mutex<Vec<(Vec<UserInputQuestion>, oneshot::Sender<Vec<UserInputAnswer>>)>>>;

fn asking_controls() -> (RunControls, CancellationToken, Asked) {
    let (mut controls, _steer, token) = controls();
    let asked: Asked = Arc::default();
    let record = asked.clone();
    controls.request_input = Box::new(move |questions| {
        let (tx, rx) = oneshot::channel();
        record.lock().unwrap().push((questions, tx));
        rx
    });
    (controls, token, asked)
}

async fn wait(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The next question the run asks, with the sender that answers it.
async fn next_question(
    asked: &Asked,
) -> (Vec<UserInputQuestion>, oneshot::Sender<Vec<UserInputAnswer>>) {
    wait("a question", || !asked.lock().unwrap().is_empty()).await;
    asked.lock().unwrap().remove(0)
}

fn pick(questions: &[UserInputQuestion], label: &str) -> Vec<UserInputAnswer> {
    vec![UserInputAnswer {
        question_id: questions[0].id.clone(),
        labels: vec![label.into()],
    }]
}

fn message(id: &str, prompt: &str) -> SteerMessage {
    SteerMessage {
        prompt: prompt.into(),
        message_id: Some(id.into()),
        attachments: Vec::new(),
        config: None,
    }
}

fn event_posts(rig: &Rig) -> Vec<RecordedRequest> {
    rig.fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path.ends_with("/events"))
        .collect()
}

fn payloads_of(post: &RecordedRequest) -> Vec<Value> {
    post.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["payload"].clone())
        .collect()
}

/// Every event payload sent to the session, in order.
fn posted(rig: &Rig) -> Vec<Value> {
    event_posts(rig).iter().flat_map(payloads_of).collect()
}

fn of_type(payloads: Vec<Value>, kind: &str) -> Vec<Value> {
    payloads.into_iter().filter(|p| p["type"] == kind).collect()
}

/// A fresh run of `session`, opening with prompt `prompt_uuid`.
async fn start(
    rig: &Rig,
    session: &ProviderSession,
    prompt_uuid: &str,
    request: RunRequest,
    controls: RunControls,
) -> Stream {
    rig.harness.provider_cloud().unwrap().bind_run(RunBinding {
        session: session.clone(),
        prompt_uuid: Some(prompt_uuid.into()),
    });
    rig.harness.run(request, controls).await.unwrap()
}

fn done_status(events: &[AgentEvent]) -> Option<DoneStatus> {
    match events.last() {
        Some(AgentEvent::Done { status, .. }) => Some(*status),
        _ => None,
    }
}

fn count(events: &[AgentEvent], pred: impl Fn(&AgentEvent) -> bool) -> usize {
    events.iter().filter(|e| pred(e)).count()
}

#[tokio::test]
async fn a_steer_mid_turn_reaches_the_session_at_once_and_its_turn_ends_the_turn() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    let cwd = zeron_proto::provider_cloud::claude_cwd(&session.session_id);
    let (controls, steer, _interrupt) = controls();
    let mut stream = start(&rig, &session, FIRST, request(&cwd, "Run uname -a"), controls).await;
    turn(&mut stream).await;

    let (held, gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    rig.fake.queue_turn(Turn::text_zulu());
    steer
        .send(message(SECOND, "Reply with the single word ZULU."))
        .await
        .unwrap();
    gate.reached().await;
    steer.send(message(THIRD, "Say ZULU again.")).await.unwrap();
    // The steer reaches the session while the turn it steers still runs.
    wait("the steer sent", || {
        posted(&rig).iter().any(|p| p["uuid"] == THIRD)
    })
    .await;
    assert!(rig.fake.turn_playing(&session.session_id));
    gate.release();

    // The worker ran it right after the turn: the turn's result is a
    // boundary, and the steer's own result ends the turn.
    let events = turn(&mut stream).await;
    assert_eq!(
        count(&events, |e| matches!(e, AgentEvent::Steered { .. })),
        2
    );
    assert_eq!(text(&events), "ZULUZULU");
    assert_eq!(count(&events, |e| matches!(e, AgentEvent::Done { .. })), 1);
    assert_eq!(done_status(&events), Some(DoneStatus::Completed));
    let queued: Vec<Value> = rig
        .fake
        .stored_events(&session.session_id)
        .into_iter()
        .filter(|e| e.payload["type"] == "result")
        .map(|e| e.payload["queued_turn_count"].clone())
        .collect();
    assert_eq!(queued, vec![json!(0), json!(1), json!(0)]);
    let users = of_type(posted(&rig), "user");
    assert_eq!(users.len(), 2);
    assert_eq!(users[1]["message"]["content"], "Say ZULU again.");
}

#[tokio::test]
async fn stop_interrupts_the_turn_under_the_stop_commands_id_and_a_repeat_is_a_no_op() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    let sid = session.session_id.clone();
    let cwd = zeron_proto::provider_cloud::claude_cwd(&sid);
    let (controls, steer, interrupt) = controls();
    let turn_control = controls.turn.clone();
    let mut stream = start(&rig, &session, FIRST, request(&cwd, "Run uname -a"), controls).await;
    turn(&mut stream).await;

    let (held, _gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    steer
        .send(message(SECOND, "Reply with the single word ZULU."))
        .await
        .unwrap();
    _gate.reached().await;
    // A steer waiting behind the turn is stopped with it.
    steer.send(message(THIRD, "Then say more.")).await.unwrap();
    wait("the steer sent", || {
        posted(&rig).iter().any(|p| p["uuid"] == THIRD)
    })
    .await;

    turn_control.note_stop_command(STOP);
    interrupt.cancel();
    let events = turn(&mut stream).await;
    assert_eq!(done_status(&events), Some(DoneStatus::Interrupted));
    assert_eq!(text(&events), "ZULU");
    let ended = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("the run ends");
    assert!(ended.is_none());

    let interrupts = of_type(posted(&rig), "control_request");
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0]["uuid"], STOP);
    assert_eq!(interrupts[0]["request_id"], format!("interrupt-{STOP}"));
    assert_eq!(
        interrupts[0]["request"],
        json!({"subtype": "interrupt", "cancel_queued": true})
    );
    // The session stopped the turn, and the steer behind it never ran.
    assert!(!rig.fake.turn_playing(&sid));
    let stored = rig.fake.stored_events(&sid);
    let last = stored.last().unwrap();
    assert_eq!(last.payload["subtype"], "error_during_execution");
    assert_eq!(last.payload["terminal_reason"], "aborted_streaming");
    assert_eq!(last.payload["queued_turn_count"], 0);

    // The same Stop executed again (another host, a retry) sends the same
    // uuid; the provider ignores it, and the turn now playing runs on.
    let (held, gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    let (controls, _steer, interrupt) = crate::controls();
    let turn_control = controls.turn.clone();
    let mut stream = start(&rig, &session, FOURTH, request(&cwd, "ZULU once more"), controls).await;
    gate.reached().await;
    let stored_before = rig.fake.stored_events(&sid).len();
    turn_control.note_stop_command(STOP);
    interrupt.cancel();
    // No interrupted result comes: the run ends the turn itself.
    let events = turn(&mut stream).await;
    assert_eq!(done_status(&events), Some(DoneStatus::Interrupted));
    let interrupts = of_type(posted(&rig), "control_request");
    assert_eq!(interrupts.len(), 2);
    assert!(interrupts.iter().all(|p| p["uuid"] == STOP));
    assert_eq!(rig.fake.stored_events(&sid).len(), stored_before);
    assert!(rig.fake.turn_playing(&sid));
    gate.release();
}

#[tokio::test]
async fn prompt_answers_reach_the_session_and_the_turn_completes_with_them() {
    let rig = rig().await;
    let session = create_playing(&rig, Turn::prompts(), "Try the tools", FIRST).await;
    let sid = session.session_id.clone();
    let cwd = zeron_proto::provider_cloud::claude_cwd(&sid);
    let (controls, _interrupt, asked) = asking_controls();
    let mut supervised = request(&cwd, "Try the tools");
    supervised.access = AccessMode::Supervised;
    let mut stream = start(&rig, &session, FIRST, supervised, controls).await;
    let collect = tokio::spawn(async move { turn(&mut stream).await });

    // The MCP tool asks permission, as a local Claude Code chat asks it.
    let (questions, answer) = next_question(&asked).await;
    assert_eq!(
        questions[0].question,
        "Claude wants to use Read Wiki Structure. Allow it?"
    );
    answer.send(pick(&questions, "Yes")).unwrap();
    // Then AskUserQuestion, with its own options.
    let (questions, answer) = next_question(&asked).await;
    assert_eq!(questions[0].question, PROMPTS_QUESTION);
    assert_eq!(questions[0].options, vec!["red", "blue"]);
    answer.send(pick(&questions, "blue")).unwrap();

    let events = collect.await.unwrap();
    assert_eq!(done_status(&events), Some(DoneStatus::Completed));
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
    // Each answer went once, under the UUIDv5 of its prompt's request id.
    let answers = of_type(posted(&rig), "control_response");
    assert_eq!(answers.len(), 2);
    for (answer, request_id) in answers.iter().zip(&prompts) {
        assert_eq!(answer["uuid"], answer_uuid(request_id));
        assert_eq!(answer["response"]["request_id"], request_id.as_str());
    }
    // The turn went on with the answer: the question's tool result says so,
    // and the mirrored turn resolved both prompted tools.
    let picked = format!("\"{PROMPTS_QUESTION}\"=\"blue\"");
    assert!(
        rig.fake
            .stored_events(&sid)
            .iter()
            .any(|e| e.source == "worker" && e.payload.to_string().contains(&picked.replace('"', "\\\""))),
        "no tool result with the answer"
    );
    let results = count(&events, |e| matches!(e, AgentEvent::ToolResult { .. }));
    let calls = count(&events, |e| matches!(e, AgentEvent::ToolCall { .. }));
    assert!(results >= calls && calls >= 4, "{calls} calls, {results} results");
}

#[tokio::test]
async fn no_answer_is_sent_for_a_prompt_that_already_has_a_response() {
    let rig = rig().await;
    let session = create_playing(&rig, Turn::prompts(), "Try the tools", FIRST).await;
    let sid = session.session_id.clone();
    let cwd = zeron_proto::provider_cloud::claude_cwd(&sid);
    let (controls, _interrupt, asked) = asking_controls();
    let mut supervised = request(&cwd, "Try the tools");
    supervised.access = AccessMode::Supervised;
    let mut stream = start(&rig, &session, FIRST, supervised, controls).await;
    let collect = tokio::spawn(async move { turn(&mut stream).await });

    // While the permission question is up here, another device answers.
    let (questions, answer) = next_question(&asked).await;
    let permission = rig.fake.open_prompts(&sid)[0].clone();
    rig.fake.post_as_client(
        &sid,
        json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "type": "control_response",
            "response": {"subtype": "success", "request_id": permission,
                         "response": {"behavior": "allow", "updatedInput": {"repoName": "facebook/react"}}},
        }),
    );
    // The question closes here, and a late answer to it goes nowhere.
    wait("the answered question closed", || answer.is_closed()).await;
    let _ = answer.send(pick(&questions, "No"));

    let (questions, answer) = next_question(&asked).await;
    answer.send(pick(&questions, "blue")).unwrap();
    let events = collect.await.unwrap();
    assert_eq!(done_status(&events), Some(DoneStatus::Completed));

    let prompts = rig.fake.prompts(&sid);
    assert_eq!(prompts[0], permission);
    let answers = of_type(posted(&rig), "control_response");
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["response"]["request_id"], prompts[1].as_str());
    assert_eq!(
        rig.fake.answer(&sid, &permission).unwrap()["behavior"],
        "allow"
    );
    // The first answer wins: a later one is stored and dropped.
    let late = rig.fake.post_as_client(
        &sid,
        json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "type": "control_response",
            "response": {"subtype": "success", "request_id": prompts[1],
                         "response": {"behavior": "allow",
                                      "updatedInput": {"answers": {PROMPTS_QUESTION: "red"}}}},
        }),
    );
    assert_eq!(late["duplicate"], false);
    assert_eq!(
        rig.fake.answer(&sid, &prompts[1]).unwrap()["updatedInput"]["answers"][PROMPTS_QUESTION],
        "blue"
    );
}

#[tokio::test]
async fn model_permission_and_plan_changes_go_as_control_requests_ahead_of_their_message() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    let sid = session.session_id.clone();
    let cwd = zeron_proto::provider_cloud::claude_cwd(&sid);
    let (controls, steer, _interrupt) = controls();
    let mut stream = start(&rig, &session, FIRST, request(&cwd, "Run uname -a"), controls).await;
    turn(&mut stream).await;

    // Plan mode on a different model.
    let mut plan = request(&cwd, "Plan the change");
    plan.model = Some("claude-sonnet-5".into());
    plan.plan = true;
    rig.fake.queue_turn(Turn::text_zulu());
    steer
        .send(SteerMessage {
            config: Some(Box::new(plan.clone())),
            ..message(SECOND, "Plan the change")
        })
        .await
        .unwrap();
    turn(&mut stream).await;
    let sent = payloads_of(event_posts(&rig).last().unwrap());
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(sent[0]["type"], "control_request");
    assert_eq!(sent[0]["uuid"], control_uuid(SECOND, "set_model"));
    assert_eq!(
        sent[0]["request"],
        json!({"subtype": "set_model", "model": "claude-sonnet-5"})
    );
    assert_eq!(sent[1]["uuid"], control_uuid(SECOND, "set_permission_mode"));
    assert_eq!(
        sent[1]["request"],
        json!({"subtype": "set_permission_mode", "mode": "plan"})
    );
    assert_eq!(sent[2]["uuid"], SECOND);
    assert_eq!(rig.fake.model(&sid), Some(json!("claude-sonnet-5")));
    assert_eq!(rig.fake.permission_mode(&sid).as_deref(), Some("plan"));

    // Out of plan mode, accepting edits: the mode alone changes.
    let mut edits = plan.clone();
    edits.plan = false;
    edits.access = AccessMode::AutoAcceptEdits;
    rig.fake.queue_turn(Turn::text_zulu());
    steer
        .send(SteerMessage {
            config: Some(Box::new(edits)),
            ..message(THIRD, "Go ahead")
        })
        .await
        .unwrap();
    turn(&mut stream).await;
    let sent = payloads_of(event_posts(&rig).last().unwrap());
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0]["uuid"], control_uuid(THIRD, "set_permission_mode"));
    assert_eq!(sent[0]["request"]["mode"], "acceptEdits");
    assert_eq!(sent[1]["uuid"], THIRD);
    assert_eq!(rig.fake.permission_mode(&sid).as_deref(), Some("acceptEdits"));

    // The plan command executed again by a device that knows nothing of the
    // session (another host, or this one after a restart) sends the same
    // uuids, and the provider ignores every repeat.
    let stored = rig.fake.stored_events(&sid).len();
    let posts = event_posts(&rig).len();
    let other = ClaudeCloudHarness::new(ClaudeCloudConfig {
        base_url: rig.fake.base_url().to_owned(),
        login: ClaudeConfigDir::at(rig.login.dir()),
        claude_executable: Some(rig.stub.path().to_owned()),
    });
    other.provider_cloud().unwrap().bind_run(RunBinding {
        session: session.clone(),
        prompt_uuid: Some(SECOND.into()),
    });
    let (controls, _steer, _interrupt) = crate::controls();
    let again = other.run(plan, controls).await.unwrap();
    wait("the repeat sent", || event_posts(&rig).len() > posts).await;
    let sent = payloads_of(event_posts(&rig).last().unwrap());
    let uuids: Vec<&str> = sent.iter().map(|p| p["uuid"].as_str().unwrap()).collect();
    assert_eq!(
        uuids,
        vec![
            control_uuid(SECOND, "set_model").as_str(),
            control_uuid(SECOND, "set_permission_mode").as_str(),
            SECOND,
        ]
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.fake.stored_events(&sid).len(), stored);
    assert_eq!(rig.fake.permission_mode(&sid).as_deref(), Some("acceptEdits"));
    assert!(!rig.fake.turn_playing(&sid));
    drop(again);
}

#[tokio::test]
async fn a_message_sent_from_elsewhere_shows_and_this_devices_own_echoes_dont() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    let sid = session.session_id.clone();
    let cwd = zeron_proto::provider_cloud::claude_cwd(&sid);
    let (controls, steer, _interrupt) = controls();
    let mut stream = start(&rig, &session, FIRST, request(&cwd, "Run uname -a"), controls).await;
    let first = turn(&mut stream).await;
    // The opening prompt's echo (it rode the create request) shows nothing.
    assert_eq!(
        count(&first, |e| matches!(e, AgentEvent::UserMessage { .. })),
        0
    );

    // Typed on claude.ai: the session runs a turn for it, and the chat shows
    // the message as the person's.
    rig.fake.queue_turn(Turn::text_zulu());
    rig.fake.post_as_client(
        &sid,
        json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": "",
            "type": "user",
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": "Typed on claude.ai"},
        }),
    );
    let events = turn(&mut stream).await;
    assert!(
        matches!(events.first(), Some(AgentEvent::UserMessage { text }) if text == "Typed on claude.ai"),
        "{events:?}"
    );
    assert_eq!(text(&events), "ZULU");
    assert_eq!(done_status(&events), Some(DoneStatus::Completed));

    // A message sent from here: its echo shows nothing.
    rig.fake.queue_turn(Turn::text_zulu());
    steer.send(message(SECOND, "From Zeron")).await.unwrap();
    let events = turn(&mut stream).await;
    assert_eq!(
        count(&events, |e| matches!(e, AgentEvent::UserMessage { .. })),
        0
    );
}

#[tokio::test]
async fn a_stream_that_keeps_dropping_is_reconnected_a_bounded_number_of_times_with_backoff() {
    let rig = rig().await;
    let session = create(&rig, "Run uname -a", FIRST).await;
    let sid = session.session_id.clone();
    let cwd = zeron_proto::provider_cloud::claude_cwd(&sid);
    let (controls, steer, _interrupt) = controls();
    let mut stream = start(&rig, &session, FIRST, request(&cwd, "Run uname -a"), controls).await;
    turn(&mut stream).await;
    // A held turn: the stream drops while it runs.
    let (held, gate) = Turn::text_zulu().hold_after(is_text_delta);
    rig.fake.queue_turn(held);
    steer.send(message(SECOND, "ZULU, slowly")).await.unwrap();
    gate.reached().await;
    let streams = |rig: &Rig| {
        rig.fake
            .requests()
            .iter()
            .filter(|r| r.method == "GET" && r.path.ends_with("/events/stream"))
            .count()
    };

    // One drop: the run reconnects and carries on.
    let opened = streams(&rig);
    let before = opened;
    rig.fake.drop_streams(&sid);
    wait("the reconnect", || streams(&rig) == before + 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // From here every reconnect fails: the run tries a bounded number of
    // times, further apart each time, then ends errored.
    rig.fake.inject(zeron_provider_fake::Fault {
        method: Some("GET"),
        path_contains: "/events/stream".into(),
        status: 503,
        times: 100,
    });
    let dropped_at = tokio::time::Instant::now();
    rig.fake.drop_streams(&sid);
    let events = tokio::time::timeout(Duration::from_secs(15), async {
        let mut out = Vec::new();
        while let Some(event) = stream.next().await {
            out.push(event.unwrap());
        }
        out
    })
    .await
    .expect("the run ends");
    // No turn ended and the connection never stayed up long, so both drops
    // share one budget: five reconnects in all, the last four failing after
    // 200 + 400 + 800 + 1600 ms.
    assert!(dropped_at.elapsed() >= Duration::from_millis(2500));
    assert_eq!(streams(&rig) - opened, 5, "reconnect attempts");
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::Error { message } if message.contains("connection to the Claude cloud session was lost"))));
    assert_eq!(done_status(&events), Some(DoneStatus::Errored));
    gate.release();
}
