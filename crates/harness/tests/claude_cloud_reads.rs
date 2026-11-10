//! Reads the archive and create paths rely on (tickets #10, #11), against
//! the fake Claude Code session server:
//!
//! - one session read alone, whatever its age, in whatever shape the
//!   single-session endpoint answers (its real shape isn't confirmed: the
//!   live check could read it but not decode it), falling back to the list;
//! - a connection that couldn't be made (refused, or reset during its
//!   handshake) sent nothing, so it is tried again, briefly, for every call:
//!   a create too, which is still made exactly once.

use std::time::Duration;

use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_harness::provider_cloud::{CreateSession, ProviderCloud};
use zeron_proto::ProviderSession;
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, SessionRead, StubClaude, Turn,
};

struct Rig {
    _dir: tempfile::TempDir,
    _login: ClaudeLogin,
    _stub: StubClaude,
    harness: ClaudeCloudHarness,
}

fn rig_at(base_url: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let login = ClaudeLogin::create(dir.path().join("login"), &FakeAccount::a(), "token-a-1");
    let stub = StubClaude::install(dir.path().join("bin"), &FakeAccount::a());
    let harness = ClaudeCloudHarness::new(ClaudeCloudConfig {
        base_url: base_url.to_owned(),
        login: ClaudeConfigDir::at(login.dir()),
        claude_executable: Some(stub.path().to_owned()),
    })
    .expect("harness builds");
    Rig {
        _dir: dir,
        _login: login,
        _stub: stub,
        harness,
    }
}

fn session_of(id: &str) -> ProviderSession {
    let a = FakeAccount::a();
    ProviderSession {
        provider: "claude".into(),
        session_id: id.into(),
        account_uuid: a.account_uuid,
        account_email: a.email,
        org_id: a.org_id,
    }
}

async fn fake_with_old_archived_session() -> (FakeSessionServer, String) {
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    fake.queue_turn(Turn::text_zulu());
    let sid = fake.start_elsewhere("Long ago", "Reply with the single word ZULU.");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fake.turn_playing(&sid) {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let long_ago = (chrono::Utc::now() - chrono::Duration::days(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    fake.set_times(&sid, &long_ago, &long_ago);
    fake.archive(&sid);
    (fake, sid)
}

#[tokio::test(flavor = "multi_thread")]
async fn one_session_reads_alone_in_any_shape_the_endpoint_answers() {
    let (fake, sid) = fake_with_old_archived_session().await;
    let rig = rig_at(fake.base_url());
    for shape in [SessionRead::Plain, SessionRead::Wrapped] {
        fake.session_read(shape);
        let lists = fake.list_reads().len();
        let read = rig
            .harness
            .read_session(&session_of(&sid))
            .await
            .unwrap_or_else(|e| panic!("{shape:?}: {e}"));
        assert!(read.archived, "{shape:?}");
        assert_eq!(read.session.session_id, sid);
        assert_eq!(fake.list_reads().len(), lists, "{shape:?}: no list read needed");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_single_read_falls_back_to_the_list_however_old_the_session() {
    let (fake, sid) = fake_with_old_archived_session().await;
    let rig = rig_at(fake.base_url());
    fake.session_read(SessionRead::Unreadable);
    let read = rig
        .harness
        .read_session(&session_of(&sid))
        .await
        .expect("found in the list");
    assert!(read.archived);
    assert!(fake.session_reads() >= 1, "the single read was tried first");
    assert!(!fake.list_reads().is_empty(), "then the list");
}

/// An address nothing listens on yet: connections to it are refused.
async fn refused_address() -> String {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_refused_at_first_is_tried_again() {
    let addr = refused_address().await;
    let rig = rig_at(&format!("http://{addr}"));
    // The server comes up a moment after the first try was refused.
    let serving = tokio::spawn({
        let addr = addr.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let fake = FakeSessionServer::start_at(&addr).await;
            fake.seed_session(SeedSession::environment("env_fake0001"));
            fake
        }
    });
    let listed = rig
        .harness
        .list_sessions(chrono::Utc::now() - chrono::Duration::days(7))
        .await;
    let fake = serving.await.unwrap();
    assert!(listed.is_ok(), "{listed:?}");
    assert_eq!(fake.list_reads().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_whose_connection_was_refused_is_made_once() {
    let addr = refused_address().await;
    let rig = rig_at(&format!("http://{addr}"));
    let serving = tokio::spawn({
        let addr = addr.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let fake = FakeSessionServer::start_at(&addr).await;
            fake.seed_session(SeedSession::environment("env_fake0001"));
            fake.queue_turn(Turn::text_zulu());
            fake
        }
    });
    let created = rig
        .harness
        .create_session(CreateSession {
            prompt: "Reply with the single word ZULU.".into(),
            prompt_uuid: uuid::Uuid::new_v4().to_string(),
            model: None,
            title: None,
            source: None,
        })
        .await;
    let fake = serving.await.unwrap();
    assert!(created.is_ok(), "{created:?}");
    assert_eq!(fake.created_sessions().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_never_made_is_a_plain_failure_in_bounded_time() {
    let addr = refused_address().await;
    let rig = rig_at(&format!("http://{addr}"));
    let started = std::time::Instant::now();
    let created = rig
        .harness
        .create_session(CreateSession {
            prompt: "Reply with the single word ZULU.".into(),
            prompt_uuid: uuid::Uuid::new_v4().to_string(),
            model: None,
            title: None,
            source: None,
        })
        .await;
    // Nothing went out: not "uncertain", and given up within the retries.
    assert!(
        !matches!(created, Err(zeron_harness::provider_cloud::ProviderCloudError::Uncertain(_))),
        "{created:?}"
    );
    assert!(created.is_err());
    // Four tries and 2.5 s of waits (Windows takes about 2 s to report each
    // refused connect).
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}
