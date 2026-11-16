//! The Docker Sandbox provider (#40) against a local stand-in for Docker's
//! APIs (`support/docker_stub.rs`), which answers with the shapes recorded
//! from the real service. The manual smoke run against a real account is
//! `sandboxes::docker::smoke` (ignored; see its doc).

#[path = "support/docker_stub.rs"]
mod docker_stub;

use std::time::Duration;

use docker_stub::{DockerStub, end, exit, keepalive, output, started};
use serde_json::json;
use zeron_engine::sandboxes::docker::{DockerProvider, DockerUrls};
use zeron_engine::sandboxes::{
    Command, CreateSandbox, Credential, ErrorKind, Exit, KeySlot, Ops, RunEvent, SandboxProvider,
    SandboxStatus, run_to_end,
};

struct Fixture {
    stub: DockerStub,
    dir: tempfile::TempDir,
    provider: DockerProvider,
}

impl Fixture {
    async fn new() -> Self {
        let stub = DockerStub::start().await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider(&stub, dir.path());
        Self {
            stub,
            dir,
            provider,
        }
    }

    /// Connected through the device flow.
    async fn connected() -> Self {
        let fx = Self::new().await;
        fx.stub.state().pending_polls = 0;
        let code = fx.provider.begin_connect().await.unwrap().unwrap();
        fx.provider
            .connect(Credential::DeviceCode(code))
            .await
            .unwrap();
        fx
    }

    fn slot(&self) -> Option<String> {
        KeySlot::open(self.dir.path(), "docker").get()
    }
}

fn provider(stub: &DockerStub, dir: &std::path::Path) -> DockerProvider {
    DockerProvider::with_urls(KeySlot::open(dir, "docker"), DockerUrls::all(&stub.url))
        .poll_every(Duration::from_millis(10))
}

#[tokio::test]
async fn it_names_itself_and_supplies_every_operation() {
    let fx = Fixture::new().await;
    let p = &fx.provider;
    assert_eq!((p.id(), p.name()), ("docker", "Docker Sandbox"));
    assert_eq!(p.ops(), Ops::ALL, "{:?}", p.ops().names());
    assert_eq!(p.workdir("chat-1"), "/home/agent/workspace");
    // Not connected: every call says so, as a Config error.
    assert_eq!(p.account(), None);
    let err = p.templates().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config, "{err}");
    assert!(fx.stub.seen("/sbx/").is_empty());
}

#[tokio::test]
async fn connect_uses_dockers_device_flow() {
    let fx = Fixture::new().await;
    let code = fx.provider.begin_connect().await.unwrap().unwrap();
    assert_eq!(code.user_code, "ABCD-EFGH");
    assert_eq!(code.verification_uri, "https://login.docker.com/activate");
    let begun = &fx.stub.seen("/oauth/device/code")[0];
    assert_eq!(begun.body["client_id"], DockerUrls::CLIENT_ID);
    assert_eq!(begun.body["audience"], "https://hub.docker.com");
    assert!(begun.body["scope"].as_str().unwrap().contains("offline_access"));

    // One "authorization_pending", then the user approves.
    let account = fx
        .provider
        .connect(Credential::DeviceCode(code))
        .await
        .unwrap();
    assert_eq!(account.name, "vu-test");
    assert_eq!(fx.provider.account(), Some(account));
    let polls = fx.stub.seen("/oauth/token");
    assert_eq!(polls.len(), 2);
    assert_eq!(
        polls[0].body["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(polls[0].body["device_code"], "dev-code-1");
    // The slot keeps the refresh token, never the access token.
    let kept = fx.slot().unwrap();
    assert!(kept.contains("refresh-1"), "{kept}");
    assert!(!kept.contains("access-1"), "{kept}");

    // Signed in: the account is remembered across a restart.
    let again = provider(&fx.stub, fx.dir.path());
    assert_eq!(again.account().map(|a| a.name).as_deref(), Some("vu-test"));
}

#[tokio::test]
async fn an_expired_device_code_says_to_connect_again() {
    let fx = Fixture::new().await;
    fx.stub.state().pending_polls = 1_000;
    let mut code = fx.provider.begin_connect().await.unwrap().unwrap();
    code.expires_in = 1;
    let err = fx
        .provider
        .connect(Credential::DeviceCode(code))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config, "{err}");
    assert_eq!(fx.slot(), None);
}

#[tokio::test]
async fn access_tokens_refresh_with_rotating_refresh_tokens() {
    let fx = Fixture::connected().await;
    // Tokens that are already stale: every call needs a fresh one.
    fx.stub.state().token_life = 1;
    fx.provider.templates().await.unwrap(); // access-1 still good (900 s)
    assert_eq!(fx.stub.state().refreshes, 0);

    // Expire it: the stub stops taking access-1.
    fx.stub.state().access.remove("access-1");
    fx.provider.templates().await.unwrap(); // 401 → refresh → retry
    assert_eq!(fx.stub.state().refreshes, 1);
    assert!(fx.slot().unwrap().contains("refresh-2"));

    // Many calls at once with a stale token refresh once: a rotating refresh
    // token used twice would be refused (and sign the user out).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let calls = (0..6).map(|_| fx.provider.templates());
    for result in futures::future::join_all(calls).await {
        result.unwrap();
    }
    assert_eq!(fx.stub.state().refreshes, 2);
    let kept = fx.slot().unwrap();
    assert!(kept.contains("refresh-3"), "{kept}");

    // A refresh token Docker no longer takes: connect again (Config).
    fx.stub.state().refresh = Some("someone-else".into());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let err = fx.provider.templates().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config, "{err}");
}

