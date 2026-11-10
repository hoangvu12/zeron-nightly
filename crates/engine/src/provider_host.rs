//! Hosting provider-run chats across devices (docs/design/provider-run-chats.md
//! §4, ticket #12): takeover on use, the claim, and demotion.
//!
//! - **Eligible.** This engine advertises `provider-run-v1` while the
//!   `claude-code-cloud` harness is on, and can host a chat when it also has
//!   a login for the chat's account ([`ProviderCloud::has_login`]).
//! - **Takeover on use** ([`ProviderHosting::used`]): when the person opens
//!   or sends to a provider-run chat here, and its host is offline (the
//!   relay's own view, [`HostPresence`]), lacks the capability, or said it
//!   can't host it (`hostUnavailable`), an eligible device claims it. Nothing
//!   moves a chat in the background, and an eligible host online keeps it.
//!   When this device can't host the chat either, the chat shows "Sign in as
//!   <email> to continue".
//! - **The claim** writes `deviceId` alone, clocked past the field's current
//!   clock ([`WorkspaceHost::claim_chat_host`]). Work starts only once the
//!   room echoes the merged row with this device as host
//!   ([`WorkspaceHost::is_host`] reads the echoed row): queued commands
//!   drain, the run resumes from the transcript's watermark, and the
//!   transcript says "Continued here, <device> was offline".
//! - **Demotion.** The moment the echoed host is another device, this
//!   device's run ends (closing the stream never interrupts the session),
//!   the drain stops (it asks `is_host`), and every request a run would
//!   still send asks first ([`ProviderCloud::check_host`]).
//! - **hostUnavailable.** A host whose login can't act for a chat's account
//!   says so on the row ([`publish_host_unavailable`]), and clears it once
//!   its login works again; a new host clears it on taking over.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use zeron_harness::provider_cloud::{AccountLogin, ProviderChatState};
use zeron_proto::provider_cloud::{HOST_UNAVAILABLE_ACCOUNT, HostUnavailable};
use zeron_proto::{HarnessId, ProviderSession};

use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// The capability an engine that can host provider-run chats advertises.
pub use zeron_proto::capabilities::PROVIDER_RUN_V1;

/// A claim older than this that the room never echoed is forgotten (the
/// next use may claim again).
const CLAIM_TTL: Duration = Duration::from_secs(60);
/// A claim's clock is at least this far ahead of now
/// (docs/design/provider-run-chats.md §4).
const CLAIM_LEAD: chrono::Duration = chrono::Duration::seconds(5);
/// How long a device whose claim was echoed waits for a competing claim to
/// land before it starts work (see `became_host`).
const CLAIM_SETTLE: Duration = Duration::from_secs(5);

/// Whether a device is online, as the relay sees it: its `host_offline`
/// signal (no ping for 75 s). Tests stand in a fake relay.
pub trait HostPresence: Send + Sync {
    fn online<'a>(&'a self, device_id: &'a str) -> BoxFuture<'a, bool>;
}

/// The production [`HostPresence`]: a fresh presence beat in the registry
/// room says online; otherwise the device's relay room is asked whether its
/// host socket is connected. With neither answering, only a device the
/// registry has long seen dark counts as offline.
struct RelayPresence {
    workspace: WorkspaceHost,
}

impl HostPresence for RelayPresence {
    fn online<'a>(&'a self, device_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let liveness = self.workspace.peer_liveness(device_id);
            if liveness == zeron_rpc::PeerLiveness::Live {
                return true;
            }
            match self.workspace.relay_host_connected(device_id).await {
                Some(connected) => connected,
                None => liveness != zeron_rpc::PeerLiveness::Dark,
            }
        })
    }
}

/// Why the current host isn't hosting the chat, as the device taking it over
/// tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Offline,
    /// It lacks `provider-run-v1` (an older build, or the feature is off).
    Ineligible,
    /// It said so (`hostUnavailable`).
    CantHost,
}

#[derive(Debug, Clone)]
struct Claim {
    from: String,
    why: Why,
    at: tokio::time::Instant,
}

/// Hosting provider-run chats on this engine (see module docs).
#[derive(Clone)]
pub struct ProviderHosting {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    presence: RwLock<Arc<dyn HostPresence>>,
    /// Takeovers claimed here and not yet echoed, by chat.
    claims: Mutex<HashMap<String, Claim>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProviderHosting {
    pub(crate) fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
    ) -> Self {
        let presence: Arc<dyn HostPresence> = Arc::new(RelayPresence {
            workspace: workspace.clone(),
        });
        Self {
            inner: Arc::new(Inner {
                sessions,
                doc_host,
                workspace,
                presence: RwLock::new(presence),
                claims: Mutex::default(),
            }),
        }
    }

