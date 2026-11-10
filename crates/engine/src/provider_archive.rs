//! Provider-run chats follow the person's archive and delete choices
//! (docs/design/provider-run-chats.md §6, "Archive and delete"; ticket #10).
//!
//! - **Archived at the provider** (claude.ai, the phone): the chat's host
//!   deletes the chat, from its next read of the session list
//!   ([`reconcile`]). Discovery never recreates it: the session is archived,
//!   and the tombstone beats its low-clock import. A chat the person keeps is
//!   kept instead, and shows "Archived on claude.ai" with a link
//!   ([`ProviderChatState::ArchivedOnProvider`]): one archived in Zeron, or
//!   one the person unarchived in Zeron after the session's last activity
//!   (the registry's `archived` clock says when, on every device). A device
//!   that sees the person unarchive a chat asks the provider at once
//!   ([`follow_chats`]).
//! - **Archived in Zeron:** Zeron only. The session stays active, so the
//!   chat can be unarchived and continued.
//! - **Deleted in Zeron:** the session is archived. Every device that knew
//!   the chat (the ones it has seen, kept across restarts in the
//!   [`ArchiveLedger`]) owes that archive once it sees the tombstone, even
//!   one that was off when the delete happened. What is owed is kept until it
//!   is done, and retried at start, after each list read and every few
//!   minutes; each try reads the session alone first, whatever its age. A
//!   list read also owes it for a deleted chat this device never knew.
//! - **Revived:** a session whose chat was deleted but that was active again
//!   well after the delete ([`REVIVE_MARGIN`], for clocks that disagree)
//!   gets its chat back on the device that read the list: the row with a
//!   real clock that beats the tombstone, hosted there, and its history
//!   replayed from the session.
//!
//! Only a device whose registry is current acts on the list or settles what
//! is owed, so a stale replica never deletes, archives or revives on old
//! facts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeron_harness::Harness;
use zeron_harness::provider_cloud::{ListedSession, ProviderCloudError};
use zeron_proto::provider_cloud::{ProviderChatState, chat_id_for, claude_session_url};
use zeron_proto::{HarnessId, ProviderSession};

use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// How much later than a chat's delete its session's last activity must be
/// for the chat to come back: the delete's clock is a device's, the
/// activity's the provider's, and they may disagree by this much.
pub(crate) const REVIVE_MARGIN: chrono::Duration = chrono::Duration::minutes(3);

/// How often owed archives are tried again while nothing else happens.
const RETRY_EVERY: Duration = Duration::from_secs(5 * 60);

const FILE_NAME: &str = "claude-cloud-archives.json";

/// One archive a delete still owes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owed {
    session: ProviderSession,
    /// The delete's clock, when known: activity well after it is a revive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ledger {
    /// The provider-run chats this device has seen, by chat id.
    #[serde(default)]
    known: BTreeMap<String, ProviderSession>,
    /// The archives deletes still owe, by chat id.
    #[serde(default)]
    owed: BTreeMap<String, Owed>,
}

/// The provider-run chats this device knows and the archives their deletes
/// still owe, in `{data_dir}/claude-cloud-archives.json` (see module docs).
pub(crate) struct ArchiveLedger {
    path: PathBuf,
    ledger: Mutex<Ledger>,
    /// Chats this device deleted because their session was archived at the
    /// provider: their delete owes nothing.
    deleted_here: Mutex<HashSet<String>>,
    /// One settling pass at a time.
    settling: tokio::sync::Mutex<()>,
    /// Settling was asked for since the running pass began.
    again: std::sync::atomic::AtomicBool,
}

