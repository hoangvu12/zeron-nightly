//! A sandbox's engine signed in on tokens lent by the host device (ADR 0003,
//! docs/design/cloud-byo.md): two real engines, the lender and the sandbox,
//! and a phone, against a scripted edge. The edge models what the real one
//! checks: WorkOS refresh tokens are single-use and carry their org scope, an
//! org-scoped refresh moves the session, and a device room admits any
//! unexpired bearer of the user (whatever device or session minted it).
//! Assertions are what the relay and RPCs show: dials and their bearers'
//! orgs, RPC answers, and the files a sandbox leaves on its disk.

#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use scripted_edge::{ScriptedEdge, USER};
use serde_json::{Value, json};
use zeron_engine::{Engine, EngineConfig, EngineRuntime, HarnessId, LentSignIn};
use zeron_rpc::{LinkCache, LinkCacheConfig, StaticToken, memory_client, methods};

/// The org the lender is signed in to.
const HOME: &str = "org_home";
/// The sandbox's org, when it isn't the lender's.
const BOX: &str = "org_box";
const OTHER: &str = "org_other";

// ---------------------------------------------------------------------------
// Engines
// ---------------------------------------------------------------------------

fn config(data_dir: &Path, edge: &ScriptedEdge, lent: Option<LentSignIn>) -> EngineConfig {
    EngineConfig {
        data_dir: data_dir.to_path_buf(),
        edge_url: edge.url.clone(),
        edge_token: None,
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        org_id: None,
        // As production: a WorkOS client id is configured everywhere.
        workos_client_id: Some("client_test".into()),
        lent_sign_in: lent,
    }
}

/// An engine as `zeron-engine headless` brings one up: auth, its refresh
/// loop, then the runtime (device room host relay included).
async fn start(config: &EngineConfig) -> EngineRuntime {
    let auth = Engine::build_auth(config).await;
    let scope = Engine::initial_workspace_scope(&auth);
    let profile = Engine::resolve_profile(config, &auth, scope)
        .expect("profile resolves")
        .expect("a signed-in profile");
    drop(auth.spawn_refresh_loop());
    Engine::assemble_runtime(config, auth, profile)
        .await
        .expect("runtime assembles")
}

/// The host device: signed in to `org` with a WorkOS session on its disk.
async fn lender(dir: &Path, edge: &ScriptedEdge, org: &str) -> EngineRuntime {
    std::fs::create_dir_all(dir).unwrap();
    let session = json!({
        "refreshToken": edge.seed_session(org),
        "user": { "id": USER, "email": "u@example.com" },
        "orgId": org,
    });
    std::fs::write(dir.join("session.json"), session.to_string()).unwrap();
    start(&config(dir, edge, None)).await
}

/// Another of the user's devices (a phone) with links through the relay.
fn phone(edge: &ScriptedEdge) -> Arc<LinkCache> {
    let mut config = LinkCacheConfig::new(
        edge.url.clone(),
        Arc::new(StaticToken(edge.user_token(HOME))),
    );
    config.cooldown_base = Duration::from_millis(100);
    config.cooldown_max = Duration::from_millis(200);
    LinkCache::new(config)
}

/// What `device` says it is, asked through the relay.
async fn engine_info(phone: &Arc<LinkCache>, device: &str) -> Result<Value, String> {
    let client = phone.client(device).await.map_err(|e| e.to_string())?;
    let reply = client.call(methods::ENGINE_INFO, json!({})).await;
    if reply.is_err() {
        phone.invalidate(device);
    }
    reply.map_err(|e| e.to_string())
}

