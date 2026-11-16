//! A chat on Docker Sandbox against a real Docker account and the real
//! edge (spec #37), end to end at the host engine's RPCs. Ignored: it costs
//! a few cents and needs a signed-in host. `scripts/live-docker-sandbox.sh`
//! runs it.
//!
//! - `ZERON_DOCKER_SMOKE_TOKEN`: a Docker access token (never printed).
//! - `ZERON_LIVE_HOST_DIR`: the host engine's data folder, signed in to
//!   Zeron (`zeron-engine login` there).
//! - `ZERON_EDGE_URL` (default `https://edge.zeron.sh`),
//!   `ZERON_WORKOS_CLIENT_ID` (default Zeron's public one).
//! - `ZERON_SANDBOX_ENGINE_TARBALL`: the engine the sandbox installs, when
//!   the release's doesn't have this branch's code yet.
//!
//! Connect Docker → a chat on Docker/Blank → its steps reach ready → the
//! sandbox's engine signs in and shows as a device → the first message
//! runs there → idle, it sleeps → a queued send wakes it → deleting the
//! chat deletes the sandbox in Docker. The sandbox is deleted even when a
//! check fails.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/places.rs"]
mod places;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zeron_doc::SessionCommandPayload;
use zeron_engine::sandboxes::{SandboxProvider, SandboxStatus, SleepTimings};
use zeron_engine::{EngineConfig, EngineRuntime, HarnessId};
use zeron_proto::{RunRequest, SandboxLevel, SessionStatus};
use zeron_rpc::{RpcClient, memory_client, methods};

const MINUTE: Duration = Duration::from_secs(60);
/// Zeron's WorkOS client id, public (`zeron_cli::DEFAULT_WORKOS_CLIENT_ID`).
const WORKOS_CLIENT_ID: &str = "client_01KWD0EAKZKD50YCQJNYSRE4BY";

fn timings(idle: Duration) -> SleepTimings {
    SleepTimings {
        idle,
        time_limit: Duration::from_secs(30 * 60),
        check_every: Duration::from_secs(5),
    }
}

fn say(what: impl std::fmt::Display) {
    eprintln!("live [{}]: {what}", chrono_now());
}

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{:02}:{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

async fn steps(rpc: &RpcClient, chat: &str) -> Value {
    rpc.call(methods::SANDBOX_STEPS, json!({ "chatId": chat }))
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }))
}

