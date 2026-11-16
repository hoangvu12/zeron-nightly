//! The host device (spec #37, docs/design/cloud-byo.md "Picking the host
//! device", ADR 0004), at the engine's RPCs: two of the user's devices, a
//! laptop and an always-on `zeron headless` server, share a scripted edge and
//! registry. The user connects a fake provider on the laptop; Zeron picks the
//! server as host device and sends it the key once. The laptop creates
//! sandboxes by asking the server (`targetDeviceId` forwarding), and
//! moving the host device moves the key while a running sandbox goes on, its
//! next token lent by the new host device.
//!
//! What's asserted is what the user's devices see: RPC answers, device rows,
//! the chat row, what the provider was asked to do, which device's disk holds
//! the key, and the sandbox staying online past its token's lifetime.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fake_provider::{FakeProvider, engine_config, start_engine};
use scripted_edge::{ScriptedEdge, USER};
use serde_json::{Value, json};
use zeron_engine::EngineRuntime;
use zeron_rpc::{RpcClient, memory_client, methods};

const ORG: &str = "org_home";
/// What the user pasted to connect the fake cloud. Never echoed back.
const SECRET: &str = "fake-cloud-key-do-not-echo";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One of the user's devices, signed in with a WorkOS session on its disk.
/// `headless`: an always-on `zeron headless` server.
async fn device(dir: &Path, edge: &ScriptedEdge, headless: bool) -> EngineRuntime {
    std::fs::create_dir_all(dir).unwrap();
    let session = json!({
        "refreshToken": edge.seed_session(ORG),
        "user": { "id": USER, "email": "u@example.com" },
        "orgId": ORG,
    });
    std::fs::write(dir.join("session.json"), session.to_string()).unwrap();
    let runtime = start_engine(&engine_config(dir, &edge.url, None)).await;
    if headless {
        runtime.advertise_headless();
    }
    runtime
}

/// The fake cloud account: whichever device holds its key reaches the same
/// sandboxes. Each device has the provider built in, keeping the key in its
/// own data dir, as Docker Sandbox does.
struct Cloud {
    provider: Arc<FakeProvider>,
}

impl Cloud {
    fn new(root: &Path) -> Self {
        Self {
            provider: FakeProvider::cloud(root),
        }
    }

    /// The provider built into `engine`, whose data is in `dir`.
    fn build_into(&self, engine: &EngineRuntime, dir: &Path) {
        engine
            .core()
            .sandboxes
            .register(self.provider.on_device(dir));
    }
}

async fn eventually<T>(within: Duration, mut probe: impl AsyncFnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = probe().await {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The device list as `rpc`'s device sees it.
async fn devices(rpc: &RpcClient) -> Vec<Value> {
    let mut stream = rpc
        .subscribe(methods::WATCH_DEVICES, json!({}))
        .await
        .expect("WatchDevices");
    let list = stream.recv().await.expect("a device list");
    list.as_array().cloned().unwrap_or_default()
}

fn row<'a>(devices: &'a [Value], id: &str) -> Option<&'a Value> {
    devices.iter().find(|d| d["id"] == id)
}

fn has_capability(device: &Value, capability: &str) -> bool {
    device["capabilities"]
        .as_array()
        .is_some_and(|c| c.iter().any(|v| v == capability))
}

/// Wait until `rpc`'s device sees `other` online.
async fn sees_online(rpc: &RpcClient, other: &str) {
    let seen = eventually(Duration::from_secs(30), async || {
        let list = devices(rpc).await;
        let row = row(&list, other)?.clone();
        let fresh = row["lastSeenAt"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|at| chrono::Utc::now().signed_duration_since(at).num_seconds() < 30);
        fresh.then_some(())
    })
    .await;
    assert!(seen.is_some(), "{other} never came online");
}