#[tokio::test]
async fn a_pasted_personal_access_token_connects_through_docker_hub() {
    let fx = Fixture::new().await;
    fx.stub.state().pat = Some(("vu-test".into(), "dckr_pat_good".into()));

    let err = fx
        .provider
        .connect(Credential::Token("vu-test:dckr_pat_bad".into()))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config, "{err}");
    assert_eq!(fx.slot(), None);
    let err = fx
        .provider
        .connect(Credential::Token("dckr_pat_good".into()))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("username"), "{err}");

    let account = fx
        .provider
        .connect(Credential::Token(" vu-test:dckr_pat_good\n".into()))
        .await
        .unwrap();
    assert_eq!(account.name, "vu-test");
    let exchange = fx.stub.seen("/v2/auth/token").pop().unwrap();
    assert_eq!(
        exchange.body,
        json!({"identifier": "vu-test", "secret": "dckr_pat_good"})
    );
    // The hub token signs the control API in.
    fx.provider.templates().await.unwrap();
    let listed = fx.stub.seen("ListImages").pop().unwrap();
    assert!(listed.bearer.unwrap().starts_with("hub-"));
    // When it's refused, the key is exchanged again.
    fx.stub.state().access.clear();
    fx.provider.templates().await.unwrap();
    assert_eq!(fx.stub.seen("/v2/auth/token").len(), 3);
}

#[tokio::test]
async fn a_pasted_access_token_works_until_it_expires() {
    let fx = Fixture::new().await;
    let token = docker_stub::jwt(json!({"username": "vu-test", "exp": 4_000_000_000u64}));
    fx.stub.sign_in(&token);
    let account = fx
        .provider
        .connect(Credential::Token(token.clone()))
        .await
        .unwrap();
    assert_eq!(account.name, "vu-test");
    fx.provider.templates().await.unwrap();
    // No way to refresh it: refused means connect again.
    fx.stub.state().access.clear();
    let err = fx.provider.templates().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config, "{err}");
}

#[tokio::test]
async fn templates_are_the_accounts_finished_images() {
    let fx = Fixture::connected().await;
    let templates = fx.provider.templates().await.unwrap();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].id, "tmpl_001m79a5fdr3jvbrksq53bd183b");
    assert_eq!(templates[0].name, "zeron-dev");
    let call = fx.stub.seen("ImageService/ListImages").pop().unwrap();
    assert_eq!(call.content_type.as_deref(), Some("application/json"));
    assert_eq!(call.bearer.as_deref(), Some("access-1"));
}

