//! What tests of chats on places share: the host device, a project with an
//! `origin`, creating a chat on a place and following its steps through
//! the engine's RPCs. (Same fixtures as `sandbox_place.rs`, which predates
//! this module.)

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zeron_doc::SessionCommandPayload;
use zeron_engine::EngineRuntime;
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_rpc::{RpcClient, methods};

use crate::fake_provider::{engine_config, start_engine};
use crate::scripted_edge::{ScriptedEdge, USER};

pub const ORG: &str = "org_home";
pub const SPACE: &str = "space-1";

/// The host device: signed in with a WorkOS session on its disk.
pub async fn host_device(dir: &Path, edge: &ScriptedEdge) -> EngineRuntime {
    std::fs::create_dir_all(dir).unwrap();
    let session = json!({
        "refreshToken": edge.seed_session(ORG),
        "user": { "id": USER, "email": "u@example.com" },
        "orgId": ORG,
    });
    std::fs::write(dir.join("session.json"), session.to_string()).unwrap();
    start_engine(&engine_config(dir, &edge.url, None)).await
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
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

/// Commit everything in `cwd` as a test user.
pub fn commit(cwd: &Path, message: &str) {
    git(cwd, &["add", "."]);
    git(
        cwd,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            message,
        ],
    );
}

/// A bare `origin` with one commit on `main`.
pub fn origin(at: &Path) {
    let seed = at.with_extension("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "reportly\n").unwrap();
    commit(&seed, "first");
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
pub fn project(dir: &Path, origin_url: &Path) -> PathBuf {
    let project = dir.join("reportly");
    std::fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(
        &project,
        &["remote", "add", "origin", &origin_url.to_string_lossy()],
    );
    project
}

pub async fn add_project(rpc: &RpcClient, host: &EngineRuntime, path: &Path) {
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
pub async fn create_on_place(rpc: &RpcClient, chat: &str, provider: &str) -> Value {
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

/// Send a message, as a client does: a Run command in the chat doc.
pub async fn send(rpc: &RpcClient, chat: &str, message_id: &str, cwd: &str, prompt: &str) {
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

pub async fn steps(rpc: &RpcClient, chat: &str) -> Value {
    rpc.call(methods::SANDBOX_STEPS, json!({ "chatId": chat }))
        .await
        .expect("SandboxSteps")
}

/// Poll the chat's steps until `done` says so; panics with the last answer.
pub async fn steps_until(
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

pub async fn ready(rpc: &RpcClient, chat: &str) -> Value {
    steps_until(rpc, chat, Duration::from_secs(60), |s| {
        assert!(s["state"] != "failed" && s["state"] != "gone", "{s:#}");
        s["state"] == "ready"
    })
    .await
}

/// Poll `check` until it gives a value; panics with `what` after `within`.
pub async fn eventually<T>(
    what: &str,
    within: Duration,
    mut check: impl AsyncFnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
