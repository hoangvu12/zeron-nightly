//! The Claude Code session API over HTTP + SSE, with Zeron's own identity
//! (docs/design/provider-run-chats.md §2). Every request:
//!
//! - says who it is: `User-Agent: zeron/<version>`, and never sends
//!   `anthropic-client-platform` (or the CLI's / web app's User-Agent);
//! - carries a token read from the login folder for that request
//!   ([`super::credentials::Credentials::token_for`]), after the folder's
//!   account was checked against the one the request acts for;
//! - calls only the session endpoints (list, create, events, stream,
//!   history, archive). Never the environment or settings endpoints.
//!
//! A 401 has the CLI renew the login once, then the request is sent once
//! more with the token read after it. A 401 to that retry turns the whole
//! feature off on this device, saying why, and the chat's account shows
//! "Sign in again". What each request found about its account is published in
//! [`CloudClient::status`].

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use super::credentials::{ClaudeAccount, CredentialError, Credentials};
use super::sse::SseDecoder;
use crate::provider_cloud::{AccountLogin, ProviderCloudStatus};

/// Why the feature turns itself off after a 401 survives a renewal.
pub const REJECTED_AFTER_RENEWAL: &str = "Claude cloud sessions turned off on this device: Claude refused the login even after the Claude CLI renewed it.";

/// Why the feature is off when its `DISABLED` file is in place and says
/// nothing itself.
pub const DISABLED_BY_FILE: &str =
    "Claude cloud sessions are off on this device: its DISABLED file is in place.";

/// The production API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Zeron's identity on every request.
pub fn user_agent() -> String {
    format!("zeron/{}", env!("CARGO_PKG_VERSION"))
}

/// Why a request failed. Never carries a credential.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    Credentials(#[from] CredentialError),
    #[error("Claude cloud returned {status}: {message}")]
    Status {
        status: u16,
        message: String,
        /// How long the server asked to wait (`Retry-After`), if it did.
        retry_after: Option<std::time::Duration>,
    },
    #[error("Claude cloud could not be reached: {0}")]
    Transport(String),
    #[error("Claude cloud sent a response Zeron can't read: {0}")]
    Decode(String),
    /// The feature turned itself off on this device (why).
    #[error("{0}")]
    Disabled(String),
    /// Not sent: another device hosts the chat now.
    #[error("Another device hosts this chat now.")]
    NotHost,
}

impl ApiError {
    /// Whether trying again later can work: the server couldn't be reached,
    /// was busy (429) or failed (5xx). Never a refusal or a login problem.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Status { status, .. } => *status == 429 || (500..600).contains(status),
            _ => false,
        }
    }

    /// How long the server asked to wait before trying again.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// A `Retry-After` in seconds (the API's form; a date isn't read).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

/// A sequence number, which the API writes as a string.
fn de_seq<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_u64(),
        _ => None,
    })
}

/// One session as the list and create endpoints describe it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub environment_kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// What the worker is doing, in the list's (not verified live) words.
    /// A session the list doesn't describe reads as idle.
    #[serde(default)]
    pub worker_status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_event_at: Option<String>,
}

impl SessionInfo {
    pub fn archived(&self) -> bool {
        self.status.as_deref() == Some("archived")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionPage {
    #[serde(default)]
    pub data: Vec<SessionInfo>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// The server's answer for one posted event.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventAck {
    /// The uuid was seen before: the event was ignored.
    #[serde(default)]
    pub duplicate: bool,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default, deserialize_with = "de_seq")]
    pub sequence_num: Option<u64>,
}

/// One stored event, as the history endpoint and the stream carry it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StoredEvent {
    #[serde(default, deserialize_with = "de_seq")]
    pub sequence_num: Option<u64>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryPage {
    #[serde(default)]
    pub data: Vec<StoredEvent>,
    #[serde(default)]
    pub resume_cursor: Option<String>,
}

/// One item off the session stream.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// A stored event (`client_event`): it has a sequence number, and a
    /// replay from an earlier number brings it again.
    Stored(StoredEvent),
    /// A live-only event (`ephemeral_event`: text deltas, hook progress).
    Ephemeral(Value),
    /// Anything else the stream says (`session_update`, `delivery_update`).
    Other { event: String },
    /// A stored or live event in a shape Zeron doesn't know (which event):
    /// the API changed, and the feature turns itself off.
    Unrecognised(String),
}