#[tokio::test]
async fn create_stops_at_its_time_limit_allows_all_traffic_and_gets_the_accounts_secrets() {
    let fx = Fixture::connected().await;
    let mut request = CreateSandbox::new("chat-1", None);
    request.env.insert("ZERON_X".into(), "1".into());
    let id = fx.provider.create(request.clone()).await.unwrap();

    let create = fx.stub.seen("CreateSandbox").pop().unwrap();
    let body = &create.body;
    // Blank: Docker's plain image, inline, at a set size.
    assert_eq!(body["cloud"]["imageRef"], "docker.io/sbx/shell:latest");
    assert!(body.get("image").is_none());
    assert_eq!(body["resources"]["cpus"], 2);
    // Stops (never deletes) after 30 minutes unless extended.
    assert_eq!(body["cloud"]["onTimeout"], "ON_TIMEOUT_STOP");
    assert_eq!(body["cloud"]["timeout"], "1800s");
    // The account's proxy secrets: the sandbox sees placeholders.
    assert_eq!(
        body["cloud"]["secrets"],
        json!(["docker", "docker-sbx:github", "docker-sbx:iroha"])
    );
    assert_eq!(body["environment"], json!({"ZERON_X": "1"}));
    assert_eq!(body["requestId"], "chat-1");
    assert!(body["name"].as_str().unwrap().starts_with("zeron-"));

    // Its own network policy, allow-all; the account's default untouched.
    let policy = fx.stub.seen("network-policy");
    assert_eq!(policy.len(), 1);
    assert_eq!(policy[0].method, "PUT");
    assert_eq!(policy[0].path, format!("/v1/sandboxes/{id}/network-policy"));
    assert_eq!(policy[0].body, json!({"mode": "allow-all"}));
    assert_eq!(fx.stub.state().boxes[&id].network, "allow-all");

    // Asking again for the same chat finds the same sandbox.
    assert_eq!(fx.provider.create(request).await.unwrap(), id);
    assert_eq!(fx.stub.state().boxes.len(), 1);
}

#[tokio::test]
async fn a_template_keeps_its_own_size() {
    let fx = Fixture::connected().await;
    let template = Some("tmpl_001m79a5fdr3jvbrksq53bd183b".to_string());
    fx.provider
        .create(CreateSandbox::new("chat-2", template))
        .await
        .unwrap();
    let body = fx.stub.seen("CreateSandbox").pop().unwrap().body;
    assert_eq!(body["image"], "tmpl_001m79a5fdr3jvbrksq53bd183b");
    assert!(body.get("resources").is_none());
    assert!(body["cloud"].get("imageRef").is_none());
    assert_eq!(body["cloud"]["onTimeout"], "ON_TIMEOUT_STOP");
}

#[tokio::test]
async fn save_template_snapshots_the_disk_and_the_template_is_listed() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    // Docker takes a while: the operation is done after a few polls.
    fx.stub.state().snapshot_polls = 3;
    let saved = fx
        .provider
        .save_template("sbx_a", "reportly-dev")
        .await
        .unwrap();
    assert_eq!(saved.name, "reportly-dev");
    assert!(saved.id.starts_with("tmpl_"), "{}", saved.id);

    let create = fx.stub.seen("SnapshotService/CreateSnapshot").pop().unwrap();
    // The disk only: a memory capture would keep the running engine's lent
    // sign-in, which lives in memory alone.
    assert_eq!(
        create.body,
        json!({"sandbox": {"id": "sbx_a"}, "name": "reportly-dev",
               "captureMode": "CAPTURE_MODE_DISK"})
    );
    assert!(fx.stub.seen("OperationService/GetOperation").len() >= 3);
    // The sandbox keeps running.
    assert_eq!(fx.stub.state().boxes["sbx_a"].status, "SANDBOX_STATUS_RUNNING");

    // In the template picker.
    let templates = fx.provider.templates().await.unwrap();
    assert!(
        templates.iter().any(|t| t.id == saved.id && t.name == "reportly-dev"),
        "{templates:?}"
    );

    // A sandbox that's gone.
    let err = fx
        .provider
        .save_template("sbx_gone", "x")
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound, "{err}");

    // Docker failing the snapshot says why.
    fx.stub.state().snapshot_fails = Some("disk capture failed".into());
    let err = fx
        .provider
        .save_template("sbx_a", "again")
        .await
        .unwrap_err();
    assert!(err.message.contains("disk capture failed"), "{err}");
}

