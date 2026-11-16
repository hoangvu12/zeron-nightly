//! A local stand-in for Boat's REST API (boat.dev, `/api/v1`): the shapes
//! the edge's fake speaks (`edge/test/workerd/boat-fake.ts`), served over
//! real HTTP on 127.0.0.1 so the engine's Boat adapter is tested end to end
//! through its HTTP client.
//!
//! Lifecycle as the edge's fake: create → `provisioning`, which turns `idle`
//! on the next GET; stop → `archiving` → `archived` on the next GET; resume →
//! `provisioning`. Commands run nothing: they're recorded and answer exit 0
//! with stdout `ok` (or what [`BoatStub::answer_commands`] says).
//!
//! Test controls are methods: [`BoatStub::fail`] queues an error reply for an
//! operation, [`BoatStub::calls`] is every API call, [`BoatStub::sandbox`] a
//! sandbox's state.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The one key the stub accepts.
pub const KEY: &str = "test-boat-key";

#[derive(Clone, Debug)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HashMap<String, String>,
    pub body: Value,
}

#[derive(Clone, Debug)]
pub struct Sandbox {
    pub id: String,
    pub state: String,
    pub env: Value,
    pub ttl_seconds: Value,
    pub from: Option<String>,
    pub deleted: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct Failure {
    op: String,
    status: u16,
    code: String,
    message: String,
    retryable: Option<bool>,
    action: Option<String>,
}

#[derive(Default)]
struct State {
    sandboxes: HashMap<String, Sandbox>,
    by_key: HashMap<String, String>,
    calls: Vec<Call>,
    failures: Vec<Failure>,
    command_reply: Option<Value>,
    next: u32,
}

#[derive(Clone)]
pub struct BoatStub {
    /// `http://127.0.0.1:<port>/api/v1`: the adapter's base.
    pub base: String,
    state: Arc<Mutex<State>>,
}

impl BoatStub {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let stub = Self {
            base,
            state: Arc::default(),
        };
        let serving = stub.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let stub = serving.clone();
                tokio::spawn(async move { stub.serve(stream).await });
            }
        });
        stub
    }

    /// The next request for `op` (`create`, `get`, `patch`, `delete`, `stop`,
    /// `resume`, `commands`, `list`) answers this error.
    pub fn fail(&self, op: &str, status: u16, code: &str) {
        self.fail_with(op, status, code, code, None, None);
    }

    pub fn fail_with(
        &self,
        op: &str,
        status: u16,
        code: &str,
        message: &str,
        retryable: Option<bool>,
        action: Option<&str>,
    ) {
        self.state.lock().unwrap().failures.push(Failure {
            op: op.into(),
            status,
            code: code.into(),
            message: message.into(),
            retryable,
            action: action.map(Into::into),
        });
    }

    /// What every later command answers.
    pub fn answer_commands(&self, reply: Value) {
        self.state.lock().unwrap().command_reply = Some(reply);
    }

    pub fn set_state(&self, id: &str, state: &str, error: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        let sandbox = s.sandboxes.get_mut(id).unwrap();
        sandbox.state = state.into();
        sandbox.error = error.map(Into::into);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn sandbox(&self, id: &str) -> Option<Sandbox> {
        self.state.lock().unwrap().sandboxes.get(id).cloned()
    }

    async fn serve(&self, mut stream: TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let header_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let mut lines = head.lines();
            let start = lines.next().unwrap_or_default().to_owned();
            let headers: HashMap<String, String> = lines
                .filter_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                })
                .collect();
            let length = headers
                .get("content-length")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + length {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body: Value = serde_json::from_slice(&buf[header_end..header_end + length])
                .unwrap_or(Value::Null);
            buf.drain(..header_end + length);
            let mut parts = start.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default().to_owned();
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p.to_owned(), q.to_owned()),
                None => (target, String::new()),
            };
            let (status, reply) = self.route(Call {
                method,
                path,
                query,
                headers,
                body,
            });
            let text = reply.to_string();
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{text}",
                text.len()
            );
            if stream.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn route(&self, call: Call) -> (u16, Value) {
        let mut s = self.state.lock().unwrap();
        if call.headers.get("authorization").map(String::as_str) != Some(&format!("Bearer {KEY}")) {
            return error(401, "unauthorized", "unauthorized", None, None);
        }
        let Some(rest) = call.path.strip_prefix("/api/v1/sandboxes") else {
            return error(404, "not_found", "not_found", None, None);
        };
        let mut segments = rest
            .trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty());
        let id = segments.next().map(str::to_owned);
        let action = segments.next().map(str::to_owned);
        s.calls.push(call.clone());
        let method = call.method.as_str();

        let op = match (&id, &action, method) {
            (None, _, "POST") => "create".to_owned(),
            (None, _, _) => "list".to_owned(),
            (Some(_), Some(action), _) => action.clone(),
            (Some(_), None, m) => m.to_ascii_lowercase(),
        };
        if let Some(i) = s.failures.iter().position(|f| f.op == op) {
            let f = s.failures.remove(i);
            return error(
                f.status,
                &f.code,
                &f.message,
                f.retryable,
                f.action.as_deref(),
            );
        }

        let Some(id) = id else {
            if method == "GET" {
                let all: Vec<Value> = s
                    .sandboxes
                    .values()
                    .filter(|x| !x.deleted)
                    .map(view)
                    .collect();
                return (
                    200,
                    json!({"ok": true, "type": "sandbox.list", "sandboxes": all,
                           "pageInfo": {"nextCursor": null, "hasMore": false, "limit": 200}}),
                );
            }
            let key = call.headers.get("idempotency-key").cloned();
            if let Some(existing) = key.as_ref().and_then(|k| s.by_key.get(k)).cloned() {
                let existing = s.sandboxes[&existing].clone();
                if existing.env != call.body["env"] {
                    return error(
                        409,
                        "idempotency_key_reused",
                        "idempotency_key_reused",
                        None,
                        None,
                    );
                }
                return (
                    202,
                    json!({"ok": true, "type": "sandbox.created", "status": existing.state, "sandbox": view(&existing)}),
                );
            }
            s.next += 1;
            let sandbox = Sandbox {
                id: format!("bx_{:08}", s.next),
                state: "provisioning".into(),
                env: call.body["env"].clone(),
                ttl_seconds: call.body["ttlSeconds"].clone(),
                from: call.body["from"].as_str().map(Into::into),
                deleted: false,
                error: None,
            };
            if let Some(key) = key {
                s.by_key.insert(key, sandbox.id.clone());
            }
            let reply = json!({"ok": true, "type": "sandbox.created", "status": "provisioning", "sandbox": view(&sandbox)});
            s.sandboxes.insert(sandbox.id.clone(), sandbox);
            return (202, reply);
        };

        let command_reply = s.command_reply.clone();
        let Some(sandbox) = s.sandboxes.get_mut(&id).filter(|x| !x.deleted) else {
            return error(404, "not_found", "not_found", None, None);
        };
        match (method, action.as_deref()) {
            ("GET", None) => {
                match sandbox.state.as_str() {
                    "provisioning" => sandbox.state = "idle".into(),
                    "archiving" => sandbox.state = "archived".into(),
                    _ => {}
                }
                (
                    200,
                    json!({"ok": true, "type": "sandbox.info", "sandbox": view(sandbox)}),
                )
            }
            ("PATCH", None) => {
                sandbox.ttl_seconds = call.body["ttlSeconds"].clone();
                (
                    200,
                    json!({"ok": true, "type": "sandbox.info", "sandbox": view(sandbox)}),
                )
            }
            ("DELETE", None) => {
                if call.headers.get("x-ascii-confirm-delete") != Some(&sandbox.id) {
                    return error(409, "confirm_required", "confirm_required", None, None);
                }
                sandbox.deleted = true;
                (
                    202,
                    json!({"ok": true, "type": "sandbox.deleting",
                           "operation": {"id": format!("bdop_{}", sandbox.id), "status": "pending"}}),
                )
            }
            ("POST", Some("stop")) => {
                if sandbox.state != "archived" {
                    sandbox.state = "archiving".into();
                }
                (
                    202,
                    json!({"ok": true, "type": "sandbox.stopping", "status": "archiving", "sandbox": view(sandbox)}),
                )
            }
            ("POST", Some("resume")) => {
                if ["ready", "idle", "running", "provisioning"].contains(&sandbox.state.as_str()) {
                    return error(409, "sandbox_resuming", "already resuming", None, None);
                }
                sandbox.state = "provisioning".into();
                sandbox.error = None;
                if !call.body["ttlSeconds"].is_null() {
                    sandbox.ttl_seconds = call.body["ttlSeconds"].clone();
                }
                (
                    202,
                    json!({"ok": true, "type": "sandbox.resuming", "status": "resuming", "sandbox": view(sandbox)}),
                )
            }
            ("POST", Some("commands")) => {
                if sandbox.state == "error" {
                    return error(
                        409,
                        "sandbox_not_running",
                        "No machine to run that on.",
                        None,
                        None,
                    );
                }
                if sandbox.state == "provisioning" {
                    sandbox.state = "idle".into();
                }
                let reply = command_reply.unwrap_or_else(|| {
                    json!({"ok": true, "type": "command.finished", "success": true, "exitCode": 0,
                           "stdout": "ok", "stderr": "", "timedOut": false})
                });
                (200, reply)
            }
            _ => error(404, "not_found", "not_found", None, None),
        }
    }
}

fn view(s: &Sandbox) -> Value {
    json!({
        "id": s.id, "name": format!("Sandbox {}", s.id), "state": s.state, "type": "default",
        "createdAt": "2026-11-16T00:00:00Z", "dataExpiredAt": null, "stoppedAt": null,
        "error": s.error, "healthReason": null, "hydrated": true,
    })
}

fn error(
    status: u16,
    code: &str,
    message: &str,
    retryable: Option<bool>,
    action: Option<&str>,
) -> (u16, Value) {
    let mut inner = json!({"code": code, "message": message, "status": status});
    if let Some(action) = action {
        inner["details"] = json!({"action": action});
    }
    if let Some(retryable) = retryable {
        inner["retryable"] = json!(retryable);
    }
    (
        status,
        json!({"ok": false, "type": "sandbox.error", "status": status, "code": code,
               "message": message, "error": inner, "requestId": "req_test"}),
    )
}
