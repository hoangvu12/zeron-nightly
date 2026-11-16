//! A chat on a place (spec #37, docs/design/cloud-byo.md "Steps"), end to
//! end at the engine's RPCs: the host device creates a chat on a fake
//! provider, its steps make and start a sandbox, sign it in on a lent token,
//! clone the project onto `zeron/<chat>`, and the first message, queued while
//! that happened, runs on the sandbox's own engine (a second real engine).
//! The edge is scripted: device rooms, sign-in refresh and a shared registry.
//!
//! What's asserted is what a user or another device sees: RPC answers (the
//! chat row, the steps' progress), what the provider was asked to do, the
//! checkout on the sandbox's disk, and the reply in the sandbox's transcript.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fake_provider::{FakeProvider, Resident, engine_config, start_engine};
use scripted_edge::{ScriptedEdge, USER};
use serde_json::{Value, json};
use zeron_doc::{MessageRole, MessageStatus, SessionCommandPayload};
use zeron_engine::EngineRuntime;
use zeron_engine::sandboxes::SandboxProvider;
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_rpc::{RpcClient, memory_client, methods};

const ORG: &str = "org_home";
const SPACE: &str = "space-1";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The host device: signed in with a WorkOS session on its disk.
async fn host_device(dir: &Path, edge: &ScriptedEdge) -> EngineRuntime {
    std::fs::create_dir_all(dir).unwrap();
    let session = json!({
        "refreshToken": edge.seed_session(ORG),
        "user": { "id": USER, "email": "u@example.com" },
        "orgId": ORG,
    });
    std::fs::write(dir.join("session.json"), session.to_string()).unwrap();
    start_engine(&engine_config(dir, &edge.url, None)).await
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare `origin` with one commit on `main`.
fn origin(at: &Path) {
    let seed = at.with_extension("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "reportly\n").unwrap();
    git(&seed, &["add", "."]);
    git(
        &seed,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "first",
        ],
    );
    git(
        at.parent().unwrap(),
        &[
            "clone",
            "-q",
            "--bare",
            &seed.to_string_lossy(),
            &at.to_string_lossy(),
        ],
    );
}

/// The project on the host device: a checkout of `origin_url`.
fn project(dir: &Path, origin_url: &Path) -> PathBuf {
    let project = dir.join("reportly");
    std::fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(
        &project,
        &["remote", "add", "origin", &origin_url.to_string_lossy()],
    );
    project
}

async fn add_project(rpc: &RpcClient, host: &EngineRuntime, path: &Path) {
    rpc.call(
        methods::MUTATE,
        json!({
            "op": "createSpace",
            "spaceId": SPACE,
            "deviceId": host.core().device_id,
            "path": path.to_string_lossy(),
            "gitDetected": true,
        }),
    )
    .await
    .expect("createSpace");
}

/// Create `chat` on `provider`, as the composer does: `{deviceId, cwd}`.
async fn create_on_place(rpc: &RpcClient, chat: &str, provider: &str) -> Value {
    rpc.call(
        methods::MUTATE,
        json!({
            "op": "createChat",
            "chatId": chat,
            "spaceId": SPACE,
            "place": { "provider": provider },
        }),
    )
    .await
    .expect("createChat on a place")
}

/// Send the first message, as a client does: a Run command in the chat doc.
async fn send(rpc: &RpcClient, chat: &str, cwd: &str, prompt: &str) {
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: RunRequest {
            mcp: None,
            prompt: prompt.into(),
            harness: Some(HarnessId::Mock),
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: cwd.into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            access: Default::default(),
            plan: false,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            env: Default::default(),
            extra_mcp: Vec::new(),
        },
        message_id: format!("{chat}-first"),
    })
    .unwrap();
    rpc.call(
        methods::QUEUE_COMMAND,
        json!({ "chatId": chat, "command": command }),
    )
    .await
    .expect("queue the first message");
}

async fn steps(rpc: &RpcClient, chat: &str) -> Value {
    rpc.call(methods::SANDBOX_STEPS, json!({ "chatId": chat }))
        .await
        .expect("SandboxSteps")
}

/// Poll the chat's steps until `done` says so; panics with the last answer.
async fn steps_until(
    rpc: &RpcClient,
    chat: &str,
    within: Duration,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let now = steps(rpc, chat).await;
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "steps of {chat}: {now:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn ready(rpc: &RpcClient, chat: &str) -> Value {
    steps_until(rpc, chat, Duration::from_secs(60), |s| {
        assert_ne!(s["state"], "failed", "{s:#}");
        s["state"] == "ready"
    })
    .await
}

