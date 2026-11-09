//! Provider-run chats in the engine (docs/design/provider-run-chats.md):
//! a Zeron chat tied to one provider cloud session (Claude Code cloud), whose
//! host device mirrors the session and sends to it.
//!
//! - [`create_chat`]: `createChat` with harness `claude-code-cloud` and no
//!   project. The session is created synchronously on this device (never
//!   queued, never retried), then the row is written: chat id = UUIDv5 of
//!   `claude:<session id>`, `cwd = claude-cloud://<session id>`,
//!   `providerSession` on the row and in the chat document's metadata. The
//!   first prompt rode the create request; it is queued as the chat's first
//!   run under the same id, so the transcript shows it and the run mirrors
//!   the reply (the harness doesn't send it twice).
//! - [`bind_run`]: before every fresh dispatch, the harness is told which
//!   session and account the run acts for. A chat without a provider session
//!   refuses to run.
//! - [`follow_sign_in_states`]: what the harness learns about its logins
//!   ("Sign in again", "Sign in as <email> to continue") is recorded in the
//!   chat document of every chat this device hosts for that account
//!   (`providerState`), and cleared once a request for it works again.
//!   Publishing `hostUnavailable` for a chat whose account has no login here
//!   belongs with hosting and takeover.

use std::collections::HashMap;

use zeron_doc::{SessionCommandPayload, SessionDoc};
use zeron_harness::provider_cloud::{
    CreateSession, ProviderChatState, ProviderCloudError, ProviderCloudStatus, ProviderResume,
    RunBinding,
};
use zeron_proto::{Chat, ChatConfig, ChildKind, HarnessId, ProviderSession, RunRequest};

use crate::EngineError;
use crate::doc_host::DocHost;
use crate::registry::HarnessRegistry;
use crate::workspace_host::WorkspaceHost;

/// A `createChat` for a provider-run chat.
#[derive(Debug, Clone)]
pub(crate) struct NewProviderChat {
    /// The device the request named, if any: it must be this one (the
    /// device the person is on creates the session).
    pub device_id: Option<String>,
    pub space_id: Option<String>,
    pub config: ChatConfig,
    /// The first message (required: a session is created with it).
    pub prompt: Option<String>,
    /// The first message's id (its transcript entry and event uuid); minted
    /// when absent.
    pub message_id: Option<String>,
    pub title: Option<String>,
    pub parent_chat_id: Option<String>,
    pub child_kind: Option<ChildKind>,
}

/// What [`create_chat`] made.
#[derive(Debug, Clone)]
pub(crate) struct CreatedProviderChat {
    pub chat_id: String,
    pub device_id: String,
    pub cwd: String,
    pub message_id: String,
    pub session: ProviderSession,
}

