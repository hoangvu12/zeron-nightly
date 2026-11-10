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
//! - [`follow_sign_in_states`]: a login the harness finds must be signed in
//!   again shows "Sign in again" in the chat document of every chat this
//!   device hosts for that account (`providerState`), cleared once a request
//!   for it works again. That this device has no login for an account
//!   ([`AccountLogin::CantHost`]) is never written to a chat: publishing
//!   `hostUnavailable`, and "Sign in as <email> to continue" when no eligible
//!   device is online, belong with hosting and takeover.

use std::collections::{HashMap, HashSet};

use zeron_doc::{SessionCommandPayload, SessionDoc};
use zeron_harness::provider_cloud::{
    AccountLogin, CreateSession, HostCheck, ProviderChatState, ProviderCloudError,
    ProviderCloudStatus, ProviderResume, RunBinding,
};
use zeron_proto::{Chat, ChatConfig, ChildKind, HarnessId, ProviderSession, RunRequest};

use crate::EngineError;
use crate::doc_host::DocHost;
use crate::registry::HarnessRegistry;
use crate::sessions::SessionsEngine;
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
                request: run_request(&new.config, cwd.clone(), prompt),
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
    // Every request the run sends asks first whether this device still
    // hosts the chat (ticket #12, design §4 "Demotion").
    if let Some(workspace) = workspace.cloned() {
        let chat = chat_id.to_owned();
        cloud.check_host(
            &session.session_id,
            HostCheck::new(move || workspace.is_host(&chat)),
        );
    }
    cloud.bind_run(RunBinding {
        session,
        prompt_uuid: message_id.map(zeron_proto::provider_cloud::event_uuid),
        resume: Some(resume),
    });
    Ok(())
}

/// Resume a provider-run chat as its host, with no message: the entry point
/// a device uses once it hosts the chat (after a restart, or having taken it
/// over; ticket #12 calls it once its claim is echoed). Before connecting,
/// duplicates go (the takeover sweep) and the harness is bound to the
/// chat's session with the document's resume point: the replay starts at
/// the lower of the watermark and just before the oldest prompt still open,
/// so those prompts are asked again, and a turn cut off mid-way is written
/// over again. The run then mirrors whatever the session does, and the
/// chat's next message goes to it.
///
/// Messages whose delivery is uncertain ([`uncertain_sends`]: sent from
/// Zeron, by this device before a crash or by a host that went away, and
/// never echoed by the session) go again instead, under their own ids (so
/// their original uuids): the oldest opens the run, whose harness reads the
/// session's history for it first and sends it only if it isn't there; the
/// others follow it as steers (ticket #13).
///
/// `Ok(false)`: a run is already live for the chat (nothing to do).
/// Refused for a chat with no provider session, or when provider-run chats
/// are off here. Whether this device should host the chat is the caller's
/// call.
pub(crate) async fn resume_as_host(
    sessions: &SessionsEngine,
    doc_host: &DocHost,
    chat_id: &str,
) -> Result<bool, EngineError> {
    if sessions.has_run(chat_id) {
        return Ok(false);
    }
    let workspace = doc_host.workspace();
    let session = chat_session(workspace, doc_host, chat_id)
        .ok_or_else(|| refused("This chat has no Claude cloud session, so it can't run."))?;
    let handle = doc_host.open(chat_id)?;
    handle
        .doc()
        .sweep_provider_duplicates()
        .map_err(|e| refused(format!("This chat's transcript couldn't be cleaned up: {e}")))?;
    let uncertain = uncertain_sends(handle.doc());
    if let Some((first, rest)) = uncertain.split_first() {
        let config = chat_config(workspace, chat_id);
        let request = run_request(
            &config,
            zeron_proto::provider_cloud::claude_cwd(&session.session_id),
            first.text.clone(),
        );
        tracing::info!(chat = %chat_id, messages = uncertain.len(), "sending again what may not have arrived");
        doc_host
            .dispatch_with_source_context(
                sessions,
                chat_id,
                HarnessId::ClaudeCodeCloud,
                request,
                Some(first.message_id.clone()),
            )
            .await?;
        for message in rest {
            let now = chrono::Utc::now().timestamp_millis();
            sessions
                .steer_now(
                    chat_id,
                    &message.text,
                    Vec::new(),
                    Some(message.message_id.clone()),
                    now,
                )
                .await?;
        }
        return Ok(true);
    }
    bind_run(
        sessions.registry(),
        workspace,
        doc_host,
        HarnessId::ClaudeCodeCloud,
        chat_id,
        None,
    )?;
    let config = chat_config(workspace, chat_id);
    let request = run_request(
        &config,
        zeron_proto::provider_cloud::claude_cwd(&session.session_id),
        String::new(),
    );
    sessions.start_provider_resume(chat_id, request).await?;
    Ok(true)
}

