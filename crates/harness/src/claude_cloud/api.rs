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

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};

use super::credentials::{ClaudeAccount, CredentialError, Credentials};
use super::sse::SseDecoder;

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
    Status { status: u16, message: String },
    #[error("Claude cloud could not be reached: {0}")]
    Transport(String),
    #[error("Claude cloud sent a response Zeron can't read: {0}")]
    Decode(String),
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
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub last_event_at: Option<String>,
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
}

/// The session API, acting for one login folder.
pub struct CloudClient {
    http: reqwest::Client,
    base: String,
    credentials: Arc<Credentials>,
}

impl CloudClient {
    pub fn new(base_url: &str, credentials: Arc<Credentials>) -> Self {
        let base = base_url.trim_end_matches('/').to_owned();
        let mut builder = reqwest::Client::builder().user_agent(user_agent());
        // A loopback base (tests) must never go through a system proxy.
        if is_loopback(&base) {
            builder = builder.no_proxy();
        }
        Self {
            http: builder.build().unwrap_or_else(|_| reqwest::Client::new()),
            base,
            credentials,
        }
    }

    pub fn credentials(&self) -> &Arc<Credentials> {
        &self.credentials
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        account: &ClaudeAccount,
    ) -> Result<reqwest::RequestBuilder, ApiError> {
        // Read for this request only; dropped when the request is built.
        let token = self.credentials.token_for(account).await?;
        Ok(self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(reqwest::header::AUTHORIZATION, token.bearer())
            .header("anthropic-version", API_VERSION))
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = request
            .send()
            .await
            .map_err(|e| ApiError::Transport(without_url(&e)))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| ApiError::Transport(without_url(&e)))?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                message: error_message(&body),
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
        let request = self
            .request(reqwest::Method::GET, "/v1/code/sessions", account)
            .await?
            .query(&query);
        self.send_json(request).await
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
        let request = self
            .request(reqwest::Method::POST, "/v1/code/sessions", account)
            .await?
            .json(body);
        let created: Created = self.send_json(request).await?;
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
        let request = self
            .request(
                reqwest::Method::POST,
                &format!("/v1/code/sessions/{}/events", path_segment(session_id)),
                account,
            )
            .await?
            .json(&json!({ "events": events }));
        let acks: Acks = self.send_json(request).await?;
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
        let request = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/code/sessions/{}/events", path_segment(session_id)),
                account,
            )
            .await?
            .query(&query);
        self.send_json(request).await
    }

    /// `GET /v1/code/sessions/{id}/events/stream?from_sequence_num=N`: the
    /// stored events after `N`, then everything live.
    pub async fn stream(
        &self,
        account: &ClaudeAccount,
        session_id: &str,
        from_sequence_num: u64,
    ) -> Result<BoxStream<'static, Result<StreamItem, ApiError>>, ApiError> {
        let mut request = self
            .request(
                reqwest::Method::GET,
                &format!(
                    "/v1/code/sessions/{}/events/stream",
                    path_segment(session_id)
                ),
                account,
            )
            .await?
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if from_sequence_num > 0 {
            request = request.query(&[("from_sequence_num", from_sequence_num.to_string())]);
        }
        let response = request
            .send()
            .await
            .map_err(|e| ApiError::Transport(without_url(&e)))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.bytes().await.unwrap_or_default();
            return Err(ApiError::Status {
                status: status.as_u16(),
                message: error_message(&body),
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

fn stream_item(event: &str, data: &str) -> StreamItem {
    match event {
        "client_event" => match serde_json::from_str::<StoredEvent>(data) {
            Ok(stored) => StreamItem::Stored(stored),
            Err(_) => StreamItem::Other {
                event: event.to_owned(),
            },
        },
        "ephemeral_event" => {
            let payload = serde_json::from_str::<Value>(data)
                .ok()
                .and_then(|mut v| v.get_mut("payload").map(Value::take))
                .unwrap_or(Value::Null);
            StreamItem::Ephemeral(payload)
        }
        other => StreamItem::Other {
            event: other.to_owned(),
        },
    }
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

fn is_loopback(base: &str) -> bool {
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