impl ArchiveLedger {
    pub(crate) fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE_NAME);
        let ledger = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            ledger: Mutex::new(ledger),
            deleted_here: Mutex::default(),
            settling: tokio::sync::Mutex::new(()),
            again: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, ledger: &Ledger) {
        let write = || -> std::io::Result<()> {
            let parent = self.path.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(&mut temp, ledger)?;
            temp.persist(&self.path).map_err(|e| e.error)?;
            Ok(())
        };
        if let Err(error) = write() {
            tracing::warn!(%error, "couldn't save the Claude cloud archive ledger");
        }
    }

    fn note_deleted_here(&self, chat_id: &str) {
        self.deleted_here
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(chat_id.to_owned());
    }

    fn forget_deleted_here(&self, chat_id: &str) -> bool {
        self.deleted_here
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(chat_id)
    }

    /// Learn the provider-run chats live now, and turn the known ones the
    /// registry now holds deleted into owed archives. Whether anything is
    /// owed.
    fn observe(&self, workspace: &WorkspaceHost, live: &HashMap<String, ProviderSession>) -> bool {
        let mut ledger = self.lock();
        let mut changed = false;
        for (chat_id, session) in live {
            if ledger.known.get(chat_id) != Some(session) {
                ledger.known.insert(chat_id.clone(), session.clone());
                changed = true;
            }
            changed |= ledger.owed.remove(chat_id).is_some();
        }
        let gone: Vec<String> = ledger
            .known
            .keys()
            .filter(|id| !live.contains_key(*id) && workspace.chat_deleted(id))
            .cloned()
            .collect();
        for chat_id in gone {
            let Some(session) = ledger.known.remove(&chat_id) else {
                continue;
            };
            changed = true;
            if self.forget_deleted_here(&chat_id) {
                continue;
            }
            ledger.owed.insert(
                chat_id.clone(),
                Owed {
                    session,
                    deleted_at: workspace.chat_deleted_at(&chat_id),
                },
            );
        }
        if changed {
            self.save(&ledger);
        }
        !ledger.owed.is_empty()
    }

    /// Owe the archive of a deleted chat this device never knew (a list read
    /// found its session active).
    fn owe(&self, chat_id: &str, session: &ProviderSession, deleted_at: DateTime<Utc>) {
        let mut ledger = self.lock();
        if ledger.owed.contains_key(chat_id) {
            return;
        }
        ledger.owed.insert(
            chat_id.to_owned(),
            Owed {
                session: session.clone(),
                deleted_at: Some(deleted_at),
            },
        );
        self.save(&ledger);
    }

    fn settled(&self, chat_id: &str) {
        let mut ledger = self.lock();
        if ledger.owed.remove(chat_id).is_some() {
            self.save(&ledger);
        }
    }

    /// Try every owed archive once (see module docs). A chat live again, a
    /// session already archived or active since the delete (resumed
    /// elsewhere), or one this device's login can't act for, settles without
    /// an archive; a failure stays owed.
    async fn settle(&self, workspace: &WorkspaceHost, harness: &Arc<dyn Harness>) {
        // Asked again while a pass runs: that pass goes round once more.
        self.again.store(true, std::sync::atomic::Ordering::SeqCst);
        let Ok(_one) = self.settling.try_lock() else {
            return;
        };
        while self.again.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.settle_once(workspace, harness).await;
        }
    }

    async fn settle_once(&self, workspace: &WorkspaceHost, harness: &Arc<dyn Harness>) {
        let Some(cloud) = harness.provider_cloud() else {
            return;
        };
        if !workspace.registry_current() {
            return;
        }
        let owed: Vec<(String, Owed)> = self
            .lock()
            .owed
            .iter()
            .map(|(id, owed)| (id.clone(), owed.clone()))
            .collect();
        for (chat_id, owed) in owed {
            if workspace.chat(&chat_id).ok().flatten().is_some() {
                self.settled(&chat_id);
                continue;
            }
            match cloud.read_session(&owed.session).await {
                Ok(now) if now.archived => {
                    self.settled(&chat_id);
                    continue;
                }
                // Active since the delete: resumed elsewhere (a revive's
                // business once clearly so), not this delete's to archive.
                Ok(now) if owed.deleted_at.is_some_and(|deleted| now.last_event_at > deleted) => {
                    self.settled(&chat_id);
                    continue;
                }
                Ok(_) => {}
                Err(ProviderCloudError::AccountMismatch(_)) => {
                    // Not this device's account: a device with its login
                    // that knew the chat owes it too.
                    self.settled(&chat_id);
                    continue;
                }
                Err(error) => {
                    tracing::warn!(chat = %chat_id, %error, "couldn't read the session of a deleted chat; will try again");
                    continue;
                }
            }
            match cloud.archive_session(&owed.session).await {
                Ok(()) => {
                    tracing::info!(chat = %chat_id, "archived the Claude cloud session of a chat deleted in Zeron");
                    self.settled(&chat_id);
                }
                Err(error) => {
                    tracing::warn!(chat = %chat_id, %error, "couldn't archive the session of a deleted chat; will try again");
                }
            }
        }
    }
}

/// The ledger, when provider-run chats can be on in this engine.
fn ledger_of(sessions: &SessionsEngine) -> Option<Arc<crate::claude_cloud_sessions::ClaudeCloudSessions>> {
    sessions.registry().claude_cloud().cloned()
}

/// The harness, when provider-run chats are on.
fn cloud_harness(sessions: &SessionsEngine) -> Option<Arc<dyn Harness>> {
    sessions
        .registry()
        .resolve(HarnessId::ClaudeCodeCloud)
        .ok()
        .filter(|h| h.provider_cloud().is_some())
}

