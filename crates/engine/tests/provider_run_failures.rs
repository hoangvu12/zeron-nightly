//! Provider-run chats fail safe (ticket #15; docs/design/provider-run-chats.md
//! §7, "Failure handling"): the whole feature turns itself off on this
//! device, saying why, on a `DISABLED` file in its data folder, a 401 that
//! persists after the CLI renewed the login, a 403 or 404 from the session
//! list or create, or a stream event in a shape Zeron doesn't know. Once off
//! it stays off across restarts until the file is removed. A 403 or 404 for
//! one session ends only that chat.
//!
//! One real engine through its public surface, restarted on the same data
//! folder, against the fake Claude Code session server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionCommandStatus,
    SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::{HarnessId, RunRequest, SandboxLevel};
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, Fault, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const SECOND: &str = "0d2b5c1a-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const FIRST_PROMPT: &str = "Run `uname -a` and `cat /tmp/zeron-init-marker 2>&1`, then reply DONE";
const TOKEN: &str = "token-a-1";

struct World {
    tmp: tempfile::TempDir,
    fake: FakeSessionServer,
    login: ClaudeLogin,
    stub: StubClaude,
}

impl World {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeSessionServer::start().await;
        fake.seed_session(SeedSession::environment("env_fake0001"));
        let login = ClaudeLogin::create(tmp.path().join("claude-login"), &FakeAccount::a(), TOKEN);
        let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
        Self {
            tmp,
            fake,
            login,
            stub,
        }
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("dev-a")
    }

    /// The feature's kill switch: `DISABLED` in its data folder.
    fn kill_switch(&self) -> PathBuf {
        self.dir().join("claude-cloud").join("DISABLED")
    }

    fn engine(&self) -> Host {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("device-id"), "dev-a").unwrap();
        let registry = HarnessRegistry::new();
        // The local Claude Code harness too, as on any device, pointed at a
        // harmless executable: an accidental local launch must fail before
        // it starts anything.
        registry.register(Arc::new(
            zeron_harness::ClaudeHarness::new()
                .with_executable(std::env::current_exe().expect("test binary")),
        ));
        registry.register(Arc::new(
            ClaudeCloudHarness::new(ClaudeCloudConfig {
                base_url: self.fake.base_url().to_owned(),
                login: ClaudeConfigDir::at(self.login.dir()),
                claude_executable: Some(self.stub.path().to_owned()),
            })
            .expect("harness builds"),
        ));
        {
            use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
            ClaudeCloudSessions::open(&dir)
                .save(ClaudeCloudSettings {
                    enabled: true,
                    ..ClaudeCloudSettings::default()
                })
                .expect("setting saves");
        }
        let core = EngineCore::assemble(&dir, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
            .expect("engine core assembles");
        let client = zeron_rpc::memory_client(core.rpc_service());
        Host { core, client }
    }
}

struct Host {
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

impl Host {
    async fn create(&self, message_id: &str) -> Result<(String, String), String> {
        let created = self
            .client
            .call(
                methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "client-picked-id",
                    "deviceId": self.core.device_id,
                    "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                               "sandbox": "workspace-write", "access": "full-access"},
                    "prompt": FIRST_PROMPT,
                    "messageId": message_id,
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok((
            created["chatId"].as_str().unwrap().to_owned(),
            created["cwd"].as_str().unwrap().to_owned(),
        ))
    }

    /// Why the feature is off on this device, as the settings page reads it.
    async fn disabled(&self) -> Option<String> {
        let status = self
            .client
            .call(methods::GET_CLAUDE_CLOUD_SESSIONS, serde_json::json!({}))
            .await
            .expect("settings status");
        status["disabled"].as_str().map(str::to_owned)
    }

    /// Whether this device advertises that it can host provider-run chats.
    fn eligible(&self) -> bool {
        self.core
            .workspace
            .read_devices()
            .unwrap_or_default()
            .iter()
            .find(|d| d.id == self.core.device_id)
            .is_some_and(|d| {
                d.capabilities
                    .iter()
                    .any(|c| c == zeron_proto::capabilities::PROVIDER_RUN_V1)
            })
    }

    fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
    }

