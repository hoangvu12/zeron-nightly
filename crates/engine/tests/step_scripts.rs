//! Step scripts (spec #37, docs/design/cloud-byo.md "Step scripts"), at the
//! engine's RPCs with the fake provider: the user's own scripts, stored on
//! the host device, run before, after or instead of a sandbox's steps, for
//! all sandboxes, one provider or one repo. Each runs in the sandbox when
//! it's up for that moment and on the host device otherwise, in name order.
//! A failing one doesn't hold the chat up: the sandbox gets ready, the chat
//! says which script failed and why, and Run again runs it again.
//!
//! What's asserted is what a user sees: what the scripts wrote (their own
//! log, files in the sandbox), what the provider was asked to run, the
//! steps' progress and the chat's first message running.
//!
//! The scripts are shell scripts; on Windows they run in Git's `sh`.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/places.rs"]
mod places;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fake_provider::FakeProvider;
use places::{
    SPACE, add_project, create_on_place, eventually, host_device, origin, project, ready,
    steps_until,
};
use scripted_edge::ScriptedEdge;
use serde_json::{Value, json};
use zeron_doc::{MessageRole, MessageStatus};
use zeron_engine::sandboxes::SandboxProvider;
use zeron_rpc::{RpcClient, memory_client, methods};

/// Send the chat's first message (`<chat>-first`).
async fn send(rpc: &RpcClient, chat: &str, cwd: &str, prompt: &str) {
    places::send(rpc, chat, &format!("{chat}-first"), cwd, prompt).await;
}