/// The session API, acting for one login folder.
pub struct CloudClient {
    http: reqwest::Client,
    base: String,
    credentials: Arc<Credentials>,
    /// The accounts' sign-in states and the kill switch.
    status: watch::Sender<ProviderCloudStatus>,
    /// The kill switch's file (`DISABLED` in the feature's data folder):
    /// in place, the feature is off, saying what it says; a trip writes
    /// its reason there, so the feature stays off across restarts until
    /// the file is removed.
    kill_switch: std::sync::Mutex<Option<std::path::PathBuf>>,
}

impl CloudClient {
    /// Fails rather than fall back to a client without Zeron's
    /// `User-Agent`: no request may go out unnamed.
    pub fn new(base_url: &str, credentials: Arc<Credentials>) -> Result<Self, ApiError> {
        let base = base_url.trim_end_matches('/').to_owned();
        let mut builder = reqwest::Client::builder().user_agent(user_agent());
        // A loopback base (tests) must never go through a system proxy.
        if is_loopback(&base) {
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(|e| {
            ApiError::Transport(format!("the HTTP client couldn't be set up ({e})"))
        })?;
        Ok(Self {
            http,
            base,
            credentials,
            status: watch::Sender::new(ProviderCloudStatus::default()),
            kill_switch: std::sync::Mutex::default(),
        })
    }

    /// Where the kill switch's file is (see the field); it is read now, and
    /// again before every request.
    pub fn set_kill_switch(&self, path: std::path::PathBuf) {
        *self
            .kill_switch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
        self.check_kill_switch();
    }

    fn kill_switch_path(&self) -> Option<std::path::PathBuf> {
        self.kill_switch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Off when the kill switch's file is in place, for the reason it gives.
    fn check_kill_switch(&self) {
        if self.disabled().is_some() {
            return;
        }
        let Some(path) = self.kill_switch_path() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let reason = text.trim();
        self.trip(if reason.is_empty() {
            DISABLED_BY_FILE
        } else {
            reason
        });
    }

    /// Turn the feature off on this device, saying why (an event in a shape
    /// Zeron doesn't know, say).
    pub(crate) fn disable(&self, reason: &str) {
        self.trip(reason);
    }

    pub fn credentials(&self) -> &Arc<Credentials> {
        &self.credentials
    }

    /// The accounts' sign-in states and the kill switch, as they change.
    pub fn status(&self) -> watch::Receiver<ProviderCloudStatus> {
        self.status.subscribe()
    }

    /// Why the feature turned itself off on this device, if it did.
    pub fn disabled(&self) -> Option<String> {
        self.status.borrow().disabled.clone()
    }

    /// Record what this device's login means for `account` now.
    fn note(&self, account: &ClaudeAccount, login: AccountLogin) {
        self.status.send_if_modified(|status| {
            if status.accounts.get(&account.account_uuid) == Some(&login) {
                return false;
            }
            status.accounts.insert(account.account_uuid.clone(), login);
            true
        });
    }

    /// Turn the feature off on this device, saying why, and keep it off
    /// (the kill switch's file says why from now on).
    fn trip(&self, reason: &str) {
        let tripped = self.status.send_if_modified(|status| {
            if status.disabled.is_some() {
                return false;
            }
            tracing::error!(target: "zeron_harness::claude_cloud", reason, "Claude cloud sessions turned off");
            status.disabled = Some(reason.to_owned());
            true
        });
        if let (true, Some(path)) = (tripped, self.kill_switch_path())
            && !path.exists()
        {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, format!("{reason}\n")));
            if let Err(error) = written {
                tracing::error!(target: "zeron_harness::claude_cloud", %error, "couldn't keep Claude cloud sessions off");
            }
        }
    }

    /// A 403 or 404 for the session list or create, with a login whose
    /// identity checked out: Anthropic changed or blocked the API, so the
    /// feature turns off (a single session's refusals don't come here).
    fn refused(
        &self,
        what: &str,
        result: Result<reqwest::Response, ApiError>,
    ) -> Result<reqwest::Response, ApiError> {
        let response = result?;
        let status = response.status().as_u16();
        if matches!(status, 403 | 404) {
            self.trip(&format!(
                "Claude cloud sessions turned off on this device: Claude refused {what} ({status}), so its API may have changed or be blocked."
            ));
        }
        Ok(response)
    }

