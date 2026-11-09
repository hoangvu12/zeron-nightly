//! `claude-code-cloud` logins: renewal through the stub `claude`
//! (`auth status --json`), single-flight per folder, the 401 retry, the
//! sign-in states and the kill switch, the torn-file re-read, and that Zeron
//! never writes the login folder.
//!
//! Asserted only from outside: what the fake server received, how many
//! times the stub ran, the published status, and the folder's files.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::oneshot;
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_harness::provider_cloud::{
    CreateSession, ProviderChatState, ProviderCloudError, ProviderCloudStatus, RunBinding,
};
use zeron_harness::{CancellationToken, Harness, HarnessError, RunControls};
use zeron_proto::{HarnessId, ProviderSession, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, in_ms,
};

struct Rig {
    _dir: tempfile::TempDir,
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
    harness: Arc<ClaudeCloudHarness>,
}

impl Rig {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeSessionServer::start().await;
        fake.seed_session(SeedSession::environment("env_fake0001"));
        let login = ClaudeLogin::create(dir.path().join("login"), &FakeAccount::a(), "token-a-1");
        let stub = StubClaude::install(dir.path().join("bin"), &FakeAccount::a());
        let harness = Arc::new(Self::harness_for(&fake, &login, &stub));
        Self {
            _dir: dir,
            fake,
            login,
            stub,
            harness,
        }
    }

    /// Another harness (another engine in this process) on the same login.
    fn harness_for(
        fake: &FakeSessionServer,
        login: &ClaudeLogin,
        stub: &StubClaude,
    ) -> ClaudeCloudHarness {
        ClaudeCloudHarness::new(ClaudeCloudConfig {
            base_url: fake.base_url().to_owned(),
            login: ClaudeConfigDir::at(login.dir()),
            claude_executable: Some(stub.path().to_owned()),
        })
        .expect("harness builds")
    }

    async fn create(&self) -> Result<ProviderSession, ProviderCloudError> {
        create_with(&self.harness).await
    }

    fn status(&self) -> ProviderCloudStatus {
        self.harness
            .provider_cloud()
            .unwrap()
            .status()
            .borrow()
            .clone()
    }

    /// What account "a"'s chats show (outer `None`: not tried yet).
    fn state_of_a(&self) -> Option<Option<ProviderChatState>> {
        self.status()
            .accounts
            .get(&FakeAccount::a().account_uuid)
            .cloned()
    }

    fn authorizations(&self) -> Vec<String> {
        self.fake
            .requests()
            .iter()
            .map(|r| r.header("authorization").unwrap_or_default().to_owned())
            .collect()
    }
}

async fn create_with(harness: &ClaudeCloudHarness) -> Result<ProviderSession, ProviderCloudError> {
    harness
        .provider_cloud()
        .unwrap()
        .create_session(CreateSession {
            prompt: "hello".into(),
            prompt_uuid: uuid::Uuid::new_v4().to_string(),
            model: None,
            title: None,
        })
        .await
}

fn sign_in_again() -> ProviderChatState {
    ProviderChatState::SignInAgain {
        email: FakeAccount::a().email,
    }
}

fn fresh(token: &str) -> String {
    ClaudeLogin::credentials_json(token, in_ms(3_600_000))
}

