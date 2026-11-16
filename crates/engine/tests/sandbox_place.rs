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
#[path = "support/places.rs"]
mod places;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::Path;
use std::time::{Duration, Instant};

use fake_provider::{FakeProvider, Resident};
use places::{
    ORG, add_project, create_on_place, git, host_device, origin, project, ready, steps,
    steps_until,
};
use scripted_edge::ScriptedEdge;
use serde_json::{Value, json};
use zeron_doc::{MessageRole, MessageStatus};
use zeron_engine::EngineRuntime;
use zeron_engine::sandboxes::SandboxProvider;
use zeron_rpc::{RpcClient, memory_client, methods};

// ---------------------------------------------------------------------------
// Fixtures (the shared ones are in support/places.rs)
// ---------------------------------------------------------------------------

/// Send the first message (`<chat>-first`), as a client does.
async fn send(rpc: &RpcClient, chat: &str, cwd: &str, prompt: &str) {
    places::send(rpc, chat, &format!("{chat}-first"), cwd, prompt).await;
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

/// The sandbox's device row as the host device reads it from the registry,
/// once the sandbox's engine has written it.
async fn sandbox_row(host: &EngineRuntime, device: &str) -> zeron_proto::Device {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let rows = host.core().workspace.read_devices().unwrap_or_default();
        if let Some(row) = rows.iter().find(|d| d.id == device)
            && row.sandbox_provider().is_some()
        {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the host never saw {device} as a sandbox: {rows:#?}"
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
    // Picking it opens its account's templates (the picker adds Blank).
    let templates = rpc
        .call(methods::LIST_SANDBOX_TEMPLATES, json!({ "provider": "fake" }))
        .await
        .unwrap();
    assert_eq!(templates, json!([{ "id": "zeron-dev", "name": "zeron-dev" }]));
    let unknown = rpc
        .call(methods::LIST_SANDBOX_TEMPLATES, json!({ "provider": "nope" }))
        .await;
    assert!(unknown.is_err(), "{unknown:?}");

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

    // Its device row names its provider, so lists show "Fake Sandbox", not
    // the sandbox; it isn't a Zeron Cloud machine.
    let row = sandbox_row(&host, &device).await;
    assert_eq!(row.sandbox_provider(), Some("fake"), "{row:?}");
    assert!(
        !row.supports(zeron_proto::CLOUD_SESSION_CAPABILITY),
        "{row:?}"
    );
    assert_ne!(row.platform, zeron_proto::CLOUD_PLATFORM);
    assert_eq!(
        host.core()
            .workspace
            .read_devices()
            .unwrap()
            .iter()
            .find(|d| d.id == host.core().device_id)
            .and_then(|d| d.sandbox_provider()),
        None
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_in_in_the_projects_origin_never_reaches_the_sandbox_or_its_error() {
    const TOKEN: &str = "ghp_do-not-leak-0123456789";
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let host_dir = dirs.path().join("host");
    let host = host_device(&host_dir, &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    // The project's origin carries a sign-in; nothing answers there.
    let path = dirs.path().join("reportly");
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    let origin = format!("https://someone:{TOKEN}@127.0.0.1:9/acme/reportly.git");
    git(&path, &["remote", "add", "origin", &origin]);
    add_project(&rpc, &host, &path).await;

    create_on_place(&rpc, "chat-t", "fake").await;
    let failed = steps_until(&rpc, "chat-t", Duration::from_secs(60), |s| {
        s["state"] == "failed"
    })
    .await;
    assert_eq!(failed["step"], "getCode", "{failed:#}");
    assert!(!failed.to_string().contains(TOKEN), "{failed:#}");
    // The sandbox cloned the repository without it.
    let calls = provider.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("git clone https://127.0.0.1:9/acme/reportly.git")),
        "{calls:#?}"
    );
    assert!(calls.iter().all(|c| !c.contains(TOKEN)), "{calls:#?}");
    // Nor is it kept in what this device remembers of the chat.
    let kept = std::fs::read_to_string(host_dir.join("sandboxes.json")).unwrap();
    assert!(!kept.contains(TOKEN));
}