    /// Send one request acting for `account`, `build` adding its query and
    /// body (see the module docs). A credential failure is recorded for the
    /// account on the way out.
    async fn execute(
        &self,
        method: reqwest::Method,
        path: &str,
        account: &ClaudeAccount,
        build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ApiError> {
        self.check_kill_switch();
        if let Some(reason) = self.disabled() {
            return Err(ApiError::Disabled(reason));
        }
        let sent = self
            .send_authorized(&format!("{}{path}", self.base), method, account, build)
            .await;
        if let Err(ApiError::Credentials(error)) = &sent
            && let Some(login) = error.account_login()
        {
            self.note(account, login);
        }
        sent
    }

    /// The token is read for this request only and dropped once it's sent;
    /// a 401 has the CLI renew the login, then the request goes once more.
    /// Only a successful response says the login works.
    async fn send_authorized(
        &self,
        url: &str,
        method: reqwest::Method,
        account: &ClaudeAccount,
        build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ApiError> {
        let mut token = self.credentials.token_for(account).await?;
        let mut retried = false;
        loop {
            let sent = token.fingerprint();
            let request = build(
                self.http
                    .request(method.clone(), url)
                    .header(reqwest::header::AUTHORIZATION, token.authorization())
                    .header("anthropic-version", API_VERSION),
            );
            drop(token);
            let response = request
                .send()
                .await
                .map_err(|e| ApiError::Transport(without_url(&e)))?;
            if response.status() != reqwest::StatusCode::UNAUTHORIZED {
                if response.status().is_success() {
                    self.note(account, AccountLogin::Works);
                }
                return Ok(response);
            }
            if retried {
                self.trip(REJECTED_AFTER_RENEWAL);
                return Err(CredentialError::Rejected.into());
            }
            retried = true;
            token = self.credentials.renew(account, sent).await?;
        }
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, ApiError> {
        let status = response.status();
        let wait = retry_after(response.headers());
        let body = response
            .bytes()
            .await
            .map_err(|e| ApiError::Transport(without_url(&e)))?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                message: error_message(&body),
                retry_after: wait,
            });
        }
        serde_json::from_slice(&body).map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// `GET /v1/code/sessions?limit&cursor`: newest activity first.
    pub async fn list_sessions(
        &self,
        account: &ClaudeAccount,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<SessionPage, ApiError> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor.to_owned()));
        }
        let response = self
            .execute(reqwest::Method::GET, "/v1/code/sessions", account, |r| {
                r.query(&query)
            })
            .await;
        let response = self.refused("the session list", response)?;
        self.send_json(response).await
    }

    /// `POST /v1/code/sessions`. Not idempotent: never retried here.
    pub async fn create_session(
        &self,
        account: &ClaudeAccount,
        body: &Value,
    ) -> Result<SessionInfo, ApiError> {
        #[derive(Deserialize)]
        struct Created {
            session: Option<SessionInfo>,
            #[serde(default)]
            id: Option<String>,
        }
        let response = self
            .execute(reqwest::Method::POST, "/v1/code/sessions", account, |r| {
                r.json(body)
            })
            .await;
        let response = self.refused("a new session", response)?;
        let created: Created = self.send_json(response).await?;
        match created {
            Created {
                session: Some(session),
                ..
            } if !session.id.is_empty() => Ok(session),
            Created { id: Some(id), .. } if !id.is_empty() => Ok(SessionInfo {
                id,
                ..SessionInfo::default()
            }),
            _ => Err(ApiError::Decode("the created session has no id".into())),
        }
    }

    /// `POST /v1/code/sessions/{id}/events` with `{events: [{payload}]}`.
    /// Each payload's `uuid` is the server's dedupe key.
    pub async fn send_events(
        &self,
        account: &ClaudeAccount,
        session_id: &str,
        payloads: Vec<Value>,
    ) -> Result<Vec<EventAck>, ApiError> {
        #[derive(Deserialize)]
        struct Acks {
            #[serde(default)]
            results: Vec<EventAck>,
        }
        let events: Vec<Value> = payloads
            .into_iter()
            .map(|payload| json!({ "payload": payload }))
            .collect();
        let body = json!({ "events": events });
        let response = self
            .execute(
                reqwest::Method::POST,
                &format!("/v1/code/sessions/{}/events", path_segment(session_id)),
                account,
                |r| r.json(&body),
            )
            .await?;
        let acks: Acks = self.send_json(response).await?;
        Ok(acks.results)
    }

    /// `GET /v1/code/sessions/{id}/events?sort_order=asc[&cursor]`.
    pub async fn history(
        &self,
        account: &ClaudeAccount,
        session_id: &str,
        cursor: Option<&str>,
    ) -> Result<HistoryPage, ApiError> {
        let mut query = vec![("sort_order", "asc".to_owned())];
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor.to_owned()));
        }
        let response = self
            .execute(
                reqwest::Method::GET,
                &format!("/v1/code/sessions/{}/events", path_segment(session_id)),
                account,
                |r| r.query(&query),
            )
            .await?;
        self.send_json(response).await
    }

    /// `GET /v1/code/sessions/{id}/events/stream?from_sequence_num=N`: the
    /// stored events after `N`, then everything live.
    pub async fn stream(
        &self,
        account: &ClaudeAccount,
        session_id: &str,
        from_sequence_num: u64,
    ) -> Result<BoxStream<'static, Result<StreamItem, ApiError>>, ApiError> {
        let response = self
            .execute(
                reqwest::Method::GET,
                &format!(
                    "/v1/code/sessions/{}/events/stream",
                    path_segment(session_id)
                ),
                account,
                |r| {
                    let r = r.header(reqwest::header::ACCEPT, "text/event-stream");
                    if from_sequence_num > 0 {
                        r.query(&[("from_sequence_num", from_sequence_num.to_string())])
                    } else {
                        r
                    }
                },
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            let wait = retry_after(response.headers());
            let body = response.bytes().await.unwrap_or_default();
            return Err(ApiError::Status {
                status: status.as_u16(),
                message: error_message(&body),
                retry_after: wait,
            });
        }
        let bytes = response.bytes_stream().boxed();
        let state = (bytes, SseDecoder::default(), VecDeque::<StreamItem>::new());
        Ok(
            futures::stream::unfold(state, |(mut bytes, mut decoder, mut pending)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((Ok(item), (bytes, decoder, pending)));
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            for event in decoder.push(&chunk) {
                                pending.push_back(stream_item(&event.event, &event.data));
                            }
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(ApiError::Transport(without_url(&e))),
                                (bytes, decoder, pending),
                            ));
                        }
                        None => return None,
                    }
                }
            })
            .boxed(),
        )
    }
}

