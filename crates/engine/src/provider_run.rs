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
//!
//! Seams for later tickets: GitHub projects (#6), creation uncertainty (#11),
//! hosting/takeover (#12, which adds the host check here before binding).

use zeron_doc::SessionCommandPayload;
use zeron_harness::provider_cloud::{CreateSession, ProviderCloudError, RunBinding};
use zeron_proto::{ChatConfig, ChildKind, HarnessId, ProviderSession, RunRequest};

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
            "Anthropic cloud chats start with no project for now.",
        ));
    }
    if new.device_id.as_deref().is_some_and(|d| d != device_id) {
        return Err(refused(
            "An Anthropic cloud chat is created on the device you're using.",
        ));
    }
    let prompt = new
        .prompt
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| refused("An Anthropic cloud chat starts with its first message."))?;
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
    // Zeron knows about; what an uncertain outcome shows is ticket #11's.
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
        .create_chat_hosted(
            &chat_id,
            None,
            Some(&device_id),
            Some(&device_id),
            Some(new.config.clone()),
            Some(cwd.clone()),
            new.parent_chat_id,
            new.child_kind,
        )
        .map_err(recorded)?;
    workspace
        .set_chat_provider_session(&chat_id, &session)
        .map_err(recorded)?;
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
    cloud.bind_run(RunBinding {
        session,
        prompt_uuid: message_id.map(zeron_proto::provider_cloud::event_uuid),
    });
    Ok(())
}
