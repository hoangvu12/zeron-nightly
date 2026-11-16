//! Docker Sandbox: Docker's cloud sandboxes as a provider (#40,
//! docs/design/cloud-byo.md "Docker specifics"). Every call to Docker is in
//! this module, over HTTPS from the host device; no CLI.
//!
//! Docker's API is the one its `sbx` CLI uses, undocumented, so it can
//! change. Shapes recorded from the real service on 2026-11-16 (the test
//! stub in `tests/support/docker_stub.rs` replays them):
//!
//! - Control: Connect JSON, `POST {api}/sbx/docker.sbx.v1.<Service>/<Method>`
//!   (`SandboxService/{Create,Get,Start,Stop,Delete}Sandbox`,
//!   `RenewSandboxTimeout`, `ImageService/ListImages`,
//!   `SecretService/ListSecrets`, `OperationService/GetOperation`). Errors are
//!   `{code, message}` with a Connect code. Create answers a finished
//!   operation (~4 s); Start/Stop answer one still running, so we poll
//!   `GetSandbox` for the status (start ~3 s, stop ~35 s).
//! - Blank is inline mode (`cloud.imageRef` + `resources`); a template is
//!   `image: <template id>` and keeps its own size (resources refused).
//! - Network: allow-all can't go in Create ("not supported by Cloud policy
//!   bindings"); it's `PUT {api}/v1/sandboxes/{id}/network-policy
//!   {"mode":"allow-all"}`, for that sandbox only.
//! - Secrets: `cloud.secrets` names the account's proxy secrets; the sandbox
//!   sees placeholders and Docker's proxy puts the real value on the wire.
//! - Run a command: Connect server-streaming
//!   `POST {core.endpoint.uri}/cloud.todd.v1.ProcessService/Start`
//!   (`application/connect+json`, 5-byte frames). Output is base64 by stream;
//!   the exit is `exitInfo {exitCode, exitReason}`, with `exitCode` left out
//!   when 0 and `exitReason: "timeout"` (code 137) at the time limit. A
//!   missing program ends the stream with an error.
//! - Sign-in: Docker's OAuth device flow at `{login}/oauth/*` with the `sbx`
//!   CLI's public client id; access tokens last 15 min (`expires_in: 900`)
//!   and refresh tokens rotate (a used one is refused: `invalid_grant`).
//!   A pasted personal access token is exchanged at
//!   `{hub}/v2/auth/token {identifier, secret}`, as `sbx login
//!   --password-stdin` does.
//! - Some networks reset IPv6 connections to Docker (seen while recording),
//!   so we dial IPv4, and try a dropped connection again.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::keys::KeySlot;
use super::provider::{
    Account, Command, CreateSandbox, Credential, DeviceCode, ErrorKind, Exit, Ops,
    ProviderError, RunEvent, RunStream, SandboxProvider, SandboxStatus, Template,
};

/// Where Docker's services are.
#[derive(Clone, Debug)]
pub struct DockerUrls {
    /// The sandboxes API (control, network policy).
    pub api: String,
    /// Docker's sign-in (OAuth).
    pub login: String,
    /// Docker Hub (personal access token exchange).
    pub hub: String,
}

impl DockerUrls {
    /// The `sbx` CLI's public OAuth client id (a public client: no secret).
    pub const CLIENT_ID: &'static str = "jpJu8hP8fJgYYChz0EghwE9x6veXM2N6";

    pub fn docker() -> Self {
        Self {
            api: "https://api.sandboxes-cloud.docker.com".into(),
            login: "https://login.docker.com".into(),
            hub: "https://hub.docker.com".into(),
        }
    }

    /// Every service at `base` (a test's stand-in).
    pub fn all(base: &str) -> Self {
        let base = base.trim_end_matches('/').to_owned();
        Self {
            api: base.clone(),
            login: base.clone(),
            hub: base,
        }
    }
}

/// The provider's id, persisted with each sandbox.
pub const ID: &str = "docker";
/// Blank: Docker's plain image (Debian, user `agent` with sudo).
const BLANK_IMAGE: &str = "docker.io/sbx/shell:latest";
/// Blank's size: `small`, 2 CPUs and 4 GB.
const BLANK_CPUS: u32 = 2;
const BLANK_MEMORY_MIB: &str = "4096";
/// The time limit a sandbox is created with; it stops (never deletes) when
/// it runs out, unless the host device extends it while a chat is busy.
const TIME_LIMIT: Duration = Duration::from_secs(30 * 60);
const WORKDIR: &str = "/home/agent/workspace";
const AUDIENCE: &str = "https://hub.docker.com";
const SCOPE: &str = "openid offline_access";
/// The longest Start or Stop may take before we give up waiting.
const STATUS_DEADLINE: Duration = Duration::from_secs(240);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Attempts for a call whose connection drops before an answer.
const TRIES: u32 = 3;
/// How long a Hub token from a personal access token is used before it's
/// exchanged again (Docker doesn't say; a refusal exchanges it sooner).
const HUB_TOKEN_LIFE: Duration = Duration::from_secs(5 * 60);
/// The detached launcher's `$0`, so it's easy to find with `ps`.
const LAUNCHER: &str = "zeron-detach";
/// How long a detached command's launcher may take.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