fn refused(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

/// Create the provider session, then the chat for it (see module docs).
pub(crate) async fn create_chat(
    registry: &HarnessRegistry,
    workspace: &WorkspaceHost,
    doc_host: &DocHost,
    new: NewProviderChat,
) -> Result<CreatedProviderChat, EngineError> {
    let device_id = workspace.device_id().to_owned();
    if new.space_id.is_some() {
        return Err(refused(
            "Provider-run chats start with no project for now.",
        ));
    }
    if new.device_id.as_deref().is_some_and(|d| d != device_id) {
        return Err(refused(
            "A provider-run chat is created on the device you're using.",
        ));
    }
    let prompt = new
        .prompt
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| refused("A provider-run chat starts with its first message."))?;
    let harness = registry
        .resolve(HarnessId::ClaudeCodeCloud)
        .map_err(|_| refused("Claude cloud sessions are off on this device."))?;
    let cloud = harness
        .provider_cloud()
        .ok_or_else(|| refused("Claude cloud sessions are off on this device."))?;
    let message_id = new
        .message_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let title = new
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| zeron_proto::provider_cloud::title_from_prompt(&prompt));
    // Not idempotent: exactly one attempt. A failure here created nothing
    // Zeron knows about.
    let session = cloud
        .create_session(CreateSession {
            prompt: prompt.clone(),
            prompt_uuid: zeron_proto::provider_cloud::event_uuid(&message_id),
            model: new.config.model.clone(),
            title: Some(title.clone()),
        })
        .await
        .map_err(|error| match error {
            ProviderCloudError::Provider { message, .. } => refused(message),
            other => refused(other.to_string()),
        })?;
    let chat_id = zeron_proto::provider_cloud::chat_id_for(&session);
    let cwd = zeron_proto::provider_cloud::claude_cwd(&session.session_id);
    let recorded = |error: EngineError| {
        refused(format!(
            "The Claude cloud session was created, but Zeron couldn't record its chat: {error}"
        ))
    };
    workspace
        .create_provider_run_chat(
            &chat_id,
            new.config.clone(),
            cwd.clone(),
            new.parent_chat_id,
            new.child_kind,
            &session,
        )
        .map_err(recorded)?;
    // A row that already existed (the same session, imported) gets the
    // field now if it lacked it; one naming another session is refused.
    if !workspace
        .set_chat_provider_session(&chat_id, &session)
        .map_err(recorded)?
    {
        return Err(recorded(refused("the chat names another provider session")));
    }
    workspace.rename_chat(&chat_id, &title).map_err(recorded)?;
    let handle = doc_host.open(&chat_id).map_err(recorded)?;
    handle
        .doc()
        .set_provider_run_session(&session)
        .map_err(|e| recorded(e.into()))?;
    doc_host
        .queue_command(
            &chat_id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    prompt,
                    harness: Some(HarnessId::ClaudeCodeCloud),
                    model: new.config.model.clone(),
                    reasoning: new.config.reasoning,
                    model_options: new.config.model_options.clone(),
                    cwd: cwd.clone(),
                    sandbox: new.config.sandbox,
                    auto_approve: false,
                    access: new.config.access,
                    plan: new.config.plan,
                    resume: None,
                    attachments: Vec::new(),
                    worktree: None,
                    mcp: None,
                    extra_mcp: Vec::new(),
                    env: Default::default(),
                },
                message_id: message_id.clone(),
            },
        )
        .map_err(recorded)?;
    Ok(CreatedProviderChat {
        chat_id,
        device_id,
        cwd,
        message_id,
        session,
    })
}

/// The provider session a chat is tied to: its row's, else the copy in its
/// document (a row revived by an older engine loses the field).
pub(crate) fn chat_session(
    workspace: Option<&WorkspaceHost>,
    doc_host: &DocHost,
    chat_id: &str,
) -> Option<ProviderSession> {
    workspace
        .and_then(|ws| ws.chat(chat_id).ok().flatten())
        .and_then(|chat| chat.provider_session)
        .or_else(|| doc_host.open(chat_id).ok()?.doc().provider_run_session())
}

/// Tie the chat's next fresh run to its provider session, the opening
/// prompt traveling under `message_id`'s event uuid. Refuses a chat with no
/// provider session (a copy made by an older client, say).
pub(crate) fn bind_run(
    registry: &HarnessRegistry,
    workspace: Option<&WorkspaceHost>,
    doc_host: &DocHost,
    harness: HarnessId,
    chat_id: &str,
    message_id: Option<&str>,
) -> Result<(), EngineError> {
    let session = chat_session(workspace, doc_host, chat_id)
        .ok_or_else(|| refused("This chat has no Claude cloud session, so it can't run."))?;
    let resolved = registry
        .resolve(harness)
        .map_err(|_| refused("Claude cloud sessions are off on this device."))?;
    let cloud = resolved
        .provider_cloud()
        .ok_or_else(|| refused("Claude cloud sessions are off on this device."))?;
    let resume = doc_host
        .open(chat_id)
        .map(|handle| resume_point(handle.doc()))
        .map_err(|e| refused(format!("This chat's transcript couldn't be read: {e}")))?;
    cloud.bind_run(RunBinding {
        session,
        prompt_uuid: message_id.map(zeron_proto::provider_cloud::event_uuid),
        resume: Some(resume),
    });
    Ok(())
}

/// Where a fresh run of a provider-run chat picks up, from its document
/// (docs/design/provider-run-chats.md §5, "Replay watermark"): replay from
/// the lower of the watermark and just before the oldest prompt still open,
/// and write over again a turn that was cut off mid-way (from just before
/// the first event that can have written to its entry).
pub(crate) fn resume_point(doc: &SessionDoc) -> ProviderResume {
    let watermark = doc.provider_watermark();
    let mirrored_through = doc
        .provider_turn()
        .filter(|turn| doc.entry_index(&turn.entry_id).is_some())
        .map_or(watermark, |turn| {
            turn.from_seq.saturating_sub(1).min(watermark)
        });
    let open = doc.provider_open_prompts();
    let from_sequence_num = open
        .values()
        .map(|seq| seq.saturating_sub(1))
        .fold(mirrored_through, u64::min);
    ProviderResume {
        from_sequence_num,
        mirrored_through,
        watermark,
        open_prompts: open.into_keys().collect(),
    }
}