#[tokio::test]
async fn start_and_stop_wait_for_the_status_they_asked_for() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    fx.stub.state().transition_polls = 3;

    fx.provider.stop("sbx_a").await.unwrap();
    assert_eq!(fx.stub.state().boxes["sbx_a"].status, "SANDBOX_STATUS_STOPPED");
    let polls = fx.stub.seen("GetSandbox").len();
    assert!(polls >= 4, "polled {polls} times");
    // Stopping a stopped sandbox is done at once.
    fx.provider.stop("sbx_a").await.unwrap();

    fx.provider.start("sbx_a").await.unwrap();
    assert_eq!(fx.stub.state().boxes["sbx_a"].status, "SANDBOX_STATUS_RUNNING");
    let start = fx.stub.seen("StartSandbox").pop().unwrap();
    assert_eq!(start.body, json!({"sandbox": {"id": "sbx_a"}}));

    let err = fx.provider.start("sbx_gone").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound, "{err}");
}

#[tokio::test]
async fn delete_is_done_even_when_its_already_gone() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_STOPPED");
    fx.provider.delete("sbx_a").await.unwrap();
    assert!(fx.stub.state().boxes.is_empty());
    fx.provider.delete("sbx_a").await.unwrap();
}

#[tokio::test]
async fn status_says_running_stopped_or_gone() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    fx.stub.add_box("sbx_b", "SANDBOX_STATUS_STOPPED");
    let p = &fx.provider;
    assert_eq!(p.status("sbx_a").await.unwrap(), SandboxStatus::Running);
    assert_eq!(p.status("sbx_b").await.unwrap(), SandboxStatus::Stopped);
    // Deleted (in Docker's own app, say): `not_found`.
    p.delete("sbx_a").await.unwrap();
    assert_eq!(p.status("sbx_a").await.unwrap(), SandboxStatus::Gone);
    let get = fx.stub.seen("GetSandbox").pop().unwrap();
    assert_eq!(get.body, json!({"sandbox": {"id": "sbx_a"}}));
}

#[tokio::test]
async fn extend_renews_the_time_limit() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    fx.provider
        .extend("sbx_a", Duration::from_secs(1800))
        .await
        .unwrap();
    let renew = fx.stub.seen("RenewSandboxTimeout").pop().unwrap();
    assert_eq!(
        renew.body,
        json!({"sandbox": {"id": "sbx_a"}, "timeout": "1800s"})
    );
}

#[tokio::test]
async fn run_streams_output_and_the_exit_code_from_the_sandboxs_process_service() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    fx.stub.script_run(vec![
        started(),
        keepalive(),
        output("STREAM_TYPE_STDERR", "err\n"),
        output("STREAM_TYPE_STDOUT", "out\n"),
        exit(Some(3), "normal"),
        end(None),
    ]);
    let command = Command::new(["sh", "-c", "echo out; echo err >&2; exit 3"])
        .cwd("/tmp")
        .env("FOO", "bar")
        .timeout(Duration::from_secs(30));
    let events: Vec<RunEvent> = {
        use futures::StreamExt;
        fx.provider
            .run("sbx_a", command)
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await
    };
    assert_eq!(
        events,
        vec![
            RunEvent::Stderr(b"err\n".to_vec()),
            RunEvent::Stdout(b"out\n".to_vec()),
            RunEvent::Exit(Exit::code(3)),
        ]
    );
    let start = fx.stub.seen("ProcessService/Start").pop().unwrap();
    assert_eq!(start.path, "/box/sbx_a/cloud.todd.v1.ProcessService/Start");
    assert_eq!(start.content_type.as_deref(), Some("application/connect+json"));
    assert_eq!(start.bearer.as_deref(), Some("access-1"));
    assert_eq!(
        start.body,
        json!({
            "args": ["sh", "-c", "echo out; echo err >&2; exit 3"],
            "env": {"FOO": "bar"}, "workingDir": "/tmp", "timeoutSeconds": 30,
            "includeStdout": true, "includeStderr": true,
        })
    );
}

