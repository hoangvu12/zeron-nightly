//! Provider-run chats follow the person's archive and delete choices
//! (docs/design/provider-run-chats.md §6, "Archive and delete"; ticket #10).
//!
//! - **Archived at the provider** (claude.ai, the phone): the chat's host
//!   deletes the chat, from its next read of the session list
//!   ([`reconcile`]). Discovery never recreates it: the session is archived,
//!   and the tombstone beats its low-clock import. A chat the person had
//!   archived in Zeron is kept instead, and shows "Archived on claude.ai"
//!   with a link ([`ProviderChatState::ArchivedOnProvider`]), as does one the
//!   person unarchives once the session was archived ([`follow_chats`]).
//! - **Archived in Zeron:** Zeron only. The session stays active, so the
//!   chat can be unarchived and continued.
//! - **Deleted in Zeron:** the host archives the session as soon as it sees
//!   the tombstone ([`follow_chats`]). A delete the host missed (it was off)
//!   is caught by the next list read on any device: a session still active
//!   whose chat is deleted, with no activity since the delete, is archived.
//! - **Revived:** a session whose chat was deleted but that was active again
//!   since the delete (resumed on claude.ai) gets its chat back on the
//!   device that read the list: the row with a real clock that beats the
//!   tombstone, hosted there, and its history replayed from the session.
//!
//! Only a device whose registry is current acts on the list, so a stale
//! replica never deletes, archives or revives on old facts.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use zeron_harness::Harness;
use zeron_harness::provider_cloud::ListedSession;
use zeron_proto::provider_cloud::{ProviderChatState, chat_id_for, claude_session_url};
use zeron_proto::{HarnessId, ProviderSession};

use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// The chats this device deleted because their session was archived at the
/// provider: their tombstone archives nothing (it already is).
#[derive(Clone, Default)]
pub(crate) struct DeletedForArchive(Arc<Mutex<HashSet<String>>>);

impl DeletedForArchive {
    fn note(&self, chat_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(chat_id.to_owned());
    }

    /// Whether this device deleted `chat_id` for its archived session
    /// (forgotten once asked).
    fn take(&self, chat_id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(chat_id)
    }
}

/// What the chat shows when its session is archived at the provider.
fn archived_state(session: &ProviderSession) -> ProviderChatState {
    ProviderChatState::ArchivedOnProvider {
        url: claude_session_url(&session.session_id),
    }
}

fn is_archived_state(state: Option<&ProviderChatState>) -> bool {
    matches!(state, Some(ProviderChatState::ArchivedOnProvider { .. }))
}

/// Record (`archived`) or clear "Archived on claude.ai" in the chat's
/// document. Clearing leaves any other state alone.
fn mark_archived(doc_host: &DocHost, chat_id: &str, session: &ProviderSession, archived: bool) {
    let Ok(handle) = doc_host.open(chat_id) else {
        return;
    };
    let doc = handle.doc();
    let current = doc.provider_chat_state();
    let next = if archived {
        Some(archived_state(session))
    } else if is_archived_state(current.as_ref()) {
        None
    } else {
        return;
    };
    if current == next {
        return;
    }
    if let Err(error) = doc.set_provider_chat_state(next.as_ref()) {
        tracing::warn!(chat = %chat_id, %error, "couldn't record the session's archived state");
    }
}

/// Archive `session` at the provider, in the background.
fn archive_later(doc_host: &DocHost, harness: Arc<dyn Harness>, session: ProviderSession) {
    doc_host.spawn_worker(async move {
        let Some(cloud) = harness.provider_cloud() else {
            return;
        };
        match cloud.archive_session(&session).await {
            Ok(()) => tracing::info!("archived the Claude cloud session of a chat deleted in Zeron"),
            Err(error) => {
                tracing::warn!(%error, "couldn't archive the Claude cloud session of a deleted chat")
            }
        }
    });
}

/// End this device's run of a chat that is gone (closing the stream never
/// interrupts the session).
fn end_run(sessions: &SessionsEngine, doc_host: &DocHost, chat_id: &str) {
    if !sessions.has_run(chat_id) {
        return;
    }
    let (sessions, chat) = (sessions.clone(), chat_id.to_owned());
    doc_host.spawn_worker(async move {
        if let Err(error) = sessions.terminate(&chat).await {
            tracing::warn!(%chat, %error, "couldn't end the run of a deleted chat");
        }
    });
}