/// One SSE event off the session stream. A stored event (`client_event`)
/// must carry a sequence number and an object payload, a live one
/// (`ephemeral_event`) an object payload: anything else in those envelopes
/// is a shape Zeron doesn't know ([`StreamItem::Unrecognised`]). Other
/// event names (`session_update`, `delivery_update`, and any new ones) say
/// nothing the mirror needs.
fn stream_item(event: &str, data: &str) -> StreamItem {
    let unrecognised = || StreamItem::Unrecognised(event.to_owned());
    match event {
        "client_event" => match serde_json::from_str::<StoredEvent>(data) {
            Ok(stored) if stored.sequence_num.is_some() && stored.payload.is_object() => {
                StreamItem::Stored(stored)
            }
            _ => unrecognised(),
        },
        "ephemeral_event" => {
            match serde_json::from_str::<Value>(data)
                .ok()
                .and_then(|mut v| v.get_mut("payload").map(Value::take))
            {
                Some(payload) if payload.is_object() => StreamItem::Ephemeral(payload),
                _ => unrecognised(),
            }
        }
        other => StreamItem::Other {
            event: other.to_owned(),
        },
    }
}

/// Why the feature turns off on a stream event in a shape Zeron doesn't
/// know.
pub fn unrecognised_reason(event: &str) -> String {
    format!(
        "Claude cloud sessions turned off on this device: a session sent an event in a shape Zeron doesn't know ({event}), so its API may have changed."
    )
}

/// A session id as one URL path segment (ids are `cse_…`; anything else is
/// percent-encoded rather than trusted).
fn path_segment(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Whether `base` names this machine (`127.0.0.1`, `localhost` or `::1`),
/// with or without a scheme and port.
pub fn is_loopback(base: &str) -> bool {
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .unwrap_or(base);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// A transport error's text without the request URL (which is harmless,
/// but kept out on principle: error text reaches transcripts).
fn without_url(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "the session API");
    }
    text
}

/// The API's error body (`{"error":{"message"}}`), else a short excerpt.
fn error_message(body: &[u8]) -> String {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    if let Some(message) = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("message")))
        .and_then(Value::as_str)
    {
        return crate::redact_secrets(message);
    }
    let text = String::from_utf8_lossy(body);
    let mut short: String = text.chars().take(200).collect();
    if short.is_empty() {
        short = "no details".into();
    }
    crate::redact_secrets(&short)
}
