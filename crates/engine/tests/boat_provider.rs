//! The Boat provider (spec #37 ticket #50, docs/design/cloud-byo.md "Boat
//! specifics"): the engine's Boat adapter against a local stand-in of Boat's
//! REST API ([`boat_stub`], the edge's `boat-fake.ts` shapes). The contract
//! mirrors the edge's `sandbox-providers.test.ts`: create idempotently, run,
//! extend, stop, start again, delete; unknown sandboxes read as gone; errors
//! sort as the edge's Boat provider sorts them.
//!
//! A run against a real Boat account is `real_boat_smoke` (ignored; needs
//! `ZERON_BOAT_SMOKE_KEY`).

#[path = "support/boat_stub.rs"]
mod boat_stub;

use std::time::Duration;

use base64::Engine as _;
use boat_stub::{BoatStub, KEY};
use futures::StreamExt;
use serde_json::json;
use zeron_engine::sandboxes::{
    BoatProvider, Command, CreateSandbox, Credential, ErrorKind, KeySlot, Ops, RunEvent,
    SandboxProvider, SandboxStatus, run_to_end,
};

struct Boat {
    stub: BoatStub,
    provider: BoatProvider,
    slot: KeySlot,
    _dir: tempfile::TempDir,
}

/// A Boat provider on the stub, not connected yet.
async fn boat() -> Boat {
    let stub = BoatStub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let slot = KeySlot::open(dir.path(), "boat");
    let provider = BoatProvider::new(slot.clone())
        .with_base(&stub.base)
        .with_poll(Duration::from_millis(5));
    Boat {
        stub,
        provider,
        slot,
        _dir: dir,
    }
}

async fn connected() -> Boat {
    let boat = boat().await;
    boat.provider
        .connect(Credential::Token(KEY.into()))
        .await
        .unwrap();
    boat
}

fn create(key: &str, x: &str) -> CreateSandbox {
    let mut request = CreateSandbox::new(key, None);
    request.env.insert("ZERON_X".into(), x.into());
    request
}

#[tokio::test]
async fn boat_is_its_own_provider() {
    let boat = boat().await;
    let p = &boat.provider;
    assert_eq!((p.id(), p.name(), p.icon()), ("boat", "Boat", "boat"));
    let ops = p.ops();
    for op in [
        Ops::CREATE,
        Ops::START,
        Ops::STOP,
        Ops::DELETE,
        Ops::RUN,
        Ops::CONNECT,
        Ops::EXTEND,
    ] {
        assert!(ops.contains(op), "{:?}", op.names());
    }
    assert!(!ops.contains(Ops::TEMPLATES) && !ops.contains(Ops::SAVE_TEMPLATE));
    // It connects with a pasted key only: no sign-in page.
    assert!(!ops.contains(Ops::SIGN_IN_PAGE));
    assert!(!ops.names().contains(&"signInPage"));
    assert!(p.workdir("chat-1").starts_with('/'));
}

#[tokio::test]
async fn connecting_checks_the_key_and_keeps_it_on_this_device_only() {
    let boat = boat().await;
    // Not connected: nothing is asked of Boat.
    let refused = boat.provider.create(create("c", "1")).await.unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Config, "{refused}");
    assert!(boat.stub.calls().is_empty());

    let wrong = boat
        .provider
        .connect(Credential::Token("not-the-key".into()))
        .await
        .unwrap_err();
    assert_eq!(wrong.kind, ErrorKind::Config, "{wrong}");
    assert!(!wrong.message.contains("not-the-key"), "{wrong}");
    assert_eq!(boat.slot.get(), None, "a rejected key isn't kept");
    assert_eq!(boat.provider.account(), None);

    let account = boat
        .provider
        .connect(Credential::Token(format!("  {KEY}\n")))
        .await
        .unwrap();
    assert!(!account.name.is_empty());
    assert!(!account.name.contains(KEY));
    assert!(boat.slot.get().unwrap().contains(KEY));
    // Settings shows it connected.
    assert_eq!(boat.provider.account(), Some(account));

    // Another provider built on the same slot (a restart) is connected.
    let again = BoatProvider::new(boat.slot.clone())
        .with_base(&boat.stub.base)
        .with_poll(Duration::from_millis(5));
    again.create(create("c", "1")).await.unwrap();
}