    fn settled(&self, chat: &str) -> bool {
        let entries = self.entries(chat);
        !entries.is_empty()
            && entries
                .iter()
                .all(|e| e.status != Some(MessageStatus::Streaming))
    }

    /// Every error the transcript shows.
    fn errors(&self, chat: &str) -> Vec<String> {
        self.entries(chat)
            .iter()
            .flat_map(|e| e.parts.clone())
            .filter_map(|p| match p {
                MessagePart::Error { message, .. } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn send(&self, chat: &str, cwd: &str, message_id: &str, prompt: &str) {
        self.send_with(chat, cwd, message_id, prompt, None);
    }

    /// A message whose request names `harness` (an older client's).
    fn send_with(
        &self,
        chat: &str,
        cwd: &str,
        message_id: &str,
        prompt: &str,
        harness: Option<HarnessId>,
    ) {
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Run {
                    request: RunRequest {
                        mcp: None,
                        prompt: prompt.into(),
                        harness,
                        model: None,
                        reasoning: None,
                        model_options: Default::default(),
                        cwd: cwd.into(),
                        sandbox: SandboxLevel::WorkspaceWrite,
                        auto_approve: false,
                        access: Default::default(),
                        plan: false,
                        attachments: Vec::new(),
                        worktree: None,
                        resume: None,
                        env: Default::default(),
                        extra_mcp: Vec::new(),
                    },
                    message_id: message_id.into(),
                },
            )
            .expect("queue message");
    }

    fn outcome(&self, chat: &str) -> Vec<(SessionCommandStatus, Option<String>)> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_commands().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.status, c.resolution))
            .collect()
    }