/// What's kept in the key slot. No `Debug`: it's never logged.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Kept {
    /// Signed in on Docker's page: the current (rotating) refresh token.
    #[serde(rename_all = "camelCase")]
    OAuth {
        refresh_token: String,
        account: String,
    },
    /// A pasted personal access token and its user.
    #[serde(rename_all = "camelCase")]
    Pat { username: String, token: String },
    /// A pasted access token: works until it expires (epoch seconds).
    #[serde(rename_all = "camelCase")]
    Access {
        token: String,
        account: String,
        expires_at: Option<u64>,
    },
}

impl Kept {
    fn account(&self) -> String {
        match self {
            Kept::OAuth { account, .. } | Kept::Access { account, .. } => account.clone(),
            Kept::Pat { username, .. } => username.clone(),
        }
    }
}

/// The access token in use.
struct Session {
    token: String,
    fresh_until: Instant,
}

/// Docker's answer to `/oauth/token`.
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct OAuthError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// The Docker Sandbox provider. Keeps the account's sign-in in its
/// [`KeySlot`] on this device.
pub struct DockerProvider {
    http: reqwest::Client,
    urls: DockerUrls,
    slot: KeySlot,
    poll: Duration,
    kept: Mutex<Option<Kept>>,
    /// Held while a token is fetched, so a rotating refresh token is used
    /// once however many calls need a token at the same moment.
    session: tokio::sync::Mutex<Option<Session>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn err(kind: ErrorKind, message: impl Into<String>) -> ProviderError {
    ProviderError::new(kind, message)
}

impl DockerProvider {
    /// Docker Sandbox signed in from `slot` (if the user connected it).
    pub fn new(slot: KeySlot) -> Self {
        Self::with_urls(slot, DockerUrls::docker())
    }

    pub fn with_urls(slot: KeySlot, urls: DockerUrls) -> Self {
        let http = reqwest::Client::builder()
            .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        let kept = slot.get().and_then(|raw| match serde_json::from_str(&raw) {
            Ok(kept) => Some(kept),
            Err(_) => {
                tracing::warn!("docker: unreadable sign-in in its key slot; connect again");
                None
            }
        });
        Self {
            http,
            urls,
            slot,
            poll: Duration::from_secs(1),
            kept: Mutex::new(kept),
            session: tokio::sync::Mutex::new(None),
        }
    }

    /// How often Start and Stop look at the sandbox's status.
    pub fn poll_every(mut self, every: Duration) -> Self {
        self.poll = every;
        self
    }

    fn keep(&self, kept: Kept) -> Result<(), ProviderError> {
        let raw =
            serde_json::to_string(&kept).map_err(|e| err(ErrorKind::Fatal, e.to_string()))?;
        self.slot.set(&raw).map_err(|e| {
            err(
                ErrorKind::Fatal,
                format!("Couldn't save the Docker sign-in on this device: {e}"),
            )
        })?;
        *lock(&self.kept) = Some(kept);
        Ok(())
    }

    // ----- Sign-in --------------------------------------------------------

    /// A fresh access token: the one in use, else a new one from what's kept.
    async fn access_token(&self) -> Result<String, ProviderError> {
        let mut session = self.session.lock().await;
        if let Some(s) = session.as_ref()
            && Instant::now() < s.fresh_until
        {
            return Ok(s.token.clone());
        }
        let kept = lock(&self.kept).clone().ok_or_else(|| {
            err(
                ErrorKind::Config,
                "Docker isn't connected on this device. Connect it in Settings › Sandboxes.",
            )
        })?;
        let fresh = match kept {
            Kept::OAuth {
                refresh_token,
                account,
            } => {
                let reply = self
                    .oauth_token(&[
                        ("grant_type", "refresh_token"),
                        ("client_id", DockerUrls::CLIENT_ID),
                        ("refresh_token", &refresh_token),
                    ])
                    .await
                    .map_err(|e| match e {
                        Ok(no) => err(
                            ErrorKind::Config,
                            format!(
                                "Docker signed this device out ({}). Connect Docker again.",
                                no.error_description
                            ),
                        ),
                        Err(e) => e,
                    })?;
                // Rotated: keep the new one before anything else can fail.
                if let Some(next) = &reply.refresh_token {
                    self.keep(Kept::OAuth {
                        refresh_token: next.clone(),
                        account,
                    })?;
                }
                session_from(&reply)
            }
            Kept::Pat { username, token } => Session {
                token: self.exchange_pat(&username, &token).await?,
                fresh_until: Instant::now() + HUB_TOKEN_LIFE,
            },
            Kept::Access {
                token, expires_at, ..
            } => {
                let left = expires_at.map(|at| at.saturating_sub(epoch_secs()));
                if left == Some(0) {
                    return Err(err(
                        ErrorKind::Config,
                        "The pasted Docker token expired. Connect Docker again.",
                    ));
                }
                Session {
                    token,
                    fresh_until: Instant::now()
                        + left.map_or(Duration::from_secs(3600), Duration::from_secs),
                }
            }
        };
        let token = fresh.token.clone();
        *session = Some(fresh);
        Ok(token)
    }

    /// Docker refused `used`: forget it unless another call already did.
    /// Whether a new one is worth asking for.
    async fn refused(&self, used: &str) -> bool {
        let mut session = self.session.lock().await;
        if session.as_ref().is_some_and(|s| s.token == used) {
            *session = None;
        }
        // A pasted access token can't be renewed.
        !matches!(*lock(&self.kept), Some(Kept::Access { .. }) | None)
    }

