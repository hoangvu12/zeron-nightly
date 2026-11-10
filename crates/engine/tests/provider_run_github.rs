//! Provider-run chats on GitHub projects (ticket #6), through the engine's
//! public request surface (`Mutate createSpace` / `createChat`), against the
//! fake Claude Code session server and a stub `claude` CLI.
//!
//! Asserted from outside only: what the fake server received (the create
//! request's source), the chat and project rows, and the transcript.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{MessageRole, MessageStatus};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::claude_cloud::credentials::ClaudeConfigDir;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_proto::HarnessId;
use zeron_provider_fake::{
    ClaudeLogin, FakeAccount, FakeSessionServer, SeedSession, StubClaude, Turn,
};
use zeron_rpc::methods;

const FIRST: &str = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
const PROMPT: &str = "Run `uname -a` and `cat /tmp/zeron-init-marker 2>&1`, then reply DONE";

struct Rig {
    tmp: tempfile::TempDir,
    fake: FakeSessionServer,
    _login: ClaudeLogin,
    _stub: StubClaude,
    core: EngineCore,
    client: zeron_rpc::RpcClient,
}

async fn rig() -> Rig {
    rig_on("dev-local").await
}

/// An engine with device id `device_id`, provider-run chats on.
async fn rig_on(device_id: &str) -> Rig {
    use zeron_engine::claude_cloud_sessions::{ClaudeCloudSessions, ClaudeCloudSettings};
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeSessionServer::start().await;
    fake.seed_session(SeedSession::environment("env_fake0001"));
    let login = ClaudeLogin::create(
        tmp.path().join("claude-login"),
        &FakeAccount::a(),
        "token-a-1",
    );
    let stub = StubClaude::install(tmp.path().join("bin"), &FakeAccount::a());
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeCloudHarness::new(ClaudeCloudConfig {
            base_url: fake.base_url().to_owned(),
            login: ClaudeConfigDir::at(login.dir()),
            claude_executable: Some(stub.path().to_owned()),
        })
        .expect("harness builds"),
    ));
    // The "Claude cloud sessions" setting, on before the engine starts.
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), device_id).unwrap();
    ClaudeCloudSessions::open(&data)
        .save(ClaudeCloudSettings {
            enabled: true,
            ..ClaudeCloudSettings::default()
        })
        .expect("setting saves");
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::ClaudeCodeCloud, None)
        .expect("engine core assembles");
    let client = zeron_rpc::memory_client(core.rpc_service());
    Rig {
        tmp,
        fake,
        _login: login,
        _stub: stub,
        core,
        client,
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A clone on this device whose `origin` is `remote`, on `branch`, with
/// `pushed` as the branches `origin` has (as if fetched).
fn clone_on_disk(root: &Path, name: &str, remote: Option<&str>, branch: &str) -> String {
    clone_with(root, name, remote, branch, &["main", branch])
}

fn clone_with(
    root: &Path,
    name: &str,
    remote: Option<&str>,
    branch: &str,
    pushed: &[&str],
) -> String {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "test@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "hi\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    if branch != "main" {
        git(&dir, &["checkout", "-q", "-b", branch]);
    }
    if let Some(remote) = remote {
        git(&dir, &["remote", "add", "origin", remote]);
        for pushed in pushed {
            let reference = format!("refs/remotes/origin/{pushed}");
            git(&dir, &["update-ref", &reference, "HEAD"]);
        }
    }
    dir.to_string_lossy().into_owned()
}

impl Rig {
    async fn create_space(
        &self,
        space_id: &str,
        device_id: &str,
        path: &str,
        github_repo: Option<&str>,
    ) -> Result<serde_json::Value, zeron_rpc::RpcError> {
        let mut params = serde_json::json!({
            "op": "createSpace",
            "spaceId": space_id,
            "deviceId": device_id,
            "path": path,
            "gitDetected": true,
        });
        if let Some(repo) = github_repo {
            params["githubRepo"] = repo.into();
        }
        self.client.call(methods::MUTATE, params).await
    }

    /// The composer's `createChat` for "Anthropic cloud" in a project.
    async fn create_in(
        &self,
        space_id: &str,
        branch: Option<&str>,
    ) -> Result<serde_json::Value, zeron_rpc::RpcError> {
        let mut params = serde_json::json!({
            "op": "createChat",
            "chatId": "client-picked-id",
            "deviceId": self.core.device_id,
            "spaceId": space_id,
            "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                       "sandbox": "workspace-write", "access": "full-access"},
            "prompt": PROMPT,
            "messageId": FIRST,
        });
        if let Some(branch) = branch {
            params["branch"] = branch.into();
        }
        self.client.call(methods::MUTATE, params).await
    }

    /// The `config.sources` of every session create the server received.
    fn created_sources(&self) -> Vec<serde_json::Value> {
        self.fake
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/code/sessions")
            .map(|r| r.body["config"]["sources"].clone())
            .collect()
    }

    fn mirrored(&self, chat: &str) -> bool {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
            .iter()
            .any(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
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

#[tokio::test(flavor = "multi_thread")]
async fn a_github_project_starts_a_session_on_its_repo_and_current_branch() {
    let rig = rig().await;
    let path = clone_on_disk(
        rig.tmp.path(),
        "app",
        Some("git@github.com:acme/app.git"),
        "feature/x",
    );
    rig.create_space("space-app", &rig.core.device_id, &path, None)
        .await
        .expect("createSpace");

    rig.fake.queue_turn(Turn::bash_tool());
    let created = rig.create_in("space-app", None).await.expect("createChat");

    // One session, on the project's repository and the branch checked out.
    assert_eq!(rig.fake.created_sessions().len(), 1);
    assert_eq!(
        rig.created_sources(),
        [serde_json::json!([{
            "type": "git_repository",
            "url": "https://github.com/acme/app",
            "revision": "feature/x",
        }])]
    );
    // The chat sits in the project, hosted here, in the session's pseudo cwd.
    let chat = created["chatId"].as_str().unwrap();
    let row = rig.core.workspace.chat(chat).unwrap().expect("chat row");
    assert_eq!(row.space_id.as_deref(), Some("space-app"));
    assert_eq!(row.device_id, rig.core.device_id);
    assert_eq!(row.branch.as_deref(), Some("feature/x"));
    let session = &rig.fake.created_sessions()[0];
    assert_eq!(
        row.cwd.as_deref(),
        Some(format!("claude-cloud://{session}").as_str())
    );
    assert_eq!(
        row.config.as_ref().map(|c| c.harness),
        Some(HarnessId::ClaudeCodeCloud)
    );
    wait_for("the first turn mirrored", || rig.mirrored(chat)).await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_branch_the_composer_names_wins() {
    let rig = rig().await;
    let path = clone_with(
        rig.tmp.path(),
        "app",
        Some("https://github.com/acme/app.git"),
        "main",
        &["main", "release/2"],
    );
    rig.create_space("space-app", &rig.core.device_id, &path, None)
        .await
        .expect("createSpace");
    rig.fake.queue_turn(Turn::bash_tool());
    rig.create_in("space-app", Some("release/2"))
        .await
        .expect("createChat");
    assert_eq!(
        rig.created_sources(),
        [serde_json::json!([{
            "type": "git_repository",
            "url": "https://github.com/acme/app",
            "revision": "release/2",
        }])]
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_without_a_github_remote_is_refused_before_any_session() {
    let rig = rig().await;
    let plain = clone_on_disk(rig.tmp.path(), "plain", None, "main");
    let gitlab = clone_on_disk(
        rig.tmp.path(),
        "gitlab",
        Some("https://gitlab.com/acme/app.git"),
        "main",
    );
    rig.create_space("space-plain", &rig.core.device_id, &plain, None)
        .await
        .expect("createSpace");
    rig.create_space("space-gitlab", &rig.core.device_id, &gitlab, None)
        .await
        .expect("createSpace");
    for space in ["space-plain", "space-gitlab"] {
        let error = rig
            .create_in(space, None)
            .await
            .expect_err("no GitHub repository");
        assert!(
            error.to_string().contains("has no GitHub remote"),
            "{space}: {error}"
        );
    }
    rig.create_in("space-missing", None)
        .await
        .expect_err("no such project");
    // Nothing reached the provider, and no chat was made.
    assert!(rig.fake.requests().is_empty(), "{:?}", rig.fake.requests());
    assert!(rig.core.workspace.read_chats().unwrap().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_from_github_lives_at_the_provider_and_starts_chats() {
    let rig = rig().await;
    let device = zeron_proto::provider_cloud::PROJECTS_DEVICE;
    // The engine says it understands such projects, so clients offer them.
    let me = rig
        .core
        .workspace
        .read_devices()
        .unwrap()
        .into_iter()
        .find(|d| d.id == rig.core.device_id)
        .expect("own device row");
    assert!(me.supports(zeron_proto::capabilities::PROVIDER_PROJECTS_V1));
    // "New project from GitHub…": a project no device has cloned.
    rig.create_space("space-web", device, "acme/web", Some("acme/web"))
        .await
        .expect("createSpace");
    let space = rig
        .core
        .workspace
        .space("space-web")
        .unwrap()
        .expect("project row");
    assert_eq!(space.device_id, device);
    assert_eq!(space.github_repo.as_deref(), Some("acme/web"));
    // The same repository again is the same project.
    rig.create_space("space-web-2", device, "acme/web", Some("ACME/web"))
        .await
        .expect("createSpace again");
    assert!(rig.core.workspace.space("space-web-2").unwrap().is_none());
    // A name that isn't a GitHub repository is refused.
    rig.create_space("space-bad", device, "not a repo", Some("not a repo"))
        .await
        .expect_err("not owner/name");

    rig.fake.queue_turn(Turn::bash_tool());
    let created = rig.create_in("space-web", None).await.expect("createChat");
    // Its default branch: no revision.
    assert_eq!(
        rig.created_sources(),
        [serde_json::json!([{
            "type": "git_repository",
            "url": "https://github.com/acme/web",
        }])]
    );
    let chat = created["chatId"].as_str().unwrap();
    let row = rig.core.workspace.chat(chat).unwrap().expect("chat row");
    assert_eq!(row.space_id.as_deref(), Some("space-web"));
    // Hosted by the device that created it, never by the projects device.
    assert_eq!(row.device_id, rig.core.device_id);
    wait_for("the first turn mirrored", || rig.mirrored(chat)).await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_project_still_starts_an_empty_sandbox() {
    let rig = rig().await;
    rig.fake.queue_turn(Turn::bash_tool());
    rig.client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createChat",
                "chatId": "client-picked-id",
                "deviceId": rig.core.device_id,
                "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                           "sandbox": "workspace-write", "access": "full-access"},
                "prompt": PROMPT,
                "messageId": FIRST,
            }),
        )
        .await
        .expect("createChat");
    assert_eq!(rig.created_sources(), [serde_json::json!([])]);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_github_doesnt_have_is_never_sent() {
    let rig = rig().await;
    // On a local branch that was never pushed; origin has main only.
    let path = clone_with(
        rig.tmp.path(),
        "app",
        Some("https://github.com/acme/app.git"),
        "wip/local",
        &["main"],
    );
    rig.create_space("space-app", &rig.core.device_id, &path, None)
        .await
        .expect("createSpace");

    // Named by the composer: refused before anything reaches the provider.
    let error = rig
        .create_in("space-app", Some("wip/local"))
        .await
        .expect_err("not on GitHub");
    assert!(
        error.to_string().contains("wip/local isn't on GitHub yet"),
        "{error}"
    );
    assert!(rig.fake.requests().is_empty(), "{:?}", rig.fake.requests());

    // The branch checked out, unnamed: the repository's default branch.
    rig.fake.queue_turn(Turn::bash_tool());
    let created = rig.create_in("space-app", None).await.expect("createChat");
    assert_eq!(
        rig.created_sources(),
        [serde_json::json!([{
            "type": "git_repository",
            "url": "https://github.com/acme/app",
        }])]
    );
    let chat = created["chatId"].as_str().unwrap();
    let row = rig.core.workspace.chat(chat).unwrap().expect("chat row");
    assert_eq!(row.branch, None);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_at_the_provider_takes_no_local_chat() {
    let rig = rig().await;
    let device = zeron_proto::provider_cloud::PROJECTS_DEVICE;
    rig.create_space("space-web", device, "acme/web", Some("acme/web"))
        .await
        .expect("createSpace");
    // A plain chat in it would be hosted by a device that doesn't exist.
    for params in [
        serde_json::json!({"op": "createChat", "chatId": "plain", "spaceId": "space-web"}),
        serde_json::json!({"op": "createChat", "chatId": "plain-2", "spaceId": "space-web",
                           "deviceId": rig.core.device_id,
                           "config": {"harness": "mock", "model": null, "reasoning": null,
                                      "sandbox": "workspace-write", "access": "full-access"}}),
    ] {
        let error = rig
            .client
            .call(methods::MUTATE, params)
            .await
            .expect_err("only provider-run chats");
        assert!(error.to_string().contains("Anthropic cloud"), "{error}");
    }
    assert!(rig.core.workspace.read_chats().unwrap().is_empty());
    // A provider cloud project without its repository is refused too.
    rig.client
        .call(
            methods::MUTATE,
            serde_json::json!({"op": "createSpace", "spaceId": "space-bare",
                               "deviceId": device, "path": "acme/x"}),
        )
        .await
        .expect_err("githubRepo is required");
    // A link to a branch names the repository.
    rig.create_space(
        "space-tree",
        device,
        "acme/tree",
        Some("https://github.com/acme/tree/tree/main"),
    )
    .await
    .expect("createSpace from a branch link");
    let space = rig.core.workspace.space("space-tree").unwrap().unwrap();
    assert_eq!(space.github_repo.as_deref(), Some("acme/tree"));
    rig.core.shutdown().await;
}

/// The checkout is on device A; the person, on device B, starts an
/// Anthropic cloud chat in it. B learns the repository from the synced
/// project row, creates the session on it, and hosts the chat.
#[tokio::test(flavor = "multi_thread")]
async fn a_checkout_on_another_device_starts_a_session_here() {
    let a = rig_on("dev-a").await;
    let b = rig_on("dev-b").await;
    let room = zeron_sync::registry::mock_server::MockRegistryServer::start().await;
    a.core.workspace.connect_registry_url(&room.url());
    b.core.workspace.connect_registry_url(&room.url());

    let path = clone_on_disk(
        a.tmp.path(),
        "app",
        Some("https://github.com/acme/app.git"),
        "feature/x",
    );
    a.create_space("space-app", "dev-a", &path, None)
        .await
        .expect("createSpace on A");
    // A stamps the project's GitHub repository; the row reaches B.
    wait_for("B sees the project's GitHub repository", || {
        b.core
            .workspace
            .space("space-app")
            .ok()
            .flatten()
            .is_some_and(|s| s.github_repo.as_deref() == Some("acme/app"))
    })
    .await;

    b.fake.queue_turn(Turn::bash_tool());
    // B can't read A's checkout: the composer names the branch it showed.
    let created = b
        .create_in("space-app", Some("feature/x"))
        .await
        .expect("createChat on B");
    assert_eq!(
        b.created_sources(),
        [serde_json::json!([{
            "type": "git_repository",
            "url": "https://github.com/acme/app",
            "revision": "feature/x",
        }])]
    );
    assert!(a.fake.requests().is_empty());
    let chat = created["chatId"].as_str().unwrap().to_owned();
    let row = b.core.workspace.chat(&chat).unwrap().expect("chat row");
    assert_eq!(row.device_id, "dev-b");
    assert_eq!(row.space_id.as_deref(), Some("space-app"));
    wait_for("the first turn mirrored on B", || b.mirrored(&chat)).await;
    // A sees the chat in its project, hosted by B.
    wait_for("A sees B's chat", || {
        a.core
            .workspace
            .chat(&chat)
            .ok()
            .flatten()
            .is_some_and(|row| row.device_id == "dev-b")
    })
    .await;
    // Without a branch, B (no checkout of its own) starts on the default
    // branch.
    b.fake.queue_turn(Turn::bash_tool());
    b.client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createChat", "chatId": "x", "deviceId": "dev-b", "spaceId": "space-app",
                "config": {"harness": "claude-code-cloud", "model": null, "reasoning": null,
                           "sandbox": "workspace-write", "access": "full-access"},
                "prompt": PROMPT, "messageId": "1f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f71",
            }),
        )
        .await
        .expect("second createChat on B");
    assert_eq!(
        b.created_sources()[1],
        serde_json::json!([{"type": "git_repository", "url": "https://github.com/acme/app"}])
    );
    a.core.shutdown().await;
    b.core.shutdown().await;
}
