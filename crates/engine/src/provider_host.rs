//! Hosting provider-run chats across devices (docs/design/provider-run-chats.md
//! §4, ticket #12): takeover on use, the claim, and demotion.
//!
//! - **Eligible.** This engine can host provider-run chats while the
//!   `claude-code-cloud` harness is on (the device row then advertises
//!   `provider-run-v1`, `crate::claude_cloud_sessions`), and a given chat when
//!   it also has a login for the chat's account (`ProviderCloud::has_login`).
//!   A device that isn't eligible never acts on a chat.
//! - **Takeover on use** ([`ProviderHosting::on_chat_used`]): when the person
//!   opens or sends to a provider-run chat here, and its host is offline (the
//!   relay's own view, [`HostPresence`]), lacks the capability, or said it
//!   can't host it (`hostUnavailable`), an eligible device claims it. Nothing
//!   moves a chat in the background, and an eligible host online keeps it.
//!   - When this device has no login for the chat's account either, and the
//!     host lacks the capability or said it can't host it, no device can:
//!     the chat shows "Sign in as <email> to continue". A host that is only
//!     offline keeps the chat waiting for it.
//! - **The claim** writes `deviceId` alone, clocked past the field's current
//!   clock ([`WorkspaceHost::claim_chat_host`]). Work starts only once the
//!   room echoes the merged row with this device as host and the settle wait
//!   ([`PROVIDER_HOST_SETTLE`]) has passed with it still host
//!   ([`WorkspaceHost::is_host`]): every takeover costs that wait, so two
//!   devices claiming at once never both start. Then queued commands drain,
//!   the run resumes from the transcript's watermark, and the transcript
//!   says "Continued here, <device> was offline".
//! - **Demotion.** The moment the echoed host is another device, this
//!   device's run ends (closing the stream never interrupts the session),
//!   the drain stops (it asks `is_host`), and every request a run would
//!   still send asks first (`ProviderCloud::check_host`). After a restart,
//!   nothing is hosted until the room has answered.
//! - **hostUnavailable.** A host whose login can't act for a chat's account
//!   says so on the row ([`publish_host_unavailable`]), and clears it once
//!   its login works again; a new host clears it on taking over.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use zeron_harness::provider_cloud::{AccountLogin, ProviderChatState};
use zeron_proto::provider_cloud::{HostUnavailable, HostUnavailableReason};
use zeron_proto::{HarnessId, ProviderSession};

use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::{PROVIDER_HOST_SETTLE, WorkspaceHost};

/// The capability an engine that can host provider-run chats advertises.
pub use zeron_proto::capabilities::PROVIDER_RUN_V1;

/// A claim older than this that the room never echoed is forgotten (the
/// next use may claim again).
const CLAIM_TTL: Duration = Duration::from_secs(60);
/// A claim's clock is at least this far ahead of now
/// (docs/design/provider-run-chats.md §4).
const CLAIM_LEAD: chrono::Duration = chrono::Duration::seconds(5);

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
enum TakeoverReason {
    Offline,
    /// It lacks `provider-run-v1` (an older build, or the feature is off).
    Ineligible,
    /// It said so (`hostUnavailable`).
    CantHost,
}

#[derive(Debug, Clone)]
struct Claim {
    from: String,
    reason: TakeoverReason,
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
    /// Counts the uses of provider-run chats decided here
    /// ([`ProviderHosting::watch_decisions`]).
    decisions: tokio::sync::watch::Sender<u64>,
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
                decisions: tokio::sync::watch::channel(0).0,
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

    /// Follow the chats' hosts for as long as the engine runs.
    pub(crate) fn start(&self) {
        let hosting = self.clone();
        self.inner
            .doc_host
            .spawn_worker(async move { hosting.follow_hosts().await });
    }

    /// How many uses of provider-run chats this engine has finished deciding
    /// about ([`Self::on_chat_used`]), as it changes.
    pub fn watch_decisions(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.decisions.subscribe()
    }

    /// The person opened or sent to `chat_id` on this device: take it over
    /// when its host can't host it (see module docs). Returns at once.
    pub fn on_chat_used(&self, chat_id: &str) {
        if !self.inner.workspace.is_provider_run_id(chat_id) {
            return;
        }
        let hosting = self.clone();
        let chat = chat_id.to_owned();
        self.inner.doc_host.spawn_worker(async move {
            hosting.consider(&chat).await;
            hosting.inner.decisions.send_modify(|n| *n += 1);
        });
    }

    /// Whether this engine can host provider-run chats at all: the
    /// `claude-code-cloud` harness is on.
    fn eligible_engine(&self) -> bool {
        self.inner
            .sessions
            .registry()
            .resolve(HarnessId::ClaudeCodeCloud)
            .is_ok_and(|harness| harness.provider_cloud().is_some())
    }

