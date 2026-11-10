//! Serve the fake Claude Code session server for a hand check of the app
//! (never a real Anthropic call):
//!
//! ```sh
//! cargo run -p zeron-provider-fake --example serve -- <folder>
//! ```
//!
//! Writes a fake login (account `a@example.test`) to `<folder>/login`, a stub
//! `claude` to `<folder>/bin`, queues a few text turns, and prints the
//! environment to launch the app with: `CLAUDE_CONFIG_DIR`,
//! `CLAUDE_CODE_EXECUTABLE` and `ZERON_CLAUDE_CLOUD_API`. Two sessions
//! "started on claude.ai" are already there for the sidebar: one finished,
//! one waiting on a question; an old one (10 days quiet) stays out of it.
//! Touching
//! `<folder>/reject-token` makes the server refuse that login from then on.
//! A second account, `b@example.test`, has a session of its own; "Add
//! account" signs in as it (the stub's `auth login`). Touching
//! `<folder>/switch-login` switches the device's login to account B (as an
//! account switcher would), after which "Add account" signs in as A.
//! Runs until killed.

use zeron_provider_fake::{ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn};

#[tokio::main]
async fn main() {
    let folder = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zeron-provider-fake"));
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    fake.own_token("fake-token-a", "a");
    fake.own_token("fake-token-b", "b");
    // Sessions started outside Zeron, for the sidebar.
    fake.queue_turn(Turn::text_zulu());
    fake.start_elsewhere("Started on claude.ai", "Reply with the single word ZULU.");
    fake.queue_turn(Turn::prompts());
    fake.start_elsewhere("Waiting for you on claude.ai", "Ask me which color I like.");
    fake.queue_turn(Turn::text_zulu());
    let old = fake.start_elsewhere("Old and quiet", "Reply with the single word ZULU.");
    let long_ago = (chrono::Utc::now() - chrono::Duration::days(10))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    fake.set_times(&old, &long_ago, &long_ago);
    fake.queue_turn(Turn::text_zulu());
    fake.start_elsewhere_as("b", "B's session on claude.ai", "Reply with the single word ZULU.");
    for _ in 0..20 {
        fake.queue_turn(Turn::text_zulu());
    }
    let account = FakeAccount::a();
    let login = ClaudeLogin::create(folder.join("login"), &account, "fake-token-a");
    let stub = StubClaude::install(folder.join("bin"), &account);
    stub.sign_in_as(&FakeAccount::b(), "fake-token-b");
    println!("ZERON_CLAUDE_CLOUD_API={}", fake.base_url());
    println!("CLAUDE_CONFIG_DIR={}", login.dir().display());
    println!("CLAUDE_CODE_EXECUTABLE={}", stub.path().display());
    // Touch `<folder>/reject-token` to refuse the fake login from then on
    // (a persistent 401: the chat shows "Sign in again").
    let reject = folder.join("reject-token");
    let mut rejected = false;
    let switch = folder.join("switch-login");
    let mut switched = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if !switched && switch.exists() {
            login.set_account(&FakeAccount::b());
            login.set_token("fake-token-b", zeron_provider_fake::in_ms(3_600_000));
            stub.sign_in_as(&account, "fake-token-a");
            switched = true;
            println!("the device's login is account B now");
        }
        if !rejected && reject.exists() {
            fake.reject_token("fake-token-a");
            rejected = true;
            println!("rejecting the fake login's token");
        }
        let created = fake.created_sessions();
        if !created.is_empty() {
            println!("sessions created: {created:?}");
        }
    }
}
