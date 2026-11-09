//! The fake session server. Shapes follow the spike's recordings
//! (`claude-cloud-spike/fixtures`): create answers `{deduplicated, session}`,
//! events answer `{results: [{duplicate, event_id, sequence_num}]}`, the
//! stream speaks SSE with `client_event` (stored, `id:` = sequence number)
//! and `ephemeral_event`, history answers `{data, resume_cursor}`, the list
//! answers `{data, next_cursor, resume_token}`. Sequence numbers are strings
//! on the wire, as the real API writes them.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::{Step, Turn};

type Body = UnsyncBoxBody<Bytes, Infallible>;

/// One request as the server received it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    /// Lowercased header names.
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn query_param(&self, key: &str) -> Option<String> {
        self.query.as_deref()?.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (k == key).then(|| v.to_owned())
        })
    }
}

/// An injected failure: the next `times` requests whose method matches (any,
/// when `None`) and whose path contains `path_contains` get `status`.
#[derive(Debug, Clone)]
pub struct Fault {
    pub method: Option<&'static str>,
    pub path_contains: String,
    pub status: u16,
    pub times: usize,
}

/// A session that exists before the test starts (another device's, or one
/// started on claude.ai).
#[derive(Debug, Clone)]
pub struct SeedSession {
    pub id: String,
    pub environment_id: String,
    /// `active` or `archived`.
    pub status: String,
    /// RFC 3339.
    pub last_event_at: String,
}

impl SeedSession {
    /// An old archived session in the account's cloud environment: what makes
    /// the environment discoverable without the environment endpoints.
    pub fn environment(environment_id: &str) -> Self {
        Self {
            id: "cse_fake_seed_env".into(),
            environment_id: environment_id.into(),
            status: "archived".into(),
            last_event_at: "2026-11-01T00:00:00.000000Z".into(),
        }
    }
}

/// One stored event of a session.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub sequence_num: u64,
    pub event_id: String,
    /// `client` (sent to the session) or `worker` (the session's own).
    pub source: String,
    pub payload: Value,
    pub created_at: String,
}

impl StoredEvent {
    fn stream_data(&self) -> Value {
        json!({
            "event_id": self.event_id,
            "sequence_num": self.sequence_num.to_string(),
            "event_type": self.payload.get("type").cloned().unwrap_or(Value::Null),
            "source": self.source,
            "payload": self.payload,
            "created_at": self.created_at,
        })
    }
}

#[derive(Clone, Debug)]
struct Live {
    event: &'static str,
    id: Option<u64>,
    data: String,
}

struct Session {
    info: Value,
    events: Vec<StoredEvent>,
    /// payload uuid → (event_id, sequence number).
    uuids: HashMap<String, (String, u64)>,
    live: broadcast::Sender<Live>,
}

impl Session {
    fn new(info: Value) -> Self {
        Self {
            info,
            events: Vec::new(),
            uuids: HashMap::new(),
            live: broadcast::channel(4096).0,
        }
    }

    fn archived(&self) -> bool {
        self.info["status"] == "archived"
    }

    /// Store `payload` (its `uuid` is the event id) and send it live.
    fn store(&mut self, source: &str, payload: Value) -> StoredEvent {
        let seq = self.events.len() as u64 + 1;
        let event_id = payload
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let event = StoredEvent {
            sequence_num: seq,
            event_id: event_id.clone(),
            source: source.into(),
            payload,
            created_at: now(),
        };
        self.uuids.insert(event_id, (event.event_id.clone(), seq));
        self.events.push(event.clone());
        self.info["last_event_at"] = json!(event.created_at);
        self.info["updated_at"] = json!(event.created_at);
        let _ = self.live.send(Live {
            event: "client_event",
            id: Some(seq),
            data: event.stream_data().to_string(),
        });
        event
    }

