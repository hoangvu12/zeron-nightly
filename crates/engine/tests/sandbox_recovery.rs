//! Sandboxes going away and coming back (spec #37 "When things go wrong",
//! docs/design/cloud-byo.md "Lifecycle"), at the engine's RPCs with a fake
//! provider whose sandboxes run a second real engine, against a scripted
//! edge:
//!
//! - deleting a chat deletes its sandbox and stops lending it tokens;
//! - a subagent shares its parent's sandbox unless it asks for its own;
//! - a sandbox deleted outside Zeron reads as gone, and Start fresh makes a
//!   new one with the work pushed to the chat's branch;
//! - a full disk is said as such, and Start fresh gets past it;
//! - a chat deleted while its host device was off takes its sandbox with it
//!   when the host device is back.
//!
//! The watchdog that restarts a crashed engine in a sandbox is tested in
//! `zeron-cli` (`watchdog.rs`); here, Zeron starts its engine under it.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/places.rs"]
mod places;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::Path;
use std::time::Duration;

use fake_provider::FakeProvider;
use places::{
    SPACE, add_project, commit, create_on_place, eventually, git, host_device, origin, project,
    ready, steps, steps_until,
};
use scripted_edge::ScriptedEdge;
use serde_json::{Value, json};
use zeron_rpc::{RpcClient, memory_client, methods};

/// Whether the host device still lends a sign-in under `key`.
async fn lends(rpc: &RpcClient, key: &str) -> bool {
    rpc.call(methods::LEND_TOKEN, json!({ "key": key }))
        .await
        .is_ok()
}

