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
//! `CLAUDE_CODE_EXECUTABLE` and `ZERON_CLAUDE_CLOUD_API`. Touching
//! `<folder>/reject-token` makes the server refuse that login from then on.
//! One-shot control files (read once, then removed): `start-elsewhere` (a
//! session started on claude.ai), `archive` / `resume` (holding a session
//! id: archived on claude.ai / resumed on the phone), `lose-create` (the
//! next create is made but its answer lost) and `fail-create` (the next
//! create fails with a 502 before making anything). Runs until killed.

use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, Fault, SeedSession, StubClaude, Turn,
};

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
    // Touch `<folder>/reject-token` to refuse the fake login from then on
    // (a persistent 401: the chat shows "Sign in again").
    let reject = folder.join("reject-token");
    let mut rejected = false;
    let mut last_report = String::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if !rejected && reject.exists() {
            fake.reject_token("fake-token-a");
            rejected = true;
            println!("rejecting the fake login's token");
        }
        // One-shot controls (ticket #10/#11 hand checks): each file is acted
        // on once, then removed. Session-id files hold the session's id.
        if take(&folder.join("start-elsewhere")).is_some() {
            let id = fake.start_elsewhere("Started on claude.ai", "Reply with the single word ZULU.");
            println!("started elsewhere: {id}");
        }
        if let Some(id) = take(&folder.join("archive")) {
            fake.archive(&id);
            println!("archived on claude.ai: {id}");
        }
        if let Some(id) = take(&folder.join("resume")) {
            fake.resume_elsewhere(&id, "Picking this up again on my phone.");
            println!("resumed on the phone: {id}");
        }
        if take(&folder.join("lose-create")).is_some() {
            fake.lose_create_answers(1);
            println!("the next create's answer will be lost");
        }
        if take(&folder.join("fail-create")).is_some() {
            fake.inject(Fault {
                method: Some("POST"),
                path_contains: "/v1/code/sessions".into(),
                status: 502,
                times: 1,
                retry_after: None,
            });
            println!("the next create fails before making anything (502)");
        }
        let report = format!(
            "sessions created: {:?}; archive requests: {:?}",
            fake.created_sessions(),
            fake.archive_requests()
        );
        if report != last_report {
            println!("{report}");
            last_report = report;
        }
    }
}

/// A control file's trimmed contents, once: the file is removed.
fn take(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    Some(text.trim().to_owned())
}