    /// `POST {login}/oauth/token`. `Err(Ok(..))`: Docker said no.
    async fn oauth_token(
        &self,
        form: &[(&str, &str)],
    ) -> Result<TokenReply, Result<OAuthError, ProviderError>> {
        let url = format!("{}/oauth/token", self.urls.login);
        let response = self
            .transport(
                || self.http.post(&url).form(form).timeout(CALL_TIMEOUT),
                true,
            )
            .await
            .map_err(Err)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| Err(err(ErrorKind::Retryable, format!("Docker sign-in: {e}"))))?;
        if status.is_success() {
            return serde_json::from_slice(&body)
                .map_err(|e| Err(err(ErrorKind::Fatal, format!("Docker sign-in: {e}"))));
        }
        match serde_json::from_slice::<OAuthError>(&body) {
            Ok(no) if !no.error.is_empty() => Err(Ok(no)),
            _ => Err(Err(http_error(status, &body))),
        }
    }

    /// A Hub token for a personal access token.
    async fn exchange_pat(&self, username: &str, token: &str) -> Result<String, ProviderError> {
        let url = format!("{}/v2/auth/token", self.urls.hub);
        let body = json!({"identifier": username, "secret": token});
        let response = self
            .transport(
                || self.http.post(&url).json(&body).timeout(CALL_TIMEOUT),
                true,
            )
            .await?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| err(ErrorKind::Retryable, format!("Docker Hub: {e}")))?;
        if matches!(status.as_u16(), 401 | 403) {
            return Err(err(
                ErrorKind::Config,
                "Docker didn't accept that username and access token.",
            ));
        }
        if !status.is_success() {
            return Err(http_error(status, &bytes));
        }
        let reply: Value = serde_json::from_slice(&bytes)
            .map_err(|e| err(ErrorKind::Fatal, format!("Docker Hub: {e}")))?;
        reply["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| err(ErrorKind::Fatal, "Docker Hub gave no access token"))
    }

    // ----- Calls ----------------------------------------------------------