async fn create_child(rpc: &RpcClient, chat: &str, parent: &str, extra: Value) -> Value {
    let mut params = json!({
        "op": "createChat",
        "chatId": chat,
        "spaceId": SPACE,
        "parentChatId": parent,
        "childKind": "subagent",
    });
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    rpc.call(methods::MUTATE, params)
        .await
        .unwrap_or_else(|e| panic!("createChat {chat}: {e}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_deleted_while_its_host_device_was_off_loses_its_sandbox_when_it_is_back() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host_dir = dirs.path().join("host");
    let host = host_device(&host_dir, &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    add_project(&rpc, &host, &project(dirs.path(), &origin_url)).await;
    create_on_place(&rpc, "chat-a", "fake").await;
    ready(&rpc, "chat-a").await;
    assert_eq!(provider.sandboxes(), ["chat-a"]);
    host.shutdown().await;
    drop(rpc);
    drop(host);

    // While it's off, the chat is deleted on another device.
    let phone = host_device(&dirs.path().join("phone"), &edge).await;
    eventually(
        "the other device has the chat",
        Duration::from_secs(30),
        async || phone.core().workspace.chat("chat-a").ok().flatten(),
    )
    .await;
    memory_client(phone.core().rpc_service())
        .call(
            methods::MUTATE,
            json!({ "op": "deleteChat", "chatId": "chat-a" }),
        )
        .await
        .expect("deleteChat, on the other device");
    assert_eq!(provider.sandboxes(), ["chat-a"]);

    // The host device is back; it hears of the deletion before its
    // providers are here (they connect after it starts). Once they are,
    // the sandbox goes.
    let host = host_device(&host_dir, &edge).await;
    eventually(
        "the host device hears the chat was deleted",
        Duration::from_secs(30),
        async || host.core().workspace.chat_deleted("chat-a").then_some(()),
    )
    .await;
    host.core()
        .sandboxes
        .set_sleep_timings(zeron_engine::sandboxes::SleepTimings {
            idle: Duration::from_secs(60 * 60),
            time_limit: Duration::from_secs(60 * 60),
            check_every: Duration::from_millis(250),
        });
    host.core().sandboxes.register(provider.clone());
    eventually(
        "the sandbox is deleted",
        Duration::from_secs(30),
        async || provider.sandboxes().is_empty().then_some(()),
    )
    .await;
    assert!(host.core().sandboxes.steps("chat-a").is_none());
    assert!(
        provider.calls().contains(&"delete chat-a".to_owned()),
        "{:?}",
        provider.calls()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_chat_deletes_its_sandbox_and_stops_lending_it_tokens() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    add_project(&rpc, &host, &project(dirs.path(), &origin_url)).await;

    let created = create_on_place(&rpc, "chat-a", "fake").await;
    let device = created["deviceId"].as_str().unwrap().to_owned();
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    ready(&rpc, "chat-a").await;
    // Zeron started its engine under the watchdog.
    assert_eq!(
        provider.calls().last().unwrap(),
        "run chat-a: zeron-engine watchdog"
    );
    let key = eventually(
        "the sandbox's engine started",
        Duration::from_secs(30),
        async || provider.lent_key("chat-a"),
    )
    .await;
    assert!(lends(&rpc, &key).await);

    // A subagent its agent starts (through the sandbox's own engine, as the
    // Zeron MCP does) shares the sandbox: its device and checkout.
    let inside = eventually(
        "the sandbox's engine is up",
        Duration::from_secs(30),
        async || provider.with_engine("chat-a", |core| memory_client(core.rpc_service())),
    )
    .await;
    let sandbox_engine_sees_parent = async || {
        provider
            .with_engine("chat-a", |core| {
                core.workspace.chat("chat-a").ok().flatten()
            })
            .flatten()
            .map(|_| ())
    };
    eventually(
        "the sandbox's engine has the chat",
        Duration::from_secs(30),
        sandbox_engine_sees_parent,
    )
    .await;
    let sub = create_child(
        &inside,
        "chat-a-sub",
        "chat-a",
        json!({ "deviceId": device }),
    )
    .await;
    assert_eq!(sub["deviceId"], device.as_str(), "{sub:#}");
    assert_eq!(sub["cwd"], cwd.as_str(), "{sub:#}");
    // So does a side chat started on the host device.
    let side = create_child(
        &rpc,
        "chat-a-side",
        "chat-a",
        json!({ "childKind": "side" }),
    )
    .await;
    assert_eq!(side["deviceId"], device.as_str(), "{side:#}");
    assert_eq!(side["cwd"], cwd.as_str(), "{side:#}");
    // A subagent that asks for a sandbox of its own gets one.
    let own = create_child(
        &rpc,
        "chat-a-own",
        "chat-a",
        json!({ "place": { "provider": "fake" } }),
    )
    .await;
    assert_ne!(own["deviceId"], device.as_str(), "{own:#}");
    ready(&rpc, "chat-a-own").await;
    assert_eq!(provider.sandboxes(), ["chat-a", "chat-a-own"]);

    // The user deletes the chat: its sandbox goes, and its children's own.
    rpc.call(
        methods::MUTATE,
        json!({ "op": "deleteChat", "chatId": "chat-a" }),
    )
    .await
    .expect("deleteChat");
    eventually(
        "the sandboxes are deleted",
        Duration::from_secs(30),
        async || provider.sandboxes().is_empty().then_some(()),
    )
    .await;
    let deletes: Vec<String> = provider
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("delete"))
        .collect();
    assert_eq!(deletes.len(), 2, "{deletes:#?}");
    assert!(
        deletes.contains(&"delete chat-a".to_owned()),
        "{deletes:#?}"
    );
    assert!(
        deletes.contains(&"delete chat-a-own".to_owned()),
        "{deletes:#?}"
    );
    // Tokens stop: its next ask is refused.
    assert!(!lends(&rpc, &key).await);
    assert!(steps(&rpc, "chat-a").await.is_null());
    // Nothing is left behind: no device list shows the sandboxes' devices
    // (seen in a real Docker run, 2026-11-17: the row outlived its sandbox).
    let own_device = own["deviceId"].as_str().unwrap().to_owned();
    eventually(
        "the sandboxes' devices leave the device list",
        Duration::from_secs(30),
        async || {
            let rows = host.core().workspace.read_devices().ok()?;
            rows.iter()
                .all(|d| d.id != device && d.id != own_device)
                .then_some(())
        },
    )
    .await;
    // Deleting it on another device deletes it here too (the rows sync):
    // shown by the child's sandbox, whose row went with its parent's.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sandbox_deleted_outside_zeron_starts_fresh_with_its_pushed_work() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    add_project(&rpc, &host, &project(dirs.path(), &origin_url)).await;

    let created = create_on_place(&rpc, "chat-b", "fake").await;
    let device = created["deviceId"].as_str().unwrap().to_owned();
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    ready(&rpc, "chat-b").await;
    let key = eventually(
        "the sandbox's engine started",
        Duration::from_secs(30),
        async || provider.lent_key("chat-b"),
    )
    .await;

    // The agent pushes some work, and leaves some unpushed.
    let checkout = Path::new(&cwd);
    std::fs::write(checkout.join("pushed.txt"), "kept\n").unwrap();
    commit(checkout, "pushed work");
    git(checkout, &["push", "-q", "origin", "zeron/chat-b"]);
    std::fs::write(checkout.join("unpushed.txt"), "lost\n").unwrap();
    commit(checkout, "unpushed work");

    // While it's there, checking finds it so.
    async fn check(rpc: &RpcClient) -> Value {
        rpc.call(methods::CHECK_SANDBOX, json!({ "chatId": "chat-b" }))
            .await
            .expect("CheckSandbox")
    }
    assert_eq!(check(&rpc).await["state"], "ready");

    // The user deletes it in the provider's console. Opening the chat
    // finds it gone: "Sandbox deleted", with Start fresh.
    provider.delete_outside("chat-b").await;
    let gone = check(&rpc).await;
    assert_eq!(gone["state"], "gone", "{gone:#}");
    assert!(
        gone["error"]
            .as_str()
            .unwrap_or_default()
            .contains("deleted"),
        "{gone:#}"
    );
    assert_eq!(steps(&rpc, "chat-b").await["state"], "gone");
    // Nothing is lent to a sandbox that's gone.
    assert!(!lends(&rpc, &key).await);

    rpc.call(methods::START_SANDBOX_FRESH, json!({ "chatId": "chat-b" }))
        .await
        .expect("Start fresh");
    let fresh = ready(&rpc, "chat-b").await;
    assert!(fresh["error"].is_null(), "{fresh:#}");
    assert_eq!(fresh["deviceId"], device.as_str());
    assert_eq!(provider.sandboxes(), ["chat-b"]);
    assert_eq!(
        provider.calls().iter().filter(|c| *c == "create").count(),
        2,
        "{:#?}",
        provider.calls()
    );
    // Gone already: nothing to delete.
    assert!(
        !provider.calls().iter().any(|c| c.starts_with("delete")),
        "{:#?}",
        provider.calls()
    );

    // The chat keeps its device and checkout; what was pushed came back on
    // its branch, what wasn't is lost.
    let row = host.core().workspace.chat("chat-b").unwrap().unwrap();
    assert_eq!(row.device_id, device);
    assert_eq!(row.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(git(checkout, &["branch", "--show-current"]), "zeron/chat-b");
    assert!(checkout.join("pushed.txt").exists());
    assert!(!checkout.join("unpushed.txt").exists());
    // Its new engine is lent a sign-in afresh and comes online as the chat's
    // host.
    let fresh_key = eventually(
        "the new engine started",
        Duration::from_secs(30),
        async || provider.lent_key("chat-b"),
    )
    .await;
    assert_ne!(fresh_key, key);
    assert!(lends(&rpc, &fresh_key).await);
    eventually(
        "the new engine hosts the chat's device",
        Duration::from_secs(30),
        async || edge.host_connected(&device).then_some(()),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_disk_is_said_in_the_chat_and_start_fresh_gets_past_it() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    add_project(&rpc, &host, &project(dirs.path(), &origin_url)).await;

    provider.set_disk_full(true);
    create_on_place(&rpc, "chat-d", "fake").await;
    let failed = steps_until(&rpc, "chat-d", Duration::from_secs(30), |s| {
        s["state"] == "failed"
    })
    .await;
    assert_eq!(failed["step"], "getCode", "{failed:#}");
    assert_eq!(failed["diskFull"], true, "{failed:#}");
    let error = failed["error"].as_str().unwrap_or_default();
    assert!(
        error.starts_with("The sandbox's disk is full."),
        "{failed:#}"
    );
    assert!(error.contains("No space left on device"), "{failed:#}");

    // Start fresh: the full sandbox is deleted, and a new one made.
    provider.set_disk_full(false);
    rpc.call(methods::START_SANDBOX_FRESH, json!({ "chatId": "chat-d" }))
        .await
        .expect("Start fresh");
    let fresh = ready(&rpc, "chat-d").await;
    assert_eq!(fresh["diskFull"], false, "{fresh:#}");
    let calls = provider.calls();
    assert_eq!(
        calls.iter().filter(|c| *c == "create").count(),
        2,
        "{calls:#?}"
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "delete chat-d").count(),
        1,
        "{calls:#?}"
    );
}
