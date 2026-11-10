//! `ZERON_CLAUDE_CLOUD_API` and the environment switch
//! (`ZERON_CLAUDE_CLOUD_SESSIONS=1`): the production registry builds the
//! Claude cloud harness on a loopback override, and registers none when the
//! override names anything else, never falling back to the real API.
//!
//! Its own test binary: it sets process-wide environment variables, so its
//! cases run one after another in a single test.

use std::time::Duration;

use zeron_engine::registry::registry_for_platform;
use zeron_harness::claude_cloud::ENABLE_ENV;
use zeron_proto::HarnessId;
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};

const API_ENV: &str = zeron_engine::claude_cloud_sessions::API_OVERRIDE_ENV;

fn set(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: the only test in this binary; nothing else reads the
    // environment concurrently.
    unsafe { std::env::set_var(key, value) };
}

#[tokio::test(flavor = "multi_thread")]
async fn the_environment_switch_uses_a_loopback_override_and_refuses_any_other() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    fake.queue_turn(Turn::text_zulu());
    let sid = fake.start_elsewhere("Started on claude.ai", "Reply with the single word ZULU.");
    let login = ClaudeLogin::create(tmp.path().join("login"), &FakeAccount::a(), "token-a-1");
    let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
    set(ENABLE_ENV, "1");
    set("CLAUDE_CONFIG_DIR", login.dir());
    set("CLAUDE_CODE_EXECUTABLE", stub.path());

    // A loopback override: the harness talks to it.
    set(API_ENV, fake.base_url());
    let registry = registry_for_platform("windows");
    let harness = registry
        .resolve(HarnessId::ClaudeCodeCloud)
        .expect("the switch registers the harness");
    let listed = tokio::time::timeout(
        Duration::from_secs(20),
        harness
            .provider_cloud()
            .expect("a provider cloud")
            .list_sessions(chrono::Utc::now() - chrono::Duration::days(7)),
    )
    .await
    .expect("the list answers")
    .expect("the fake server lists");
    assert!(listed.iter().any(|s| s.session.session_id == sid));
    assert_eq!(
        fake.list_reads().len(),
        1,
        "the read went to the fake server"
    );

    // `::1` is loopback too.
    set(API_ENV, "http://[::1]:9");
    assert!(registry_for_platform("windows").is_registered(HarnessId::ClaudeCodeCloud));

    // Anything else: no harness at all, rather than the production API.
    for rejected in [
        "http://example.test",
        "https://api.anthropic.com",
        "not a url",
    ] {
        set(API_ENV, rejected);
        assert!(
            !registry_for_platform("windows").is_registered(HarnessId::ClaudeCodeCloud),
            "{rejected}"
        );
    }
}