/// Each step's state, by name: `{"create": "done", "start": "skipped", …}`.
fn states(steps: &Value) -> Vec<(String, String)> {
    steps["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|s| {
            (
                s["step"].as_str().unwrap().to_owned(),
                s["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn expect_states(steps: &Value, expected: &[(&str, &str)]) {
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(states(steps), expected, "{steps:#}");
}

/// The first message ran on the sandbox's engine: its transcript holds the
/// user's message and a finished reply.
async fn replied(provider: &FakeProvider, sandbox: &str, chat: &str, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let entries = provider
            .with_engine(sandbox, |core| {
                core.doc_host
                    .open(chat)
                    .ok()
                    .and_then(|h| h.doc().read_entries().ok())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let asked = entries
            .iter()
            .any(|e| e.role == MessageRole::User && e.id == format!("{chat}-first"));
        let answered = entries
            .iter()
            .any(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete));
        if asked && answered {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{chat}'s first message never ran on {sandbox}: {} entries",
            entries.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_on_a_place_gets_its_own_sandbox_and_runs_its_first_message() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());

    let offered = rpc
        .call(methods::LIST_SANDBOX_PROVIDERS, json!({}))
        .await
        .unwrap();
    // The built-in providers are listed too; this one is the fake.
    let fake = offered
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "fake")
        .cloned();
    assert_eq!(
        fake,
        Some(json!({ "id": "fake", "name": "Fake Sandbox", "icon": "box",
                     "ops": ["create", "start", "stop", "delete", "run", "templates", "saveTemplate", "extend"],
                     "account": null }))
    );
    assert!(
        offered
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "boat" && p["name"] == "Boat"),
        "{offered}"
    );

    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    // Creating the chat returns at once, while its sandbox is still being made.
    provider.hold_create();
    let asked = Instant::now();
    let created = create_on_place(&rpc, "chat-a", "fake").await;
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "{:?}",
        asked.elapsed()
    );
    let device = created["deviceId"]
        .as_str()
        .expect("the sandbox's device")
        .to_owned();
    assert_ne!(device, host.core().device_id);
    assert!(!device.starts_with("cloud-"), "{device}");
    let cwd = created["cwd"].as_str().expect("the checkout").to_owned();
    assert_eq!(
        Path::new(&cwd),
        Path::new(&provider.workdir("chat-a")).join("reportly")
    );
    let row = host
        .core()
        .workspace
        .chat("chat-a")
        .unwrap()
        .expect("the chat row");
    assert_eq!(row.device_id, device);
    assert_eq!(row.cwd.as_deref(), Some(cwd.as_str()));

    // The compact progress line: the step it's on, since when.
    let making = steps(&rpc, "chat-a").await;
    assert_eq!(making["state"], "starting", "{making:#}");
    assert_eq!(making["step"], "create", "{making:#}");
    assert_eq!(making["provider"], "fake");
    assert_eq!(making["deviceId"], device.as_str());
    assert!(making["since"].as_i64().is_some(), "{making:#}");

    // The first message waits in the chat while the steps run.
    host.core()
        .workspace
        .rename_chat("chat-a", "Pre-titled")
        .unwrap();
    send(&rpc, "chat-a", &cwd, "hello sandbox").await;
    provider.release_create();

    let done = ready(&rpc, "chat-a").await;
    expect_states(
        &done,
        &[
            ("create", "done"),
            ("start", "done"),
            // The template already has Zeron.
            ("installZeron", "skipped"),
            ("signIn", "done"),
            ("secrets", "skipped"),
            ("getCode", "done"),
            ("setup", "skipped"),
            ("ready", "done"),
        ],
    );
    assert_eq!(done["sandboxId"], "chat-a");
    assert!(done["error"].is_null());

    // Zeron did the steps through the provider's five operations.
    let calls = provider.calls();
    assert_eq!(calls[0], "create");
    assert_eq!(calls[1], "start chat-a");
    assert!(
        calls.iter().any(|c| c.starts_with("run chat-a: git clone")),
        "{calls:#?}"
    );
    assert_eq!(
        calls.last().unwrap(),
        "run chat-a: zeron-engine watchdog",
        "{calls:#?}"
    );

    // The project is cloned on the chat's own branch.
    assert_eq!(
        git(Path::new(&cwd), &["branch", "--show-current"]),
        "zeron/chat-a"
    );
    assert!(Path::new(&cwd).join("README.md").exists());

    // The sandbox is the chat's host, signed in on a lent token, and its
    // first message runs there.
    replied(&provider, "chat-a", "chat-a", Duration::from_secs(60)).await;
    let hosts = edge.host_dials(&device);
    assert!(
        !hosts.is_empty()
            && hosts
                .iter()
                .all(|d| d.admitted && d.org.as_deref() == Some(ORG)),
        "{hosts:?}"
    );

    // Another chat gets a sandbox of its own.
    let other = create_on_place(&rpc, "chat-b", "fake").await;
    let other_device = other["deviceId"].as_str().unwrap();
    assert_ne!(other_device, device);
    ready(&rpc, "chat-b").await;
    assert_eq!(provider.sandboxes(), ["chat-a", "chat-b"]);
    assert_eq!(
        git(
            Path::new(other["cwd"].as_str().unwrap()),
            &["branch", "--show-current"]
        ),
        "zeron/chat-b"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steps_a_provider_cant_do_are_skipped() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    // An SSH box: it can only run commands, so Zeron makes nothing and
    // starts nothing, and does everything else through `run`.
    let ssh = FakeProvider::run_only(&dirs.path().join("ssh"));
    host.core().sandboxes.register(ssh.clone());
    let created = create_on_place(&rpc, "chat-ssh", "fake-ssh").await;
    host.core()
        .workspace
        .rename_chat("chat-ssh", "Pre-titled")
        .unwrap();
    send(
        &rpc,
        "chat-ssh",
        created["cwd"].as_str().unwrap(),
        "hello box",
    )
    .await;
    let done = ready(&rpc, "chat-ssh").await;
    expect_states(
        &done,
        &[
            ("create", "skipped"),
            ("start", "skipped"),
            ("installZeron", "skipped"),
            ("signIn", "done"),
            ("secrets", "skipped"),
            ("getCode", "done"),
            ("setup", "skipped"),
            ("ready", "done"),
        ],
    );
    assert!(
        ssh.calls().iter().all(|c| c.starts_with("run box: ")),
        "{:#?}",
        ssh.calls()
    );
    replied(&ssh, "box", "chat-ssh", Duration::from_secs(60)).await;

    // A PC at home: it can only be started, and it already runs Zeron,
    // signed in itself. Nothing is installed, signed in or cloned.
    let resident = Resident {
        device_id: "device-pc".into(),
        edge_url: edge.url.clone(),
        sign_in: host.core().lend_sign_in("device-pc", ORG).await.unwrap(),
    };
    let pc = FakeProvider::start_only(&dirs.path().join("pc"), resident);
    host.core().sandboxes.register(pc.clone());
    let created = create_on_place(&rpc, "chat-pc", "fake-pc").await;
    assert_eq!(created["deviceId"], "device-pc");
    let done = ready(&rpc, "chat-pc").await;
    expect_states(
        &done,
        &[
            ("create", "skipped"),
            ("start", "done"),
            ("installZeron", "skipped"),
            ("signIn", "skipped"),
            ("secrets", "skipped"),
            ("getCode", "skipped"),
            ("setup", "skipped"),
            ("ready", "done"),
        ],
    );
    assert_eq!(pc.calls(), ["start pc"]);
    assert!(edge.host_connected("device-pc"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_clone_shows_its_error_and_runs_again() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    // The project's origin isn't there (yet).
    let origin_url = dirs.path().join("origin.git");
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    let created = create_on_place(&rpc, "chat-c", "fake").await;
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    host.core()
        .workspace
        .rename_chat("chat-c", "Pre-titled")
        .unwrap();
    send(&rpc, "chat-c", &cwd, "hello again").await;

    let failed = steps_until(&rpc, "chat-c", Duration::from_secs(30), |s| {
        s["state"] == "failed"
    })
    .await;
    assert_eq!(failed["step"], "getCode", "{failed:#}");
    let error = failed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("origin.git") || error.to_lowercase().contains("repository"),
        "git's own words: {error}"
    );
    // Nothing after it ran: the sandbox's engine isn't up.
    assert!(
        !provider
            .calls()
            .iter()
            .any(|c| c.contains("zeron-engine watchdog")),
        "{:#?}",
        provider.calls()
    );

    // The user fixes GitHub and runs it again: it goes on from there.
    origin(&origin_url);
    rpc.call(
        methods::RUN_SANDBOX_STEPS_AGAIN,
        json!({ "chatId": "chat-c" }),
    )
    .await
    .expect("Run again");
    let done = ready(&rpc, "chat-c").await;
    assert!(done["error"].is_null(), "{done:#}");
    assert_eq!(
        provider.calls().iter().filter(|c| *c == "create").count(),
        1,
        "{:#?}",
        provider.calls()
    );
    assert_eq!(
        git(Path::new(&cwd), &["branch", "--show-current"]),
        "zeron/chat-c"
    );
    replied(&provider, "chat-c", "chat-c", Duration::from_secs(60)).await;
}
