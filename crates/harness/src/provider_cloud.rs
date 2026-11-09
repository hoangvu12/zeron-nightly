//! The provider-cloud facet of a harness: what the engine needs beyond
//! [`crate::Harness::run`] to drive a provider-run chat
//! (docs/design/provider-run-chats.md). Provider-neutral; Claude's Code
//! cloud ([`crate::claude_cloud`]) is the one implementation.
//!
//! The engine creates the session itself (synchronously, on the device the
//! person is using, never queued or retried), records the returned
//! [`ProviderSession`] on the chat, and before every fresh dispatch hands the
//! harness a [`RunBinding`] naming the session and the account that owns it.
//! A run whose session has no binding refuses to start (fail closed).

use async_trait::async_trait;
use zeron_proto::ProviderSession;

/// Why a provider-cloud operation failed. The message is for people; it
/// never carries a credential.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderCloudError {
    /// This device has no usable login for the account (none, logged out,
    /// expired past renewal, or credentials moved off the file).
    #[error("{0}")]
    SignInRequired(String),
    /// The device's login is a different account than the chat's: this
    /// device can't host the chat right now.
    #[error("{0}")]
    AccountMismatch(String),
    /// The provider refused or failed the request (status, short reason).
    #[error("{message}")]
    Provider {
        status: Option<u16>,
        message: String,
    },
    /// Anything else (network, a response this build can't read).
    #[error("{0}")]
    Other(String),
}

/// One new provider cloud session, created with its first prompt.
#[derive(Debug, Clone)]
pub struct CreateSession {
    /// The first prompt, sent inside the create request.
    pub prompt: String,
    /// The provider event uuid the first prompt travels under (derived from
    /// the Zeron message id, [`zeron_proto::provider_cloud::event_uuid`]).
    pub prompt_uuid: String,
    pub model: Option<String>,
    pub title: Option<String>,
}

/// What a fresh run of a provider-run chat needs to know: its session and
/// owning account, and the provider uuid of the prompt it opens with.
#[derive(Debug, Clone)]
pub struct RunBinding {
    pub session: ProviderSession,
    /// The uuid the run's opening prompt is sent under; `None` mints one.
    pub prompt_uuid: Option<String>,
}

#[async_trait]
pub trait ProviderCloud: Send + Sync {
    /// Create one provider cloud session in this device's own login, with
    /// `request.prompt` as its first message. NOT idempotent: two calls make
    /// two sessions, so callers never retry it on their own.
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<ProviderSession, ProviderCloudError>;

    /// Tie the chat's next fresh run to its session. Called by the engine
    /// before every fresh dispatch; replaces any earlier binding for the
    /// same session.
    fn bind_run(&self, binding: RunBinding);
}
