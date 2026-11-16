//! A sandbox's engine signed in on tokens lent by the host device (ADR 0003,
//! docs/design/cloud-byo.md): two real engines, the lender and the sandbox,
//! and a phone, against a scripted edge. The edge models what the real one
//! checks: WorkOS refresh tokens are single-use and carry their org scope, an
//! org-scoped refresh moves the session, and a device room admits any
//! unexpired bearer of the user (whatever device or session minted it).
//! Assertions are what the relay and RPCs show: dials and their bearers'
//! orgs, RPC answers, and the files a sandbox leaves on its disk.

// tungstenite fixes the handshake callback's Err type as a full Response.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use zeron_engine::{Engine, EngineConfig, EngineRuntime, HarnessId, LentSignIn};
use zeron_rpc::device_room::{CLIENT_CLOSED, CLIENT_GONE, HOST_CLOSED, HOST_OFFLINE, RELAY_KIND};
use zeron_rpc::{
    DeviceFrameHeader, LinkCache, LinkCacheConfig, StaticToken, decode_device_frame,
    encode_device_frame, memory_client, methods,
};

const USER: &str = "user_1";
/// The org the lender is signed in to.
const HOME: &str = "org_home";
/// The sandbox's org, when it isn't the lender's.
const BOX: &str = "org_box";
const OTHER: &str = "org_other";

// ---------------------------------------------------------------------------
// Scripted edge
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// An unsigned JWT with the claims the engine and this edge read.
fn jwt(org: &str, ttl: i64) -> String {
    let iat = now_secs();
    let claims =
        json!({ "sub": USER, "sid": "session_1", "org_id": org, "iat": iat, "exp": iat + ttl });
    format!("e30.{}.sig", B64URL.encode(claims.to_string()))
}

fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&B64URL.decode(payload).ok()?).ok()
}

/// One WebSocket dial of a device room, as the edge saw it.
#[derive(Clone, Debug)]
struct Dial {
    device: String,
    role: String,
    org: Option<String>,
    admitted: bool,
}

enum Out {
    Frame(Vec<u8>),
    Close,
}

type Tx = tokio::sync::mpsc::UnboundedSender<Out>;

#[derive(Default)]
struct Room {
    host: Option<Tx>,
    clients: HashMap<String, Tx>,
}

#[derive(Default)]
struct EdgeState {
    /// Live WorkOS refresh tokens and the org each is scoped to (single-use).
    refresh_tokens: HashMap<String, String>,
    minted: usize,
    /// Access-token lifetime per org (seconds); 3600 otherwise.
    ttl: HashMap<String, i64>,
    dials: Vec<Dial>,
    rooms: HashMap<String, Room>,
    /// `POST /plugin-keys` bodies: a device registering a signing key.
    plugin_keys: Vec<Value>,
}

impl EdgeState {
    fn ttl(&self, org: &str) -> i64 {
        self.ttl.get(org).copied().unwrap_or(3600)
    }
}