/// Wait until `rpc`'s device has `other`'s row saying it's headless.
async fn sees_headless(rpc: &RpcClient, other: &str) {
    let seen = eventually(Duration::from_secs(30), async || {
        let list = devices(rpc).await;
        row(&list, other)
            .is_some_and(|d| has_capability(d, "headless-v1"))
            .then_some(())
    })
    .await;
    assert!(seen.is_some(), "{other}'s row never said it's headless");
}

async fn host_of(rpc: &RpcClient) -> Value {
    rpc.call(methods::SANDBOX_HOST, json!({ "provider": "fake" }))
        .await
        .expect("SandboxHost")
}

/// Wait until `rpc`'s device names `host` the fake cloud's host device.
async fn names_host(rpc: &RpcClient, host: &str) -> Value {
    let named = eventually(Duration::from_secs(30), async || {
        let now = host_of(rpc).await;
        (now["hostDeviceId"] == host).then_some(now)
    })
    .await;
    match named {
        Some(named) => named,
        None => panic!("never named {host}: {:#}", host_of(rpc).await),
    }
}

/// Every file under `dir` that holds `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
            {
                out.push(path);
            }
        }
    }
    out
}

async fn steps(rpc: &RpcClient, chat: &str) -> Value {
    rpc.call(methods::SANDBOX_STEPS, json!({ "chatId": chat }))
        .await
        .expect("SandboxSteps")
}

async fn ready(rpc: &RpcClient, chat: &str) -> Value {
    let done = eventually(Duration::from_secs(60), async || {
        let now = steps(rpc, chat).await;
        assert_ne!(now["state"], "failed", "{now:#}");
        (now["state"] == "ready").then_some(now)
    })
    .await;
    done.unwrap_or_else(|| panic!("{chat} never got ready"))
}

/// Wait until the token `auth` holds now has expired (by its own `exp`).
async fn token_runs_out(auth: &zeron_engine::Auth) {
    // (Between two tokens it may hold none for a moment.)
    let exp = eventually(Duration::from_secs(30), async || {
        auth.access_token_with_expiry().await.ok()?.1
    })
    .await
    .expect("a token that says when it runs out");
    let left = exp + 1 - scripted_edge::now_secs();
    if left > 0 {
        tokio::time::sleep(Duration::from_secs(left as u64)).await;
    }
}