async fn eventually<T>(within: Duration, mut probe: impl AsyncFnMut() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Some(value) = probe().await {
            return Some(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn reaches(phone: &Arc<LinkCache>, device: &str, within: Duration) -> bool {
    eventually(within, async || {
        engine_info(phone, device)
            .await
            .ok()
            .filter(|info| info["deviceId"] == device)
    })
    .await
    .is_some()
}

/// Every file under `dir`, with its bytes.
fn files(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
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
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push((path, bytes));
            }
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sandbox_on_a_lent_token_hosts_its_room_and_serves_the_users_devices() {
    let edge = ScriptedEdge::start().await;
    let dirs = tempfile::tempdir().unwrap();
    let host = lender(&dirs.path().join("host"), &edge, HOME).await;
    let lent = host
        .core()
        .lend_sign_in("sandbox-1", HOME)
        .await
        .expect("the host device lends a sign-in");
    assert_eq!(lent.lender, host.core().device_id);

    let sandbox_dir = dirs.path().join("sandbox");
    let sandbox = start(&config(&sandbox_dir, &edge, Some(lent.clone()))).await;
    let device = sandbox.core().device_id.clone();
    let phone = phone(&edge);

    assert!(
        reaches(&phone, &device, Duration::from_secs(20)).await,
        "the sandbox serves the phone through its device room"
    );
    let hosts = edge.host_dials(&device);
    assert!(
        !hosts.is_empty()
            && hosts
                .iter()
                .all(|d| d.admitted && d.org.as_deref() == Some(HOME))
    );

    // Its own window sees it signed in, in the lent org.
    let local = memory_client(sandbox.core().rpc_service());
    let mut status = local
        .subscribe(methods::AUTH_STATUS, json!({}))
        .await
        .unwrap();
    let state = status.recv().await.unwrap();
    assert_eq!(state["state"], "signedIn");
    assert_eq!(state["orgId"], HOME);

    // No browser sign-in, no step-up, and so no plugin signing key.
    for method in [
        methods::SIGN_IN,
        methods::SIGN_IN_HEADLESS,
        methods::STEP_UP,
    ] {
        let refused = local.call(method, json!({})).await;
        assert!(refused.is_err(), "{method} answered {refused:?}");
    }
    let made = local
        .call(methods::NEW_PLUGIN, json!({ "id": "mine", "name": "Mine" }))
        .await;
    assert!(
        made.is_err(),
        "a plugin needing a signing key was made: {made:?}"
    );
    assert!(
        local
            .call(methods::PLUGIN_SIGNING_KEY, json!({}))
            .await
            .is_err()
    );
    assert!(!edge.plugin_key_devices().contains(&device));

    // Nothing long-lived on its disk: no session, no key, no lending key.
    tokio::time::sleep(Duration::from_secs(1)).await;
    for (path, bytes) in files(&sandbox_dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name != "session.json" && name != "plugin-signing-key",
            "{}",
            path.display()
        );
        assert!(
            !contains(&bytes, &lent.key),
            "lending key in {}",
            path.display()
        );
        assert!(
            !contains(&bytes, "refresh-secret"),
            "refresh token in {}",
            path.display()
        );
        assert!(
            !contains(&bytes, &lent.token),
            "lent token in {}",
            path.display()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lent_tokens_keep_coming_and_stay_in_the_sandboxs_org() {
    let edge = ScriptedEdge::start().await;
    // The sandbox's tokens live seconds, so it outlives several of them.
    let ttl = 12;
    edge.set_ttl(BOX, ttl);
    let dirs = tempfile::tempdir().unwrap();
    let host = lender(&dirs.path().join("host"), &edge, HOME).await;
    let lent = host.core().lend_sign_in("sandbox-2", BOX).await.unwrap();
    let sandbox = start(&config(&dirs.path().join("sandbox"), &edge, Some(lent))).await;
    let device = sandbox.core().device_id.clone();
    let phone = phone(&edge);
    assert!(reaches(&phone, &device, Duration::from_secs(20)).await);

    let host_rpc = memory_client(host.core().rpc_service());
    for lifetime in 0..3 {
        if lifetime == 1 {
            // The user switches org on the host device: the sandbox stays put.
            host_rpc
                .call(methods::SELECT_ORG, json!({ "organizationId": OTHER }))
                .await
                .expect("the host device switches org");
        }
        tokio::time::sleep(Duration::from_secs(ttl as u64)).await;
        // Every socket closed: the sandbox must dial again with a live token.
        edge.drop_room(&device);
        phone.invalidate(&device);
        assert!(
            reaches(&phone, &device, Duration::from_secs(10)).await,
            "the sandbox is back after token lifetime {lifetime}"
        );
    }

    let dials = edge.host_dials(&device);
    assert!(dials.len() >= 4, "{dials:?}");
    assert!(
        dials.iter().all(|d| d.admitted),
        "an expired token was presented: {dials:?}"
    );
    assert!(
        dials.iter().all(|d| d.org.as_deref() == Some(BOX)),
        "{dials:?}"
    );
    // And the host device is where the user put it.
    let mut status = host_rpc
        .subscribe(methods::AUTH_STATUS, json!({}))
        .await
        .unwrap();
    assert_eq!(status.recv().await.unwrap()["orgId"], OTHER);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sandbox_goes_dark_within_a_token_lifetime_once_lending_stops() {
    let edge = ScriptedEdge::start().await;
    let ttl = 12;
    edge.set_ttl(BOX, ttl);
    let dirs = tempfile::tempdir().unwrap();
    let host = lender(&dirs.path().join("host"), &edge, HOME).await;
    let lent = host.core().lend_sign_in("sandbox-3", BOX).await.unwrap();
    let sandbox = start(&config(&dirs.path().join("sandbox"), &edge, Some(lent))).await;
    let device = sandbox.core().device_id.clone();
    let phone = phone(&edge);
    assert!(reaches(&phone, &device, Duration::from_secs(20)).await);

    host.core().stop_lending("sandbox-3");
    let dark = eventually(Duration::from_secs(ttl as u64 + 5), async || {
        (!edge.host_connected(&device)).then_some(())
    })
    .await;
    assert!(
        dark.is_some(),
        "the sandbox still hosts its room a token lifetime later"
    );
    assert!(engine_info(&phone, &device).await.is_err());

    // It stays dark, never dialing on an expired token or signing in another way.
    let dials = edge.host_dials(&device).len();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!edge.host_connected(&device));
    assert_eq!(edge.host_dials(&device).len(), dials);
    let local = memory_client(sandbox.core().rpc_service());
    assert!(local.call(methods::SIGN_IN, json!({})).await.is_err());
}