/// The key moves with the host device (#42): exported once, kept by the
/// next device, forgotten by the last.
#[tokio::test]
async fn the_key_moves_with_the_host_device() {
    let old = connected().await;
    let dir = tempfile::tempdir().unwrap();
    let next = BoatProvider::new(KeySlot::open(dir.path(), "boat"))
        .with_base(&old.stub.base)
        .with_poll(Duration::from_millis(5));
    assert_eq!(next.export_key(), None, "not connected there yet");

    let key = old.provider.export_key().expect("connected here");
    let account = next.adopt_key(key).await.unwrap();
    assert_eq!(next.account(), Some(account));
    next.create(create("moved", "1")).await.unwrap();

    old.provider.forget_key().await;
    assert_eq!(old.provider.account(), None);
    assert_eq!(old.slot.get(), None);
    let refused = old.provider.create(create("x", "1")).await.unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Config, "{refused}");

    let empty = next.adopt_key("  ".into()).await.unwrap_err();
    assert_eq!(empty.kind, ErrorKind::Config, "{empty}");
}

#[tokio::test]
async fn creates_idempotently_runs_extends_stops_starts_and_deletes() {
    let boat = connected().await;
    let p = &boat.provider;

    let id = p.create(create("chat-1", "1")).await.unwrap();
    assert!(id.starts_with("bx_"), "{id}");
    // Created and started: it reads ready before create returns.
    assert_eq!(boat.stub.sandbox(&id).unwrap().state, "idle");
    assert_eq!(p.create(create("chat-1", "1")).await.unwrap(), id);
    let reused = p.create(create("chat-1", "2")).await.unwrap_err();
    assert_eq!(reused.kind, ErrorKind::Conflict, "{reused}");

    let posted: Vec<_> = boat
        .stub
        .calls()
        .into_iter()
        .filter(|c| c.method == "POST" && c.path == "/api/v1/sandboxes")
        .collect();
    let first = &posted[0];
    assert_eq!(first.headers.get("idempotency-key").unwrap(), "chat-1");
    assert_eq!(
        first.body["noEnv"],
        json!(true),
        "nothing of the account leaks in"
    );
    assert_eq!(first.body["env"], json!({"ZERON_X": "1"}));
    let ttl = first.body["ttlSeconds"].as_u64().unwrap();
    assert!(
        (60..=7200).contains(&ttl),
        "created with a time limit: {ttl}"
    );

    let ran = run_to_end(
        p,
        &id,
        Command::new(["echo", "it's here"])
            .cwd("/home/user")
            .env("A", "b c"),
    )
    .await
    .unwrap();
    assert!(ran.exit.success());
    assert_eq!(ran.stdout, "ok");
    let command = boat.stub.calls().last().unwrap().body.clone();
    let line = command["command"].as_str().unwrap();
    assert!(line.contains("'it'\\''s here'"), "{line}");
    assert!(line.contains("cd '/home/user'"), "{line}");
    assert!(line.contains("'A=b c'"), "{line}");
    assert!(command["timeoutSeconds"].as_u64().unwrap() > 0);

    p.extend(&id, Duration::from_secs(3600)).await.unwrap();
    assert_eq!(boat.stub.sandbox(&id).unwrap().ttl_seconds, json!(3600));

    assert_eq!(p.status(&id).await.unwrap(), SandboxStatus::Running);
    p.stop(&id).await.unwrap();
    assert_eq!(boat.stub.sandbox(&id).unwrap().state, "archiving");
    // Stopping, then stopped: both are stopped to Zeron.
    assert_eq!(p.status(&id).await.unwrap(), SandboxStatus::Stopped);
    assert_eq!(boat.stub.sandbox(&id).unwrap().state, "archived");
    assert_eq!(p.status(&id).await.unwrap(), SandboxStatus::Stopped);
    p.start(&id).await.unwrap();
    assert_eq!(boat.stub.sandbox(&id).unwrap().state, "idle");
    // Starting one that runs asks nothing more of Boat than its state.
    let before = boat.stub.calls().len();
    p.start(&id).await.unwrap();
    assert!(
        boat.stub.calls()[before..]
            .iter()
            .all(|c| c.method == "GET")
    );

    p.delete(&id).await.unwrap();
    let deleted = boat.stub.calls().last().unwrap().clone();
    assert_eq!(deleted.method, "DELETE");
    assert_eq!(deleted.headers.get("x-ascii-confirm-delete"), Some(&id));
    assert!(boat.stub.sandbox(&id).unwrap().deleted);
    assert_eq!(p.start(&id).await.unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(p.status(&id).await.unwrap(), SandboxStatus::Gone);

    // The account's key went to Boat as the bearer, never into a sandbox.
    for call in boat.stub.calls() {
        assert!(!call.body.to_string().contains(KEY), "{call:?}");
    }
}

#[tokio::test]
async fn a_template_is_a_named_snapshot() {
    let boat = connected().await;
    let id = boat
        .provider
        .create(CreateSandbox::new("chat-t", Some("node-22".into())))
        .await
        .unwrap();
    assert_eq!(
        boat.stub.sandbox(&id).unwrap().from.as_deref(),
        Some("node-22")
    );
}

#[tokio::test]
async fn unknown_sandboxes_are_gone() {
    let boat = connected().await;
    let p = &boat.provider;
    // Deleted outside Zeron (or never there): gone, which recovery acts on.
    assert_eq!(p.status("bx_zzzzzzzz").await.unwrap(), SandboxStatus::Gone);
    for err in [
        p.start("bx_zzzzzzzz").await.unwrap_err(),
        p.stop("bx_zzzzzzzz").await.unwrap_err(),
        p.extend("bx_zzzzzzzz", Duration::from_secs(60))
            .await
            .unwrap_err(),
    ] {
        assert_eq!(err.kind, ErrorKind::NotFound, "{err}");
    }
}

#[tokio::test]
async fn a_failed_sandbox_says_why() {
    let boat = connected().await;
    let id = boat.provider.create(create("chat-e", "1")).await.unwrap();
    boat.provider.stop(&id).await.unwrap();
    boat.stub.set_state(
        &id,
        "error",
        Some("This sandbox's data was deleted 14 days after it stopped."),
    );
    // A sandbox in error is resumed; Boat refuses with the reason.
    boat.stub.fail_with(
        "resume",
        409,
        "snapshot_data_expired",
        "This sandbox's data was deleted 14 days after it stopped.",
        None,
        None,
    );
    let err = boat.provider.start(&id).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
    assert!(err.message.contains("deleted 14 days"), "{err}");
}

/// Boat's reply (status, code, `retryable` hint, permission named) and the
/// kind it sorts to.
type ErrorCase = (
    u16,
    &'static str,
    Option<bool>,
    Option<&'static str>,
    ErrorKind,
);

#[tokio::test]
async fn errors_sort_as_the_edge_sorts_them() {
    let boat = connected().await;
    let id = boat.provider.create(create("chat-x", "1")).await.unwrap();
    let cases: &[ErrorCase] = &[
        (500, "internal", None, None, ErrorKind::Retryable),
        (429, "rate_limited", None, None, ErrorKind::Retryable),
        (409, "sandbox_starting", None, None, ErrorKind::Retryable),
        (400, "machine_not_running", None, None, ErrorKind::Retryable),
        (401, "unauthorized", None, None, ErrorKind::Config),
        (402, "billing_required", None, None, ErrorKind::Config),
        (
            403,
            "api_key_action_forbidden",
            None,
            Some("sandbox.update"),
            ErrorKind::Config,
        ),
        (
            400,
            "trial_auto_stop_required",
            None,
            None,
            ErrorKind::Config,
        ),
        (409, "something_else", None, None, ErrorKind::Conflict),
        // Boat's own hint wins the transient/terminal split.
        (503, "busy", Some(false), None, ErrorKind::Conflict),
        (409, "odd", Some(true), None, ErrorKind::Retryable),
        (400, "type_too_small", Some(true), None, ErrorKind::Conflict),
        (422, "invalid", None, None, ErrorKind::Fatal),
    ];
    for (status, code, retryable, action, want) in cases {
        boat.stub
            .fail_with("patch", *status, code, "said Boat", *retryable, *action);
        let err = boat
            .provider
            .extend(&id, Duration::from_secs(60))
            .await
            .unwrap_err();
        assert_eq!(err.kind, *want, "{status} {code}: {err}");
        if let Some(action) = action {
            assert!(err.message.contains(action), "{err}");
        }
    }
}

#[tokio::test]
async fn a_command_reports_its_output_and_exit() {
    let boat = connected().await;
    let id = boat.provider.create(create("chat-c", "1")).await.unwrap();
    boat.stub.answer_commands(
        json!({"ok": true, "type": "command.finished", "success": false,
        "exitCode": 128, "stdout": "partial", "stderr": "fatal: nope", "timedOut": false}),
    );
    let events: Vec<_> = boat
        .provider
        .run(&id, Command::new(["git", "clone", "x"]))
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0], RunEvent::Stdout(b"partial".to_vec()));
    assert_eq!(events[1], RunEvent::Stderr(b"fatal: nope".to_vec()));
    let RunEvent::Exit(exit) = events[2] else {
        panic!("{events:?}")
    };
    assert_eq!(exit.code, Some(128));

    boat.stub.answer_commands(
        json!({"ok": true, "exitCode": null, "stdout": "", "stderr": "",
        "timedOut": true}),
    );
    let done = run_to_end(
        &boat.provider,
        &id,
        Command::new(["sleep", "999"]).timeout(Duration::from_secs(2)),
    )
    .await
    .unwrap();
    assert!(done.exit.timed_out && !done.exit.success());
    assert_eq!(
        boat.stub.calls().last().unwrap().body["timeoutSeconds"],
        json!(2)
    );
}

