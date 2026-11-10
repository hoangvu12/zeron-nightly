//! Sessions from claude.ai in the sidebar (docs/design/provider-run-chats.md
//! §6, "Session list"): every non-archived session of each enabled account
//! with activity in the last 7 days becomes a provider-run chat, wherever it
//! was started, with its history.
//!
//! - **When.** Only while the sidebar is visible (the app says so on each
//!   `DiscoverProviderSessions` call) and at most once a minute: a call
//!   inside the minute answers from the last read. The engine never reads
//!   the list on its own.
//! - **Rows.** A session with no chat gets one through the low-clock upsert
//!   ([`WorkspaceHost::upsert_chat_at`], clocked at the session's
//!   `created_at`): it creates a truly missing row and loses every field to
//!   any real write and the row to a delete's tombstone, so a device with a
//!   stale registry can't move a chat or overwrite its fields. A chat that
//!   exists, or was deleted, is never written by the import. Two devices
//!   importing one session write the same chat id (UUIDv5 of the session)
//!   and converge on one row.
//! - **Archive, delete and revive** ([`crate::provider_archive::reconcile`],
//!   once the registry is current): a session archived at the provider
//!   deletes its chat; a chat deleted in Zeron whose session is still active
//!   archives it, unless the session was active again since the delete, which
//!   brings the chat back with its history.
//! - **History.** A chat this device hosts whose transcript is still empty
//!   is claimed (its importer is its default host: a host-field-only write
//!   with a real clock, so a later stale low-clock import can't tie-break
//!   it away) and resumed as its host ([`provider_run::resume_as_host`]),
//!   which replays the session's history from the start and mirrors what it
//!   does next; the run's idle reaper ends the stream once nobody uses it.
//!   Only once the registry is current, so a stale replica doesn't start
//!   mirroring a chat another device hosts.
//! - **Overlay.** Title, activity (working, waiting for you, idle) and the
//!   last activity time come back to the app from the latest read and are
//!   laid over the chat there; they are never written to the row.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use zeron_harness::provider_cloud::{ListedSession, SessionActivity};
use zeron_proto::{AccessMode, Chat, ChatConfig, HarnessId, SandboxLevel};

use crate::doc_host::DocHost;
use crate::provider_run;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// The shortest time between two reads of the session list (design §7).
pub const LIST_INTERVAL: Duration = Duration::from_secs(60);

/// How far back the sidebar shows sessions, by last activity.
pub fn window() -> chrono::Duration {
    chrono::Duration::days(7)
}

/// One session as the sidebar shows it over its chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionOverlay {
    pub chat_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub activity: SessionActivity,
    pub last_event_at: DateTime<Utc>,
}

/// `DiscoverProviderSessions` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryReply {
    pub sessions: Vec<ProviderSessionOverlay>,
    /// When the list was last read (`None`: never, this run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
    /// Why the last read failed, for people (never a credential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    /// When the list was last read (attempted), on the monotonic clock.
    last_read: Option<Instant>,
    reply: DiscoveryReply,
}

/// The device's discovery: its throttle and its latest read.
pub struct ProviderDiscovery {
    /// Held across a read, so concurrent calls share one.
    state: tokio::sync::Mutex<State>,
    /// [`LIST_INTERVAL`], shortened only by tests.
    interval: std::sync::Mutex<Duration>,
}

impl Default for ProviderDiscovery {
    fn default() -> Self {
        Self {
            state: Default::default(),
            interval: std::sync::Mutex::new(LIST_INTERVAL),
        }
    }
}

/// Whether a read is due at `now`, the last having been at `last`.
pub(crate) fn read_due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= interval)
}

impl ProviderDiscovery {
    /// Test seam: read the list as often as every `interval`.
    pub(crate) fn set_interval(&self, interval: Duration) {
        *self
            .interval
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = interval;
    }

