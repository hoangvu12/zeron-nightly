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

    // Two token lifetimes later the sandbox is still online: its tokens came
    // from the laptop, since the server lends it none any more.
    tokio::time::sleep(Duration::from_secs(2 * ttl as u64)).await;
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
