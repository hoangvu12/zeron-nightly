//! A local stand-in for Docker's sandbox APIs, answering with the request
//! and response shapes recorded from the real service on 2026-11-16 (see
//! `docker_sandbox.rs`): the control API (Connect JSON under
//! `/sbx/docker.sbx.v1.*`), the v1 REST network policy, the sandbox's
//! process service (Connect server-streaming, `application/connect+json`),
//! Docker's OAuth device flow (`/oauth/*`) and Docker Hub's token exchange
//! (`/v2/auth/token`). Hand-rolled HTTP/1.1, one request per connection.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One request the stub saw.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub bearer: Option<String>,
    pub content_type: Option<String>,
    /// JSON bodies parsed; a form body as `{key: value}`; a Connect stream's
    /// first message unwrapped.
    pub body: Value,
}

/// A sandbox as the stub keeps it.
#[derive(Clone, Debug)]
pub struct StubBox {
    pub name: String,
    pub status: &'static str,
    /// Where a Start/Stop is heading, and after how many more `GetSandbox`
    /// calls it gets there.
    pub heading: Option<(&'static str, u32)>,
    pub network: &'static str,
}

pub struct State {
    pub seen: Vec<Seen>,
    pub boxes: BTreeMap<String, StubBox>,
    next: u32,
    /// Access tokens it accepts.
    pub access: HashSet<String>,
    /// The one refresh token it takes now (rotating: each use replaces it).
    pub refresh: Option<String>,
    pub refreshes: u32,
    /// `expires_in` of the access tokens it issues.
    pub token_life: u64,
    /// `authorization_pending` answers before the device code is approved.
    pub pending_polls: u32,
    /// Hub: `(username, personal access token)` it accepts.
    pub pat: Option<(String, String)>,
    /// `GetSandbox` calls a Start/Stop takes to finish.
    pub transition_polls: u32,
    /// Frames for the next process starts (each a list of Connect messages:
    /// `(flags, json)`); the default runs and exits 0.
    pub runs: VecDeque<Vec<(u8, Value)>>,
    /// Close this many next connections without answering (a reset).
    pub resets: u32,
    pub username: String,
}

#[derive(Clone)]
pub struct DockerStub {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

pub fn jwt(claims: Value) -> String {
    let b64 = |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    format!("{}.{}.sig", b64(&json!({"alg": "RS256"})), b64(&claims))
}

impl DockerStub {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stub = Self {
            url,
            state: Arc::new(Mutex::new(State {
                seen: Vec::new(),
                boxes: BTreeMap::new(),
                next: 0,
                access: HashSet::new(),
                refresh: None,
                refreshes: 0,
                token_life: 900,
                pending_polls: 1,
                pat: None,
                transition_polls: 2,
                runs: VecDeque::new(),
                resets: 0,
                username: "vu-test".into(),
            })),
        };
        let serving = stub.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let stub = serving.clone();
                tokio::spawn(async move { stub.serve(socket).await });
            }
        });
        stub
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Accept `token` as a signed-in access token (and say who it is).
    pub fn sign_in(&self, token: &str) {
        self.state().access.insert(token.into());
    }

    pub fn seen(&self, path_part: &str) -> Vec<Seen> {
        self.state()
            .seen
            .iter()
            .filter(|s| s.path.contains(path_part))
            .cloned()
            .collect()
    }

    pub fn add_box(&self, id: &str, status: &'static str) {
        self.state().boxes.insert(
            id.into(),
            StubBox {
                name: id.into(),
                status,
                heading: None,
                network: "deny-all",
            },
        );
    }

    pub fn script_run(&self, frames: Vec<(u8, Value)>) {
        self.state().runs.push_back(frames);
    }

    async fn serve(&self, mut socket: TcpStream) {
        if {
            let mut state = self.state();
            let reset = state.resets > 0;
            state.resets = state.resets.saturating_sub(1);
            reset
        } {
            return; // dropped: the client sees the connection close
        }
        let Some((method, path, headers, body)) = read_request(&mut socket).await else {
            return;
        };
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let content_type = header("content-type");
        let bearer = header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_owned));
        let parsed = match content_type.as_deref() {
            Some("application/x-www-form-urlencoded") => Value::Object(
                url_form(&body)
                    .into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect(),
            ),
            Some("application/connect+json") if body.len() >= 5 => {
                serde_json::from_slice(&body[5..]).unwrap_or(Value::Null)
            }
            _ => serde_json::from_slice(&body).unwrap_or(Value::Null),
        };
        self.state().seen.push(Seen {
            method: method.clone(),
            path: path.clone(),
            bearer: bearer.clone(),
            content_type: content_type.clone(),
            body: parsed.clone(),
        });

        // Sign-in endpoints need no bearer.
        if path == "/oauth/device/code" {
            return respond(&mut socket, 200, json!({
                "device_code": "dev-code-1", "user_code": "ABCD-EFGH",
                "verification_uri": "https://login.docker.com/activate",
                "verification_uri_complete": "https://login.docker.com/activate?user_code=ABCD-EFGH",
                "expires_in": 900, "interval": 1,
            })).await;
        }
        if path == "/oauth/token" {
            let (status, reply) = self.token(&parsed);
            return respond(&mut socket, status, reply).await;
        }
        if path == "/v2/auth/token" {
            let ok = {
                let state = self.state();
                state.pat.as_ref().is_some_and(|(user, pat)| {
                    parsed["identifier"] == *user.as_str() && parsed["secret"] == *pat.as_str()
                })
            };
            if parsed.get("identifier").is_none() {
                return respond(&mut socket, 400, json!({"message": "body is invalid", "errinfo": {"identifier": "value is required"}})).await;
            }
            if !ok {
                return respond(&mut socket, 401, json!({"message": "unauthorized", "errinfo": {}})).await;
            }
            let token = format!("hub-{}", self.state().access.len());
            self.sign_in(&token);
            return respond(&mut socket, 200, json!({"access_token": token})).await;
        }

        let signed_in = bearer
            .as_deref()
            .is_some_and(|token| self.state().access.contains(token));
        if !signed_in {
            return respond(&mut socket, 401, json!({"code": "unauthenticated", "message": "invalid token"})).await;
        }

        if let Some(rest) = path.strip_prefix("/box/") {
            let sandbox = rest.split('/').next().unwrap_or_default().to_owned();
            return self.process(&mut socket, &sandbox, &path).await;
        }
        if let Some(rest) = path.strip_prefix("/v1/sandboxes/") {
            let id = rest.trim_end_matches("/network-policy").to_owned();
            let mut state = self.state();
            let Some(sandbox) = state.boxes.get_mut(&id) else {
                drop(state);
                return respond(&mut socket, 404, json!({"message": "sandbox not found"})).await;
            };
            if method == "PUT" {
                match parsed["mode"].as_str() {
                    Some("allow-all") => sandbox.network = "allow-all",
                    Some("deny-all") => sandbox.network = "deny-all",
                    _ => {
                        drop(state);
                        return respond(&mut socket, 400, json!({"message": "request validation failed: invalid field \"mode\""})).await;
                    }
                }
            }
            let mode = sandbox.network;
            drop(state);
            return respond(&mut socket, 200, json!({"mode": mode})).await;
        }
        if let Some(rpc) = path.strip_prefix("/sbx/docker.sbx.v1.") {
            let (status, reply) = self.control(rpc, &parsed);
            return respond(&mut socket, status, reply).await;
        }
        respond(&mut socket, 404, json!({"code": "not_found", "message": "no route"})).await
    }

    fn token(&self, form: &Value) -> (u16, Value) {
        let mut state = self.state();
        match form["grant_type"].as_str() {
            Some("urn:ietf:params:oauth:grant-type:device_code") => {
                if state.pending_polls > 0 {
                    state.pending_polls -= 1;
                    return (403, json!({"error": "authorization_pending", "error_description": "User has yet to authorize device code."}));
                }
                drop(state);
                (200, self.issue())
            }
            Some("refresh_token") => {
                if state.refresh.as_deref() != form["refresh_token"].as_str() {
                    return (403, json!({"error": "invalid_grant", "error_description": "Unknown or invalid refresh token."}));
                }
                state.refreshes += 1;
                drop(state);
                (200, self.issue())
            }
            _ => (400, json!({"error": "unsupported_grant_type"})),
        }
    }

    /// New tokens, the refresh token rotated.
    fn issue(&self) -> Value {
        let mut state = self.state();
        state.next += 1;
        let access = format!("access-{}", state.next);
        let refresh = format!("refresh-{}", state.next);
        state.access.insert(access.clone());
        state.refresh = Some(refresh.clone());
        let id_token = jwt(json!({"username": state.username, "email": "vu@example.com"}));
        json!({
            "access_token": access, "refresh_token": refresh, "id_token": id_token,
            "scope": "openid offline_access", "expires_in": state.token_life, "token_type": "Bearer",
        })
    }

    fn sandbox_json(&self, id: &str, b: &StubBox) -> Value {
        json!({
            "core": {
                "id": id, "name": b.name, "status": b.status,
                "image": "docker.io/sbx/shell:latest",
                "resources": {"cpus": 2, "memoryMib": "4096"},
                "platform": {"os": "linux", "architecture": "amd64"},
                "createdAt": "2026-11-16T04:41:15.089795Z",
                "endpoint": {"uri": format!("{}/box/{id}", self.url), "protocol": "SANDBOX_ENDPOINT_PROTOCOL_CONNECT", "sandboxId": id},
            },
            "cloud": {
                "lifecycle": if b.status == "SANDBOX_STATUS_RUNNING" { "CLOUD_LIFECYCLE_STATE_RUNNING" } else { "CLOUD_LIFECYCLE_STATE_HIBERNATED" },
                "expiresAt": "2026-11-16T05:12:06.760187Z", "autoResume": true, "onTimeout": "ON_TIMEOUT_STOP",
            },
        })
    }

    fn operation(&self, kind: &str, id: &str, done: bool) -> Value {
        let mut op = json!({
            "id": format!("op_{kind}_{id}"),
            "metadata": {"kind": format!("OPERATION_KIND_{kind}_SANDBOX"), "target": id,
                         "createdAt": "2026-11-16T04:42:19.601422Z", "updatedAt": "2026-11-16T04:42:19.601422Z"},
        });
        if done {
            op["done"] = json!(true);
            op["response"] = json!({"@type": "type.googleapis.com/docker.sbx.v1.Sandbox", "core": {"id": id}});
        }
        op
    }

    fn control(&self, rpc: &str, body: &Value) -> (u16, Value) {
        let not_found = (404, json!({"code": "not_found", "message": "Sandbox not found"}));
        let target = body["sandbox"]["id"].as_str().unwrap_or_default().to_owned();
        match rpc {
            "ImageService/ListImages" => (200, json!({"images": [
                {"id": "tmpl_001m79a5fdr3jvbrksq53bd183b", "name": "zeron-dev", "status": "IMAGE_STATUS_COMPLETED", "source": "IMAGE_SOURCE_SANDBOX", "totalSizeBytes": "2026708046", "captureMode": "CAPTURE_MODE_DISK", "platform": {"os": "linux", "architecture": "amd64"}},
                {"id": "tmpl_001m7a3bhe5tz4mrrx1sjf2d3sp", "name": "half-saved", "status": "IMAGE_STATUS_PENDING", "source": "IMAGE_SOURCE_SANDBOX"},
            ]})),
            "SecretService/ListSecrets" => (200, json!({"secrets": [
                {"id": "docker", "name": "docker", "serviceType": "docker", "type": "SECRET_TYPE_OAUTH_REFRESH", "scopes": ["offline_access"]},
                {"id": "docker-sbx:github", "name": "docker-sbx:github", "serviceType": "github", "type": "SECRET_TYPE_TOKEN"},
                {"id": "docker-sbx:iroha", "name": "docker-sbx:iroha", "serviceType": "custom:docker-sbx:iroha", "type": "SECRET_TYPE_CUSTOM",
                 "injection": {"hosts": ["iroha.example.dev"], "header": "Authorization", "format": "Bearer %s", "envVar": "IROHA_API_KEY"}, "placeholder": "sbx-cs-x"},
            ]})),
            "SandboxService/CreateSandbox" => {
                if body.get("networkPolicies").is_some() {
                    return (400, json!({"code": "failed_precondition", "message": "network_policies[0]: allowAll is not supported by Cloud policy bindings"}));
                }
                if body["image"].is_string() && body.get("resources").is_some() {
                    return (400, json!({"code": "invalid_argument", "message": "'cpus' can only be specified with 'imageRef' (inline mode), not with 'template'"}));
                }
                let mut state = self.state();
                // Same request id: the same sandbox (idempotent create).
                let request = body["requestId"].as_str().unwrap_or_default();
                let id = format!("sbx_{}", request.replace(|c: char| !c.is_ascii_alphanumeric(), ""));
                state.boxes.entry(id.clone()).or_insert(StubBox {
                    name: body["name"].as_str().unwrap_or_default().into(),
                    status: "SANDBOX_STATUS_RUNNING",
                    heading: None,
                    network: "deny-all",
                });
                drop(state);
                (200, self.operation("CREATE", &id, true))
            }
            "SandboxService/GetSandbox" => {
                let mut state = self.state();
                let Some(b) = state.boxes.get_mut(&target) else {
                    return not_found;
                };
                if let Some((to, left)) = b.heading {
                    if left == 0 {
                        b.status = to;
                        b.heading = None;
                    } else {
                        b.heading = Some((to, left - 1));
                    }
                }
                let b = b.clone();
                drop(state);
                (200, self.sandbox_json(&target, &b))
            }
            "SandboxService/StartSandbox" | "SandboxService/StopSandbox" => {
                let start = rpc.ends_with("StartSandbox");
                let mut state = self.state();
                let polls = state.transition_polls;
                let Some(b) = state.boxes.get_mut(&target) else {
                    return not_found;
                };
                let to = if start { "SANDBOX_STATUS_RUNNING" } else { "SANDBOX_STATUS_STOPPED" };
                let done = b.status == to;
                if !done {
                    b.status = if start { "SANDBOX_STATUS_STARTING" } else { "SANDBOX_STATUS_STOPPING" };
                    b.heading = Some((to, polls));
                }
                drop(state);
                (200, self.operation(if start { "START" } else { "STOP" }, &target, done))
            }
            "SandboxService/DeleteSandbox" => {
                if self.state().boxes.remove(&target).is_none() {
                    return not_found;
                }
                (200, self.operation("DELETE", &target, true))
            }
            "SandboxService/RenewSandboxTimeout" => {
                let b = self.state().boxes.get(&target).cloned();
                match b {
                    Some(b) => (200, self.sandbox_json(&target, &b)),
                    None => not_found,
                }
            }
            _ => (501, json!({"code": "unimplemented", "message": format!("{rpc} is not implemented")})),
        }
    }

    async fn process(&self, socket: &mut TcpStream, sandbox: &str, path: &str) {
        if !path.ends_with("/cloud.todd.v1.ProcessService/Start") {
            return respond(socket, 404, json!({"code": "not_found"})).await;
        }
        let running = self
            .state()
            .boxes
            .get(sandbox)
            .is_some_and(|b| b.status == "SANDBOX_STATUS_RUNNING");
        if !running {
            return respond(socket, 503, json!({"code": "unavailable", "message": "sandbox is not running"})).await;
        }
        let frames = self.state().runs.pop_front().unwrap_or_else(|| {
            vec![
                (0, json!({"event": {"processData": {"processId": "proc-1", "pid": 458}}})),
                (0, json!({"event": {"processData": {"processId": "proc-1", "exitInfo": {"exitTime": "1794804136", "exitReason": "normal"}}}})),
                (2, json!({})),
            ]
        });
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/connect+json\r\nConnection: close\r\n\r\n";
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        for (flags, message) in frames {
            let bytes = message.to_string().into_bytes();
            let mut frame = vec![flags];
            frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            frame.extend_from_slice(&bytes);
            if socket.write_all(&frame).await.is_err() {
                return;
            }
            let _ = socket.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let _ = socket.shutdown().await;
    }
}

