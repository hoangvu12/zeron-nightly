//! The fake session server. Shapes follow the spike's recordings
//! (`claude-cloud-spike/fixtures`): create answers `{deduplicated, session}`,
//! events answer `{results: [{duplicate, event_id, sequence_num}]}`, the
//! stream speaks SSE with `client_event` (stored, `id:` = sequence number)
//! and `ephemeral_event`, history answers `{data, resume_cursor}`, the list
//! answers `{data, next_cursor, resume_token}`. Sequence numbers are strings
//! on the wire, as the real API writes them.
//!
//! The worker's controls follow `07-honest` and `10-conflict`: an interrupt is
//! stored, acknowledged by a worker `control_response` (`still_queued`), and
//! a playing turn ends with `[Request interrupted by user]` and a `result`
//! `error_during_execution` / `aborted_streaming`; a client
//! `control_response` is always stored, but only the first per `request_id`
//! counts.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast};

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
    /// A `Retry-After` (seconds) on the failure.
    pub retry_after: Option<u64>,
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

/// A [`Live`] item that ends the streams receiving it.
const DROP_STREAM: &str = "zeron-fake-drop";

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
    /// The turn playing now.
    playing: Option<tokio::task::AbortHandle>,
    /// Counts turns started: a playback stores only while it is the
    /// session's current turn (an aborted one may be mid-step).
    turns_started: u64,
    /// Messages posted while a turn played, waiting for it (their uuids).
    queued: VecDeque<String>,
    /// Worker prompts asked so far (`request_id`s), in order.
    prompts: Vec<String>,
    /// The winning (first) answer to each prompt: its `response` body.
    answers: HashMap<String, Value>,
    answered: Arc<Notify>,
    permission_mode: Option<String>,
    model: Option<Value>,
}

impl Session {
    fn new(info: Value) -> Self {
        Self {
            info,
            events: Vec::new(),
            uuids: HashMap::new(),
            live: broadcast::channel(4096).0,
            playing: None,
            turns_started: 0,
            queued: VecDeque::new(),
            prompts: Vec::new(),
            answers: HashMap::new(),
            answered: Arc::new(Notify::new()),
            permission_mode: None,
            model: None,
        }
    }

    fn archived(&self) -> bool {
        self.info["status"] == "archived"
    }