#[tokio::test]
async fn run_reads_dockers_exits_as_they_come() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    // Exit code 0 is left out of the JSON.
    fx.stub.script_run(vec![
        started(),
        output("STREAM_TYPE_STDOUT", "200"),
        exit(None, "normal"),
        end(None),
    ]);
    let done = run_to_end(&fx.provider, "sbx_a", Command::new(["true"]))
        .await
        .unwrap();
    assert_eq!((done.exit, done.stdout.as_str()), (Exit::code(0), "200"));

    // Killed at its timeout.
    fx.stub
        .script_run(vec![started(), exit(Some(137), "timeout"), end(None)]);
    let done = run_to_end(&fx.provider, "sbx_a", Command::new(["sleep", "10"]))
        .await
        .unwrap();
    assert!(done.exit.timed_out && !done.exit.success(), "{:?}", done.exit);

    // A program that isn't there: Docker ends the stream with an error.
    fx.stub.script_run(vec![end(Some((
        "internal",
        "command not found: command \"zeron-engine\" not found in container PATH",
    )))]);
    let err = run_to_end(&fx.provider, "sbx_a", Command::new(["zeron-engine", "--version"]))
        .await
        .unwrap_err();
    assert!(err.message.contains("not found"), "{err}");

    // The connection lost before an exit.
    fx.stub
        .script_run(vec![started(), output("STREAM_TYPE_STDOUT", "half")]);
    let err = run_to_end(&fx.provider, "sbx_a", Command::new(["x"]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Retryable, "{err}");
}

#[tokio::test]
async fn a_detached_command_outlives_the_call() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_RUNNING");
    fx.stub.script_run(vec![
        started(),
        output("STREAM_TYPE_STDOUT", "started\n"),
        exit(None, "normal"),
        end(None),
    ]);
    let command = Command::new(["zeron-engine", "watchdog"])
        .env("ZERON_EDGE_URL", "https://edge.example")
        .detached();
    let done = run_to_end(&fx.provider, "sbx_a", command).await.unwrap();
    assert!(done.exit.success());
    let body = fx.stub.seen("ProcessService/Start").pop().unwrap().body;
    let args: Vec<String> = serde_json::from_value(body["args"].clone()).unwrap();
    // `sh -c <launcher> <$0> <name> <program> <args>…`: the program in a
    // session of its own, one per name (a rerun replaces it). Restarting it
    // is `zeron-engine watchdog`'s job.
    assert_eq!(&args[..2], ["sh", "-c"]);
    assert!(args[2].contains("setsid") && args[2].contains("pkill"), "{}", args[2]);
    assert_eq!(&args[3..], ["zeron-detach", "zeron-engine", "zeron-engine", "watchdog"]);
    assert_eq!(body["env"]["ZERON_EDGE_URL"], "https://edge.example");
    assert!(body.get("timeoutSeconds").is_none_or(|t| t.as_i64() <= Some(60)));
}

#[tokio::test]
async fn a_dropped_connection_is_tried_again() {
    let fx = Fixture::connected().await;
    // This network resets connections to Docker now and then (seen while
    // recording).
    fx.stub.state().resets = 2;
    let templates = fx.provider.templates().await.unwrap();
    assert_eq!(templates.len(), 1);
}

#[tokio::test]
async fn running_in_a_stopped_sandbox_says_so() {
    let fx = Fixture::connected().await;
    fx.stub.add_box("sbx_a", "SANDBOX_STATUS_STOPPED");
    let err = fx
        .provider
        .run("sbx_a", Command::new(["true"]))
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
}