    fn interval(&self) -> Duration {
        *self
            .interval
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The sidebar asks (see module docs). `visible`: whether the sidebar is
    /// on screen now; a hidden sidebar never causes a read.
    pub(crate) async fn refresh(
        &self,
        sessions: &SessionsEngine,
        doc_host: &DocHost,
        visible: bool,
    ) -> DiscoveryReply {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if !visible || !read_due(state.last_read, now, self.interval()) {
            return state.reply.clone();
        }
        let Ok(harness) = sessions.registry().resolve(HarnessId::ClaudeCodeCloud) else {
            return DiscoveryReply::default();
        };
        let Some(cloud) = harness.provider_cloud() else {
            return DiscoveryReply::default();
        };
        let Some(workspace) = doc_host.workspace().cloned() else {
            return DiscoveryReply::default();
        };
        // Spent whatever the outcome: nothing reads in a loop.
        state.last_read = Some(now);
        let read_at = Utc::now();
        match cloud.list_sessions(read_at - window()).await {
            Ok(listed) => {
                let revived = if workspace.registry_current() {
                    crate::provider_archive::reconcile(sessions, doc_host, &workspace, &listed)
                } else {
                    Vec::new()
                };
                let live: Vec<ListedSession> = listed.into_iter().filter(|s| !s.archived).collect();
                import(&workspace, &live);
                mirror_new(sessions, doc_host, &workspace, &live, revived);
                state.reply = DiscoveryReply {
                    sessions: live.iter().map(overlay).collect(),
                    read_at: Some(read_at),
                    error: None,
                };
            }
            Err(error) => {
                tracing::warn!(%error, "couldn't read the Claude cloud session list");
                state.reply.error = Some(error.to_string());
            }
        }
        state.reply.clone()
    }
}

fn overlay(listed: &ListedSession) -> ProviderSessionOverlay {
    ProviderSessionOverlay {
        chat_id: zeron_proto::provider_cloud::chat_id_for(&listed.session),
        session_id: listed.session.session_id.clone(),
        title: listed.title.clone(),
        activity: listed.activity,
        last_event_at: listed.last_event_at,
    }
}

/// The row a discovered session's chat starts as. Its prompts are asked,
/// never answered by an access mode the person didn't pick.
pub(crate) fn discovered_row(device_id: &str, listed: &ListedSession) -> Chat {
    let session_id = &listed.session.session_id;
    Chat {
        id: zeron_proto::provider_cloud::chat_id_for(&listed.session),
        device_id: device_id.to_owned(),
        title: listed.title.clone(),
        archived: false,
        cwd: Some(zeron_proto::provider_cloud::claude_cwd(session_id)),
        branch: None,
        checkout_id: None,
        source_context: None,
        config: Some(ChatConfig {
            harness: HarnessId::ClaudeCodeCloud,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            access: AccessMode::Supervised,
            plan: false,
        }),
        last_message_preview: None,
        last_message_at: None,
        created_at: listed.created_at,
        harness_session_id: None,
        harness_session_cwd: None,
        harness_session_harness: None,
        space_id: None,
        last_seen_at: None,
        room_gen: Some(2),
        parent_chat_id: None,
        child_kind: None,
        sleep: None,
        provider_session: Some(listed.session.clone()),
    }
}

/// Create the rows of sessions with no chat (low clock; see module docs).
fn import(workspace: &WorkspaceHost, listed: &[ListedSession]) {
    for session in listed {
        let row = discovered_row(workspace.device_id(), session);
        let known = match workspace.chat(&row.id) {
            Ok(found) => found.is_some(),
            // Unreadable: write nothing.
            Err(_) => true,
        };
        if known || workspace.chat_deleted(&row.id) {
            continue;
        }
        if let Err(error) = workspace.upsert_chat_at(&row, session.created_at) {
            tracing::warn!(chat = %row.id, %error, "couldn't import the Claude cloud session");
        }
    }
}

/// Bring in the history of listed sessions this device hosts whose chats
/// have none yet, and of chats `revived` here (see module docs).
fn mirror_new(
    sessions: &SessionsEngine,
    doc_host: &DocHost,
    workspace: &WorkspaceHost,
    listed: &[ListedSession],
    revived: Vec<String>,
) {
    if !workspace.registry_current() {
        return;
    }
    let mut chats = revived;
    for session in listed {
        let chat_id = zeron_proto::provider_cloud::chat_id_for(&session.session);
        let hosted = workspace.chat(&chat_id).ok().flatten().is_some_and(|chat| {
            chat.device_id == workspace.device_id()
                && chat.provider_session.as_ref() == Some(&session.session)
        });
        if !hosted || sessions.has_run(&chat_id) || chats.contains(&chat_id) {
            continue;
        }
        let Ok(handle) = doc_host.open(&chat_id) else {
            continue;
        };
        let doc = handle.doc();
        let empty = doc.read_entries().is_ok_and(|entries| entries.is_empty())
            && doc.provider_watermark() == 0;
        if !empty {
            continue;
        }
        if let Err(error) = doc.set_provider_run_session(&session.session) {
            tracing::warn!(chat = %chat_id, %error, "couldn't record the chat's provider session");
            continue;
        }
        // The importer is the chat's default host: it claims the host field
        // with a real clock, so a stale device's low-clock import (which
        // would otherwise tie on the clock and win by device id) can't move
        // the chat it already mirrors.
        if let Err(error) = workspace.claim_chat_host(&chat_id, Utc::now()) {
            tracing::warn!(chat = %chat_id, %error, "couldn't claim the imported chat");
            continue;
        }
        chats.push(chat_id);
    }
    if chats.is_empty() {
        return;
    }
    let (sessions, host) = (sessions.clone(), doc_host.clone());
    doc_host.spawn_worker(async move {
        for chat_id in chats {
            if let Err(error) = provider_run::resume_as_host(&sessions, &host, &chat_id).await {
                tracing::warn!(chat = %chat_id, %error, "couldn't bring in the session's history");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_is_due_once_a_minute() {
        let start = Instant::now();
        let every = LIST_INTERVAL;
        assert!(read_due(None, start, every));
        assert!(!read_due(Some(start), start, every));
        assert!(!read_due(
            Some(start),
            start + Duration::from_secs(59),
            every
        ));
        assert!(read_due(
            Some(start),
            start + Duration::from_secs(60),
            every
        ));
    }
}