struct ScriptedEdge {
    url: String,
    state: Arc<Mutex<EdgeState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ScriptedEdge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ScriptedEdge {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(EdgeState::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self { url, state, task }
    }

    /// A signed-in session for the lender's disk, scoped to `org`.
    fn seed_session(&self, org: &str) -> String {
        let token = "refresh-secret-seed".to_string();
        self.state
            .lock()
            .unwrap()
            .refresh_tokens
            .insert(token.clone(), org.into());
        token
    }

    fn set_ttl(&self, org: &str, secs: i64) {
        self.state.lock().unwrap().ttl.insert(org.into(), secs);
    }

    /// A bearer as another of the user's devices (the phone) holds one.
    fn user_token(&self, org: &str) -> String {
        jwt(org, 3600)
    }

    fn dials(&self, device: &str) -> Vec<Dial> {
        let state = self.state.lock().unwrap();
        state
            .dials
            .iter()
            .filter(|d| d.device == device)
            .cloned()
            .collect()
    }

    /// The host sockets `device` opened on its own room.
    fn host_dials(&self, device: &str) -> Vec<Dial> {
        self.dials(device)
            .into_iter()
            .filter(|d| d.role == "host")
            .collect()
    }

    fn host_connected(&self, device: &str) -> bool {
        let state = self.state.lock().unwrap();
        state
            .rooms
            .get(device)
            .is_some_and(|room| room.host.is_some())
    }

    /// Every socket of `device`'s room closed by the edge (a deploy, an
    /// eviction): its host must dial again, with whatever bearer it has now.
    fn drop_room(&self, device: &str) {
        let state = self.state.lock().unwrap();
        if let Some(room) = state.rooms.get(device) {
            for tx in room.host.iter().chain(room.clients.values()) {
                let _ = tx.send(Out::Close);
            }
        }
    }

    fn plugin_key_devices(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        state
            .plugin_keys
            .iter()
            .filter_map(|body| body["deviceId"].as_str().map(str::to_owned))
            .collect()
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<EdgeState>>) {
    // The whole request head, left in the socket for whoever serves it.
    let mut head = vec![0u8; 16 * 1024];
    let mut n = 0;
    for _ in 0..100 {
        let Ok(read) = stream.peek(&mut head).await else {
            return;
        };
        n = read;
        if read == 0 || head[..read].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let text = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
    if text.contains("upgrade: websocket") {
        serve_ws(stream, state).await;
    } else {
        serve_http(&mut stream, state).await;
    }
}

async fn read_http(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buf.len() < end + 4 + length {
                let n = stream.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let mut line = head.lines().next()?.split_whitespace();
            let method = line.next()?.to_owned();
            let target = line.next()?.to_owned();
            return Some((method, target, buf[end + 4..end + 4 + length].to_vec()));
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn serve_http(stream: &mut TcpStream, state: Arc<Mutex<EdgeState>>) {
    let Some((method, target, body)) = read_http(stream).await else {
        return;
    };
    let path = target.split('?').next().unwrap_or_default();
    let body: Value = serde_json::from_slice(&body).unwrap_or_default();
    let (status, reply) = match (method.as_str(), path) {
        // WorkOS refresh through the edge: single-use tokens; a scoped
        // refresh moves the session to that org, a plain one keeps its org.
        ("POST", "/auth/refresh") => {
            let mut state = state.lock().unwrap();
            let presented = body["refreshToken"].as_str().unwrap_or_default();
            match state.refresh_tokens.remove(presented) {
                None => (401, json!({ "error": "invalid_grant" })),
                Some(scope) => {
                    let org = body["organizationId"].as_str().map_or(scope, str::to_owned);
                    state.minted += 1;
                    let rotated = format!("refresh-secret-{}", state.minted);
                    state.refresh_tokens.insert(rotated.clone(), org.clone());
                    let ttl = state.ttl(&org);
                    (
                        200,
                        json!({ "accessToken": jwt(&org, ttl), "refreshToken": rotated }),
                    )
                }
            }
        }
        ("POST", "/plugin-keys") => {
            state.lock().unwrap().plugin_keys.push(body);
            (403, json!({ "error": "step_up_required" }))
        }
        _ => (404, json!({ "error": "not_found" })),
    };
    let reply = reply.to_string();
    let response = format!(
        "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn refuse(status: u16) -> ErrorResponse {
    http::Response::builder()
        .status(status)
        .body(Some("refused".into()))
        .unwrap()
}

/// `/device/{id}/ws`: the DeviceRoom relay, admitting any unexpired bearer of
/// the user (it checks nothing about the device or session that minted it).
async fn serve_ws(stream: TcpStream, state: Arc<Mutex<EdgeState>>) {
    let seen: Arc<Mutex<Option<(String, HashMap<String, String>)>>> = Arc::default();
    let record = seen.clone();
    let gate = state.clone();
    let callback = move |request: &Request, response: Response| {
        let uri = request.uri().to_string();
        let (path, query) = uri.split_once('?').unwrap_or((uri.as_str(), ""));
        let query: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let ["device", device, "ws"] = parts.as_slice() else {
            return Err(refuse(404));
        };
        let bearer = query.get("token").and_then(|t| claims(t));
        let org = bearer
            .as_ref()
            .and_then(|c| c["org_id"].as_str().map(str::to_owned));
        let admitted = bearer.as_ref().is_some_and(|c| {
            c["sub"] == USER && c["exp"].as_i64().is_some_and(|exp| exp > now_secs())
        });
        gate.lock().unwrap().dials.push(Dial {
            device: device.to_string(),
            role: query.get("role").cloned().unwrap_or_default(),
            org,
            admitted,
        });
        if !admitted {
            return Err(refuse(401));
        }
        *record.lock().unwrap() = Some((device.to_string(), query));
        Ok(response)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
        return;
    };
    let Some((device, query)) = seen.lock().unwrap().take() else {
        return;
    };
    relay(ws, device, query, state).await;
}

fn relay_error(code: &str) -> Vec<u8> {
    json!({ "error": code }).to_string().into_bytes()
}

/// DeviceRoom semantics (edge/src/device-room.ts), as the client crate's
/// mock edge has them.
async fn relay(
    ws: tokio_tungstenite::WebSocketStream<TcpStream>,
    device: String,
    query: HashMap<String, String>,
    state: Arc<Mutex<EdgeState>>,
) {
    let is_host = query.get("role").map(String::as_str) == Some("host");
    let conn_id = query
        .get("connId")
        .cloned()
        .unwrap_or_else(|| "anon".into());
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
    {
        let mut state = state.lock().unwrap();
        let room = state.rooms.entry(device.clone()).or_default();
        if is_host {
            if let Some(old) = room.host.replace(tx.clone()) {
                let _ = old.send(Out::Close);
            }
        } else {
            room.clients.insert(conn_id.clone(), tx.clone());
        }
    }
    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            match out {
                Out::Frame(bytes) => {
                    if sink.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Out::Close => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });
    let encode = |header: &DeviceFrameHeader, payload: &[u8]| {
        Out::Frame(encode_device_frame(header, payload).unwrap())
    };
    loop {
        let bytes = tokio::select! {
            message = stream.next() => match message {
                Some(Ok(Message::Binary(bytes))) => bytes,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            },
            // Closed by the edge (`drop_room`, or a newer host).
            () = tx.closed() => break,
        };
        let Ok((header, payload)) = decode_device_frame(&bytes) else {
            break;
        };
        let mut state = state.lock().unwrap();
        let room = state.rooms.entry(device.clone()).or_default();
        if !is_host {
            match &room.host {
                Some(host) => {
                    let mut routed = DeviceFrameHeader::new(header.s, header.k);
                    routed.from = Some(conn_id.clone());
                    let _ = host.send(encode(&routed, &payload));
                }
                None => {
                    let bounce = DeviceFrameHeader::new(header.s, RELAY_KIND);
                    let _ = tx.send(encode(&bounce, &relay_error(HOST_OFFLINE)));
                }
            }
            continue;
        }
        let Some(to) = header.to else { continue };
        match room.clients.get(&to) {
            Some(client) => {
                let _ = client.send(encode(
                    &DeviceFrameHeader::new(header.s, header.k),
                    &payload,
                ));
            }
            None => {
                let bounce = DeviceFrameHeader::new(header.s, RELAY_KIND).with_to(to);
                let _ = tx.send(encode(&bounce, &relay_error(CLIENT_GONE)));
            }
        }
    }
    {
        let mut state = state.lock().unwrap();
        let room = state.rooms.entry(device.clone()).or_default();
        if is_host {
            if room.host.as_ref().is_some_and(|h| h.same_channel(&tx)) {
                room.host = None;
                for client in room.clients.values() {
                    let _ = client.send(encode(
                        &DeviceFrameHeader::new("", RELAY_KIND),
                        &relay_error(HOST_CLOSED),
                    ));
                }
            }
        } else {
            room.clients.remove(&conn_id);
            if let Some(host) = &room.host {
                let mut header = DeviceFrameHeader::new("", RELAY_KIND);
                header.from = Some(conn_id.clone());
                let _ = host.send(encode(&header, &relay_error(CLIENT_CLOSED)));
            }
        }
    }
    writer.abort();
}

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