    fn config(&self, chat: &str) -> Option<zeron_proto::ChatConfig> {
        self.core.workspace.chat(chat).ok().flatten()?.config
    }
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_disabled(host: &Host) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(reason) = host.disabled().await {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the feature to turn off"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Create a chat and wait for its first turn to be mirrored.
async fn chat(world: &World, host: &Host) -> (String, String) {
    world.fake.queue_turn(Turn::bash_tool());
    let (chat, cwd) = host.create(FIRST).await.expect("createChat");
    wait_for("the first turn mirrored", || {
        host.settled(&chat)
            && host
                .entries(&chat)
                .iter()
                .any(|e| e.role == MessageRole::Assistant)
    })
    .await;
    (chat, cwd)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_file_keeps_the_feature_off_saying_why() {
    let world = World::new().await;
    std::fs::create_dir_all(world.kill_switch().parent().unwrap()).unwrap();
    std::fs::write(world.kill_switch(), "Turned off by hand.\n").unwrap();
    let host = world.engine();

    let reason = host.disabled().await.expect("off, with a reason");
    assert!(reason.contains("Turned off by hand."), "{reason}");
    let refused = host.create(FIRST).await.expect_err("nothing is created");
    assert!(refused.contains("Turned off by hand."), "{refused}");
    assert!(
        world.fake.requests().is_empty(),
        "nothing reached Anthropic"
    );
    assert!(!host.eligible(), "the device doesn't offer to host");
    host.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_file_placed_while_running_stops_everything_at_once() {
    let world = World::new().await;
    let host = world.engine();
    let (chat, cwd) = chat(&world, &host).await;
    assert!(host.eligible());
    let before = world.fake.requests().len();

    std::fs::create_dir_all(world.kill_switch().parent().unwrap()).unwrap();
    std::fs::write(world.kill_switch(), "").unwrap();
    host.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    let reason = wait_disabled(&host).await;
    assert!(reason.contains("DISABLED"), "{reason}");
    wait_for("the chat says why", || {
        host.errors(&chat).iter().any(|e| e.contains("DISABLED"))
    })
    .await;
    let after: Vec<_> = world.fake.requests()[before..]
        .iter()
        .filter(|r| r.method == "POST")
        .map(|r| r.path.clone())
        .collect();
    assert!(after.is_empty(), "nothing was sent: {after:?}");
    wait_for("the device no longer offers to host", || !host.eligible()).await;
    host.core.shutdown().await;
}

/// A refusal of the session list or of create, from a login whose identity
/// checked out: Anthropic changed or blocked the API. The feature turns off
/// and stays off after a restart (the reason is kept in `DISABLED`).
async fn a_refusal_turns_the_feature_off_for_good(method: &'static str, status: u16) {
    let world = World::new().await;
    let host = world.engine();
    world.fake.inject(Fault {
        method: Some(method),
        path_contains: "/v1/code/sessions".into(),
        status,
        times: 1,
        retry_after: None,
    });
    let refused = host.create(FIRST).await.expect_err("refused");
    let reason = wait_disabled(&host).await;
    assert!(reason.contains(&status.to_string()), "{reason}");
    assert!(refused.contains(&status.to_string()), "{refused}");
    assert!(world.fake.created_sessions().is_empty());
    assert!(world.kill_switch().exists(), "the switch is kept");
    host.core.shutdown().await;
    drop(host);

    let host = world.engine();
    let still = host.disabled().await.expect("still off after a restart");
    assert_eq!(still, reason);
    let requests = world.fake.requests().len();
    assert!(host.create(FIRST).await.is_err());
    assert_eq!(world.fake.requests().len(), requests, "nothing sent");
    host.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_403_from_the_session_list_turns_the_feature_off_for_good() {
    a_refusal_turns_the_feature_off_for_good("GET", 403).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_404_from_create_turns_the_feature_off_for_good() {
    a_refusal_turns_the_feature_off_for_good("POST", 404).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_that_outlasts_renewal_turns_the_feature_off_for_good() {
    let world = World::new().await;
    let host = world.engine();
    world.fake.reject_token(TOKEN);
    assert!(host.create(FIRST).await.is_err());
    let reason = wait_disabled(&host).await;
    assert!(world.kill_switch().exists(), "{reason}");
    host.core.shutdown().await;
    drop(host);
    let host = world.engine();
    assert_eq!(host.disabled().await.as_deref(), Some(reason.as_str()));
    host.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_event_in_an_unknown_shape_turns_the_feature_off() {
    let world = World::new().await;
    let host = world.engine();
    let (chat, _cwd) = chat(&world, &host).await;
    let session = world.fake.created_sessions()[0].clone();

    world
        .fake
        .send_raw(&session, "client_event", r#"{"what":"is this"}"#);
    let reason = wait_disabled(&host).await;
    assert!(reason.to_lowercase().contains("doesn't know"), "{reason}");
    wait_for("the chat says why", || !host.errors(&chat).is_empty()).await;
    assert!(world.kill_switch().exists());
    host.core.shutdown().await;
}

/// One session gone or forbidden (a 404 or 403 for it) ends that chat only:
/// the feature stays on, and another chat works.
#[tokio::test(flavor = "multi_thread")]
async fn a_404_for_one_session_ends_only_that_chat() {
    let world = World::new().await;
    let host = world.engine();
    let (chat, cwd) = chat(&world, &host).await;
    let session = world.fake.created_sessions()[0].clone();
    host.core.shutdown().await;
    drop(host);

    let host = world.engine();
    world.fake.inject(Fault {
        method: None,
        path_contains: format!("/v1/code/sessions/{session}/"),
        status: 404,
        times: 100,
        retry_after: None,
    });
    host.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the chat says it can't go on", || {
        !host.errors(&chat).is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.disabled().await, None);
    assert!(!world.kill_switch().exists());

    // Another chat starts and runs.
    world.fake.queue_turn(Turn::bash_tool());
    let (other, _) = host
        .create("7e6d5c4b-3a29-4180-9f7e-6d5c4b3a2918")
        .await
        .expect("createChat");
    wait_for("the other chat's reply", || {
        host.entries(&other)
            .iter()
            .any(|e| e.role == MessageRole::Assistant)
    })
    .await;
    host.core.shutdown().await;
}

/// An older build wiped the chat's settings (its harness is another one
/// now): the host restores them from the chat's provider session, and the
/// chat keeps working.
#[tokio::test(flavor = "multi_thread")]
async fn wiped_settings_are_restored_and_the_chat_keeps_working() {
    let world = World::new().await;
    let host = world.engine();
    let (chat, cwd) = chat(&world, &host).await;
    let mut wiped = host.config(&chat).expect("config");
    wiped.harness = HarnessId::ClaudeCode;
    wiped.model = Some("claude-sonnet-5".into());
    host.client
        .call(
            methods::MUTATE,
            serde_json::json!({"op": "setChatConfig", "chatId": chat, "config": wiped}),
        )
        .await
        .expect("an older client's write");
    wait_for("the settings restored", || {
        host.config(&chat)
            .is_some_and(|c| c.harness == HarnessId::ClaudeCodeCloud)
    })
    .await;
    // Only the harness changes back; the rest of what was written stands.
    assert_eq!(
        host.config(&chat).unwrap().model.as_deref(),
        Some("claude-sonnet-5")
    );
    assert_eq!(
        host.core.workspace.chat(&chat).unwrap().unwrap().device_id,
        host.core.device_id
    );

    world.fake.queue_turn(Turn::text_zulu());
    host.send(&chat, &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the reply", || {
        host.settled(&chat)
            && host
                .entries(&chat)
                .last()
                .is_some_and(|e| e.role == MessageRole::Assistant)
            && host.entries(&chat).iter().any(|e| e.id == SECOND)
    })
    .await;
    host.core.shutdown().await;
}

/// A chat an older client copied from a provider-run one: it names the
/// harness (and the session's pseudo folder) but has no provider session.
/// It refuses to run, and nothing reaches Anthropic.
#[tokio::test(flavor = "multi_thread")]
async fn a_copied_chat_without_its_provider_session_refuses_to_run() {
    let world = World::new().await;
    let host = world.engine();
    let (chat, cwd) = chat(&world, &host).await;
    let config = host.config(&chat).expect("config");
    host.core
        .workspace
        .create_chat(
            "copy-1",
            None,
            Some(&host.core.device_id),
            Some(config),
            Some(cwd.clone()),
        )
        .expect("the copy");
    let before = world.fake.requests().len();

    host.send("copy-1", &cwd, SECOND, "Reply with the single word ZULU.");
    wait_for("the send refused", || {
        host.outcome("copy-1")
            .iter()
            .any(|(status, _)| *status == SessionCommandStatus::Rejected)
    })
    .await;
    let outcome = host.outcome("copy-1");
    assert!(
        outcome.iter().any(|(_, why)| why
            .as_deref()
            .is_some_and(|w| w.contains("no Claude cloud session"))),
        "{outcome:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(world.fake.requests().len(), before, "nothing sent");
    host.core.shutdown().await;
}

/// A provider-run chat launched locally by mistake (an older client's
/// message naming the local Claude Code harness): its working directory is
/// the session's pseudo path, which exists nowhere, so the launch fails
/// before anything runs.
#[tokio::test(flavor = "multi_thread")]
async fn an_accidental_local_launch_fails_on_its_missing_folder() {
    let world = World::new().await;
    let host = world.engine();
    let (chat, cwd) = chat(&world, &host).await;
    assert!(cwd.starts_with("claude-cloud://"), "{cwd}");
    assert!(!std::path::Path::new(&cwd).is_dir());
    let before = world.fake.requests().len();

    host.send_with(
        &chat,
        &cwd,
        SECOND,
        "Reply with the single word ZULU.",
        Some(HarnessId::ClaudeCode),
    );
    wait_for("the launch failed", || {
        let errors = host.errors(&chat);
        let refused = host
            .outcome(&chat)
            .into_iter()
            .any(|(status, why)| status == SessionCommandStatus::Rejected && why.is_some());
        !errors.is_empty() || refused
    })
    .await;
    let said: Vec<String> = host
        .errors(&chat)
        .into_iter()
        .chain(host.outcome(&chat).into_iter().filter_map(|(_, why)| why))
        .collect();
    assert!(
        said.iter()
            .any(|s| s.contains("working directory") || s.contains("claude-cloud://")),
        "{said:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(world.fake.requests().len(), before, "nothing sent");
    host.core.shutdown().await;
}