fn expect_states(steps: &Value, expected: &[(&str, &str)]) {
    let got: Vec<(String, String)> = steps["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|s| {
            (
                s["step"].as_str().unwrap().to_owned(),
                s["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(got, expected, "{steps:#}");
}

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

/// A path as a shell script spells it (`C:/Users/…` on Windows).
fn sh(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Write a script file, making its folders.
fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
}

/// The lines the scripts appended to `log`.
fn lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The index of the first provider call containing `needle`.
fn call_at(calls: &[String], needle: &str) -> usize {
    calls
        .iter()
        .position(|c| c.contains(needle))
        .unwrap_or_else(|| panic!("no call with {needle:?}: {calls:#?}"))
}

/// Save a script through `WriteStepScript`, as Settings does.
async fn write(
    rpc: &RpcClient,
    scope: Value,
    hook: &str,
    name: &str,
    content: &str,
) -> Result<Value, String> {
    let params = json!({ "for": scope, "hook": hook, "name": name, "content": content });
    rpc.call(methods::WRITE_STEP_SCRIPT, params)
        .await
        .map_err(|e| format!("{e:?}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripts_run_where_their_moment_says_in_name_order() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host_dir = dirs.path().join("host");
    let host = host_device(&host_dir, &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    let log = dirs.path().join("order.log");
    let log_sh = sh(&log);
    let note = |label: &str| format!("echo {label} >> \"{log_sh}\"");
    let steps_dir = host_dir.join("steps");
    // Every sandbox (the user's steps folder).
    script(
        &steps_dir.join("before-create/a.sh"),
        &format!("echo \"before-create sandbox=[$ZERON_SANDBOX]\" >> \"{log_sh}\""),
    );
    script(
        &steps_dir.join("after-create/b.sh"),
        &format!(
            "echo \"after-create $ZERON_PROVIDER $ZERON_SANDBOX $ZERON_CHAT $ZERON_REPO\" >> \"{log_sh}\"\n\
             zeron-sandbox 'echo from-host > made-by-host.txt' || exit 1"
        ),
    );
    script(
        &steps_dir.join("after-start/c.sh"),
        &format!("{}\ntouch after-start-ran", note("after-start")),
    );
    script(
        &steps_dir.join("instead-of-secrets/s.sh"),
        &note("instead-of-secrets"),
    );
    script(&steps_dir.join("after-setup/10-x.sh"), &note("10-x"));
    script(&steps_dir.join("after-setup/30-z.sh"), &note("30-z"));
    // Only this provider's sandboxes, and not another's.
    script(&steps_dir.join("fake/after-setup/20-y.sh"), &note("20-y"));
    script(
        &steps_dir.join("fake-other/after-setup/no.sh"),
        &note("other-provider"),
    );
    // Not this moment.
    script(&steps_dir.join("after-wake/w.sh"), &note("after-wake"));
    // The repo's own.
    script(
        &path.join(".zeron/setup.sh"),
        &format!("{}\ntouch setup-ran", note("setup.sh")),
    );
    script(
        &path.join(".zeron/before-ready/r.sh"),
        &note("before-ready"),
    );

    let created = create_on_place(&rpc, "chat-a", "fake").await;
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    host.core()
        .workspace
        .rename_chat("chat-a", "Pre-titled")
        .unwrap();
    send(&rpc, "chat-a", &cwd, "hello scripts").await;

    let done = ready(&rpc, "chat-a").await;
    expect_states(
        &done,
        &[
            ("create", "done"),
            ("start", "done"),
            ("installZeron", "skipped"),
            ("signIn", "done"),
            // A script did it instead.
            ("secrets", "done"),
            ("getCode", "done"),
            // The repo's setup.sh ran.
            ("setup", "done"),
            ("ready", "done"),
        ],
    );
    assert_eq!(done["scripts"], json!([]), "{done:#}");

    // In order: each moment's scripts by name, whatever folder they're in.
    let ran = lines(&log);
    assert_eq!(ran.len(), 9, "{ran:#?}");
    assert_eq!(ran[0], "before-create sandbox=[]");
    let after_create = &ran[1];
    assert!(
        after_create.starts_with("after-create fake chat-a chat-a ")
            && after_create.contains("origin.git"),
        "provider, sandbox, chat and repo: {after_create}"
    );
    assert_eq!(
        &ran[2..],
        [
            "after-start",
            "instead-of-secrets",
            "setup.sh",
            "10-x",
            "20-y",
            "30-z",
            "before-ready"
        ]
    );

    // Where: the host-side ones never went through the provider; the
    // others ran in the sandbox, after it started and before Zeron did.
    let calls = provider.calls();
    for host_side in ["before-create/a.sh", "after-create/b.sh"] {
        assert!(
            !calls.iter().any(|c| c.contains(host_side)),
            "{host_side} ran on the host device: {calls:#?}"
        );
    }
    let start = call_at(&calls, "start chat-a");
    let engine = call_at(&calls, "zeron-engine watchdog");
    let mut last = start;
    for in_sandbox in [
        "after-start/c.sh",
        "instead-of-secrets/s.sh",
        "reportly/.zeron/setup.sh",
        "after-setup/10-x.sh",
        "fake/after-setup/20-y.sh",
        "after-setup/30-z.sh",
        "reportly/.zeron/before-ready/r.sh",
    ] {
        let at = call_at(&calls, in_sandbox);
        assert!(at > last && at < engine, "{in_sandbox}: {calls:#?}");
        last = at;
    }
    // Files they made are on the sandbox's disk: a sandbox script in the
    // checkout once there is one, the host's command where the provider runs.
    let home = PathBuf::from(provider.workdir("chat-a"));
    assert!(home.join("after-start-ran").exists());
    assert!(Path::new(&cwd).join("setup-ran").exists());
    assert_eq!(
        std::fs::read_to_string(home.join("made-by-host.txt"))
            .unwrap()
            .trim(),
        "from-host"
    );

    replied(&provider, "chat-a", "chat-a", Duration::from_secs(60)).await;

    // Deleting the chat: its delete scripts run on the host device, the
    // one before it while the sandbox is still there to act on.
    std::fs::remove_file(&log).unwrap();
    script(
        &steps_dir.join("before-delete/keep.sh"),
        &format!(
            "echo \"before-delete $ZERON_SANDBOX\" >> \"{log_sh}\"\n\
             zeron-sandbox 'echo last words > last-words.txt' && echo sandbox-ok >> \"{log_sh}\""
        ),
    );
    script(
        &steps_dir.join("after-delete/bye.sh"),
        &note("after-delete"),
    );
    rpc.call(
        methods::MUTATE,
        json!({ "op": "deleteChat", "chatId": "chat-a" }),
    )
    .await
    .expect("deleteChat");
    eventually(
        "the delete scripts ran",
        Duration::from_secs(30),
        async || (lines(&log).len() == 3).then_some(()),
    )
    .await;
    assert_eq!(
        lines(&log),
        ["before-delete chat-a", "sandbox-ok", "after-delete"]
    );
    let calls = provider.calls();
    let words = call_at(&calls, "last-words.txt");
    let deleted = call_at(&calls, "delete chat-a");
    assert!(words < deleted, "{calls:#?}");
    for host_side in ["before-delete/keep.sh", "after-delete/bye.sh"] {
        assert!(!calls.iter().any(|c| c.contains(host_side)), "{calls:#?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_script_opens_the_chat_with_its_error_and_runs_again() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host_dir = dirs.path().join("host");
    let host = host_device(&host_dir, &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    let log = dirs.path().join("order.log");
    let fixed = dirs.path().join("fixed");
    script(
        &path.join(".zeron/setup.sh"),
        &format!(
            "echo setup.sh >> \"{}\"\n\
             test -f \"{}\" || {{ echo 'npm ERR! missing script: build' >&2; exit 1; }}",
            sh(&log),
            sh(&fixed)
        ),
    );
    script(
        &host_dir.join("steps/after-setup/ok.sh"),
        &format!("echo ok >> \"{}\"", sh(&log)),
    );

    let created = create_on_place(&rpc, "chat-f", "fake").await;
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    host.core()
        .workspace
        .rename_chat("chat-f", "Pre-titled")
        .unwrap();
    send(&rpc, "chat-f", &cwd, "hello anyway").await;

    // The sandbox gets ready and the first message runs all the same.
    let done = ready(&rpc, "chat-f").await;
    assert!(done["error"].is_null(), "{done:#}");
    let failed = &done["scripts"];
    assert_eq!(failed.as_array().map(Vec::len), Some(1), "{done:#}");
    assert_eq!(failed[0]["script"], "reportly/.zeron/setup.sh");
    assert_eq!(failed[0]["hook"], "setup");
    assert_eq!(failed[0]["state"], "failed");
    let error = failed[0]["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("npm ERR! missing script"),
        "its own words: {error}"
    );
    assert_eq!(lines(&log), ["setup.sh", "ok"]);
    replied(&provider, "chat-f", "chat-f", Duration::from_secs(60)).await;

    // Fixed; Run again runs just that script, and the chat stays ready.
    std::fs::write(&fixed, "").unwrap();
    rpc.call(
        methods::RUN_SANDBOX_STEPS_AGAIN,
        json!({ "chatId": "chat-f" }),
    )
    .await
    .expect("Run again");
    let again = steps_until(&rpc, "chat-f", Duration::from_secs(30), |s| {
        assert_eq!(s["state"], "ready", "{s:#}");
        s["scripts"] == json!([])
    })
    .await;
    assert!(again["error"].is_null(), "{again:#}");
    assert_eq!(lines(&log), ["setup.sh", "ok", "setup.sh"]);
    let calls = provider.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.contains("zeron-engine watchdog"))
            .count(),
        1,
        "{calls:#?}"
    );
    assert_eq!(calls.iter().filter(|c| *c == "create").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripts_are_listed_written_and_read_on_the_host_device() {
    let edge = ScriptedEdge::start().await;
    let dirs = tempfile::tempdir().unwrap();
    let host_dir = dirs.path().join("host");
    let host = host_device(&host_dir, &edge).await;
    let rpc = memory_client(host.core().rpc_service());
    let origin_url = dirs.path().join("origin.git");
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    // Nothing yet: no empty rows.
    let none = rpc
        .call(methods::LIST_STEP_SCRIPTS, json!({}))
        .await
        .unwrap();
    assert_eq!(none, json!([]));

    write(
        &rpc,
        json!({"kind": "all"}),
        "after-setup",
        "fleet-sync.sh",
        "fleet sync\n",
    )
    .await
    .expect("all sandboxes");
    write(
        &rpc,
        json!({"kind": "provider", "provider": "docker"}),
        "after-wake",
        "git-fetch.sh",
        "git fetch\n",
    )
    .await
    .expect("one provider");
    write(
        &rpc,
        json!({"kind": "project", "spaceId": SPACE}),
        "setup",
        "setup.sh",
        "npm ci\n",
    )
    .await
    .expect("one project");
    write(
        &rpc,
        json!({"kind": "all"}),
        "before-delete",
        "backup.sh",
        "echo bye\n",
    )
    .await
    .expect("a host-side one");

    // Plain files where the design says, so Fleet and git can keep them.
    assert_eq!(
        std::fs::read_to_string(host_dir.join("steps/after-setup/fleet-sync.sh")).unwrap(),
        "fleet sync\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_dir.join("steps/docker/after-wake/git-fetch.sh")).unwrap(),
        "git fetch\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join(".zeron/setup.sh")).unwrap(),
        "npm ci\n"
    );

    let listed = rpc
        .call(methods::LIST_STEP_SCRIPTS, json!({}))
        .await
        .unwrap();
    let rows: Vec<Value> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            json!({
                "for": row["for"], "hook": row["hook"], "name": row["name"],
                "timing": row["timing"], "step": row["step"], "runsOn": row["runsOn"],
            })
        })
        .collect();
    assert_eq!(
        rows,
        [
            json!({"for": {"kind": "all"}, "hook": "after-setup", "name": "fleet-sync.sh",
                   "timing": "after", "step": "setup", "runsOn": "sandbox"}),
            json!({"for": {"kind": "all"}, "hook": "before-delete", "name": "backup.sh",
                   "timing": "before", "step": "delete", "runsOn": "host"}),
            json!({"for": {"kind": "provider", "provider": "docker"}, "hook": "after-wake",
                   "name": "git-fetch.sh", "timing": "after", "step": "wake", "runsOn": "sandbox"}),
            json!({"for": {"kind": "project", "spaceId": SPACE}, "hook": "setup",
                   "name": "setup.sh", "timing": "step", "step": "setup", "runsOn": "sandbox"}),
        ],
        "{listed:#}"
    );

    // Read it back to edit it; save over it.
    let read = rpc
        .call(
            methods::READ_STEP_SCRIPT,
            json!({"for": {"kind": "all"}, "hook": "after-setup", "name": "fleet-sync.sh"}),
        )
        .await
        .unwrap();
    assert_eq!(read["content"], "fleet sync\n");
    write(
        &rpc,
        json!({"kind": "all"}),
        "after-setup",
        "fleet-sync.sh",
        "fleet sync --all\n",
    )
    .await
    .unwrap();
    let read = rpc
        .call(
            methods::READ_STEP_SCRIPT,
            json!({"for": {"kind": "all"}, "hook": "after-setup", "name": "fleet-sync.sh"}),
        )
        .await
        .unwrap();
    assert_eq!(read["content"], "fleet sync --all\n");

    // Delete: the row goes.
    rpc.call(
        methods::DELETE_STEP_SCRIPT,
        json!({"for": {"kind": "provider", "provider": "docker"}, "hook": "after-wake", "name": "git-fetch.sh"}),
    )
    .await
    .unwrap();
    let listed = rpc
        .call(methods::LIST_STEP_SCRIPTS, json!({}))
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 3, "{listed:#}");
    assert!(
        !host_dir
            .join("steps/docker/after-wake/git-fetch.sh")
            .exists()
    );

    // Nothing lands outside the folders a script may be in.
    for (scope, hook, name) in [
        (json!({"kind": "all"}), "after-setup", "../escape.sh"),
        (json!({"kind": "all"}), "after-setup", "a/b.sh"),
        (json!({"kind": "all"}), "after-lunch", "x.sh"),
        (json!({"kind": "all"}), "setup", "setup.sh"),
        (
            json!({"kind": "provider", "provider": "../x"}),
            "after-wake",
            "x.sh",
        ),
        (
            json!({"kind": "project", "spaceId": "nope"}),
            "after-setup",
            "x.sh",
        ),
        (
            json!({"kind": "project", "spaceId": SPACE}),
            "setup",
            "other.sh",
        ),
    ] {
        let refused = write(&rpc, scope.clone(), hook, name, "x").await;
        assert!(refused.is_err(), "{scope} {hook} {name}");
    }
    assert!(!dirs.path().join("escape.sh").exists());
    assert!(!host_dir.join("steps/escape.sh").exists());
}