/// Whether `sandbox`'s engine answers through its device room, asked by
/// `rpc`'s device.
async fn answers(engine: &EngineRuntime, sandbox: &str) -> bool {
    let Some(links) = engine.core().links() else {
        return false;
    };
    let asked = tokio::time::timeout(Duration::from_secs(10), async {
        let client = links.client(sandbox).await.ok()?;
        client.call(methods::ENGINE_INFO, json!({})).await.ok()
    })
    .await;
    if !matches!(&asked, Ok(Some(info)) if info["deviceId"] == sandbox) {
        links.invalidate(sandbox);
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_say_whether_they_are_headless_and_since_when_they_are_online() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let laptop = device(&dirs.path().join("laptop"), &edge, false).await;
    let server = device(&dirs.path().join("server"), &edge, true).await;
    let rpc = memory_client(laptop.core().rpc_service());
    let server_id = server.core().device_id.clone();
    sees_online(&rpc, &server_id).await;

    let list = eventually(Duration::from_secs(30), async || {
        let list = devices(&rpc).await;
        row(&list, &server_id)
            .is_some_and(|s| has_capability(s, "headless-v1"))
            .then_some(list)
    })
    .await
    .expect("the server's row says it's headless");
    let server_row = row(&list, &server_id).unwrap();
    let laptop_row = row(&list, &laptop.core().device_id).unwrap();
    assert!(!has_capability(laptop_row, "headless-v1"), "{laptop_row:#}");
    // Both can be host devices.
    assert!(has_capability(server_row, "sandbox-host-v1"), "{server_row:#}");
    assert!(has_capability(laptop_row, "sandbox-host-v1"), "{laptop_row:#}");
    // Each says since when it's online (this run).
    for row in [server_row, laptop_row] {
        let since = row["onlineSince"].as_str().expect("onlineSince");
        let since = chrono::DateTime::parse_from_rfc3339(since).unwrap();
        assert!(
            chrono::Utc::now().signed_duration_since(since).num_seconds() < 120,
            "{row:#}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_device_keeps_the_key_and_other_devices_ask_it() {
    let edge = ScriptedEdge::with_registry().await;
    // Tokens live seconds, so a running sandbox asks for its next one
    // several times during the test.
    let ttl = 16;
    edge.set_ttl(ORG, ttl);
    let dirs = tempfile::tempdir().unwrap();
    let cloud = Cloud::new(&dirs.path().join("cloud"));
    let laptop = device(&dirs.path().join("laptop"), &edge, false).await;
    let server = device(&dirs.path().join("server"), &edge, true).await;
    cloud.build_into(&laptop, &dirs.path().join("laptop"));
    cloud.build_into(&server, &dirs.path().join("server"));
    let laptop_id = laptop.core().device_id.clone();
    let server_id = server.core().device_id.clone();
    let on_laptop = memory_client(laptop.core().rpc_service());
    let on_server = memory_client(server.core().rpc_service());
    sees_online(&on_laptop, &server_id).await;
    sees_headless(&on_laptop, &server_id).await;
    // Nothing is connected yet.
    assert!(host_of(&on_laptop).await.is_null());

    // The user connects the cloud on the laptop. The server, online and
    // headless, becomes the host device, and the key goes there, once.
    let connected = on_laptop
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake", "token": SECRET }),
        )
        .await
        .expect("ConnectSandboxProvider");
    assert_eq!(connected["hostDeviceId"], server_id.as_str(), "{connected:#}");
    assert!(!connected.to_string().contains(SECRET), "the key came back");
    let host = names_host(&on_laptop, &server_id).await;
    assert_eq!(host["connectedOn"], laptop_id.as_str(), "{host:#}");
    names_host(&on_server, &server_id).await;
    assert!(
        files_holding(&dirs.path().join("laptop"), SECRET).is_empty(),
        "the laptop kept the key"
    );
    assert!(!files_holding(&dirs.path().join("server"), SECRET).is_empty());

    // The laptop creates a chat on the place: the server makes its sandbox.
    let created = on_laptop
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "chat-a", "place": { "provider": "fake" } }),
        )
        .await
        .expect("createChat on a place, from the laptop");
    let sandbox = created["deviceId"].as_str().expect("the sandbox's device").to_owned();
    assert_ne!(sandbox, laptop_id);
    assert_ne!(sandbox, server_id);
    // Its steps are the server's, asked from the laptop.
    let done = ready(&on_laptop, "chat-a").await;
    assert_eq!(done["deviceId"], sandbox.as_str());
    assert!(server.core().sandboxes.steps("chat-a").is_some());
    assert!(laptop.core().sandboxes.steps("chat-a").is_none());
    assert_eq!(cloud.provider.calls()[0], "create");
    assert!(answers(&laptop, &sandbox).await);

    // The user moves the host device to the laptop: the key moves with it,
    // and the running sandbox keeps going.
    let moved = on_laptop
        .call(
            methods::SET_SANDBOX_HOST,
            json!({ "provider": "fake", "deviceId": laptop_id }),
        )
        .await
        .expect("SetSandboxHost");
    assert_eq!(moved["hostDeviceId"], laptop_id.as_str(), "{moved:#}");
    assert!(!moved.to_string().contains(SECRET), "the key came back");
    names_host(&on_server, &laptop_id).await;
    assert!(
        files_holding(&dirs.path().join("server"), SECRET).is_empty(),
        "the server kept the key"
    );
    assert!(!files_holding(&dirs.path().join("laptop"), SECRET).is_empty());
    // The chat's steps moved too.
    assert!(laptop.core().sandboxes.steps("chat-a").is_some());
    assert!(server.core().sandboxes.steps("chat-a").is_none());

    // Two of its tokens later the sandbox is still online: its tokens came
    // from the laptop, since the server lends it none any more.
    for _ in 0..2 {
        let auth = cloud
            .provider
            .with_engine("chat-a", |core| core.auth())
            .expect("the sandbox's engine");
        token_runs_out(&auth).await;
    }
    edge.drop_room(&sandbox);
    let back = eventually(Duration::from_secs(15), async || {
        answers(&server, &sandbox).await.then_some(())
    })
    .await;
    assert!(back.is_some(), "the sandbox went dark after the host device moved");
    let dials = edge.host_dials(&sandbox);
    assert!(dials.iter().all(|d| d.admitted), "{dials:?}");

    // The chat is deleted on the server: the laptop, its host device now,
    // deletes the sandbox.
    on_server
        .call(
            methods::MUTATE,
            json!({ "op": "deleteChat", "chatId": "chat-a" }),
        )
        .await
        .expect("deleteChat, on the server");
    let gone = eventually(Duration::from_secs(30), async || {
        cloud.provider.sandboxes().is_empty().then_some(())
    })
    .await;
    assert!(gone.is_some(), "{:?}", cloud.provider.calls());
    assert!(laptop.core().sandboxes.steps("chat-a").is_none());
}