    /// Replace how device presence is judged (tests: a fake relay).
    pub fn set_presence(&self, presence: Arc<dyn HostPresence>) {
        *self
            .inner
            .presence
            .write()
            .unwrap_or_else(PoisonError::into_inner) = presence;
    }

    fn presence(&self) -> Arc<dyn HostPresence> {
        self.inner
            .presence
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Follow the chats' hosts for as long as the engine runs. (The device
    /// row's `provider-run-v1` follows the "Claude cloud sessions" setting,
    /// `crate::claude_cloud_sessions`.)
    pub(crate) fn start(&self) {
        let hosting = self.clone();
        self.inner
            .doc_host
            .spawn_worker(async move { hosting.follow_hosts().await });
    }

    /// The person opened or sent to `chat_id` on this device: take it over
    /// when its host can't host it (see module docs). Returns at once.
    pub fn used(&self, chat_id: &str) {
        let workspace = &self.inner.workspace;
        let provider = workspace.is_provider_chat(chat_id)
            || workspace
                .chat(chat_id)
                .ok()
                .flatten()
                .is_some_and(|chat| chat.provider_session.is_some());
        if !provider {
            return;
        }
        let hosting = self.clone();
        let chat = chat_id.to_owned();
        self.inner.doc_host.spawn_worker(async move {
            hosting.consider(&chat).await;
        });
    }

    /// Decide whether to take `chat_id` over (see [`Self::used`]).
    async fn consider(&self, chat_id: &str) {
        let workspace = &self.inner.workspace;
        let me = workspace.device_id().to_owned();
        let Some(row) = workspace.chat(chat_id).ok().flatten() else {
            return;
        };
        let Some(session) = row.provider_session.clone().or_else(|| {
            self.inner
                .doc_host
                .open(chat_id)
                .ok()?
                .doc()
                .provider_run_session()
        }) else {
            return;
        };
        let Some(host) = workspace.provider_chat_host(chat_id) else {
            return;
        };
        if host == me {
            return;
        }
        if let Some(claim) = lock(&self.inner.claims).get(chat_id)
            && claim.from == host
            && claim.at.elapsed() < CLAIM_TTL
        {
            return; // waiting for the room's echo
        }
        let Some(why) = self.why_not_hosting(chat_id, &host).await else {
            return; // an eligible host is online: it keeps the chat
        };
        if !self.can_host_here(&session).await {
            // Neither the host nor this device can run it.
            self.show_sign_in_as(chat_id, &session);
            return;
        }
        match workspace.claim_chat_host(chat_id, chrono::Utc::now() + CLAIM_LEAD) {
            Ok(true) => {
                tracing::info!(chat = %chat_id, from = %host, ?why, "claiming the provider-run chat");
                lock(&self.inner.claims).insert(
                    chat_id.to_owned(),
                    Claim {
                        from: host,
                        why,
                        at: tokio::time::Instant::now(),
                    },
                );
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(chat = %chat_id, %error, "host claim failed"),
        }
    }

    /// Why `host` isn't hosting `chat_id` right now; `None` when it is an
    /// eligible device that is online.
    async fn why_not_hosting(&self, chat_id: &str, host: &str) -> Option<Why> {
        let workspace = &self.inner.workspace;
        if workspace
            .chat_host_unavailable(chat_id)
            .is_some_and(|unavailable| unavailable.device_id == host)
        {
            return Some(Why::CantHost);
        }
        let capable = workspace
            .read_devices()
            .unwrap_or_default()
            .iter()
            .any(|d| d.id == host && d.capabilities.iter().any(|c| c == PROVIDER_RUN_V1));
        if !capable {
            return Some(Why::Ineligible);
        }
        if !self.presence().online(host).await {
            return Some(Why::Offline);
        }
        None
    }

    /// Whether this device can host chats of `session`'s account now.
    async fn can_host_here(&self, session: &ProviderSession) -> bool {
        let Ok(harness) = self
            .inner
            .sessions
            .registry()
            .resolve(HarnessId::ClaudeCodeCloud)
        else {
            return false;
        };
        let Some(cloud) = harness.provider_cloud() else {
            return false;
        };
        cloud.has_login(&session.account_uuid).await
    }

    /// "Sign in as <email> to continue": no device that could host the chat
    /// is hosting it, and this one has no login for its account.
    fn show_sign_in_as(&self, chat_id: &str, session: &ProviderSession) {
        let state = ProviderChatState::SignInAs {
            email: session.account_email.clone(),
        };
        let Ok(handle) = self.inner.doc_host.open(chat_id) else {
            return;
        };
        if handle.doc().provider_chat_state().as_ref() == Some(&state) {
            return;
        }
        if let Err(error) = handle.doc().set_provider_chat_state(Some(&state)) {
            tracing::warn!(chat = %chat_id, %error, "couldn't show the sign-in state");
        }
    }

    /// Follow every provider-run chat's echoed host: start work on the ones
    /// this device just became host of, stop on the ones it just lost.
    async fn follow_hosts(&self) {
        let workspace = self.inner.workspace.clone();
        let me = workspace.device_id().to_owned();
        let mut changes = workspace.watch_changes();
        let mut hosts: HashMap<String, Option<String>> = HashMap::new();
        let mut first = true;
        loop {
            let chats = workspace.read_chats().unwrap_or_default();
            let mut seen: HashMap<String, Option<String>> = HashMap::new();
            for chat in &chats {
                if chat.provider_session.is_none() && !workspace.is_provider_chat(&chat.id) {
                    continue;
                }
                let host = workspace
                    .is_host(&chat.id)
                    .then(|| me.clone())
                    .or_else(|| workspace.provider_chat_host(&chat.id));
                seen.insert(chat.id.clone(), host);
            }
            // Chats whose row went away while hosted here: not host now.
            for (chat, host) in &hosts {
                if host.as_deref() == Some(me.as_str()) && !seen.contains_key(chat) {
                    seen.insert(chat.clone(), None);
                }
            }
            for (chat, host) in &seen {
                let before = hosts.get(chat).cloned().flatten();
                let hosted_before = before.as_deref() == Some(me.as_str());
                let hosts_now = host.as_deref() == Some(me.as_str());
                if hosts_now && !hosted_before {
                    // A claim echoed (or the room moved the chat here). A
                    // chat first seen hosted here (created here, or at boot)
                    // starts as it always did.
                    let claimed = lock(&self.inner.claims).remove(chat);
                    if !first && (before.is_some() || claimed.is_some()) {
                        self.became_host(chat, before, claimed);
                    }
                } else if !hosts_now && (hosted_before || self.inner.sessions.has_run(chat)) {
                    // Lost it, or a run outlived the loss (it started as the
                    // demotion landed).
                    self.demoted(chat, host.clone());
                } else if !hosts_now {
                    // A claim the room never made good on is forgotten in
                    // time (another device may hold the chat only for a
                    // moment, so not sooner).
                    let mut claims = lock(&self.inner.claims);
                    if claims
                        .get(chat)
                        .is_some_and(|c| c.at.elapsed() >= CLAIM_TTL)
                    {
                        claims.remove(chat);
                    }
                }
            }
            hosts = seen;
            first = false;
            if changes.changed().await.is_err() {
                return;
            }
        }
    }

    /// This device hosts `chat_id` now: say why in the transcript, clear the
    /// row's and the chat's "can't host" states, run what was queued, and
    /// resume mirroring the session.
    fn became_host(&self, chat_id: &str, before: Option<String>, claim: Option<Claim>) {
        tracing::info!(chat = %chat_id, from = ?before, "hosting the provider-run chat");
        let hosting = self.clone();
        let chat = chat_id.to_owned();
        self.inner.doc_host.spawn_worker(async move {
            let inner = &hosting.inner;
            let workspace = &inner.workspace;
            // Devices that claimed at the same moment converge within the
            // room's round trip: this device acts as host (drains, sends)
            // only once that has passed with it still host
            // (`WorkspaceHost::is_host`), so the losing claimant never
            // starts work it then may not finish.
            let deadline = tokio::time::Instant::now() + CLAIM_SETTLE;
            loop {
                if workspace.is_host(&chat) {
                    break;
                }
                let still_ours =
                    workspace.provider_chat_host(&chat).as_deref() == Some(workspace.device_id());
                if !still_ours || tokio::time::Instant::now() >= deadline {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if workspace.chat_host_unavailable(&chat).is_some()
                && let Err(error) = workspace.set_chat_host_unavailable(&chat, None)
            {
                tracing::warn!(chat = %chat, %error, "couldn't clear hostUnavailable");
            }
            let Ok(handle) = inner.doc_host.open(&chat) else {
                return;
            };
            if matches!(
                handle.doc().provider_chat_state(),
                Some(ProviderChatState::SignInAs { .. })
            ) && let Err(error) = handle.doc().set_provider_chat_state(None)
            {
                tracing::warn!(chat = %chat, %error, "couldn't clear the sign-in state");
            }
            // The note goes in before anything runs here, so it sits ahead of
            // what this device mirrors.
            if let Some(claim) = &claim {
                hosting.note_takeover(&handle, claim);
            }
            inner.doc_host.drain_commands(&handle).await;
            if workspace.is_host(&chat)
                && let Err(error) =
                    crate::provider_run::resume_as_host(&inner.sessions, &inner.doc_host, &chat)
                        .await
            {
                tracing::warn!(chat = %chat, %error, "couldn't resume the provider-run chat");
            }
            if !workspace.is_host(&chat) {
                // Another claim won meanwhile (two devices claimed at once):
                // whatever started here stops.
                hosting.demoted(&chat, workspace.provider_chat_host(&chat));
                return;
            }
        });
    }

    /// "Continued here, <device> was offline." in the transcript.
    fn note_takeover(&self, handle: &crate::doc_host::ChatDocHandle, claim: &Claim) {
        let name = self
            .inner
            .workspace
            .read_devices()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.id == claim.from)
            .map(|d| d.name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "the other device".to_owned());
        let text = takeover_note(&name, claim.why);
        let now = chrono::Utc::now().timestamp_millis();
        let id = format!("takeover:{}:{now}", self.inner.workspace.device_id());
        let entry = SessionMessageEntry {
            id: id.clone(),
            role: MessageRole::System,
            parts: vec![MessagePart::Text { id, text }],
            created_at: now,
            device_id: self.inner.workspace.device_id().to_owned(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            duration_ms: None,
        };
        if let Err(error) = handle.doc().push_message(&entry) {
            tracing::warn!(%error, "couldn't note the takeover");
        }
    }

    /// Another device hosts `chat_id` now (or nobody can tell): stop
    /// mirroring and sending. Closing the run never interrupts the session.
    fn demoted(&self, chat_id: &str, to: Option<String>) {
        tracing::info!(chat = %chat_id, to = ?to, "no longer hosting the provider-run chat");
        let sessions = self.inner.sessions.clone();
        let chat = chat_id.to_owned();
        self.inner.doc_host.spawn_worker(async move {
            if let Err(error) = sessions.terminate(&chat).await {
                tracing::warn!(chat = %chat, %error, "couldn't stop the demoted run");
            }
        });
    }
}

/// The quiet note a takeover leaves in the transcript.
fn takeover_note(device: &str, why: Why) -> String {
    match why {
        Why::Offline => format!("Continued here, {device} was offline."),
        Why::Ineligible => format!("Continued here, {device} can't run Claude cloud chats."),
        Why::CantHost => format!("Continued here, {device} couldn't host this chat."),
    }
}

/// A chat this device hosts, whose account its login is (`login`, as the
/// harness last found it): `hostUnavailable` says so when it can't act for
/// the account, and is cleared once it can (see module docs).
pub(crate) fn publish_host_unavailable(
    doc_host: &DocHost,
    chat_id: &str,
    login: Option<AccountLogin>,
) {
    let Some(workspace) = doc_host.workspace() else {
        return;
    };
    let me = workspace.device_id();
    let current = workspace.chat_host_unavailable(chat_id);
    let names_me = current.as_ref().is_some_and(|u| u.device_id == me);
    let write = match login {
        Some(AccountLogin::CantHost) if !names_me => Some(Some(HostUnavailable {
            device_id: me.to_owned(),
            reason: HOST_UNAVAILABLE_ACCOUNT.to_owned(),
        })),
        Some(AccountLogin::Works) if names_me => Some(None),
        _ => None,
    };
    if let Some(value) = write
        && let Err(error) = workspace.set_chat_host_unavailable(chat_id, value.as_ref())
    {
        tracing::warn!(chat = %chat_id, %error, "couldn't publish hostUnavailable");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_names_the_device_and_why() {
        assert_eq!(
            takeover_note("PC", Why::Offline),
            "Continued here, PC was offline."
        );
        assert!(takeover_note("PC", Why::CantHost).starts_with("Continued here, PC "));
    }
}