    fn ephemeral(&self, payload: Value) {
        let _ = self.live.send(Live {
            event: "ephemeral_event",
            id: None,
            data: json!({
                "event_type": payload.get("type").cloned().unwrap_or(Value::Null),
                "payload": payload,
                "timestamp": now(),
                "source": "worker",
            })
            .to_string(),
        });
    }
}

#[derive(Default)]
struct Inner {
    /// In creation order.
    sessions: Vec<(String, Session)>,
    created: Vec<String>,
    requests: Vec<RecordedRequest>,
    turns: VecDeque<Turn>,
    faults: Vec<Fault>,
}

impl Inner {
    fn session(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|(sid, _)| sid == id)
            .map(|(_, s)| s)
    }
}

type State = Arc<Mutex<Inner>>;

fn lock(state: &State) -> std::sync::MutexGuard<'_, Inner> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// The fake Claude Code session API, on `127.0.0.1:<random port>`. Dropping
/// it stops accepting connections.
pub struct FakeSessionServer {
    state: State,
    base_url: String,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for FakeSessionServer {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl FakeSessionServer {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake session server binds");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state: State = Arc::default();
        let accepting = state.clone();
        let accept = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let state = accepting.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |req| handle(state.clone(), req));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self {
            state,
            base_url,
            accept,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn seed_session(&self, seed: SeedSession) {
        let info = json!({
            "id": seed.id,
            "environment_id": seed.environment_id,
            "environment_kind": "anthropic_cloud",
            "status": seed.status,
            "title": "seeded session",
            "created_at": seed.last_event_at,
            "updated_at": seed.last_event_at,
            "last_event_at": seed.last_event_at,
            "tags": [],
            "config": {"sources": [], "outcomes": []},
        });
        lock(&self.state)
            .sessions
            .push((seed.id, Session::new(info)));
    }

    /// The next message a session accepts (a create's first prompt, or a
    /// posted `user` event that isn't a repeat) plays `turn`. Turns play in
    /// the order queued.
    pub fn queue_turn(&self, turn: Turn) {
        lock(&self.state).turns.push_back(turn);
    }

    pub fn inject(&self, fault: Fault) {
        lock(&self.state).faults.push(fault);
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state).requests.clone()
    }

    /// Ids of the sessions created through `POST /v1/code/sessions`.
    pub fn created_sessions(&self) -> Vec<String> {
        lock(&self.state).created.clone()
    }

    /// The stored events of a session, in order.
    pub fn stored_events(&self, session_id: &str) -> Vec<StoredEvent> {
        lock(&self.state)
            .session(session_id)
            .map(|s| s.events.clone())
            .unwrap_or_default()
    }

    pub fn session_status(&self, session_id: &str) -> Option<String> {
        lock(&self.state)
            .session(session_id)
            .and_then(|s| s.info["status"].as_str().map(str::to_owned))
    }
}

fn json_response(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed_unsync())
        .unwrap()
}

fn error(status: StatusCode, kind: &str, message: &str) -> Response<Body> {
    json_response(
        status,
        json!({"type": "error", "error": {"type": kind, "message": message}}),
    )
}