/// Record the harness's sign-in states in the chats this device hosts (see
/// module docs), for as long as the engine runs. Nothing to follow when
/// provider-run chats are off.
pub(crate) fn follow_sign_in_states(registry: &HarnessRegistry, doc_host: &DocHost) {
    let Ok(harness) = registry.resolve(HarnessId::ClaudeCodeCloud) else {
        return;
    };
    let Some(cloud) = harness.provider_cloud() else {
        return;
    };
    let Some(workspace) = doc_host.workspace().cloned() else {
        return;
    };
    let mut status = cloud.status();
    let mut chats = workspace.watch_chats();
    let host = doc_host.clone();
    doc_host.spawn_worker(async move {
        // What each chat's document was last given, so a chat-list change
        // only opens the documents whose state moves.
        let mut written: HashMap<String, Option<ProviderChatState>> = HashMap::new();
        let mut disabled = None;
        loop {
            let current = status.borrow_and_update().clone();
            let rows = chats.borrow_and_update().clone();
            if current.disabled != disabled {
                if let Some(reason) = &current.disabled {
                    tracing::error!(reason, "provider-run chats turned off");
                }
                disabled = current.disabled.clone();
            }
            record_states(&host, &current, &rows, &mut written);
            tokio::select! {
                changed = status.changed() => if changed.is_err() { return },
                changed = chats.changed() => if changed.is_err() { return },
            }
        }
    });
}

/// One pass of [`follow_sign_in_states`]: each chat hosted here whose
/// account the harness has an answer for gets that answer.
fn record_states(
    host: &DocHost,
    status: &ProviderCloudStatus,
    chats: &[Chat],
    written: &mut HashMap<String, Option<ProviderChatState>>,
) {
    for chat in chats {
        if chat.device_id != host.device_id() {
            continue;
        }
        let Some(session) = &chat.provider_session else {
            continue;
        };
        let Some(state) = status.accounts.get(&session.account_uuid) else {
            continue;
        };
        if written.get(&chat.id) == Some(state) {
            continue;
        }
        let recorded = host
            .open(&chat.id)
            .and_then(|handle| Ok(handle.doc().set_provider_chat_state(state.as_ref())?));
        match recorded {
            Ok(()) => {
                written.insert(chat.id.clone(), state.clone());
            }
            Err(error) => {
                tracing::warn!(chat = %chat.id, %error, "couldn't record the chat's sign-in state")
            }
        }
    }
}

/// A provider-run chat's run in the engine's event loop: what the mirror
/// bookkeeping (docs/design/provider-run-chats.md §5) needs between events.
/// The loop itself applies each [`zeron_proto::AgentEvent::ProviderEvent`]
/// (see `drive_run`).
#[derive(Debug, Default)]
pub(crate) struct MirrorState {
    /// The replay watermark, as last committed.
    pub watermark: u64,
    /// The first provider event that can have written the current segment.
    pub segment_floor: u64,
    /// The entry the document's `providerTurn` names.
    pub turn_entry: Option<String>,
    /// The entry already stamped with the provider event it starts from.
    pub stamped: Option<String>,
    /// A cut-off entry being written over again holds its writes until a
    /// marker at or past this sequence number (what it showed is replayed).
    pub catch_up: Option<u64>,
    /// A person's message on its way to the transcript: (uuid, seq).
    pub pending_user: Option<(String, u64)>,
}

impl MirrorState {
    pub fn new(doc: &SessionDoc) -> Self {
        let watermark = doc.provider_watermark();
        Self {
            watermark,
            segment_floor: watermark + 1,
            turn_entry: doc.provider_turn().map(|turn| turn.entry_id),
            ..Self::default()
        }
    }

    /// The entry of a turn that was cut off mid-way (a restart, a dropped
    /// run, another host's), to be written over again from the replay:
    /// (entry id, position). [`resume_point`] replays from just before its
    /// first event.
    pub fn rebuild_target(&mut self, doc: &SessionDoc) -> Option<(String, usize)> {
        let turn = doc.provider_turn()?;
        let index = doc.entry_index(&turn.entry_id)?;
        self.segment_floor = turn.from_seq;
        self.stamped = None;
        self.turn_entry = Some(turn.entry_id.clone());
        self.catch_up = (turn.from_seq <= self.watermark).then_some(self.watermark);
        Some((turn.entry_id, index))
    }
}