/// Act on one read of the session list (see module docs); the caller has
/// checked the registry is current. Returns the chats revived here, whose
/// history the caller brings in as their host.
pub(crate) fn reconcile(
    sessions: &SessionsEngine,
    doc_host: &DocHost,
    workspace: &WorkspaceHost,
    harness: &Arc<dyn Harness>,
    deleted_for_archive: &DeletedForArchive,
    listed: &[ListedSession],
) -> Vec<String> {
    let me = workspace.device_id().to_owned();
    let mut revived = Vec::new();
    for found in listed {
        let chat_id = chat_id_for(&found.session);
        let row = match workspace.chat(&chat_id) {
            Ok(row) => row,
            // Unreadable: act on nothing.
            Err(_) => continue,
        };
        match (found.archived, row) {
            // Archived at the provider, and this device hosts the chat.
            (true, Some(chat)) if chat.device_id == me => {
                let kept = doc_host
                    .open(&chat_id)
                    .is_ok_and(|h| is_archived_state(h.doc().provider_chat_state().as_ref()));
                if chat.archived {
                    // Archived in Zeron too: kept, and says why it can't
                    // take messages once unarchived.
                    mark_archived(doc_host, &chat_id, &found.session, true);
                } else if !kept {
                    deleted_for_archive.note(&chat_id);
                    end_run(sessions, doc_host, &chat_id);
                    match workspace.delete_chat(&chat_id) {
                        Ok(_) => doc_host.purge_chat(&chat_id),
                        Err(error) => {
                            deleted_for_archive.take(&chat_id);
                            tracing::warn!(chat = %chat_id, %error, "couldn't delete the chat of an archived session");
                        }
                    }
                }
            }
            // Active, with a chat: it no longer reads as archived.
            (false, Some(chat)) if chat.device_id == me => {
                mark_archived(doc_host, &chat_id, &found.session, false);
            }
            // Active, and its chat was deleted in Zeron.
            (false, None) => {
                let Some(deleted_at) = workspace.chat_deleted_at(&chat_id) else {
                    continue;
                };
                if found.last_event_at > deleted_at {
                    // Resumed since the delete: the chat comes back here.
                    let row = crate::provider_discovery::discovered_row(&me, found);
                    if let Err(error) = workspace.revive_chat(&row) {
                        tracing::warn!(chat = %chat_id, %error, "couldn't bring back the chat of a resumed session");
                        continue;
                    }
                    if let Ok(handle) = doc_host.open(&chat_id) {
                        let doc = handle.doc();
                        if let Err(error) = doc.set_provider_run_session(&found.session) {
                            tracing::warn!(chat = %chat_id, %error, "couldn't record the revived chat's session");
                        }
                        if is_archived_state(doc.provider_chat_state().as_ref()) {
                            let _ = doc.set_provider_chat_state(None);
                        }
                    }
                    revived.push(chat_id);
                } else {
                    // Deleted in Zeron while its host was away: archive it.
                    archive_later(doc_host, harness.clone(), found.session.clone());
                }
            }
            _ => {}
        }
    }
    revived
}

/// What [`follow_chats`] knows of one provider-run chat this device hosts.
struct Hosted {
    session: ProviderSession,
    archived: bool,
}

/// For as long as the engine runs, watch the provider-run chats this device
/// hosts (see module docs): one deleted in Zeron has its session archived
/// at once (and its run ended); one unarchived in Zeron asks the provider
/// whether its session is archived, and shows "Archived on claude.ai" if it
/// is. Does nothing while provider-run chats are off.
pub(crate) fn follow_chats(sessions: &SessionsEngine, doc_host: &DocHost) {
    let Some(workspace) = doc_host.workspace().cloned() else {
        return;
    };
    let mut chats = workspace.watch_chats();
    let (sessions, host) = (sessions.clone(), doc_host.clone());
    doc_host.spawn_worker(async move {
        let mut known: HashMap<String, Hosted> = HashMap::new();
        loop {
            let rows = chats.borrow_and_update().clone();
            let now: HashMap<String, Hosted> = rows
                .into_iter()
                .filter(|chat| chat.device_id == host.device_id())
                .filter_map(|chat| {
                    let session = chat.provider_session?;
                    Some((
                        chat.id,
                        Hosted {
                            session,
                            archived: chat.archived,
                        },
                    ))
                })
                .collect();
            let harness = sessions
                .registry()
                .resolve(HarnessId::ClaudeCodeCloud)
                .ok()
                .filter(|h| h.provider_cloud().is_some());
            if let Some(harness) = harness {
                let deleted_for_archive = sessions
                    .registry()
                    .claude_cloud()
                    .map(|store| store.discovery().deleted_for_archive.clone())
                    .unwrap_or_default();
                for (chat_id, was) in &known {
                    if now.contains_key(chat_id) || !workspace.chat_deleted(chat_id) {
                        continue;
                    }
                    end_run(&sessions, &host, chat_id);
                    if !deleted_for_archive.take(chat_id) {
                        archive_later(&host, harness.clone(), was.session.clone());
                    }
                }
                for (chat_id, is) in &now {
                    let unarchived = known
                        .get(chat_id)
                        .is_some_and(|was| was.archived && !is.archived);
                    if unarchived {
                        check_unarchived(&host, harness.clone(), chat_id, &is.session);
                    }
                }
            }
            known = now;
            if chats.changed().await.is_err() {
                return;
            }
        }
    });
}

/// A chat unarchived in Zeron: whether its session is archived at the
/// provider decides whether it shows "Archived on claude.ai".
fn check_unarchived(
    doc_host: &DocHost,
    harness: Arc<dyn Harness>,
    chat_id: &str,
    session: &ProviderSession,
) {
    let (host, chat, session) = (doc_host.clone(), chat_id.to_owned(), session.clone());
    doc_host.spawn_worker(async move {
        let Some(cloud) = harness.provider_cloud() else {
            return;
        };
        match cloud.session_archived(&session).await {
            Ok(archived) => mark_archived(&host, &chat, &session, archived),
            Err(error) => {
                tracing::warn!(%chat, %error, "couldn't check whether the chat's session is archived")
            }
        }
    });
}