/// The chat's configuration, from its row (a default one when the row has
/// none).
fn chat_config(workspace: Option<&WorkspaceHost>, chat_id: &str) -> ChatConfig {
    workspace
        .and_then(|ws| ws.chat(chat_id).ok().flatten())
        .and_then(|chat| chat.config)
        .unwrap_or_else(|| ChatConfig {
            harness: HarnessId::ClaudeCodeCloud,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: zeron_proto::SandboxLevel::WorkspaceWrite,
            access: Default::default(),
            plan: false,
        })
}

/// A message sent to a provider-run chat from Zeron whose delivery is
/// uncertain ([`uncertain_sends`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UncertainSend {
    pub message_id: String,
    pub text: String,
}

/// The messages this chat sent from Zeron (a Run or Steer command that went
/// through) that the session never echoed, after the last entry it did echo
/// or write: the device sending them may have died before, or right after,
/// the session got them. Oldest first. A message the transcript shows
/// answered is past (its echo or reply is stamped), and so is a failed send
/// (its command didn't go through; the person retries it).
pub(crate) fn uncertain_sends(doc: &SessionDoc) -> Vec<UncertainSend> {
    let stamped: HashSet<String> = doc
        .provider_entries()
        .into_iter()
        .filter(|entry| entry.provider_uuid.is_some())
        .map(|entry| entry.id)
        .collect();
    let sent: HashMap<String, String> = doc
        .read_commands()
        .unwrap_or_default()
        .into_iter()
        .filter(|command| command.status == zeron_doc::SessionCommandStatus::Applied)
        .filter_map(|command| match command.payload {
            SessionCommandPayload::Run {
                message_id,
                request,
            } => Some((message_id, request.prompt)),
            SessionCommandPayload::Steer {
                message_id: Some(message_id),
                prompt,
            } => Some((message_id, prompt)),
            _ => None,
        })
        .collect();
    let entries = doc.read_entries().unwrap_or_default();
    let after = entries
        .iter()
        .rposition(|entry| stamped.contains(&entry.id))
        .map_or(0, |at| at + 1);
    entries[after..]
        .iter()
        .filter(|entry| entry.role == zeron_doc::MessageRole::User)
        .filter_map(|entry| {
            let text = sent.get(&entry.id)?;
            Some(UncertainSend {
                message_id: entry.id.clone(),
                text: text.clone(),
            })
        })
        .collect()
}

/// A provider-run chat's run in configuration `config`.
fn run_request(config: &ChatConfig, cwd: String, prompt: String) -> RunRequest {
    RunRequest {
        prompt,
        harness: Some(HarnessId::ClaudeCodeCloud),
        model: config.model.clone(),
        reasoning: config.reasoning,
        model_options: config.model_options.clone(),
        cwd,
        sandbox: config.sandbox,
        auto_approve: false,
        access: config.access,
        plan: config.plan,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
        mcp: None,
        extra_mcp: Vec::new(),
        env: Default::default(),
    }
}

/// Commit provider event `sequence_num` (its turn and prompt bookkeeping,
/// and the replay watermark), then, when `sweep`, the host's duplicate
/// sweep. `false` when the commit failed (the watermark stays put).
pub(crate) fn commit_marker(
    doc: &SessionDoc,
    chat_id: &str,
    sequence_num: u64,
    turn: zeron_doc::TurnUpdate,
    prompt: Option<&zeron_proto::ProviderPrompt>,
    sweep: bool,
) -> bool {
    let committed = match doc.commit_provider_event(sequence_num, turn, prompt) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(chat = %chat_id, error = %err, "replay watermark write failed");
            false
        }
    };
    if sweep && let Err(err) = doc.sweep_provider_duplicates() {
        tracing::warn!(chat = %chat_id, error = %err, "duplicate sweep failed");
    }
    committed
}