/// Who `rpc`'s device says the fake cloud is connected as (`null`: not).
async fn account_on(rpc: &RpcClient) -> Value {
    let providers = rpc
        .call(methods::LIST_SANDBOX_PROVIDERS, json!({}))
        .await
        .expect("ListSandboxProviders");
    providers
        .as_array()
        .and_then(|list| list.iter().find(|p| p["id"] == "fake"))
        .map(|p| p["account"].clone())
        .expect("the fake cloud is listed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnecting_on_any_device_clears_the_key_on_the_host_device() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let cloud = Cloud::new(&dirs.path().join("cloud"));
    let laptop = device(&dirs.path().join("laptop"), &edge, false).await;
    let server = device(&dirs.path().join("server"), &edge, true).await;
    cloud.build_into(&laptop, &dirs.path().join("laptop"));
    cloud.build_into(&server, &dirs.path().join("server"));
    let server_id = server.core().device_id.clone();
    let on_laptop = memory_client(laptop.core().rpc_service());
    let on_server = memory_client(server.core().rpc_service());
    sees_online(&on_laptop, &server_id).await;
    sees_headless(&on_laptop, &server_id).await;

    // Connected on the laptop, kept by the server.
    on_laptop
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake", "token": SECRET }),
        )
        .await
        .expect("ConnectSandboxProvider");
    names_host(&on_laptop, &server_id).await;
    assert!(!account_on(&on_server).await.is_null());

    // The user disconnects it from the laptop: the server lets the key go.
    let done = on_laptop
        .call(
            methods::DISCONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake" }),
        )
        .await
        .expect("DisconnectSandboxProvider");
    assert_eq!(done["ok"], true, "{done:#}");
    assert!(account_on(&on_server).await.is_null(), "the server's still connected");
    assert!(
        files_holding(&dirs.path().join("server"), SECRET).is_empty(),
        "the server kept the key"
    );
    assert!(files_holding(&dirs.path().join("laptop"), SECRET).is_empty());
    // No device is its host device now, on either.
    assert!(host_of(&on_server).await.is_null());
    let cleared = eventually(Duration::from_secs(30), async || {
        host_of(&on_laptop).await.is_null().then_some(())
    })
    .await;
    assert!(cleared.is_some(), "{:#}", host_of(&on_laptop).await);

    // Disconnecting again is nothing; connecting again names a host again.
    on_laptop
        .call(
            methods::DISCONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake" }),
        )
        .await
        .expect("a second Disconnect");
    on_laptop
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake", "token": SECRET }),
        )
        .await
        .expect("ConnectSandboxProvider, again");
    names_host(&on_laptop, &server_id).await;
    assert!(!account_on(&on_server).await.is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_headless_device_online_the_device_it_was_connected_on_is_host() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let cloud = Cloud::new(&dirs.path().join("cloud"));
    let laptop = device(&dirs.path().join("laptop"), &edge, false).await;
    let desktop = device(&dirs.path().join("desktop"), &edge, false).await;
    cloud.build_into(&laptop, &dirs.path().join("laptop"));
    cloud.build_into(&desktop, &dirs.path().join("desktop"));
    let on_laptop = memory_client(laptop.core().rpc_service());
    let laptop_id = laptop.core().device_id.clone();
    sees_online(&on_laptop, &desktop.core().device_id).await;

    let connected = on_laptop
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake", "token": SECRET }),
        )
        .await
        .expect("ConnectSandboxProvider");
    assert_eq!(connected["hostDeviceId"], laptop_id.as_str(), "{connected:#}");
    assert!(!files_holding(&dirs.path().join("laptop"), SECRET).is_empty());
    assert!(files_holding(&dirs.path().join("desktop"), SECRET).is_empty());
    names_host(&memory_client(desktop.core().rpc_service()), &laptop_id).await;

    // A key the provider refuses connects nothing.
    let refused = memory_client(desktop.core().rpc_service())
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake", "token": "wrong" }),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    names_host(&on_laptop, &laptop_id).await;
}

