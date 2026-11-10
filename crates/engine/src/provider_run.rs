//! Provider-run chats in the engine (docs/design/provider-run-chats.md):
//! a Zeron chat tied to one provider cloud session (Claude Code cloud), whose
//! host device mirrors the session and sends to it.
//!
//! - [`create_chat`]: `createChat` with harness `claude-code-cloud`, in no
//!   project (an empty sandbox) or a GitHub project (the session clones its
//!   repository at a branch, [`github_source`]; the chat sits in the project
//!   but is hosted here). The session is created synchronously on this
//!   device (never queued, never retried). A create whose answer is lost is
//!   looked for in the session list by its tag (`zeron:<first prompt uuid>`)
//!   and adopted when found; otherwise it is "Creation uncertain"
//!   ([`CreateFailure::Uncertain`]) and only the person may create again.
//!   Then the row is written: chat id = UUIDv5 of
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
    ProviderCloudStatus, ProviderResume, RunBinding, SessionSource,
};
use zeron_proto::{Chat, ChatConfig, ChildKind, HarnessId, ProviderSession, RunRequest};

use crate::EngineError;
use crate::doc_host::DocHost;
use crate::registry::HarnessRegistry;
use crate::repos::Repos;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// A `createChat` for a provider-run chat.
#[derive(Debug, Clone)]
pub(crate) struct NewProviderChat {
    /// The device the request named, if any: it must be this one (the
    /// device the person is on creates the session).
    pub device_id: Option<String>,
    /// The project: a GitHub one (any device's checkout, or one living at
    /// the provider). `None`: an empty sandbox.
    pub space_id: Option<String>,
    /// The branch the session starts on, as the composer named it; `None`:
    /// the branch this device's checkout has out, else the default branch.
    pub branch: Option<String>,
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

/// How far before a create's start its session is looked for (the
/// provider's clock may run behind this device's).
const CREATE_LOOKBACK: chrono::Duration = chrono::Duration::minutes(5);

/// The bound on one provider-run create, from the request through the
/// lookup by its tag: clearly inside the composer's wait (150 s), so the
/// composer never shows "Creation uncertain" while the engine still works on
/// it. Past it, the outcome is unknown and nothing is retried.
pub(crate) const CREATE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(110);

/// Why [`create_chat`] made no chat.
#[derive(Debug)]
pub(crate) enum CreateFailure {
    /// Nothing was created (or the session was created but its chat
    /// couldn't be recorded: the message says so).
    Refused(EngineError),
    /// The create's outcome is unknown, and the session list has no
    /// session with its tag: "Creation uncertain". Only the person may
    /// create again (a new create, with a new first-message id).
    Uncertain,
}

impl From<EngineError> for CreateFailure {
    fn from(error: EngineError) -> Self {
        Self::Refused(error)
    }
}

/// Create the provider session, then the chat for it (see module docs).
pub(crate) async fn create_chat(
    registry: &HarnessRegistry,
    workspace: &WorkspaceHost,
    repos: &Repos,
    doc_host: &DocHost,
    new: NewProviderChat,
) -> Result<CreatedProviderChat, CreateFailure> {
    let device_id = workspace.device_id().to_owned();
    if new.device_id.as_deref().is_some_and(|d| d != device_id) {
        return Err(refused("A provider-run chat is created on the device you're using.").into());
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
    let prompt_uuid = zeron_proto::provider_cloud::event_uuid(&message_id);
    let deadline = registry
        .claude_cloud()
        .map_or(CREATE_DEADLINE, |store| store.create_deadline());
    // Whether the create request may have gone out: past that, running out
    // of time is "Creation uncertain", before it a plain failure.
    let sent = std::sync::atomic::AtomicBool::new(false);
    let obtained = tokio::time::timeout(deadline, async {
        let source = match new.space_id.as_deref() {
            Some(space_id) => Some(github_source(workspace, repos, space_id, new.branch).await?),
            None => None,
        };
        let branch = source.as_ref().and_then(|s| s.branch.clone());
        let attempted = chrono::Utc::now();
        sent.store(true, std::sync::atomic::Ordering::SeqCst);
        // Not idempotent: exactly one attempt. A refusal created nothing; an
        // unknown outcome is looked up by the create's tag, never retried.
        let created = cloud
            .create_session(CreateSession {
                prompt: prompt.clone(),
                prompt_uuid: prompt_uuid.clone(),
                model: new.config.model.clone(),
                title: Some(title.clone()),
                source,
            })
            .await;
        let session = match created {
            Ok(session) => session,
            Err(ProviderCloudError::Uncertain(why)) => {
                tracing::warn!(reason = %why, "a Claude cloud create's outcome is unknown; looking for its session");
                match cloud
                    .find_created(&prompt_uuid, attempted - CREATE_LOOKBACK)
                    .await
                {
                    Ok(Some(session)) => session,
                    Ok(None) => return Err(CreateFailure::Uncertain),
                    Err(error) => {
                        tracing::warn!(%error, "couldn't look for the created session");
                        return Err(CreateFailure::Uncertain);
                    }
                }
            }
            Err(ProviderCloudError::Provider { message, .. }) => {
                return Err(CreateFailure::Refused(refused(message)));
            }
            Err(other) => return Err(CreateFailure::Refused(refused(other.to_string()))),
        };
        Ok::<_, CreateFailure>((session, branch))
    })
    .await;
    let (session, branch) = match obtained {
        Ok(obtained) => obtained?,
        Err(_) if sent.load(std::sync::atomic::Ordering::SeqCst) => {
            tracing::warn!("a Claude cloud create ran out of time; its outcome is unknown");
            return Err(CreateFailure::Uncertain);
        }
        Err(_) => {
            return Err(refused("Claude cloud didn't answer in time; nothing was created.").into());
        }
    };
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
            new.space_id.as_deref(),
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
        return Err(recorded(refused("the chat names another provider session")).into());
    }
    workspace.rename_chat(&chat_id, &title).map_err(recorded)?;
    // The branch the session started from: the chat's "project · branch".
    if let Some(branch) = &branch {
        workspace
            .set_chat_branch(&chat_id, branch)
            .map_err(recorded)?;
    }
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

/// The repository and branch a session in project `space_id` clones: the
/// project's GitHub repository (its owner stamps it; this device's own
/// checkout is asked directly, so a project just added works at once), and
/// the branch the composer named, else the one this device's checkout has
/// out, else (another device's checkout, a project living at the provider,
/// a detached head) the repository's default branch. A project without a
/// GitHub remote is refused before anything reaches the provider.
async fn github_source(
    workspace: &WorkspaceHost,
    repos: &Repos,
    space_id: &str,
    branch: Option<String>,
) -> Result<SessionSource, EngineError> {
    let space = workspace
        .space(space_id)?
        .ok_or_else(|| refused("That project no longer exists."))?;
    let checkout = (space.device_id == workspace.device_id())
        .then(|| std::path::PathBuf::from(&space.path));
    let mut github_repo = space.github_repo.clone();
    if github_repo.is_none()
        && let Some(path) = &checkout
    {
        github_repo = repos.github_repo(path).await;
    }
    let github_repo = github_repo.ok_or_else(|| {
        refused(format!(
            "{} has no GitHub remote, so Anthropic cloud can't work on it.",
            space.display_name()
        ))
    })?;
    let mut branch = branch.filter(|b| !b.trim().is_empty());
    if branch.is_none()
        && let Some(path) = &checkout
    {
        branch = repos
            .current_branch(path)
            .await
            .ok()
            .filter(|b| b != "HEAD" && !b.is_empty());
    }
    Ok(SessionSource {
        github_repo,
        branch,
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
    bind_run(
        sessions.registry(),
        workspace,
        doc_host,
        HarnessId::ClaudeCodeCloud,
        chat_id,
        None,
    )?;
    let config = workspace
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
        });
    let request = run_request(
        &config,
        zeron_proto::provider_cloud::claude_cwd(&session.session_id),
        String::new(),
    );
    sessions.start_provider_resume(chat_id, request).await?;
    Ok(true)
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
            let unfinished = host
                .open(&chat.id)
                .is_ok_and(|handle| mirror_unfinished(handle.doc()));
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
        loop {
            let current = status.borrow_and_update().clone();
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
        // A fact about the session ("Archived on claude.ai") isn't the
        // login's to replace or clear.
        let recorded = host.open(&chat.id).and_then(|handle| {
            let doc = handle.doc();
            if doc
                .provider_chat_state()
                .is_some_and(|current| !current.is_sign_in())
            {
                return Ok(());
            }
            Ok(doc.set_provider_chat_state(state.as_ref())?)
        });
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