async fn handle(state: State, req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().map(str::to_owned);
    let headers: BTreeMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let recorded = RecordedRequest {
        method: method.clone(),
        path: path.clone(),
        query,
        headers,
        body: body.clone(),
    };
    {
        let mut inner = lock(&state);
        inner.requests.push(recorded.clone());
        if let Some(fault) = inner.faults.iter_mut().find(|f| {
            f.times > 0
                && f.method.is_none_or(|m| m.eq_ignore_ascii_case(&method))
                && path.contains(&f.path_contains)
        }) {
            fault.times -= 1;
            let status =
                StatusCode::from_u16(fault.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return Ok(error(status, "injected_error", "injected by the test"));
        }
    }
    let authorized = recorded
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| !t.trim().is_empty());
    if !authorized {
        return Ok(error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing bearer token",
        ));
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    Ok(match (method.as_str(), segments.as_slice()) {
        ("GET", ["v1", "code", "sessions"]) => list(&state, &recorded),
        ("POST", ["v1", "code", "sessions"]) => create(&state, body),
        ("GET", ["v1", "code", "sessions", id]) => match lock(&state).session(id) {
            Some(session) => json_response(StatusCode::OK, session.info.clone()),
            None => not_found(),
        },
        ("POST", ["v1", "code", "sessions", id, "events"]) => post_events(&state, id, body),
        ("GET", ["v1", "code", "sessions", id, "events"]) => history(&state, id, &recorded),
        ("GET", ["v1", "code", "sessions", id, "events", "stream"]) => {
            stream(&state, id, &recorded)
        }
        ("POST", ["v1", "code", "sessions", id, "archive"]) => match lock(&state).session(id) {
            Some(session) => {
                session.info["status"] = json!("archived");
                json_response(StatusCode::OK, session.info.clone())
            }
            None => not_found(),
        },
        _ => not_found(),
    })
}

fn not_found() -> Response<Body> {
    error(StatusCode::NOT_FOUND, "not_found_error", "not found")
}

fn list(state: &State, req: &RecordedRequest) -> Response<Body> {
    let limit: usize = req
        .query_param("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .max(1);
    let offset: usize = req
        .query_param("cursor")
        .and_then(|c| c.strip_prefix("off-").and_then(|n| n.parse().ok()))
        .unwrap_or(0);
    let inner = lock(state);
    let mut all: Vec<&Value> = inner.sessions.iter().map(|(_, s)| &s.info).collect();
    // Newest activity first, as the real list orders.
    all.sort_by(|a, b| {
        b["last_event_at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["last_event_at"].as_str().unwrap_or(""))
    });
    let page: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit)
        .map(|v| (*v).clone())
        .collect();
    let mut body = json!({ "data": page, "resume_token": "fake-resume-token" });
    if offset + limit < all.len() {
        body["next_cursor"] = json!(format!("off-{}", offset + limit));
    }
    json_response(StatusCode::OK, body)
}

fn create(state: &State, body: Value) -> Response<Body> {
    let mut inner = lock(state);
    let id = format!("cse_fake{:04}", inner.created.len() + 1);
    let created_at = now();
    let environment_id = body["environment_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let info = json!({
        "id": id,
        "environment_id": environment_id,
        "environment_kind": "anthropic_cloud",
        "status": "active",
        "title": body["title"].clone(),
        "created_at": created_at,
        "updated_at": created_at,
        "last_event_at": created_at,
        "tags": ["config:no-git-repo"],
        "config": body["config"].clone(),
        "session_url": format!("https://claude.ai/code/{id}"),
    });
    let mut session = Session::new(info);
    let mut plays = false;
    for event in body["events"].as_array().cloned().unwrap_or_default() {
        let payload = event["payload"].clone();
        plays |= payload["type"] == "user";
        session.store("client", payload);
    }
    let reply = json!({ "deduplicated": false, "session": session.info.clone() });
    inner.created.push(id.clone());
    inner.sessions.push((id.clone(), session));
    if plays {
        start_turn(state, &mut inner, &id);
    }
    json_response(StatusCode::OK, reply)
}

fn post_events(state: &State, id: &str, body: Value) -> Response<Body> {
    let mut inner = lock(state);
    let Some(session) = inner.session(id) else {
        return not_found();
    };
    if session.archived() {
        return error(
            StatusCode::CONFLICT,
            "invalid_request_error",
            "session is archived",
        );
    }
    let mut results = Vec::new();
    let mut plays = false;
    for event in body["events"].as_array().cloned().unwrap_or_default() {
        let payload = event["payload"].clone();
        let Some(uuid) = payload
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "payload.uuid is required",
            );
        };
        if let Some((event_id, seq)) = session.uuids.get(&uuid).cloned() {
            results.push(
                json!({"duplicate": true, "event_id": event_id, "sequence_num": seq.to_string()}),
            );
            continue;
        }
        plays |= payload["type"] == "user";
        let stored = session.store("client", payload);
        results.push(json!({
            "duplicate": false,
            "event_id": stored.event_id,
            "sequence_num": stored.sequence_num.to_string(),
        }));
    }
    if plays {
        start_turn(state, &mut inner, id);
    }
    json_response(StatusCode::OK, json!({ "results": results }))
}