/// Follow the chat's steps, saying each change, until `done`; panics with
/// the last answer after `within` or when they fail.
async fn steps_until(
    rpc: &RpcClient,
    chat: &str,
    within: Duration,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    loop {
        let now = steps(rpc, chat).await;
        let line = format!("{} {} {}", now["state"], now["step"], now["error"]);
        if line != last {
            say(format!("steps: {line}"));
            last = line;
        }
        if done(&now) {
            return now;
        }
        assert!(now["state"] != "failed", "steps failed: {now:#}");
        assert!(Instant::now() < deadline, "steps of {chat}: {now:#}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn eventually<T>(
    what: &str,
    within: Duration,
    mut check: impl AsyncFnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = check().await {
            say(format!("ok: {what}"));
            return value;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn send(rpc: &RpcClient, chat: &str, message_id: &str, cwd: &str, prompt: &str) {
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
        message_id: message_id.into(),
    })
    .unwrap();
    rpc.call(
        methods::QUEUE_COMMAND,
        json!({ "chatId": chat, "command": command }),
    )
    .await
    .expect("queue the message");
}

/// The chat's session as the registry says it, when there's one.
fn session(host: &EngineRuntime, chat: &str) -> Option<zeron_proto::Session> {
    host.core()
        .workspace
        .read_sessions()
        .ok()?
        .into_iter()
        .find(|s| s.chat_id == chat)
}

/// A turn after `before` ran on the sandbox and finished: the chat's session,
/// as the sandbox's engine publishes it, is idle with a newer completed turn.
async fn ran(
    host: &EngineRuntime,
    chat: &str,
    device: &str,
    before: Option<String>,
    within: Duration,
) -> Option<String> {
    eventually(&format!("a turn after {before:?} ran on the sandbox"), within, async || {
        let s = session(host, chat)?;
        let done = s.device_id == device
            && s.status == SessionStatus::Idle
            && s.last_completed_turn.is_some()
            && s.last_completed_turn != before;
        done.then(|| {
            say(format!("session: {}", serde_json::to_string(&s).unwrap_or_default()));
            s.last_completed_turn.clone()
        })
    })
    .await
}

async fn docker_status(provider: &Arc<dyn SandboxProvider>, id: &str) -> SandboxStatus {
    provider.status(id).await.expect("Docker says how it is")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a Docker account and a signed-in host: scripts/live-docker-sandbox.sh"]
async fn a_chat_on_docker_sandbox_end_to_end() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let token = std::env::var("ZERON_DOCKER_SMOKE_TOKEN").expect("ZERON_DOCKER_SMOKE_TOKEN");
    let data_dir = PathBuf::from(std::env::var("ZERON_LIVE_HOST_DIR").expect("ZERON_LIVE_HOST_DIR"));
    let edge_url =
        std::env::var("ZERON_EDGE_URL").unwrap_or_else(|_| "https://edge.zeron.sh".into());
    let config = EngineConfig {
        data_dir,
        edge_url,
        edge_token: None,
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        org_id: None,
        workos_client_id: Some(
            std::env::var("ZERON_WORKOS_CLIENT_ID").unwrap_or_else(|_| WORKOS_CLIENT_ID.into()),
        ),
        lent_sign_in: None,
    };
    let host = fake_provider::start_engine(&config).await;
    host.core().sandboxes.set_sleep_timings(timings(60 * MINUTE));
    let rpc = memory_client(host.core().rpc_service());
    say(format!("host device {}", host.core().device_id));

    // 1. Connect Docker (a pasted access token: Settings → Sandboxes).
    let connected = rpc
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "docker", "token": token }),
        )
        .await
        .expect("connect Docker");
    say(format!(
        "connected as {}, host {}",
        connected["account"]["name"], connected["hostDeviceId"]
    ));
    assert_eq!(connected["hostDeviceId"], host.core().device_id.as_str());
    let docker = host.core().sandboxes.provider_named("docker").unwrap();

    // 2. A chat on Docker/Blank, with no project.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let chat = format!("live-{stamp:x}");
    let created = rpc
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": chat, "place": { "provider": "docker" } }),
        )
        .await
        .expect("createChat on Docker");
    say(format!("created {chat}: {created}"));
    let device = created["deviceId"].as_str().expect("the sandbox's device").to_owned();
    let cwd = created["cwd"].as_str().unwrap_or("~").to_owned();
    host.core().workspace.rename_chat(&chat, "Docker live test").unwrap();
    send(&rpc, &chat, &format!("{chat}-first"), &cwd, "hello from the live test").await;

    let checks = async {
        // 3. Its steps reach ready.
        let ready = steps_until(&rpc, &chat, 15 * MINUTE, |s| {
            s["sandboxId"].is_string() && s["state"] == "ready"
        })
        .await;
        say(format!("ready: {ready:#}"));
        let sandbox = ready["sandboxId"].as_str().unwrap().to_owned();
        assert_eq!(docker_status(&docker, &sandbox).await, SandboxStatus::Running);

        // 4. Its engine signed in and shows as a device on Docker.
        eventually("the sandbox shows as a Docker device", 3 * MINUTE, async || {
            let rows = host.core().workspace.read_devices().ok()?;
            let row = rows.into_iter().find(|d| d.id == device)?;
            (row.sandbox_provider() == Some("docker")).then(|| say(format!("device: {row:?}")))
        })
        .await;

        // 5. The first message ran there and its reply came back.
        let first = ran(&host, &chat, &device, None, 5 * MINUTE).await;

        // 6. Idle: it sleeps, and Docker stops it.
        host.core().sandboxes.set_sleep_timings(timings(Duration::from_secs(20)));
        steps_until(&rpc, &chat, 5 * MINUTE, |s| s["state"] == "asleep").await;
        eventually("Docker has it stopped", 3 * MINUTE, async || {
            (docker_status(&docker, &sandbox).await == SandboxStatus::Stopped).then_some(())
        })
        .await;
        host.core().sandboxes.set_sleep_timings(timings(60 * MINUTE));

        // 7. A queued send wakes it, and runs.
        send(&rpc, &chat, &format!("{chat}-second"), &cwd, "are you awake?").await;
        steps_until(&rpc, &chat, 10 * MINUTE, |s| s["state"] == "ready").await;
        assert_eq!(docker_status(&docker, &sandbox).await, SandboxStatus::Running);
        ran(&host, &chat, &device, first, 5 * MINUTE).await;
        sandbox
    };
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(checks)).await;

    // 8. Deleting the chat deletes its sandbox in Docker.
    let sandbox = host
        .core()
        .sandboxes
        .steps(&chat)
        .and_then(|s| s.sandbox_id);
    rpc.call(methods::MUTATE, json!({ "op": "deleteChat", "chatId": chat }))
        .await
        .expect("deleteChat");
    if let Some(sandbox) = &sandbox {
        let gone = tokio::time::timeout(5 * MINUTE, async {
            while docker_status(&docker, sandbox).await != SandboxStatus::Gone {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
        .await;
        if gone.is_err() {
            // Never leave one behind: delete it ourselves, then fail.
            let _ = docker.delete(sandbox).await;
            if outcome.is_ok() {
                panic!("deleting the chat left {sandbox} in Docker");
            }
        } else {
            say(format!("{sandbox} is gone from Docker"));
        }
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