fn request(cwd: &str) -> RunRequest {
    RunRequest {
        mcp: None,
        prompt: "hello".into(),
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

fn controls() -> RunControls {
    RunControls {
        realtime: None,
        execution_lease: None,
        request_input: Box::new(|_| oneshot::channel().1),
        steering: tokio::sync::mpsc::channel(8).1,
        interrupt: CancellationToken::new(),
        turn: Default::default(),
    }
}

/// Start a run for `session` on `harness` (the stream is opened, then
/// dropped): `Err` when the login can't act for the session's account.
async fn open_run(
    harness: &ClaudeCloudHarness,
    session: &ProviderSession,
) -> Result<(), HarnessError> {
    harness.provider_cloud().unwrap().bind_run(RunBinding {
        session: session.clone(),
        prompt_uuid: None,
    });
    let cwd = zeron_proto::provider_cloud::claude_cwd(&session.session_id);
    let mut empty = request(&cwd);
    empty.prompt.clear();
    harness.run(empty, controls()).await.map(drop)
}

/// Every file in `dir` with its bytes and modification time.
fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            (
                entry.file_name().to_string_lossy().into_owned(),
                (
                    std::fs::read(&path).unwrap(),
                    std::fs::metadata(&path).unwrap().modified().unwrap(),
                ),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn chats_needing_a_renewal_at_once_share_one_auth_status_run() {
    let rig = Rig::new().await;
    rig.login.set_token("token-a-old", in_ms(60_000));
    rig.stub.renew_with(&fresh("token-a-fresh"));
    // Slow enough that every caller arrives while the run is in flight.
    rig.stub.set_delay(Duration::from_secs(1));
    // A second engine in this process, on the same login folder.
    let other = Arc::new(Rig::harness_for(&rig.fake, &rig.login, &rig.stub));

    let mut creating = Vec::new();
    for harness in [
        &rig.harness,
        &other,
        &rig.harness,
        &other,
        &rig.harness,
        &other,
    ] {
        let harness = harness.clone();
        creating.push(tokio::spawn(async move { create_with(&harness).await }));
    }
    for created in creating {
        created.await.unwrap().expect("created after the renewal");
    }

    assert_eq!(rig.stub.call_count(), 1, "{:?}", rig.stub.calls());
    assert_eq!(rig.fake.created_sessions().len(), 6);
    assert!(
        rig.authorizations()
            .iter()
            .all(|a| a == "Bearer token-a-fresh"),
        "{:?}",
        rig.authorizations()
    );
    assert_eq!(rig.state_of_a(), Some(None));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_renews_the_login_once_then_retries_with_the_new_token() {
    let rig = Rig::new().await;
    rig.fake.reject_token("token-a-1");
    rig.stub.renew_with(&fresh("token-a-2"));

    rig.create().await.expect("created after the retry");

    assert_eq!(rig.stub.call_count(), 1);
    assert_eq!(
        rig.authorizations(),
        vec!["Bearer token-a-1", "Bearer token-a-2", "Bearer token-a-2"],
        "the refused list, its retry, then the create"
    );
    assert_eq!(rig.fake.created_sessions().len(), 1);
    // Later requests keep using the renewed token, with no more renewals.
    rig.create().await.unwrap();
    assert_eq!(rig.stub.call_count(), 1);
    assert_eq!(rig.authorizations().last().unwrap(), "Bearer token-a-2");
    assert_eq!(rig.state_of_a(), Some(None));
    assert_eq!(rig.status().disabled, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_that_survives_a_renewal_asks_to_sign_in_again_and_turns_the_feature_off() {
    let rig = Rig::new().await;
    rig.fake.reject_token("token-a-1");
    // The CLI answers but the token stays the refused one.

    match rig.create().await {
        Err(ProviderCloudError::SignInRequired(message)) => {
            assert!(message.contains("Sign in again"), "{message}")
        }
        other => panic!("expected a sign-in prompt, got {other:?}"),
    }
    assert_eq!(rig.stub.call_count(), 1, "one renewal, never a loop");
    assert_eq!(rig.authorizations().len(), 2, "the request and one retry");
    assert!(rig.fake.created_sessions().is_empty());
    assert_eq!(rig.state_of_a(), Some(Some(sign_in_again())));
    let reason = rig
        .status()
        .disabled
        .expect("the feature turned itself off");
    assert!(reason.contains("refused the login"), "{reason}");

    // Off means off: nothing more reaches the server, and no renewal runs.
    match rig.create().await {
        Err(ProviderCloudError::Disabled(why)) => assert_eq!(why, reason),
        other => panic!("expected the feature off, got {other:?}"),
    }
    assert_eq!(rig.authorizations().len(), 2);
    assert_eq!(rig.stub.call_count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_logged_out_login_asks_to_sign_in_again_without_turning_the_feature_off() {
    let rig = Rig::new().await;
    rig.login.set_token("token-a-old", in_ms(60_000));
    rig.stub.log_out();

    let error = rig.create().await.unwrap_err();
    assert!(
        matches!(error, ProviderCloudError::SignInRequired(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("Sign in again"), "{error}");
    assert_eq!(rig.stub.call_count(), 1);
    assert!(
        rig.fake.requests().is_empty(),
        "nothing sent with a dying token"
    );
    assert_eq!(rig.state_of_a(), Some(Some(sign_in_again())));

    // Logged out behind a 401 too: still a sign-in, never the kill switch.
    rig.login.set_token("token-a-1", in_ms(3_600_000));
    rig.fake.reject_token("token-a-1");
    assert!(matches!(
        rig.create().await,
        Err(ProviderCloudError::SignInRequired(_))
    ));
    assert_eq!(rig.stub.call_count(), 2);
    assert_eq!(rig.authorizations().len(), 1, "no retry without a renewal");
    assert_eq!(rig.status().disabled, None);

    // Signed in again (the CLI renewed): the state clears on the next request.
    rig.stub.report_account(&FakeAccount::a());
    rig.stub.renew_with(&fresh("token-a-2"));
    rig.create().await.expect("works once signed in");
    assert_eq!(rig.state_of_a(), Some(None));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renewal_reporting_another_account_means_this_device_cant_host() {
    let rig = Rig::new().await;
    rig.login.set_token("token-a-old", in_ms(60_000));
    rig.stub.renew_with(&fresh("token-a-fresh"));
    rig.stub.report_account(&FakeAccount::b());

    match rig.create().await {
        Err(ProviderCloudError::AccountMismatch(message)) => {
            assert!(message.contains("Sign in as a@example.test"), "{message}")
        }
        other => panic!("expected an account mismatch, got {other:?}"),
    }
    assert_eq!(rig.stub.call_count(), 1);
    assert!(
        rig.fake.requests().is_empty(),
        "nothing sent for the wrong account"
    );
    assert_eq!(
        rig.state_of_a(),
        Some(Some(ProviderChatState::SignInAs {
            email: FakeAccount::a().email
        }))
    );
    assert_eq!(
        rig.status().disabled,
        None,
        "a mismatch never trips the switch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_without_a_login_for_the_chats_account_asks_to_sign_in_as_it() {
    let rig = Rig::new().await;
    let session = rig.create().await.unwrap();
    open_run(&rig.harness, &session).await.expect("runs as a");
    assert_eq!(rig.state_of_a(), Some(None));
    let before = rig.fake.requests().len();

    // The device's login now belongs to someone else.
    rig.login.set_account(&FakeAccount::b());
    let error = open_run(&rig.harness, &session).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Sign in as a@example.test to continue"),
        "{error}"
    );
    assert_eq!(rig.fake.requests().len(), before, "nothing sent as b");
    assert_eq!(
        rig.state_of_a(),
        Some(Some(ProviderChatState::SignInAs {
            email: FakeAccount::a().email
        }))
    );
    assert_eq!(rig.stub.call_count(), 0, "a file read decides, no CLI run");

    // Back to a: the chat works again and the state clears.
    rig.login.set_account(&FakeAccount::a());
    open_run(&rig.harness, &session)
        .await
        .expect("runs as a again");
    assert_eq!(rig.state_of_a(), Some(None));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renewal_that_overstays_ten_seconds_gives_up() {
    let rig = Rig::new().await;
    rig.login.set_token("token-a-old", in_ms(60_000));
    rig.stub.renew_with(&fresh("token-a-fresh"));
    rig.stub.set_delay(Duration::from_secs(14));

    let started = Instant::now();
    let error = rig.create().await.unwrap_err();
    let took = started.elapsed();
    assert!(
        matches!(error, ProviderCloudError::SignInRequired(_)),
        "{error:?}"
    );
    assert!(took >= Duration::from_secs(10), "gave up early: {took:?}");
    assert!(
        took < Duration::from_secs(13),
        "waited past the limit: {took:?}"
    );
    assert_eq!(rig.stub.call_count(), 1);
    assert!(rig.fake.requests().is_empty());
    assert_eq!(rig.state_of_a(), Some(Some(sign_in_again())));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_file_that_doesnt_parse_is_read_once_more() {
    let rig = Rig::new().await;
    let file = rig.login.credentials_file();
    std::fs::write(&file, "{\"claudeAiOauth\": {\"accessT").unwrap();
    let fixed = file.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        std::fs::write(&fixed, fresh("token-a-whole")).unwrap();
    });
    rig.create()
        .await
        .expect("read again after the CLI finished writing");
    writer.await.unwrap();
    assert!(
        rig.authorizations()
            .iter()
            .all(|a| a == "Bearer token-a-whole")
    );

    // Still torn on the second read: refused, nothing sent, no guess, and
    // not a reason to ask for a sign-in.
    let sent = rig.fake.requests().len();
    std::fs::write(&file, "not json").unwrap();
    let error = rig.create().await.unwrap_err();
    assert!(error.to_string().contains("could not be read"), "{error}");
    assert_eq!(rig.fake.requests().len(), sent);
    assert_eq!(rig.stub.call_count(), 0);
    assert_eq!(rig.state_of_a(), Some(None));
}

#[tokio::test(flavor = "multi_thread")]
async fn zeron_never_writes_the_login_folder() {
    // A login near expiry that the CLI won't (or can't) renew.
    let rig = Rig::new().await;
    rig.login.set_token("token-a-old", in_ms(60_000));
    let before = snapshot(rig.login.dir());
    assert!(
        rig.create().await.is_err(),
        "still expiring after the renewal"
    );
    rig.stub.report_account(&FakeAccount::b());
    assert!(rig.create().await.is_err(), "mismatch");
    rig.stub.log_out();
    assert!(rig.create().await.is_err(), "logged out");
    assert_eq!(rig.stub.call_count(), 3);
    assert_eq!(
        snapshot(rig.login.dir()),
        before,
        "the login folder changed"
    );

    // A working login: create, run, then a 401 that survives the renewal.
    let rig = Rig::new().await;
    let before = snapshot(rig.login.dir());
    let session = rig.create().await.unwrap();
    open_run(&rig.harness, &session).await.unwrap();
    rig.fake.reject_token("token-a-1");
    assert!(rig.create().await.is_err());
    assert!(rig.status().disabled.is_some());
    assert_eq!(rig.stub.call_count(), 1);
    assert_eq!(
        snapshot(rig.login.dir()),
        before,
        "the login folder changed"
    );
}