    /// Send what `build` makes, trying again when the connection drops
    /// before an answer (`any_drop`), or only when it couldn't connect.
    async fn transport(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
        any_drop: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match build().send().await {
                Ok(response) => return Ok(response),
                Err(e)
                    if attempt < TRIES
                        && (e.is_connect() || (any_drop && e.status().is_none())) =>
                {
                    tracing::debug!(error = %e, attempt, "docker: connection dropped; trying again");
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                }
                Err(e) => {
                    return Err(err(
                        ErrorKind::Retryable,
                        format!("Couldn't reach Docker: {e}"),
                    ));
                }
            }
        }
    }

    /// Send with the account's bearer token, renewing it once if refused.
    async fn signed(
        &self,
        build: impl Fn(&str) -> reqwest::RequestBuilder,
        any_drop: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut renewed = false;
        loop {
            let token = self.access_token().await?;
            let response = self.transport(|| build(&token), any_drop).await?;
            if response.status().as_u16() != 401 {
                return Ok(response);
            }
            if renewed || !self.refused(&token).await {
                return Err(err(
                    ErrorKind::Config,
                    "Docker refused this device's sign-in. Connect Docker again.",
                ));
            }
            renewed = true;
        }
    }

    /// A control-API call: `rpc` is `<Service>/<Method>`.
    async fn control(&self, rpc: &str, body: Value) -> Result<Value, ProviderError> {
        let url = format!("{}/sbx/docker.sbx.v1.{rpc}", self.urls.api);
        let response = self
            .signed(
                |token| {
                    self.http
                        .post(&url)
                        .bearer_auth(token)
                        .header("Connect-Protocol-Version", "1")
                        .json(&body)
                        .timeout(CALL_TIMEOUT)
                },
                true,
            )
            .await?;
        read_json(response).await
    }

    async fn get_sandbox(&self, sandbox: &str) -> Result<Value, ProviderError> {
        self.control(
            "SandboxService/GetSandbox",
            json!({"sandbox": {"id": sandbox}}),
        )
        .await
    }

    /// Poll until it reports `want`.
    async fn wait_for(&self, sandbox: &str, want: &str) -> Result<Value, ProviderError> {
        let deadline = Instant::now() + STATUS_DEADLINE;
        loop {
            let found = self.get_sandbox(sandbox).await?;
            let status = found["core"]["status"].as_str().unwrap_or_default();
            if status == want {
                return Ok(found);
            }
            if status.ends_with("_FAILED") || status.ends_with("_ERROR") {
                return Err(err(
                    ErrorKind::Fatal,
                    format!("Docker says the sandbox is {}", plain_status(status)),
                ));
            }
            if Instant::now() >= deadline {
                return Err(err(
                    ErrorKind::Retryable,
                    format!(
                        "The sandbox was still {} after {} s",
                        plain_status(status),
                        STATUS_DEADLINE.as_secs()
                    ),
                ));
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    /// Wait for a long-running operation to finish.
    async fn finish(&self, mut op: Value) -> Result<Value, ProviderError> {
        let deadline = Instant::now() + STATUS_DEADLINE;
        while op["done"] != true {
            if Instant::now() >= deadline {
                return Err(err(ErrorKind::Retryable, "Docker is still working on it"));
            }
            tokio::time::sleep(self.poll).await;
            let id = op["id"].as_str().unwrap_or_default().to_owned();
            op = self
                .control("OperationService/GetOperation", json!({"id": id}))
                .await?;
        }
        operation_failed(&op)?;
        Ok(op)
    }

    /// The account's proxy secrets, by name.
    async fn secret_names(&self) -> Result<Vec<String>, ProviderError> {
        let listed = self.control("SecretService/ListSecrets", json!({})).await?;
        Ok(listed["secrets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["name"].as_str().map(str::to_owned))
            .collect())
    }

    /// This sandbox lets everything out; the account's default is untouched.
    async fn allow_all(&self, sandbox: &str) -> Result<(), ProviderError> {
        let url = format!("{}/v1/sandboxes/{sandbox}/network-policy", self.urls.api);
        let body = json!({"mode": "allow-all"});
        let response = self
            .signed(
                |token| {
                    self.http
                        .put(&url)
                        .bearer_auth(token)
                        .json(&body)
                        .timeout(CALL_TIMEOUT)
                },
                true,
            )
            .await?;
        let reply = read_json(response).await?;
        if reply["mode"] != "allow-all" {
            return Err(err(
                ErrorKind::Fatal,
                format!("Docker kept the sandbox's network at {}", reply["mode"]),
            ));
        }
        Ok(())
    }

    async fn connect_device(&self, code: DeviceCode) -> Result<Account, ProviderError> {
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = Duration::from_secs(code.interval.max(1));
        let reply = loop {
            tokio::time::sleep(interval).await;
            if Instant::now() >= deadline {
                return Err(err(
                    ErrorKind::Config,
                    "The Docker sign-in code expired before it was approved. Connect again.",
                ));
            }
            match self
                .oauth_token(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", DockerUrls::CLIENT_ID),
                    ("device_code", &code.device_code),
                ])
                .await
            {
                Ok(reply) => break reply,
                Err(Ok(no)) if no.error == "authorization_pending" => {}
                Err(Ok(no)) if no.error == "slow_down" => interval += Duration::from_secs(5),
                Err(Ok(no)) => {
                    return Err(err(
                        ErrorKind::Config,
                        format!("Docker didn't sign in: {}", no.error_description),
                    ));
                }
                Err(Err(e)) if e.kind == ErrorKind::Retryable => {}
                Err(Err(e)) => return Err(e),
            }
        };
        let refresh_token = reply
            .refresh_token
            .clone()
            .ok_or_else(|| err(ErrorKind::Fatal, "Docker's sign-in gave no refresh token"))?;
        let named = reply
            .id_token
            .as_deref()
            .and_then(claims)
            .or_else(|| claims(&reply.access_token));
        let account = account_name(named.as_ref());
        self.keep(Kept::OAuth {
            refresh_token,
            account: account.clone(),
        })?;
        *self.session.lock().await = Some(session_from(&reply));
        Ok(Account { name: account })
    }

    async fn connect_pasted(&self, pasted: &str) -> Result<Account, ProviderError> {
        let pasted = pasted.trim();
        if let Some(found) = claims(pasted) {
            // An access token itself: check it, use it until it expires.
            let kept = Kept::Access {
                token: pasted.to_owned(),
                account: account_name(Some(&found)),
                expires_at: found["exp"].as_u64(),
            };
            let previous = lock(&self.kept).replace(kept.clone());
            *self.session.lock().await = None;
            let checked = self
                .control("ImageService/ListImages", json!({"pageSize": 1}))
                .await;
            if let Err(e) = checked {
                *lock(&self.kept) = previous;
                *self.session.lock().await = None;
                return Err(e);
            }
            self.keep(kept.clone())?;
            return Ok(Account {
                name: kept.account(),
            });
        }
        let Some((username, token)) = pasted
            .split_once(':')
            .map(|(u, t)| (u.trim(), t.trim()))
            .filter(|(u, t)| !u.is_empty() && !t.is_empty())
        else {
            return Err(err(
                ErrorKind::Config,
                "Paste your Docker username and personal access token as username:token.",
            ));
        };
        let hub = self.exchange_pat(username, token).await?;
        self.keep(Kept::Pat {
            username: username.to_owned(),
            token: token.to_owned(),
        })?;
        *self.session.lock().await = Some(Session {
            token: hub,
            fresh_until: Instant::now() + HUB_TOKEN_LIFE,
        });
        Ok(Account {
            name: username.to_owned(),
        })
    }
}

fn session_from(reply: &TokenReply) -> Session {
    let life = Duration::from_secs(reply.expires_in.unwrap_or(900));
    // Renew a little early: a minute, or half its life if it's short.
    let margin = (life / 2).min(Duration::from_secs(60));
    Session {
        token: reply.access_token.clone(),
        fresh_until: Instant::now() + life - margin,
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A JWT's claims, unverified (only to name the account).
fn claims(jwt: &str) -> Option<Value> {
    let mut parts = jwt.split('.');
    let (_, payload, _) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
}

fn account_name(claims: Option<&Value>) -> String {
    claims
        .and_then(|c| {
            ["username", "https://hub.docker.com/username", "email", "sub"]
                .iter()
                .find_map(|k| c[*k].as_str().filter(|v| !v.is_empty()))
        })
        .unwrap_or("Docker account")
        .to_owned()
}

/// `SANDBOX_STATUS_STOPPING` → "stopping".
fn plain_status(status: &str) -> String {
    status
        .trim_start_matches("SANDBOX_STATUS_")
        .to_ascii_lowercase()
}

/// A failed operation's error, as ours.
fn operation_failed(op: &Value) -> Result<(), ProviderError> {
    match op.get("error") {
        Some(error) if !error.is_null() => Err(err(
            ErrorKind::Fatal,
            format!(
                "Docker: {}",
                error["message"].as_str().unwrap_or("the operation failed")
            ),
        )),
        _ => Ok(()),
    }
}

/// A Connect (or REST) error answer, as ours.
fn http_error(status: reqwest::StatusCode, body: &[u8]) -> ProviderError {
    let reply: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let code = reply["code"].as_str().unwrap_or_default();
    let message = reply["message"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("HTTP {status}"));
    connect_error(Some(status.as_u16()), code, &message)
}

fn connect_error(status: Option<u16>, code: &str, message: &str) -> ProviderError {
    let kind = match code {
        "not_found" => ErrorKind::NotFound,
        "unauthenticated" | "permission_denied" => ErrorKind::Config,
        "failed_precondition" | "already_exists" | "aborted" | "out_of_range" => {
            ErrorKind::Conflict
        }
        "resource_exhausted" | "unavailable" | "deadline_exceeded" => ErrorKind::Retryable,
        "unimplemented" => ErrorKind::Unsupported,
        _ => match status {
            Some(404) => ErrorKind::NotFound,
            Some(401 | 403) => ErrorKind::Config,
            Some(409) => ErrorKind::Conflict,
            Some(429 | 502..=504) => ErrorKind::Retryable,
            _ => ErrorKind::Fatal,
        },
    };
    err(kind, format!("Docker: {message}"))
}

async fn read_json(response: reqwest::Response) -> Result<Value, ProviderError> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|e| err(ErrorKind::Retryable, format!("Docker: {e}")))?;
    if !status.is_success() {
        return Err(http_error(status, &body));
    }
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&body).map_err(|e| err(ErrorKind::Fatal, format!("Docker: {e}")))
}

/// A short, valid sandbox name for chat `key`.
fn sandbox_name(key: &str) -> String {
    let clean: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches('-');
    format!("zeron-{}", &clean[..clean.len().min(40)])
        .trim_end_matches('-')
        .to_owned()
}

/// Start `args` detached from the call, in a session of its own, so it keeps
/// running after the process service's stream ends. Restarting it when it
/// stops is the program's own job (Zeron's engine runs as `zeron-engine
/// watchdog`; there is no systemd in a Docker sandbox). One per program
/// name: starting it again ends the old one and everything it started, so a
/// Run again starts on the new environment (a fresh lent sign-in). Its
/// output goes to `~/.zeron/run/<name>.log`.
fn detached(args: &[String]) -> Vec<String> {
    // `pkill -g`: Debian's `dash` has no `kill -- -<group>`.
    const SCRIPT: &str = r#"name="$1"; shift
dir="$HOME/.zeron/run"; mkdir -p "$dir"
pid="$dir/$name.pid"; log="$dir/$name.log"
if [ -s "$pid" ]; then pkill -TERM -g "$(cat "$pid")" 2>/dev/null || true; sleep 1; fi
setsid sh -c 'echo $$ >"$0"; log="$1"; shift; exec "$@" >>"$log" 2>&1' "$pid" "$log" "$@" </dev/null >/dev/null 2>&1 &
sleep 1
if kill -0 "$(cat "$pid" 2>/dev/null)" 2>/dev/null; then echo started; else echo "$name stopped as it started:" >&2; tail -n 20 "$log" >&2; exit 1; fi"#;
    let name: String = args
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("program")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let mut wrapped = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        SCRIPT.to_owned(),
        LAUNCHER.to_owned(),
        name,
    ];
    wrapped.extend(args.iter().cloned());
    wrapped
}