/// Zeron's engine (a detached command) runs as a systemd service on Boat, so
/// it restarts and self-updates; its sign-in stays in memory (`/run`), never
/// on the sandbox's disk.
#[tokio::test]
async fn the_engine_runs_as_a_service() {
    let boat = connected().await;
    let id = boat.provider.create(create("chat-d", "1")).await.unwrap();
    let engine = Command::new(["zeron-engine", "headless"])
        .env("ZERON_LENT_TOKEN", "lent-secret")
        .env("ZERON_EDGE_URL", "https://edge.test")
        .detached();
    let done = run_to_end(&boat.provider, &id, engine).await.unwrap();
    assert!(done.exit.success());
    let line = boat.stub.calls().last().unwrap().body["command"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !line.contains("lent-secret"),
        "the token isn't on the command line in clear"
    );
    // The script and its pieces travel base64-encoded.
    let decoded = decode_all(&line);
    assert!(decoded.contains("unit=zeron-sandbox.service"), "{decoded}");
    assert!(
        decoded.contains("sudo systemctl restart \"$unit\""),
        "{decoded}"
    );
    assert!(decoded.contains("run=/run/zeron-sandbox"), "{decoded}");
    assert!(decoded.contains("EnvironmentFile=$run/env"), "{decoded}");
    assert!(decoded.contains("Restart=always"), "{decoded}");
    // The env file (in /run) has the token.
    assert!(
        decoded.contains("ZERON_LENT_TOKEN=\"lent-secret\""),
        "{decoded}"
    );
    assert!(
        decoded.contains("ZERON_SERVICE_UNIT=\"zeron-sandbox.service\""),
        "{decoded}"
    );
    assert!(decoded.contains("headless"), "{decoded}");
}

