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
//! `CLAUDE_CODE_EXECUTABLE` and `ZERON_CLAUDE_CLOUD_API`. Runs until killed.

use zeron_provider_fake::{ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn};

#[tokio::main]
async fn main() {
    let folder = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zeron-provider-fake"));
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    for _ in 0..20 {
        fake.queue_turn(Turn::text_zulu());
    }
    let account = FakeAccount::a();
    let login = ClaudeLogin::create(folder.join("login"), &account, "fake-token-a");
    let stub = StubClaude::install(folder.join("bin"), &account);
    println!("ZERON_CLAUDE_CLOUD_API={}", fake.base_url());
    println!("CLAUDE_CONFIG_DIR={}", login.dir().display());
    println!("CLAUDE_CODE_EXECUTABLE={}", stub.path().display());
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let created = fake.created_sessions();
        if !created.is_empty() {
            println!("sessions created: {created:?}");
        }
    }
}
