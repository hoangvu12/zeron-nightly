//! `claude-code-cloud` against the fake session server: create, mirror a
//! recorded turn, send a follow-up, honest identity on every request, token
//! renewal through the stub `claude`, and the account check.

use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_harness::provider_cloud::{CreateSession, RunBinding};
use zeron_harness::{CancellationToken, Harness, HarnessError, RunControls, SteerMessage};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, ProviderSession, RunRequest, SandboxLevel, ToolCall,
};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn, in_ms,
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
    })
    .expect("harness builds");
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
    rig.fake.queue_turn(Turn::bash_tool());
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