/// A recorded process output frame (base64 data, as Docker sends it).
pub fn output(stream: &str, text: &str) -> (u8, Value) {
    let data = base64::engine::general_purpose::STANDARD.encode(text);
    (0, json!({"event": {"processData": {"processId": "proc-1", "data": data, "streamType": stream}}}))
}

pub fn exit(code: Option<i32>, reason: &str) -> (u8, Value) {
    let mut info = json!({"exitTime": "1794804128", "exitReason": reason});
    if let Some(code) = code {
        info["exitCode"] = json!(code);
    }
    (0, json!({"event": {"processData": {"processId": "proc-1", "exitInfo": info}}}))
}

pub fn started() -> (u8, Value) {
    (0, json!({"event": {"processData": {"processId": "proc-1", "pid": 458}}}))
}

pub fn keepalive() -> (u8, Value) {
    (0, json!({"event": {"keepalive": {}}}))
}

pub fn end(error: Option<(&str, &str)>) -> (u8, Value) {
    match error {
        Some((code, message)) => (2, json!({"error": {"code": code, "message": message}})),
        None => (2, json!({})),
    }
}

async fn respond(socket: &mut TcpStream, status: u16, body: Value) {
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let reply = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(reply.as_bytes()).await;
    let _ = socket.shutdown().await;
}

type Request = (String, String, Vec<(String, String)>, Vec<u8>);

async fn read_request(socket: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < length {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some((method, path, headers, body))
}

fn url_form(body: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