/// One Connect message: 1 flag byte, 4-byte big-endian length, JSON.
fn envelope(message: &Value) -> Vec<u8> {
    let bytes = message.to_string().into_bytes();
    let mut frame = Vec::with_capacity(bytes.len() + 5);
    frame.push(0);
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&bytes);
    frame
}

/// What one process-service message says, if anything for us.
fn process_event(message: &Value) -> Option<RunEvent> {
    let data = &message["event"]["processData"];
    if let Some(info) = data.get("exitInfo") {
        return Some(RunEvent::Exit(Exit {
            code: Some(info["exitCode"].as_i64().unwrap_or(0) as i32),
            timed_out: info["exitReason"] == "timeout",
        }));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data["data"].as_str()?)
        .ok()?;
    match data["streamType"].as_str() {
        Some("STREAM_TYPE_STDERR") => Some(RunEvent::Stderr(bytes)),
        _ => Some(RunEvent::Stdout(bytes)),
    }
}

/// Read a Connect stream's frames as run events.
fn run_events(body: BoxStream<'static, reqwest::Result<Bytes>>) -> RunStream {
    struct Reader {
        body: BoxStream<'static, reqwest::Result<Bytes>>,
        buf: Vec<u8>,
        ended: bool,
    }
    let reader = Reader {
        body,
        buf: Vec::new(),
        ended: false,
    };
    futures::stream::unfold(reader, |mut r| async move {
        loop {
            if r.ended {
                return None;
            }
            if r.buf.len() >= 5 {
                let len = u32::from_be_bytes([r.buf[1], r.buf[2], r.buf[3], r.buf[4]]) as usize;
                if r.buf.len() >= 5 + len {
                    let flags = r.buf[0];
                    let message: Value =
                        serde_json::from_slice(&r.buf[5..5 + len]).unwrap_or(Value::Null);
                    r.buf.drain(..5 + len);
                    if flags & 0x02 != 0 {
                        // End of stream: maybe an error.
                        r.ended = true;
                        let error = &message["error"];
                        if error.is_object() {
                            let failure = connect_error(
                                None,
                                error["code"].as_str().unwrap_or_default(),
                                error["message"].as_str().unwrap_or("the command failed"),
                            );
                            return Some((Err(failure), r));
                        }
                        return None;
                    }
                    if let Some(event) = process_event(&message) {
                        return Some((Ok(event), r));
                    }
                    continue;
                }
            }
            match r.body.next().await {
                Some(Ok(chunk)) => r.buf.extend_from_slice(&chunk),
                // Lost before the end: `run_to_end` says so.
                Some(Err(_)) | None => return None,
            }
        }
    })
    .boxed()
}

