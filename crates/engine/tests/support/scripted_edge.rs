//! A scripted edge for engines on a borrowed sign-in (ADR 0003): the user's
//! devices and sandboxes against one port. It models what the real edge
//! checks: WorkOS refresh tokens are single-use and carry their org scope, an
//! org-scoped refresh moves the session, and a device room admits any
//! unexpired bearer of the user (whatever device or session minted it).
//!
//! - `POST /auth/refresh`: WorkOS refresh through the edge.
//! - `/device/{id}/ws`: DeviceRoom relay (edge/src/device-room.ts), as the
//!   client crate's mock edge has it; every dial is recorded.
//! - `/registry/{org}/ws`, with [`ScriptedEdge::with_registry`]: proxied to
//!   zeron-sync's `MockRegistryServer`, so devices' and chats' rows reach
//!   every engine.
//! - Anything else (chat rooms, nudges, releases): 404, which engines retry
//!   quietly.

// Each test binary uses its own part of this.
#![allow(dead_code)]
// tungstenite fixes the handshake callback's Err type as a full Response.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
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
use zeron_rpc::device_room::{CLIENT_CLOSED, CLIENT_GONE, HOST_CLOSED, HOST_OFFLINE, RELAY_KIND};
use zeron_rpc::{DeviceFrameHeader, decode_device_frame, encode_device_frame};
use zeron_sync::registry::mock_server::MockRegistryServer;

pub const USER: &str = "user_1";

pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// An unsigned JWT with the claims the engine and this edge read.
pub fn jwt(org: &str, ttl: i64) -> String {
    let iat = now_secs();
    let claims =
        json!({ "sub": USER, "sid": "session_1", "org_id": org, "iat": iat, "exp": iat + ttl });
    format!("e30.{}.sig", B64URL.encode(claims.to_string()))
}

pub fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&B64URL.decode(payload).ok()?).ok()
}

/// One WebSocket dial of a device room, as the edge saw it.
#[derive(Clone, Debug)]
pub struct Dial {
    pub device: String,
    pub role: String,
    pub org: Option<String>,
    pub admitted: bool,
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

pub struct ScriptedEdge {
    pub url: String,
    pub registry: Option<MockRegistryServer>,
    state: Arc<Mutex<EdgeState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ScriptedEdge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ScriptedEdge {
    /// Device rooms and sign-in; the registry is a 404.
    pub async fn start() -> Self {
        Self::serve(None).await
    }

    /// And a registry every engine shares.
    pub async fn with_registry() -> Self {
        Self::serve(Some(MockRegistryServer::start().await)).await
    }

    async fn serve(registry: Option<MockRegistryServer>) -> Self {
        let registry_url = registry.as_ref().map(MockRegistryServer::url);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(EdgeState::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_one(stream, shared.clone(), registry_url.clone()));
            }
        });
        Self {
            url,
            registry,
            state,
            task,
        }
    }

    /// A signed-in session for the lender's disk, scoped to `org`.
    pub fn seed_session(&self, org: &str) -> String {
        let token = "refresh-secret-seed".to_string();
        self.state
            .lock()
            .unwrap()
            .refresh_tokens
            .insert(token.clone(), org.into());
        token
    }

    pub fn set_ttl(&self, org: &str, secs: i64) {
        self.state.lock().unwrap().ttl.insert(org.into(), secs);
    }

    /// A bearer as another of the user's devices (the phone) holds one.
    pub fn user_token(&self, org: &str) -> String {
        jwt(org, 3600)
    }

    pub fn dials(&self, device: &str) -> Vec<Dial> {
        let state = self.state.lock().unwrap();
        state
            .dials
            .iter()
            .filter(|d| d.device == device)
            .cloned()
            .collect()
    }

    /// The host sockets `device` opened on its own room.
    pub fn host_dials(&self, device: &str) -> Vec<Dial> {
        self.dials(device)
            .into_iter()
            .filter(|d| d.role == "host")
            .collect()
    }

    pub fn host_connected(&self, device: &str) -> bool {
        let state = self.state.lock().unwrap();
        state
            .rooms
            .get(device)
            .is_some_and(|room| room.host.is_some())
    }

    /// Every socket of `device`'s room closed by the edge (a deploy, an
    /// eviction): its host must dial again, with whatever bearer it has now.
    pub fn drop_room(&self, device: &str) {
        let state = self.state.lock().unwrap();
        if let Some(room) = state.rooms.get(device) {
            for tx in room.host.iter().chain(room.clients.values()) {
                let _ = tx.send(Out::Close);
            }
        }
    }

    pub fn plugin_key_devices(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        state
            .plugin_keys
            .iter()
            .filter_map(|body| body["deviceId"].as_str().map(str::to_owned))
            .collect()
    }
}

async fn serve_one(mut stream: TcpStream, state: Arc<Mutex<EdgeState>>, registry: Option<String>) {
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
        serve_ws(stream, state, registry).await;
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

/// What a WebSocket upgrade asked for, as the handshake saw it.
enum Upgrade {
    /// A device room: its device id and the query.
    Room(String, HashMap<String, String>),
    Registry,
}

/// `/device/{id}/ws`: the DeviceRoom relay, admitting any unexpired bearer of
/// the user (it checks nothing about the device or session that minted it).
/// `/registry/{org}/ws`: the registry.
async fn serve_ws(stream: TcpStream, state: Arc<Mutex<EdgeState>>, registry: Option<String>) {
    let has_registry = registry.is_some();
    let seen: Arc<Mutex<Option<Upgrade>>> = Arc::default();
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
        let device = match parts.as_slice() {
            ["device", device, "ws"] => device.to_string(),
            ["registry", _, "ws"] if has_registry => {
                *record.lock().unwrap() = Some(Upgrade::Registry);
                return Ok(response);
            }
            _ => return Err(refuse(404)),
        };
        let bearer = query.get("token").and_then(|t| claims(t));
        let org = bearer
            .as_ref()
            .and_then(|c| c["org_id"].as_str().map(str::to_owned));
        let admitted = bearer.as_ref().is_some_and(|c| {
            c["sub"] == USER && c["exp"].as_i64().is_some_and(|exp| exp > now_secs())
        });
        gate.lock().unwrap().dials.push(Dial {
            device: device.clone(),
            role: query.get("role").cloned().unwrap_or_default(),
            org,
            admitted,
        });
        if !admitted {
            return Err(refuse(401));
        }
        *record.lock().unwrap() = Some(Upgrade::Room(device, query));
        Ok(response)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
        return;
    };
    let upgrade = seen.lock().unwrap().take();
    match upgrade {
        Some(Upgrade::Room(device, query)) => relay(ws, device, query, state).await,
        Some(Upgrade::Registry) => {
            if let Some(registry) = registry {
                proxy(ws, &registry).await;
            }
        }
        None => {}
    }
}

/// The registry socket, passed through to the mock registry.
async fn proxy(client: tokio_tungstenite::WebSocketStream<TcpStream>, upstream: &str) {
    let Ok((server, _)) = tokio_tungstenite::connect_async(upstream).await else {
        return;
    };
    let (mut client_tx, mut client_rx) = client.split();
    let (mut server_tx, mut server_rx) = server.split();
    let up = async {
        while let Some(Ok(message)) = client_rx.next().await {
            if server_tx.send(message).await.is_err() {
                return;
            }
        }
        let _ = server_tx.close().await;
    };
    let down = async {
        while let Some(Ok(message)) = server_rx.next().await {
            if client_tx.send(message).await.is_err() {
                return;
            }
        }
        let _ = client_tx.close().await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
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