    /// Whether turn number `turn` is the one playing.
    fn plays(&self, turn: u64) -> bool {
        self.playing.is_some() && self.turns_started == turn
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
    /// Bearer tokens the server refuses with 401 (revoked or expired).
    rejected_tokens: std::collections::HashSet<String>,
    /// Creates still to make whose answer is then lost.
    lost_creates: usize,
    /// Whether a create keeps the custom tags it was sent (on by default).
    keep_tags: bool,
    /// How long a create's answer is held after the session is made.
    create_delay: Option<std::time::Duration>,
    /// Archive requests still to fail with a 500 (the session stays active).
    failed_archives: usize,
    /// How `GET /v1/code/sessions/{id}` answers.
    session_read: SessionRead,
}

/// How the fake answers a single-session read (`GET /v1/code/sessions/{id}`),
/// whose real shape isn't confirmed: the live check could read it but not
/// decode it as the list's entries are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionRead {
    /// The session as the list describes it.
    #[default]
    Plain,
    /// Wrapped: `{"session": {...}}`.
    Wrapped,
    /// Something that describes no session Zeron can read.
    Unreadable,
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
        Self::start_at("127.0.0.1:0").await
    }

    /// [`Self::start`] on a chosen address (a test that refuses connections
    /// to it first, then starts the server there).
    pub async fn start_at(addr: &str) -> Self {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("fake session server binds");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state: State = Arc::new(Mutex::new(Inner {
            keep_tags: true,
            ..Inner::default()
        }));
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
        self.seed_session_of_kind(seed, "anthropic_cloud");
    }

    /// [`Self::seed_session`] in an environment of another kind (a
    /// self-hosted `byoc` one, say).
    pub fn seed_session_of_kind(&self, seed: SeedSession, environment_kind: &str) {
        let info = json!({
            "id": seed.id,
            "environment_id": seed.environment_id,
            "environment_kind": environment_kind,
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

    /// A session someone started outside Zeron (claude.ai, the phone), with
    /// `prompt` as its first message: it plays the next queued turn, as a
    /// create does. Not counted in [`Self::created_sessions`]. Returns its id.
    pub fn start_elsewhere(&self, title: &str, prompt: &str) -> String {
        let mut inner = lock(&self.state);
        let id = format!("cse_elsewhere{:04}", inner.sessions.len() + 1);
        let created_at = now();
        let info = json!({
            "id": id,
            "environment_id": "env_fake0001",
            "environment_kind": "anthropic_cloud",
            "status": "active",
            "title": title,
            "created_at": created_at,
            "updated_at": created_at,
            "last_event_at": created_at,
            "tags": ["config:no-git-repo"],
            "config": {"sources": [], "outcomes": []},
            "session_url": format!("https://claude.ai/code/{id}"),
        });
        inner.sessions.push((id.clone(), Session::new(info)));
        let payload = json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": id,
            "type": "user",
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": prompt},
        });
        let _ = accept_events(&self.state, &mut inner, &id, vec![payload]);
        id
    }

    /// Set when a session was created and last active (RFC 3339): an old
    /// session, or an old one resumed.
    pub fn set_times(&self, session_id: &str, created_at: &str, last_event_at: &str) {
        if let Some(session) = lock(&self.state).session(session_id) {
            session.info["created_at"] = json!(created_at);
            session.info["last_event_at"] = json!(last_event_at);
            session.info["updated_at"] = json!(last_event_at);
        }
    }

    /// Archive a session, as claude.ai's archive does.
    pub fn archive(&self, session_id: &str) {
        if let Some(session) = lock(&self.state).session(session_id) {
            session.info["status"] = json!("archived");
        }
    }

    /// Resume an archived session on claude.ai: it is active again, and
    /// `prompt` is its next message (which plays the next queued turn).
    pub fn resume_elsewhere(&self, session_id: &str, prompt: &str) {
        let mut inner = lock(&self.state);
        let Some(session) = inner.session(session_id) else {
            return;
        };
        session.info["status"] = json!("active");
        let payload = json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "session_id": session_id,
            "type": "user",
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": prompt},
        });
        let _ = accept_events(&self.state, &mut inner, session_id, vec![payload]);
    }

    /// The next `times` creates make their session, but their answer is
    /// lost on the way back (the client sees a 502).
    pub fn lose_create_answers(&self, times: usize) {
        lock(&self.state).lost_creates = times;
    }

    /// From now on a create makes its session at once but holds its answer
    /// for `delay` (a slow gateway); `None` answers at once.
    pub fn delay_create_answers(&self, delay: Option<std::time::Duration>) {
        lock(&self.state).create_delay = delay;
    }

    /// How single-session reads answer from now on.
    pub fn session_read(&self, shape: SessionRead) {
        lock(&self.state).session_read = shape;
    }

    /// The single-session reads served so far (`GET /v1/code/sessions/{id}`).
    pub fn session_reads(&self) -> usize {
        lock(&self.state)
            .requests
            .iter()
            .filter(|r| r.method == "GET" && r.path.trim_matches('/').split('/').count() == 4)
            .count()
    }

    /// The next `times` archive requests fail with a 500 and archive
    /// nothing.
    pub fn fail_archives(&self, times: usize) {
        lock(&self.state).failed_archives = times;
    }

    /// Whether creates keep the custom tags they're sent (they do unless
    /// told otherwise).
    pub fn keep_tags(&self, keep: bool) {
        lock(&self.state).keep_tags = keep;
    }

    /// The archive requests served so far (`POST …/archive`), by session id.
    pub fn archive_requests(&self) -> Vec<String> {
        lock(&self.state)
            .requests
            .iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/archive"))
            .filter_map(|r| r.path.trim_matches('/').split('/').nth(3).map(str::to_owned))
            .collect()
    }

    /// Rename a session, as claude.ai's rename does.
    pub fn set_title(&self, session_id: &str, title: &str) {
        if let Some(session) = lock(&self.state).session(session_id) {
            session.info["title"] = json!(title);
        }
    }

    /// The list reads served so far (`GET /v1/code/sessions`, every page).
    pub fn list_reads(&self) -> Vec<RecordedRequest> {
        lock(&self.state)
            .requests
            .iter()
            .filter(|r| r.method == "GET" && r.path.trim_end_matches('/') == "/v1/code/sessions")
            .cloned()
            .collect()
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

    /// From now on, every request carrying `token` gets a 401
    /// `authentication_error`, as the real API answers a revoked or expired
    /// token.
    pub fn reject_token(&self, token: &str) {
        lock(&self.state).rejected_tokens.insert(token.to_owned());
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

    /// The `request_id`s of the worker's prompts (permission and
    /// AskUserQuestion `control_request`s) so far, in order.
    pub fn prompts(&self, session_id: &str) -> Vec<String> {
        lock(&self.state)
            .session(session_id)
            .map(|s| s.prompts.clone())
            .unwrap_or_default()
    }

    /// The worker's prompts no client has answered yet.
    pub fn open_prompts(&self, session_id: &str) -> Vec<String> {
        lock(&self.state)
            .session(session_id)
            .map(|s| {
                s.prompts
                    .iter()
                    .filter(|id| !s.answers.contains_key(*id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The answer that counted for prompt `request_id` (the first a client
    /// sent): the `control_response`'s inner `response` body.
    pub fn answer(&self, session_id: &str, request_id: &str) -> Option<Value> {
        lock(&self.state)
            .session(session_id)
            .and_then(|s| s.answers.get(request_id).cloned())
    }

    /// The permission mode the last `set_permission_mode` chose.
    pub fn permission_mode(&self, session_id: &str) -> Option<String> {
        lock(&self.state)
            .session(session_id)
            .and_then(|s| s.permission_mode.clone())
    }

    /// The model the last `set_model` chose (`null` = the default).
    pub fn model(&self, session_id: &str) -> Option<Value> {
        lock(&self.state)
            .session(session_id)
            .and_then(|s| s.model.clone())
    }

    /// End every stream open on the session now, as a dropped connection
    /// would (the client may reconnect).
    pub fn drop_streams(&self, session_id: &str) {
        if let Some(session) = lock(&self.state).session(session_id) {
            let _ = session.live.send(Live {
                event: DROP_STREAM,
                id: None,
                data: String::new(),
            });
        }
    }

    /// Whether a turn is playing on the session now.
    pub fn turn_playing(&self, session_id: &str) -> bool {
        lock(&self.state)
            .session(session_id)
            .is_some_and(|s| s.playing.is_some())
    }

    /// Post `payload` as another client would (another device, claude.ai):
    /// handled as `POST …/events` is, but not recorded as a request. Returns
    /// its ack.
    pub fn post_as_client(&self, session_id: &str, payload: Value) -> Value {
        let mut inner = lock(&self.state);
        accept_events(&self.state, &mut inner, session_id, vec![payload])
            .ok()
            .and_then(|mut acks| acks.pop())
            .unwrap_or(Value::Null)
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
            let mut response = error(status, "injected_error", "injected by the test");
            if let Some(seconds) = fault.retry_after {
                response
                    .headers_mut()
                    .insert(hyper::header::RETRY_AFTER, seconds.into());
            }
            return Ok(response);
        }
    }
    let token = recorded
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default();
    if token.is_empty() {
        return Ok(error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing bearer token",
        ));
    }
    if lock(&state).rejected_tokens.contains(token) {
        return Ok(error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "OAuth token has expired",
        ));
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    Ok(match (method.as_str(), segments.as_slice()) {
        ("GET", ["v1", "code", "sessions"]) => list(&state, &recorded),
        ("POST", ["v1", "code", "sessions"]) => {
            let response = create(&state, body);
            let delay = lock(&state).create_delay;
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            response
        }
        ("GET", ["v1", "code", "sessions", id]) => {
            let mut inner = lock(&state);
            let shape = inner.session_read;
            match inner.session(id) {
                Some(session) => {
                    let info = session.info.clone();
                    json_response(
                        StatusCode::OK,
                        match shape {
                            SessionRead::Plain => info,
                            SessionRead::Wrapped => json!({ "session": info }),
                            SessionRead::Unreadable => json!({ "kind": "session", "ok": true }),
                        },
                    )
                }
                None => not_found(),
            }
        }
        ("POST", ["v1", "code", "sessions", id, "events"]) => post_events(&state, id, body),
        ("GET", ["v1", "code", "sessions", id, "events"]) => history(&state, id, &recorded),
        ("GET", ["v1", "code", "sessions", id, "events", "stream"]) => {
            stream(&state, id, &recorded)
        }
        ("POST", ["v1", "code", "sessions", _, "archive"]) if take_failed_archive(&state) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            "archive failed",
        ),
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

/// Whether this archive request is one [`FakeSessionServer::fail_archives`]
/// set to fail (counting it).
fn take_failed_archive(state: &State) -> bool {
    let mut inner = lock(state);
    if inner.failed_archives == 0 {
        return false;
    }
    inner.failed_archives -= 1;
    true
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
    // What the worker is doing shows in the list's status: `running` while a
    // turn plays, `requires_action` while a prompt waits for an answer,
    // `idle` otherwise; `archived` stays.
    let mut all: Vec<Value> = inner
        .sessions
        .iter()
        .map(|(_, s)| {
            let mut info = s.info.clone();
            if !s.archived() {
                let waiting = s.prompts.iter().any(|id| !s.answers.contains_key(id));
                info["status"] = json!(if waiting {
                    "requires_action"
                } else if s.playing.is_some() {
                    "running"
                } else {
                    "idle"
                });
            }
            info
        })
        .collect();
    // Newest activity first, as the real list orders.
    all.sort_by(|a, b| {
        b["last_event_at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["last_event_at"].as_str().unwrap_or(""))
    });
    let page: Vec<Value> = all.iter().skip(offset).take(limit).cloned().collect();
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
    // The provider's own tag (a session on a repository isn't tagged
    // repo-less), then the create's (whether the real API keeps custom tags
    // is the ticket #11 live check).
    let on_repo = body["config"]["sources"]
        .as_array()
        .is_some_and(|s| !s.is_empty());
    let mut tags = if on_repo {
        Vec::new()
    } else {
        vec![json!("config:no-git-repo")]
    };
    if inner.keep_tags {
        tags.extend(
            body["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t.is_string())
                .cloned(),
        );
    }
    let info = json!({
        "id": id,
        "environment_id": environment_id,
        "environment_kind": "anthropic_cloud",
        "status": "active",
        "title": body["title"].clone(),
        "created_at": created_at,
        "updated_at": created_at,
        "last_event_at": created_at,
        "tags": tags,
        "config": body["config"].clone(),
        "session_url": format!("https://claude.ai/code/{id}"),
    });
    let events: Vec<Value> = body["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|event| event["payload"].clone())
        .collect();
    inner.created.push(id.clone());
    inner.sessions.push((id.clone(), Session::new(info)));
    let _ = accept_events(state, &mut inner, &id, events);
    let info = inner
        .session(&id)
        .map(|s| s.info.clone())
        .unwrap_or_default();
    if inner.lost_creates > 0 {
        // Created, but the answer never reaches the client (a gateway that
        // timed out on the way back).
        inner.lost_creates -= 1;
        return error(
            StatusCode::BAD_GATEWAY,
            "api_error",
            "upstream connect error or disconnect/reset before headers",
        );
    }
    json_response(
        StatusCode::OK,
        json!({ "deduplicated": false, "session": info }),
    )
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
    let payloads: Vec<Value> = body["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|event| event["payload"].clone())
        .collect();
    if payloads
        .iter()
        .any(|p| p.get("uuid").and_then(Value::as_str).is_none())
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "payload.uuid is required",
        );
    }
    match accept_events(state, &mut inner, id, payloads) {
        Ok(results) => json_response(StatusCode::OK, json!({ "results": results })),
        Err(response) => *response,
    }
}

/// Store client events on session `id` and act on them as the worker would:
/// a repeated uuid is acknowledged `duplicate: true` and does nothing; a
/// message starts a turn, or waits for the one playing; controls and answers
/// are handled as the module docs say. Returns one ack per event.
fn accept_events(
    state: &State,
    inner: &mut Inner,
    id: &str,
    payloads: Vec<Value>,
) -> Result<Vec<Value>, Box<Response<Body>>> {
    let mut results = Vec::new();
    for payload in payloads {
        let Some(session) = inner.session(id) else {
            return Err(Box::new(not_found()));
        };
        let uuid = payload
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Some((event_id, seq)) = session.uuids.get(&uuid).cloned() {
            results.push(
                json!({"duplicate": true, "event_id": event_id, "sequence_num": seq.to_string()}),
            );
            continue;
        }
        let stored = session.store("client", payload.clone());
        results.push(json!({
            "duplicate": false,
            "event_id": stored.event_id,
            "sequence_num": stored.sequence_num.to_string(),
        }));
        match payload["type"].as_str() {
            Some("user") => {
                if session.playing.is_some() {
                    session.queued.push_back(uuid);
                } else {
                    start_turn(state, inner, id);
                }
            }
            Some("control_request") => control(state, inner, id, &payload),
            Some("control_response") => {
                let request_id = payload
                    .pointer("/response/request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let answer = payload
                    .pointer("/response/response")
                    .cloned()
                    .unwrap_or(Value::Null);
                // The first answer wins; a later one is stored and dropped.
                if let std::collections::hash_map::Entry::Vacant(slot) =
                    session.answers.entry(request_id)
                {
                    slot.insert(answer);
                    session.answered.notify_waiters();
                }
            }
            _ => {}
        }
    }
    Ok(results)
}

/// The worker's reply to a client `control_request`.
fn control(state: &State, inner: &mut Inner, id: &str, payload: &Value) {
    let Some(session) = inner.session(id) else {
        return;
    };
    let request_id = payload["request_id"].clone();
    let request = &payload["request"];
    let reply = |response: Value| {
        json!({
            "type": "control_response",
            "uuid": uuid::Uuid::new_v4().to_string(),
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        })
    };
    match request["subtype"].as_str().unwrap_or_default() {
        "interrupt" => {
            if request["cancel_queued"] == true {
                session.queued.clear();
            }
            let still_queued: Vec<Value> = session.queued.iter().map(|u| json!(u)).collect();
            session.store("worker", reply(json!({ "still_queued": still_queued })));
            // Interrupting a turn ends it; interrupting while idle (or again)
            // does nothing more.
            let Some(playing) = session.playing.take() else {
                return;
            };
            playing.abort();
            let queued = session.queued.len();
            for step in Turn::from_jsonl(include_str!("../fixtures/interrupted.jsonl")).steps {
                if let Step::Stored(mut payload) = step {
                    payload["uuid"] = json!(uuid::Uuid::new_v4().to_string());
                    if payload["type"] == "result" {
                        payload["queued_turn_count"] = json!(queued);
                    }
                    session.store("worker", payload);
                }
            }
            if session.queued.pop_front().is_some() {
                start_turn(state, inner, id);
            }
        }
        "set_permission_mode" => {
            let mode = request["mode"].clone();
            session.permission_mode = mode.as_str().map(str::to_owned);
            session.store("worker", reply(json!({ "mode": mode })));
        }
        "set_model" => {
            let model = request["model"].clone();
            session.model = Some(model.clone());
            session.store("worker", reply(json!({ "model": model })));
        }
        other => {
            let error = json!({
                "type": "control_response",
                "uuid": uuid::Uuid::new_v4().to_string(),
                "response": {
                    "subtype": "error",
                    "request_id": request_id,
                    "error": format!("unsupported control request: {other}"),
                },
            });
            session.store("worker", error);
        }
    }
}

/// Play the next queued turn on `session_id`, if one is queued.
fn start_turn(state: &State, inner: &mut Inner, session_id: &str) {
    let Some(turn) = inner.turns.pop_front() else {
        return;
    };
    let Some(session) = inner.session(session_id) else {
        return;
    };
    // Playback needs this lock (the caller holds it) for its first step, so
    // the turn is marked playing before anything of it is stored.
    session.turns_started += 1;
    let task = tokio::spawn(play(
        state.clone(),
        session_id.to_owned(),
        session.turns_started,
        turn,
    ));
    session.playing = Some(task.abort_handle());
}

/// The tool-use ids a `user` payload carries results for.
fn tool_result_ids(payload: &Value) -> Vec<String> {
    payload
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "tool_result")
                .filter_map(|b| b["tool_use_id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Rewrite a prompted tool's result to say what the winning answer was: a
/// refusal's message, or AskUserQuestion's picks (in the worker's words).
fn apply_answer(block: &mut Value, answer: &Value) {
    match answer["behavior"].as_str() {
        Some("deny") => {
            block["content"] = answer["message"].clone();
            block["is_error"] = json!(true);
        }
        Some("allow") => {
            let Some(answers) = answer
                .pointer("/updatedInput/answers")
                .and_then(Value::as_object)
            else {
                return;
            };
            let picks: Vec<String> = answers
                .iter()
                .map(|(question, pick)| {
                    let pick = match pick {
                        Value::String(s) => s.clone(),
                        Value::Array(all) => all
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        other => other.to_string(),
                    };
                    format!("\"{question}\"=\"{pick}\"")
                })
                .collect();
            block["content"] = json!(format!(
                "Your questions have been answered: {}. You can now continue with these answers in mind.",
                picks.join(", ")
            ));
        }
        _ => {}
    }
}

/// The answers to `waits` (request ids), once every one has one.
async fn answers_for(
    state: &State,
    session_id: &str,
    waits: &HashSet<String>,
) -> Option<HashMap<String, Value>> {
    loop {
        let answered = {
            let mut inner = lock(state);
            let session = inner.session(session_id)?;
            if waits.iter().all(|r| session.answers.contains_key(r)) {
                return Some(
                    waits
                        .iter()
                        .map(|r| (r.clone(), session.answers[r].clone()))
                        .collect(),
                );
            }
            session.answered.clone()
        };
        let notified = answered.notified();
        let ready = lock(state)
            .session(session_id)
            .is_none_or(|s| waits.iter().all(|r| s.answers.contains_key(r)));
        if !ready {
            notified.await;
        }
    }
}

/// Play one worker turn on `session_id`, then the next waiting message's.
async fn play(state: State, session_id: String, number: u64, turn: Turn) {
    // Act on the session only while this is its current turn.
    let current = |f: &mut dyn FnMut(&mut Session)| -> bool {
        match lock(&state).session(&session_id) {
            Some(session) if session.plays(number) => {
                f(session);
                true
            }
            _ => false,
        }
    };
    // tool_use_id → the request id its prompt was asked under.
    let mut prompted: HashMap<String, String> = HashMap::new();
    for step in turn.steps {
        // Fresh ids per playback: a fixture may play more than once.
        let fresh = |mut payload: Value| {
            if payload.get("uuid").is_some() {
                payload["uuid"] = json!(uuid::Uuid::new_v4().to_string());
            }
            payload
        };
        match step {
            Step::Stored(payload) => {
                let mut payload = fresh(payload);
                if payload["type"] == "control_request" {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    payload["request_id"] = json!(request_id);
                    if let Some(tool) = payload
                        .pointer("/request/tool_use_id")
                        .and_then(Value::as_str)
                    {
                        prompted.insert(tool.to_owned(), request_id.clone());
                    }
                    let mut request_id = Some(request_id);
                    current(&mut |s| s.prompts.extend(request_id.take()));
                }
                // A prompted tool's result waits for the prompt's answer.
                let waits: HashSet<String> = tool_result_ids(&payload)
                    .iter()
                    .filter_map(|tool| prompted.get(tool).cloned())
                    .collect();
                let Some(answers) = answers_for(&state, &session_id, &waits).await else {
                    return;
                };
                if let Some(blocks) = payload
                    .pointer_mut("/message/content")
                    .and_then(Value::as_array_mut)
                {
                    for block in blocks.iter_mut().filter(|b| b["type"] == "tool_result") {
                        let answer = block["tool_use_id"]
                            .as_str()
                            .and_then(|tool| prompted.get(tool))
                            .and_then(|request| answers.get(request))
                            .cloned();
                        if let Some(answer) = answer {
                            apply_answer(block, &answer);
                        }
                    }
                }
                let mut payload = Some(payload);
                let stored = current(&mut |s| {
                    if let Some(mut payload) = payload.take() {
                        if payload["type"] == "result" {
                            payload["queued_turn_count"] = json!(s.queued.len());
                        }
                        s.store("worker", payload);
                    }
                });
                if !stored {
                    return;
                }
            }
            Step::Ephemeral(payload) => {
                let mut payload = Some(fresh(payload));
                if !current(&mut |s| s.ephemeral(payload.take().unwrap_or_default())) {
                    return;
                }
            }
            Step::Hold(gate) => gate.pass().await,
        }
        tokio::task::yield_now().await;
    }
    // The turn is over: the next waiting message's turn plays.
    let mut inner = lock(&state);
    let Some(session) = inner.session(&session_id) else {
        return;
    };
    if !session.plays(number) {
        return;
    }
    session.playing = None;
    if session.queued.pop_front().is_some() {
        start_turn(&state, &mut inner, &session_id);
    }
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
                Ok(item) if item.event == DROP_STREAM => return None,
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