#[async_trait]
impl SandboxProvider for DockerProvider {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        "Docker Sandbox"
    }

    fn icon(&self) -> &str {
        "docker"
    }

    fn ops(&self) -> Ops {
        Ops::CREATE
            | Ops::START
            | Ops::STOP
            | Ops::DELETE
            | Ops::RUN
            | Ops::TEMPLATES
            | Ops::CONNECT
            | Ops::EXTEND
    }

    fn workdir(&self, _chat_id: &str) -> String {
        WORKDIR.into()
    }

    fn account(&self) -> Option<Account> {
        lock(&self.kept)
            .as_ref()
            .map(|k| Account { name: k.account() })
    }

    async fn begin_connect(&self) -> Result<Option<DeviceCode>, ProviderError> {
        let url = format!("{}/oauth/device/code", self.urls.login);
        let form = [
            ("client_id", DockerUrls::CLIENT_ID),
            ("scope", SCOPE),
            ("audience", AUDIENCE),
        ];
        let response = self
            .transport(
                || self.http.post(&url).form(&form).timeout(CALL_TIMEOUT),
                true,
            )
            .await?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| err(ErrorKind::Retryable, format!("Docker sign-in: {e}")))?;
        if !status.is_success() {
            return Err(http_error(status, &body));
        }
        let reply: Value = serde_json::from_slice(&body)
            .map_err(|e| err(ErrorKind::Fatal, format!("Docker sign-in: {e}")))?;
        let text = |k: &str| reply[k].as_str().map(str::to_owned);
        Ok(Some(DeviceCode {
            user_code: text("user_code").unwrap_or_default(),
            verification_uri: text("verification_uri").unwrap_or_default(),
            verification_uri_complete: text("verification_uri_complete"),
            expires_in: reply["expires_in"].as_u64().unwrap_or(900),
            interval: reply["interval"].as_u64().unwrap_or(5),
            device_code: text("device_code")
                .ok_or_else(|| err(ErrorKind::Fatal, "Docker gave no device code"))?,
        }))
    }

    async fn connect(&self, credential: Credential) -> Result<Account, ProviderError> {
        match credential {
            Credential::DeviceCode(code) => self.connect_device(code).await,
            Credential::Token(pasted) => self.connect_pasted(&pasted).await,
        }
    }

    fn export_key(&self) -> Option<String> {
        lock(&self.kept)
            .as_ref()
            .and_then(|kept| serde_json::to_string(kept).ok())
    }

    async fn adopt_key(&self, raw: String) -> Result<Account, ProviderError> {
        let kept: Kept = serde_json::from_str(&raw).map_err(|_| {
            err(
                ErrorKind::Config,
                "The Docker sign-in from the other device couldn't be read; connect again",
            )
        })?;
        let account = Account {
            name: kept.account(),
        };
        // A token in use belonged to the sign-in this replaces.
        let mut session = self.session.lock().await;
        self.keep(kept)?;
        *session = None;
        Ok(account)
    }

    async fn forget_key(&self) {
        let mut session = self.session.lock().await;
        if let Err(e) = self.slot.clear() {
            tracing::warn!(error = %e, "docker: couldn't remove the sign-in from its key slot");
        }
        *lock(&self.kept) = None;
        *session = None;
    }

    async fn create(&self, request: CreateSandbox) -> Result<String, ProviderError> {
        let secrets = self.secret_names().await?;
        let mut cloud = json!({
            "timeout": format!("{}s", TIME_LIMIT.as_secs()),
            "onTimeout": "ON_TIMEOUT_STOP",
        });
        if !secrets.is_empty() {
            cloud["secrets"] = json!(secrets);
        }
        let env: BTreeMap<&String, &String> = request.env.iter().collect();
        let mut body = json!({
            "name": sandbox_name(&request.key),
            "requestId": request.key,
            "environment": env,
        });
        match &request.template {
            Some(template) => body["image"] = json!(template),
            None => {
                cloud["imageRef"] = json!(BLANK_IMAGE);
                body["resources"] = json!({"cpus": BLANK_CPUS, "memoryMib": BLANK_MEMORY_MIB});
            }
        }
        body["cloud"] = cloud;
        let op = self.control("SandboxService/CreateSandbox", body).await?;
        let id = op["metadata"]["target"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| err(ErrorKind::Fatal, "Docker didn't say which sandbox it made"))?;
        self.finish(op).await?;
        self.allow_all(&id).await?;
        self.wait_for(&id, "SANDBOX_STATUS_RUNNING").await?;
        Ok(id)
    }

    async fn start(&self, sandbox: &str) -> Result<(), ProviderError> {
        let op = self
            .control(
                "SandboxService/StartSandbox",
                json!({"sandbox": {"id": sandbox}}),
            )
            .await?;
        operation_failed(&op)?;
        self.wait_for(sandbox, "SANDBOX_STATUS_RUNNING").await?;
        Ok(())
    }

    async fn stop(&self, sandbox: &str) -> Result<(), ProviderError> {
        let op = self
            .control(
                "SandboxService/StopSandbox",
                json!({"sandbox": {"id": sandbox}}),
            )
            .await?;
        operation_failed(&op)?;
        self.wait_for(sandbox, "SANDBOX_STATUS_STOPPED").await?;
        Ok(())
    }

    async fn delete(&self, sandbox: &str) -> Result<(), ProviderError> {
        match self
            .control(
                "SandboxService/DeleteSandbox",
                json!({"sandbox": {"id": sandbox}}),
            )
            .await
        {
            Ok(op) => operation_failed(&op),
            Err(e) if e.kind == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn status(&self, sandbox: &str) -> Result<SandboxStatus, ProviderError> {
        let found = match self.get_sandbox(sandbox).await {
            Ok(found) => found,
            // Docker answers `not_found` once it's deleted.
            Err(e) if e.kind == ErrorKind::NotFound => return Ok(SandboxStatus::Gone),
            Err(e) => return Err(e),
        };
        Ok(match found["core"]["status"].as_str().unwrap_or_default() {
            "SANDBOX_STATUS_RUNNING" | "SANDBOX_STATUS_STARTING" | "SANDBOX_STATUS_CREATING" => {
                SandboxStatus::Running
            }
            status if status.contains("DELET") => SandboxStatus::Gone,
            _ => SandboxStatus::Stopped,
        })
    }

    async fn extend(&self, sandbox: &str, ttl: Duration) -> Result<(), ProviderError> {
        self.control(
            "SandboxService/RenewSandboxTimeout",
            json!({"sandbox": {"id": sandbox}, "timeout": format!("{}s", ttl.as_secs())}),
        )
        .await?;
        Ok(())
    }

    async fn templates(&self) -> Result<Vec<Template>, ProviderError> {
        let mut templates = Vec::new();
        let mut page = String::new();
        loop {
            let mut ask = json!({"pageSize": 100});
            if !page.is_empty() {
                ask["pageToken"] = json!(page);
            }
            let listed = self.control("ImageService/ListImages", ask).await?;
            templates.extend(
                listed["images"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|image| image["status"] == "IMAGE_STATUS_COMPLETED")
                    .filter_map(|image| {
                        Some(Template {
                            id: image["id"].as_str()?.to_owned(),
                            name: image["name"].as_str().or(image["id"].as_str())?.to_owned(),
                        })
                    }),
            );
            page = listed["nextPageToken"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if page.is_empty() {
                return Ok(templates);
            }
        }
    }

    async fn run(&self, sandbox: &str, command: Command) -> Result<RunStream, ProviderError> {
        let found = self.get_sandbox(sandbox).await?;
        let status = found["core"]["status"].as_str().unwrap_or_default();
        if status != "SANDBOX_STATUS_RUNNING" {
            return Err(err(
                ErrorKind::Conflict,
                format!("The sandbox isn't running (it's {}).", plain_status(status)),
            ));
        }
        let endpoint = found["core"]["endpoint"]["uri"]
            .as_str()
            .filter(|uri| !uri.is_empty())
            .ok_or_else(|| err(ErrorKind::Fatal, "Docker gave no address for the sandbox"))?
            .trim_end_matches('/')
            .to_owned();
        let (args, timeout) = if command.detach {
            (
                detached(&command.args),
                Some(command.timeout.unwrap_or(LAUNCH_TIMEOUT)),
            )
        } else {
            (command.args.clone(), command.timeout)
        };
        let mut start = json!({
            "args": args,
            "includeStdout": true,
            "includeStderr": true,
        });
        if !command.env.is_empty() {
            start["env"] = json!(command.env);
        }
        if let Some(cwd) = &command.cwd {
            start["workingDir"] = json!(cwd);
        }
        if let Some(timeout) = timeout {
            start["timeoutSeconds"] = json!(timeout.as_secs().max(1));
        }
        let frame = envelope(&start);
        let url = format!("{endpoint}/cloud.todd.v1.ProcessService/Start");
        let response = self
            .signed(
                |token| {
                    self.http
                        .post(&url)
                        .bearer_auth(token)
                        .header("Content-Type", "application/connect+json")
                        .header("Connect-Protocol-Version", "1")
                        .body(frame.clone())
                },
                // Not again once it may have started: only if it never connected.
                false,
            )
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            return Err(http_error(status, &body));
        }
        Ok(run_events(response.bytes_stream().boxed()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_short_and_plain() {
        assert_eq!(sandbox_name("Chat_1/x"), "zeron-chat-1-x");
        assert!(sandbox_name(&"a".repeat(100)).len() <= 46);
        assert_eq!(sandbox_name("--"), "zeron");
    }

    #[test]
    fn a_detached_program_gets_its_name() {
        let args = detached(&["/usr/local/bin/zeron-engine".into(), "watchdog".into()]);
        assert_eq!(
            &args[3..],
            [
                LAUNCHER,
                "zeron-engine",
                "/usr/local/bin/zeron-engine",
                "watchdog"
            ]
        );
    }

    #[test]
    fn errors_map_by_connect_code() {
        assert_eq!(
            connect_error(Some(404), "not_found", "x").kind,
            ErrorKind::NotFound
        );
        assert_eq!(
            connect_error(Some(400), "failed_precondition", "x").kind,
            ErrorKind::Conflict
        );
        assert_eq!(connect_error(Some(503), "", "x").kind, ErrorKind::Retryable);
        assert_eq!(
            connect_error(Some(400), "invalid_argument", "x").kind,
            ErrorKind::Fatal
        );
    }

    #[test]
    fn only_a_jwt_reads_as_an_access_token() {
        assert!(claims("vu:dckr_pat_x").is_none());
        assert!(claims("a.b").is_none());
        let jwt = format!(
            "x.{}.y",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"username":"vu"}"#)
        );
        assert_eq!(account_name(claims(&jwt).as_ref()), "vu");
    }

    #[tokio::test]
    async fn a_sign_in_moves_to_another_device_and_leaves_this_one() {
        let (here, there) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let laptop = DockerProvider::new(KeySlot::open(here.path(), ID));
        assert_eq!(laptop.export_key(), None);
        laptop
            .keep(Kept::Pat {
                username: "vu".into(),
                token: "dckr_pat_not-a-real-one".into(),
            })
            .unwrap();
        let raw = laptop.export_key().expect("connected");

        let server = DockerProvider::new(KeySlot::open(there.path(), ID));
        let account = server.adopt_key(raw).await.unwrap();
        assert_eq!(account.name, "vu");
        // Kept on the server's disk: it's still connected after a restart.
        let restarted = DockerProvider::new(KeySlot::open(there.path(), ID));
        assert_eq!(restarted.account().map(|a| a.name).as_deref(), Some("vu"));

        laptop.forget_key().await;
        assert_eq!(laptop.account(), None);
        assert_eq!(KeySlot::open(here.path(), ID).get(), None);
        assert!(server.adopt_key("garbage".into()).await.is_err());
    }
}

/// The manual smoke run against a real Docker account (ignored by default;
/// `scripts/smoke/docker-sandbox.ps1` runs it).
///
/// `ZERON_DOCKER_SMOKE_TOKEN` is a Docker access token; it's never printed.
/// Makes one small Blank sandbox, runs commands, checks the network allows
/// everything, installs Zeron from `ZERON_EDGE_URL` (default
/// `https://edge.zeron.sh`) with the Install Zeron step's script, checks a
/// detached program keeps running and is replaced by a rerun, extends,
/// stops, starts (a suspend: the
/// program survives) and deletes it. Costs a few cents.
#[cfg(test)]
mod smoke {
    use super::*;
    use crate::sandboxes::{Finished, run_to_end};

    async fn sh(p: &DockerProvider, sandbox: &str, script: &str) -> Finished {
        let command = Command::new(["sh", "-c", script]).timeout(Duration::from_secs(120));
        run_to_end(p, sandbox, command).await.unwrap()
    }

    #[tokio::test]
    #[ignore = "needs a Docker account: ZERON_DOCKER_SMOKE_TOKEN"]
    async fn a_real_docker_sandbox_end_to_end() {
        let token = std::env::var("ZERON_DOCKER_SMOKE_TOKEN").expect("ZERON_DOCKER_SMOKE_TOKEN");
        let edge =
            std::env::var("ZERON_EDGE_URL").unwrap_or_else(|_| "https://edge.zeron.sh".into());
        let dir = tempfile::tempdir().unwrap();
        let p = DockerProvider::new(KeySlot::open(dir.path(), ID));
        let account = p.connect(Credential::Token(token)).await.unwrap();
        eprintln!("smoke: connected as {}", account.name);
        eprintln!("smoke: {} templates", p.templates().await.unwrap().len());

        let key = format!("smoke-{}", crate::new_id());
        let id = p.create(CreateSandbox::new(&key, None)).await.unwrap();
        eprintln!("smoke: created {id}");
        let checks = async {
            let done = sh(&p, &id, "echo out; echo err >&2; whoami; exit 3").await;
            assert_eq!(done.exit.code, Some(3));
            assert!(done.stdout.contains("agent") && done.stderr.contains("err"));
            let net = sh(
                &p,
                &id,
                "curl -s -o /dev/null -w '%{http_code}' https://example.com",
            )
            .await;
            assert_eq!(net.stdout.trim(), "200", "allow-all: {net:?}");

            // Install Zeron, with the step's own script.
            let probe = Command::new([super::super::ENGINE_PROGRAM, "--version"]);
            let probe = run_to_end(&p, &id, probe).await;
            assert!(!probe.is_ok_and(|done| done.exit.success()), "Blank has no Zeron");
            let install = run_to_end(&p, &id, super::super::install_command(&edge))
                .await
                .unwrap();
            assert!(install.exit.success(), "install: {}", install.failure());
            eprintln!("smoke: installed {}", install.stdout.trim());

            // A detached program outlives its call, and starting it again
            // replaces it.
            let detach = || Command::new(["sleep", "600"]).detached();
            let started = run_to_end(&p, &id, detach()).await.unwrap();
            assert!(started.exit.success(), "{}", started.failure());
            let first = sh(&p, &id, "pgrep -x sleep").await.stdout;
            assert_eq!(first.lines().count(), 1, "running: {first}");
            run_to_end(&p, &id, detach()).await.unwrap();
            let second = sh(&p, &id, "pgrep -x sleep").await.stdout;
            assert!(
                second.lines().count() == 1 && second != first,
                "replaced: {first} → {second}"
            );

            p.extend(&id, TIME_LIMIT).await.unwrap();
            p.stop(&id).await.unwrap();
            eprintln!("smoke: stopped");
            p.start(&id).await.unwrap();
            let after = sh(&p, &id, "pgrep -x sleep | head -1").await.stdout;
            assert_eq!(after, second, "a stop and start is a suspend");
        };
        // Delete it even when a check failed.
        let outcome =
            futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(checks)).await;
        p.delete(&id).await.unwrap();
        eprintln!("smoke: deleted {id}");
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}