/// Settle what is owed, in the background.
fn settle_later(sessions: &SessionsEngine, doc_host: &DocHost) {
    let (Some(store), Some(harness), Some(workspace)) = (
        ledger_of(sessions),
        cloud_harness(sessions),
        doc_host.workspace().cloned(),
    ) else {
        return;
    };
    doc_host.spawn_worker(async move {
        store.archives().settle(&workspace, &harness).await;
    });
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
    listed: &[ListedSession],
) -> Vec<String> {
    let Some(store) = ledger_of(sessions) else {
        return Vec::new();
    };
    let archives = store.archives();
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
                let shown = doc_host
                    .open(&chat_id)
                    .is_ok_and(|h| is_archived_state(h.doc().provider_chat_state().as_ref()));
                // Unarchived in Zeron after the session last did anything:
                // the person kept it, maybe while this device was away.
                let kept_by_person = workspace
                    .chat_unarchived_at(&chat_id)
                    .is_some_and(|at| at > found.last_event_at);
                if chat.archived || kept_by_person {
                    // Kept, and says why it can't take messages.
                    mark_archived(doc_host, &chat_id, &found.session, true);
                } else if !shown {
                    archives.note_deleted_here(&chat_id);
                    end_run(sessions, doc_host, &chat_id);
                    match workspace.delete_chat(&chat_id) {
                        Ok(_) => doc_host.purge_chat(&chat_id),
                        Err(error) => {
                            archives.forget_deleted_here(&chat_id);
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
                if found.last_event_at > deleted_at + REVIVE_MARGIN {
                    // Resumed well after the delete: the chat comes back here.
                    let row = crate::provider_discovery::discovered_row(&me, found);
                    if let Err(error) = workspace.revive_chat(&row) {
                        tracing::warn!(chat = %chat_id, %error, "couldn't bring back the chat of a resumed session");
                        continue;
                    }
                    if let Ok(handle) = doc_host.open(&chat_id)
                        && let Err(error) = handle.doc().set_provider_run_session(&found.session)
                    {
                        tracing::warn!(chat = %chat_id, %error, "couldn't record the revived chat's session");
                    }
                    mark_archived(doc_host, &chat_id, &found.session, false);
                    revived.push(chat_id);
                } else if found.last_event_at <= deleted_at {
                    // Deleted in Zeron, the session still active: archive it.
                    archives.owe(&chat_id, &found.session, deleted_at);
                }
                // Activity just after the delete, within the clocks'
                // disagreement: neither, until it's clear.
            }
            _ => {}
        }
    }
    settle_later(sessions, doc_host);
    revived
}

/// What [`follow_chats`] saw of one provider-run chat.
struct Seen {
    session: ProviderSession,
    unarchived_at: Option<DateTime<Utc>>,
}

/// For as long as the engine runs, watch the provider-run chats (see module
/// docs): every one seen is remembered in the ledger, a known one deleted
/// owes its session's archive (settled at once, and retried), this device's
/// run of it ends, and one the person unarchives has its session read, to
/// show "Archived on claude.ai" if it is.
pub(crate) fn follow_chats(sessions: &SessionsEngine, doc_host: &DocHost) {
    let Some(workspace) = doc_host.workspace().cloned() else {
        return;
    };
    let mut chats = workspace.watch_chats();
    let (sessions, host) = (sessions.clone(), doc_host.clone());
    doc_host.spawn_worker(async move {
        // Unarchives from here on are this run's to check (one earlier was
        // checked by whoever saw it, or is caught by the host's list read).
        let started = Utc::now() - chrono::Duration::seconds(1);
        let mut seen: HashMap<String, Seen> = HashMap::new();
        loop {
            let rows = chats.borrow_and_update().clone();
            let now: HashMap<String, Seen> = rows
                .into_iter()
                .filter_map(|chat| {
                    let session = chat.provider_session?;
                    let unarchived_at = workspace.chat_unarchived_at(&chat.id);
                    Some((
                        chat.id,
                        Seen {
                            session,
                            unarchived_at,
                        },
                    ))
                })
                .collect();
            for chat_id in seen.keys() {
                if !now.contains_key(chat_id) && workspace.chat_deleted(chat_id) {
                    end_run(&sessions, &host, chat_id);
                }
            }
            if let Some(harness) = cloud_harness(&sessions) {
                for (chat_id, is) in &now {
                    let unarchived = is.unarchived_at.is_some_and(|at| at >= started)
                        && seen.get(chat_id).map(|was| was.unarchived_at) != Some(is.unarchived_at);
                    if unarchived {
                        check_unarchived(&host, harness.clone(), chat_id, &is.session);
                    }
                }
            }
            if let Some(store) = ledger_of(&sessions) {
                let live: HashMap<String, ProviderSession> = now
                    .iter()
                    .map(|(id, is)| (id.clone(), is.session.clone()))
                    .collect();
                if store.archives().observe(&workspace, &live) {
                    settle_later(&sessions, &host);
                }
            }
            seen = now;
            tokio::select! {
                changed = chats.changed() => if changed.is_err() { return },
                () = tokio::time::sleep(RETRY_EVERY) => settle_later(&sessions, &host),
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
        match cloud.read_session(&session).await {
            Ok(now) => mark_archived(&host, &chat, &session, now.archived),
            Err(error) => {
                tracing::warn!(%chat, %error, "couldn't check whether the chat's session is archived")
            }
        }
    });
}