/// The ids a `ListHarnesses` answer lists.
fn ids(listed: &Value) -> Vec<String> {
    listed
        .as_array()
        .expect("a list of agents")
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn agents_on(rpc: &RpcClient, target: &str) -> Vec<String> {
    let listed = tokio::time::timeout(
        Duration::from_secs(30),
        rpc.call(
            methods::LIST_HARNESSES,
            json!({ "targetDeviceId": target, "includeRegistry": true }),
        ),
    )
    .await
    .expect("answered")
    .expect("ListHarnesses");
    ids(&listed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_composer_on_any_device_offers_what_the_template_had_on_the_host_device() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    // The template has only the mock agent.
    let cloud = FakeProvider::cloud_with_agents(
        &dirs.path().join("cloud"),
        &[zeron_engine::HarnessId::Mock],
    );
    let laptop = device(&dirs.path().join("laptop"), &edge, false).await;
    let server = device(&dirs.path().join("server"), &edge, true).await;
    laptop
        .core()
        .sandboxes
        .register(cloud.on_device(&dirs.path().join("laptop")));
    server
        .core()
        .sandboxes
        .register(cloud.on_device(&dirs.path().join("server")));
    let server_id = server.core().device_id.clone();
    let on_laptop = memory_client(laptop.core().rpc_service());
    sees_online(&on_laptop, &server_id).await;
    sees_headless(&on_laptop, &server_id).await;
    on_laptop
        .call(
            methods::CONNECT_SANDBOX_PROVIDER,
            json!({ "provider": "fake", "token": SECRET }),
        )
        .await
        .expect("ConnectSandboxProvider");
    names_host(&on_laptop, &server_id).await;

    // A chat on the template runs, made by the server (the host device).
    on_laptop
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "chat-a",
                    "place": { "provider": "fake", "template": "zeron-dev" } }),
        )
        .await
        .expect("createChat on a place");
    ready(&on_laptop, "chat-a").await;

    // The laptop never ran it, yet its composer offers what it had: the
    // server keeps that, and answers.
    let place = zeron_proto::places::place_target("fake", Some("zeron-dev"));
    let offered = eventually(Duration::from_secs(30), async || {
        let offered = agents_on(&on_laptop, &place).await;
        (offered == ["mock"]).then_some(offered)
    })
    .await;
    assert!(offered.is_some(), "{:?}", agents_on(&on_laptop, &place).await);
    assert!(
        laptop
            .core()
            .sandboxes
            .catalog("fake", Some("zeron-dev"))
            .is_none()
    );
    let mine = ids(
        &on_laptop
            .call(methods::LIST_HARNESSES, json!({ "includeRegistry": true }))
            .await
            .unwrap(),
    );
    assert!(mine.len() > 1, "{mine:?}");

    // The host device is off: the laptop offers its own agents.
    server.shutdown().await;
    assert_eq!(agents_on(&on_laptop, &place).await, mine);
}