/// Play the next queued turn on `session_id`, if one is queued.
fn start_turn(state: &State, inner: &mut Inner, session_id: &str) {
    let Some(turn) = inner.turns.pop_front() else {
        return;
    };
    let state = state.clone();
    let session_id = session_id.to_owned();
    tokio::spawn(async move {
        for step in turn.steps {
            // Fresh uuids per playback: a fixture may play more than once.
            let fresh = |mut payload: Value| {
                if payload.get("uuid").is_some() {
                    payload["uuid"] = json!(uuid::Uuid::new_v4().to_string());
                }
                payload
            };
            match step {
                Step::Stored(payload) => {
                    if let Some(session) = lock(&state).session(&session_id) {
                        session.store("worker", fresh(payload));
                    }
                }
                Step::Ephemeral(payload) => {
                    if let Some(session) = lock(&state).session(&session_id) {
                        session.ephemeral(fresh(payload));
                    }
                }
                Step::Hold(gate) => gate.pass().await,
            }
            tokio::task::yield_now().await;
        }
    });
}

fn history(state: &State, id: &str, req: &RecordedRequest) -> Response<Body> {
    let after: u64 = req
        .query_param("cursor")
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let limit: usize = req
        .query_param("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(50)
        .max(1);
    let mut inner = lock(state);
    let Some(session) = inner.session(id) else {
        return not_found();
    };
    let page: Vec<&StoredEvent> = session
        .events
        .iter()
        .filter(|e| e.sequence_num > after)
        .take(limit)
        .collect();
    let mut body = json!({
        "data": page.iter().map(|e| e.stream_data()).collect::<Vec<_>>(),
    });
    if let Some(last) = page.last() {
        body["resume_cursor"] = json!(last.sequence_num.to_string());
    }
    json_response(StatusCode::OK, body)
}

fn sse(event: &str, id: Option<u64>, data: &str) -> Bytes {
    let mut frame = format!("event: {event}\n");
    if let Some(id) = id {
        frame.push_str(&format!("id: {id}\n"));
    }
    frame.push_str(&format!("data: {data}\n\n"));
    Bytes::from(frame)
}

fn stream(state: &State, id: &str, req: &RecordedRequest) -> Response<Body> {
    let from: u64 = req
        .query_param("from_sequence_num")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let (replay, live) = {
        let mut inner = lock(state);
        let Some(session) = inner.session(id) else {
            return not_found();
        };
        // Snapshot and subscribe under one lock: nothing stored in between
        // is missed or sent twice.
        let replay: Vec<Bytes> = session
            .events
            .iter()
            .filter(|e| e.sequence_num > from)
            .map(|e| {
                sse(
                    "client_event",
                    Some(e.sequence_num),
                    &e.stream_data().to_string(),
                )
            })
            .collect();
        (replay, session.live.subscribe())
    };
    let opening = vec![
        Bytes::from_static(b": keepalive\n\n"),
        sse(
            "session_update",
            None,
            r#"{"connection_status":"connected"}"#,
        ),
    ];
    let live = futures::stream::unfold(live, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(item) => return Some((sse(item.event, item.id, &item.data), rx)),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let frames = futures::stream::iter(opening.into_iter().chain(replay))
        .chain(live)
        .map(|bytes| Ok::<_, Infallible>(Frame::data(bytes)));
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(StreamBody::new(frames).boxed_unsync())
        .unwrap()
}