/// Whether a provider-run chat's document shows the session mid-way: a turn
/// cut off before its end was mirrored, or a prompt still open.
pub(crate) fn mirror_unfinished(doc: &SessionDoc) -> bool {
    doc.provider_turn().is_some() || !doc.provider_open_prompts().is_empty()
}

/// On boot: resume every provider-run chat this device hosts whose
/// transcript was cut off mid-turn or has a prompt open ([`resume_as_host`]),
/// so the transcript catches up and the prompt is asked again without
/// waiting for the next message. Idle chats catch up on their next message.
pub(crate) fn resume_hosted_chats(sessions: &SessionsEngine, doc_host: &DocHost) {
    let Ok(harness) = sessions.registry().resolve(HarnessId::ClaudeCodeCloud) else {
        return;
    };
    if harness.provider_cloud().is_none() {
        return;
    }
    let Some(workspace) = doc_host.workspace().cloned() else {
        return;
    };
    let (sessions, host) = (sessions.clone(), doc_host.clone());
    doc_host.spawn_worker(async move {
        // Rows from before this boot may be stale (another device may have
        // taken a chat over meanwhile): only once the room has answered.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        while !workspace.registry_synced() {
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let chats = workspace.read_chats().unwrap_or_default();
        for chat in chats {
            if chat.provider_session.is_none() || !workspace.is_host(&chat.id) {
                continue;
            }
            // Cut off mid-turn, or with a send that may not have arrived.
            let unfinished = host.open(&chat.id).is_ok_and(|handle| {
                mirror_unfinished(handle.doc()) || !uncertain_sends(handle.doc()).is_empty()
            });
            if !unfinished {
                continue;
            }
            if let Err(error) = resume_as_host(&sessions, &host, &chat.id).await {
                tracing::warn!(chat = %chat.id, %error, "couldn't resume the provider-run chat");
            }
        }
    });
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
        let mut withdrawn = false;
        loop {
            let current = status.borrow_and_update().clone();
            // Turned off (the kill switch): this device no longer offers to
            // host provider-run chats.
            if current.disabled.is_some() && !withdrawn {
                withdrawn = true;
                if let Err(error) =
                    workspace.set_own_capability(zeron_proto::capabilities::PROVIDER_RUN_V1, false)
                {
                    tracing::warn!(%error, "provider-run capability update failed");
                }
            }
            let rows = chats.borrow_and_update().clone();
            record_states(&host, &current, &rows, &mut written);
            tokio::select! {
                changed = status.changed() => if changed.is_err() { return },
                changed = chats.changed() => if changed.is_err() { return },
            }
        }
    });
}

/// One pass of [`follow_sign_in_states`]. A chat hosted here whose account's
/// login must be signed in again shows "Sign in again"; one whose account's
/// login works shows nothing. A device without a login for the account
/// writes nothing: that it can't host is its own fact, not the chat's.
fn record_states(
    host: &DocHost,
    status: &ProviderCloudStatus,
    chats: &[Chat],
    written: &mut HashMap<String, Option<ProviderChatState>>,
) {
    let mut hosted = HashSet::new();
    for chat in chats {
        let Some(session) = &chat.provider_session else {
            continue;
        };
        if !host.workspace().is_some_and(|ws| ws.is_host(&chat.id)) {
            continue;
        }
        hosted.insert(chat.id.as_str());
        let login = status.accounts.get(&session.account_uuid).copied();
        crate::provider_host::publish_host_unavailable(host, &chat.id, login);
        let state = match login {
            Some(AccountLogin::Works) => None,
            Some(AccountLogin::SignInAgain) => Some(ProviderChatState::SignInAgain {
                email: session.account_email.clone(),
            }),
            Some(AccountLogin::CantHost) | None => continue,
        };
        if written.get(&chat.id) == Some(&state) {
            continue;
        }
        let recorded = host
            .open(&chat.id)
            .and_then(|handle| Ok(handle.doc().set_provider_chat_state(state.as_ref())?));
        match recorded {
            Ok(()) => {
                written.insert(chat.id.clone(), state);
            }
            Err(error) => {
                tracing::warn!(chat = %chat.id, %error, "couldn't record the chat's sign-in state")
            }
        }
    }
    written.retain(|chat, _| hosted.contains(chat.as_str()));
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