/// Save a script from `rpc`'s device, as Settings does (no target device).
async fn write_script(rpc: &RpcClient, scope: Value, hook: &str, name: &str, content: &str) {
    rpc.call(
        methods::WRITE_STEP_SCRIPT,
        json!({ "for": scope, "hook": hook, "name": name, "content": content }),
    )
    .await
    .unwrap_or_else(|e| panic!("WriteStepScript {name}: {e:?}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn step_scripts_are_kept_on_the_host_device_of_each_provider_they_are_for() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let (laptop_dir, server_dir) = (dirs.path().join("laptop"), dirs.path().join("server"));
    let laptop = device(&laptop_dir, &edge, false).await;
    let server = device(&server_dir, &edge, true).await;
    let laptop_id = laptop.core().device_id.clone();
    let server_id = server.core().device_id.clone();
    let on_laptop = memory_client(laptop.core().rpc_service());
    sees_online(&on_laptop, &server_id).await;
    // Docker is kept on the server, Boat on the laptop.
    for (provider, host) in [("docker", &server_id), ("boat", &laptop_id)] {
        laptop
            .core()
            .workspace
            .set_sandbox_host(&zeron_proto::SandboxHost {
                provider: provider.into(),
                host_device_id: host.clone(),
                connected_on: Some(laptop_id.clone()),
            });
    }

    // Settings on the laptop saves each script; each lands where it runs.
    let docker = json!({ "kind": "provider", "provider": "docker" });
    let boat = json!({ "kind": "provider", "provider": "boat" });
    let all = json!({ "kind": "all" });
    write_script(&on_laptop, docker.clone(), "after-ready", "d.sh", "docker\n").await;
    write_script(&on_laptop, boat.clone(), "after-ready", "b.sh", "boat\n").await;
    write_script(&on_laptop, all.clone(), "after-setup", "fleet-sync.sh", "fleet sync\n").await;
    let at = |dir: &Path, path: &str| std::fs::read_to_string(dir.join(path)).ok();
    assert_eq!(at(&server_dir, "steps/docker/after-ready/d.sh").as_deref(), Some("docker\n"));
    assert_eq!(at(&laptop_dir, "steps/docker/after-ready/d.sh"), None);
    assert_eq!(at(&laptop_dir, "steps/boat/after-ready/b.sh").as_deref(), Some("boat\n"));
    assert_eq!(at(&server_dir, "steps/boat/after-ready/b.sh"), None);
    // For all sandboxes: on every host device.
    for dir in [&laptop_dir, &server_dir] {
        assert_eq!(
            at(dir, "steps/after-setup/fleet-sync.sh").as_deref(),
            Some("fleet sync\n")
        );
    }

    // Listed once each, from both host devices.
    let listed = on_laptop
        .call(methods::LIST_STEP_SCRIPTS, json!({}))
        .await
        .unwrap();
    let rows: Vec<(Value, String)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["for"].clone(), r["name"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(
        rows,
        [
            (all.clone(), "fleet-sync.sh".to_owned()),
            (boat.clone(), "b.sh".to_owned()),
            (docker.clone(), "d.sh".to_owned()),
        ],
        "{listed:#}"
    );
    // Read from where it's kept.
    let read = on_laptop
        .call(
            methods::READ_STEP_SCRIPT,
            json!({ "for": docker, "hook": "after-ready", "name": "d.sh" }),
        )
        .await
        .unwrap();
    assert_eq!(read["content"], "docker\n");

    // Deleted from every host device.
    on_laptop
        .call(
            methods::DELETE_STEP_SCRIPT,
            json!({ "for": all, "hook": "after-setup", "name": "fleet-sync.sh" }),
        )
        .await
        .unwrap();
    for dir in [&laptop_dir, &server_dir] {
        assert_eq!(at(dir, "steps/after-setup/fleet-sync.sh"), None);
    }
    let listed = on_laptop
        .call(methods::LIST_STEP_SCRIPTS, json!({}))
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed:#}");
}