/// `text` and every single-quoted base64 piece in it, decoded, recursively.
fn decode_all(text: &str) -> String {
    let mut all = text.to_owned();
    for piece in text.split('\'').filter(|p| p.len() >= 8) {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(piece)
            && let Ok(inner) = String::from_utf8(bytes)
        {
            all.push('\n');
            all.push_str(&decode_all(&inner));
        }
    }
    all
}

/// A manual run against the user's real Boat account: create, run, extend,
/// stop, start, delete. `ZERON_BOAT_SMOKE_KEY=<key> cargo test -p zeron-engine
/// --test boat_provider real_boat_smoke -- --ignored --nocapture`. It never
/// prints the key, and deletes what it made.
#[tokio::test]
#[ignore = "needs a real Boat account (ZERON_BOAT_SMOKE_KEY)"]
async fn real_boat_smoke() {
    let key = std::env::var("ZERON_BOAT_SMOKE_KEY").expect("ZERON_BOAT_SMOKE_KEY");
    let dir = tempfile::tempdir().unwrap();
    let mut provider = BoatProvider::new(KeySlot::open(dir.path(), "boat"));
    if let Ok(base) = std::env::var("ZERON_BOAT_SMOKE_BASE") {
        provider = provider.with_base(&base);
    }
    let account = provider.connect(Credential::Token(key)).await.unwrap();
    println!("connected: {}", account.name);
    let request = CreateSandbox::new(format!("zeron-smoke-{}", std::process::id()), None);
    let id = provider.create(request).await.unwrap();
    println!("created and started {id}");
    let outcome = async {
        let done = run_to_end(
            &provider,
            &id,
            Command::new([
                "sh",
                "-c",
                "uname -a; id -un; pwd; systemctl --version | head -1; env | grep -c BOAT || true",
            ]),
        )
        .await?;
        println!(
            "ran: exit {:?}\n{}{}",
            done.exit.code, done.stdout, done.stderr
        );
        assert!(done.exit.success());
        provider.extend(&id, Duration::from_secs(1800)).await?;
        println!("extended");
        provider.stop(&id).await?;
        println!("stopping");
        provider.start(&id).await?;
        println!("started again");
        Ok::<_, zeron_engine::sandboxes::ProviderError>(())
    }
    .await;
    provider.delete(&id).await.unwrap();
    println!("deleted {id}");
    outcome.unwrap();
}