    /// Decide whether to take `chat_id` over (see [`Self::on_chat_used`]).
    async fn consider(&self, chat_id: &str) {
        if !self.eligible_engine() {
            return; // an older build, or the feature off here: hands off
        }
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
        let Some(reason) = self.why_not_hosting(chat_id, &host).await else {
            return; // an eligible host is online: it keeps the chat
        };
        if !self.can_host_here(&session).await {
            // A host that is only offline keeps the chat waiting for it. One
            // that can't run it, with no login for it here either: nobody can.
            if reason == TakeoverReason::Offline {
                crate::provider_run::show_queued(&self.inner.doc_host, chat_id);
            } else {
                self.show_sign_in_as(chat_id, &session);
            }
            return;
        }
        match workspace.claim_chat_host(chat_id, chrono::Utc::now() + CLAIM_LEAD) {
            Ok(true) => {
                tracing::info!(chat = %chat_id, from = %host, ?reason, "claiming the provider-run chat");
                lock(&self.inner.claims).insert(
                    chat_id.to_owned(),
                    Claim {
                        from: host,
                        reason,
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
    async fn why_not_hosting(&self, chat_id: &str, host: &str) -> Option<TakeoverReason> {
        let workspace = &self.inner.workspace;
        if workspace
            .chat_host_unavailable(chat_id)
            .is_some_and(|unavailable| unavailable.device_id == host)
        {
            return Some(TakeoverReason::CantHost);
        }
        let capable = workspace
            .read_devices()
            .unwrap_or_default()
            .iter()
            .any(|d| d.id == host && d.capabilities.iter().any(|c| c == PROVIDER_RUN_V1));
        if !capable {
            return Some(TakeoverReason::Ineligible);
        }
        if !self.presence().online(host).await {
            return Some(TakeoverReason::Offline);
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

    /// "Sign in as <email> to continue" (see module docs).
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
            // Until the room has answered this run, no host is known (and
            // `is_host` hosts nothing).
            let synced = workspace.registry_synced();
            let chats = workspace.read_chats().unwrap_or_default();
            let mut seen: HashMap<String, Option<String>> = HashMap::new();
            for chat in &chats {
                if !workspace.is_provider_run(chat) {
                    continue;
                }
                let host = if synced {
                    workspace
                        .is_host(&chat.id)
                        .then(|| me.clone())
                        .or_else(|| workspace.provider_chat_host(&chat.id))
                } else {
                    None
                };
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
                    // chat first seen hosted here (created here, or the room
                    // answering after a restart) runs what waited for that.
                    let claimed = lock(&self.inner.claims).remove(chat);
                    if !first && (before.is_some() || claimed.is_some()) {
                        self.became_host(chat, before, claimed);
                    } else if !first {
                        self.drain(chat);
                    }
                } else if hosted_before && !hosts_now {
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
            // The settle wait every takeover costs: devices that claimed at
            // the same moment converge within it, and `is_host` says yes only
            // once it has passed with this device still host, so the losing
            // claimant never starts work it then may not finish.
            tokio::time::sleep(PROVIDER_HOST_SETTLE + Duration::from_millis(50)).await;
            if !workspace.is_host(&chat) {
                return;
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
                Some(ProviderChatState::SignInAs { .. } | ProviderChatState::Queued)
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
                // The chat moved on meanwhile: whatever started here stops.
                hosting.demoted(&chat, workspace.provider_chat_host(&chat));
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
        let text = takeover_note(&name, claim.reason);
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

    /// Run `chat_id`'s waiting commands (this device hosts it).
    fn drain(&self, chat_id: &str) {
        let doc_host = self.inner.doc_host.clone();
        let chat = chat_id.to_owned();
        self.inner.doc_host.spawn_worker(async move {
            if let Ok(handle) = doc_host.open(&chat) {
                doc_host.drain_commands(&handle).await;
            }
        });
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
fn takeover_note(device: &str, reason: TakeoverReason) -> String {
    match reason {
        TakeoverReason::Offline => format!("Continued here, {device} was offline."),
        TakeoverReason::Ineligible => {
            format!("Continued here, {device} can't run Claude Code cloud chats.")
        }
        TakeoverReason::CantHost => format!("Continued here, {device} couldn't host this chat."),
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
            reason: HostUnavailableReason::Account,
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
            takeover_note("PC", TakeoverReason::Offline),
            "Continued here, PC was offline."
        );
        assert_eq!(
            takeover_note("PC", TakeoverReason::Ineligible),
            "Continued here, PC can't run Claude Code cloud chats."
        );
        assert!(takeover_note("PC", TakeoverReason::CantHost).starts_with("Continued here, PC "));
    }
}
