//! `claude-code-cloud`: provider-run chats on Anthropic's Claude Code cloud
//! (claude.ai/code), driven natively over its private session API — HTTP +
//! SSE, like the opencode harness — with Zeron's own identity
//! (docs/design/provider-run-chats.md, ADR 0003).
//!
//! - [`credentials`]: read-only token and account reads from the CLI's
//!   config folder.
//! - [`api`]: the honest-identity HTTP client (list, create, events, stream,
//!   history).
//! - `mirror`: session payloads → agent events through the Claude Code
//!   harness's own normalizer.
//! - `session`: one persistent run over the session stream.
//!
//! The engine creates sessions through the [`ProviderCloud`] facet and binds
//! each fresh run to its session ([`ProviderCloud::bind_run`]); a run never
//! starts for a session it wasn't told about.

pub mod api;
pub mod credentials;
mod mirror;
mod session;
pub mod sse;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::mpsc;
use zeron_proto::{
    AccessMode, AgentEvent, HarnessId, Model, ProviderSession, ReasoningLevel, RunRequest,
    SteeringMode,
};

use crate::provider_cloud::{CreateSession, ProviderCloud, ProviderCloudError, RunBinding};
use crate::{Harness, HarnessError, RunControls};
use api::{ApiError, CloudClient};
use credentials::{ClaudeConfigDir, CredentialError, Credentials};

/// The switch that turns provider-run chats on for this device, until the
/// "Claude cloud sessions" setting (ticket #4) replaces it.
pub const ENABLE_ENV: &str = "ZERON_CLAUDE_CLOUD_SESSIONS";

/// Whether provider-run chats are on: off unless [`ENABLE_ENV`] is `1`.
pub fn enabled() -> bool {
    std::env::var(ENABLE_ENV).is_ok_and(|v| v.trim() == "1")
}

/// Where and as whom the harness talks to the session API.
#[derive(Debug, Clone)]
pub struct ClaudeCloudConfig {
    /// The API's base URL ([`api::DEFAULT_BASE_URL`]; tests point it at a
    /// fake server).
    pub base_url: String,
    /// The device's own Claude login (`~/.claude`, or a folder named the way
    /// `CLAUDE_CONFIG_DIR` names one).
    pub login: ClaudeConfigDir,
    /// The `claude` the token renewal runs; `None` resolves the device's.
    pub claude_executable: Option<PathBuf>,
}

impl ClaudeCloudConfig {
    /// The production API with the device's own login. `None` without a
    /// home directory.
    pub fn device_default() -> Option<Self> {
        Some(Self {
            base_url: api::DEFAULT_BASE_URL.to_owned(),
            login: ClaudeConfigDir::device_default()?,
            claude_executable: std::env::var_os("CLAUDE_CODE_EXECUTABLE")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        })
    }
}

/// What the harness knows about one session, in memory.
#[derive(Debug, Clone)]
struct SessionState {
    session: ProviderSession,
    /// The uuid the next fresh run's opening prompt goes out under.
    prompt_uuid: Option<String>,
    /// Event uuids this device already delivered to the session.
    delivered: HashSet<String>,
    /// The highest stored sequence number mirrored so far. `None` after a
    /// restart: ticket #8's durable watermark replaces this.
    last_seq: Option<u64>,
    /// What this device last put the session in, so a fresh run whose
    /// configuration differs sends the change.
    applied: Applied,
}

/// A session's model and permission mode, as far as this device knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Applied {
    /// `None`: not known (another device's session, or after a restart).
    pub model: Option<Option<String>>,
    /// Every session starts in `default` (each recorded `system/init`).
    pub mode: String,
}

impl Default for Applied {
    fn default() -> Self {
        Self {
            model: None,
            mode: "default".into(),
        }
    }
}

/// What a fresh run starts from.
pub(crate) struct RunStart {
    pub session: ProviderSession,
    /// The opening prompt's event uuid.
    pub prompt_uuid: String,
    /// Whether the session already has the opening prompt.
    pub delivered: bool,
    /// Where the stream starts.
    pub from_seq: u64,
    pub applied: Applied,
}

/// The sessions runs may use, by session id.
#[derive(Default)]
pub(crate) struct Bindings {
    sessions: Mutex<HashMap<String, SessionState>>,
}

impl Bindings {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionState>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn created(&self, session: ProviderSession, prompt_uuid: String, model: Option<String>) {
        self.lock().insert(
            session.session_id.clone(),
            SessionState {
                session,
                prompt_uuid: None,
                delivered: HashSet::from([prompt_uuid]),
                last_seq: Some(0),
                applied: Applied {
                    model: Some(model),
                    ..Applied::default()
                },
            },
        );
    }

    fn bind(&self, binding: RunBinding) {
        let mut sessions = self.lock();
        let id = binding.session.session_id.clone();
        match sessions.get_mut(&id) {
            Some(state) => {
                state.session = binding.session;
                state.prompt_uuid = binding.prompt_uuid;
            }
            None => {
                sessions.insert(
                    id,
                    SessionState {
                        session: binding.session,
                        prompt_uuid: binding.prompt_uuid,
                        delivered: HashSet::new(),
                        last_seq: None,
                        applied: Applied::default(),
                    },
                );
            }
        }
    }

    /// What the next fresh run for the session starts from.
    fn take_run(&self, session_id: &str) -> Option<RunStart> {
        let mut sessions = self.lock();
        let state = sessions.get_mut(session_id)?;
        let uuid = state
            .prompt_uuid
            .take()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Some(RunStart {
            session: state.session.clone(),
            delivered: state.delivered.contains(&uuid),
            prompt_uuid: uuid,
            from_seq: state.last_seq.unwrap_or(0),
            applied: state.applied.clone(),
        })
    }

    pub(crate) fn note_applied(&self, session_id: &str, applied: Applied) {
        if let Some(state) = self.lock().get_mut(session_id) {
            state.applied = applied;
        }
    }

    pub(crate) fn mark_delivered(&self, session_id: &str, uuid: &str) {
        if let Some(state) = self.lock().get_mut(session_id) {
            state.delivered.insert(uuid.to_owned());
        }
    }

    /// Whether this device sent the event `uuid` to the session.
    pub(crate) fn was_delivered(&self, session_id: &str, uuid: &str) -> bool {
        self.lock()
            .get(session_id)
            .is_some_and(|state| state.delivered.contains(uuid))
    }

    pub(crate) fn note_seq(&self, session_id: &str, seq: u64) {
        if let Some(state) = self.lock().get_mut(session_id) {
            state.last_seq = Some(state.last_seq.unwrap_or(0).max(seq));
        }
    }
}

/// The `claude-code-cloud` harness. Construct with [`ClaudeCloudHarness::new`].
pub struct ClaudeCloudHarness {
    client: Arc<CloudClient>,
    bindings: Arc<Bindings>,
}

impl ClaudeCloudHarness {
    pub fn new(config: ClaudeCloudConfig) -> Self {
        let credentials = Arc::new(Credentials::new(config.login, config.claude_executable));
        Self {
            client: Arc::new(CloudClient::new(&config.base_url, credentials)),
            bindings: Arc::new(Bindings::default()),
        }
    }

    /// The session API client (its login folder included).
    pub fn client(&self) -> &Arc<CloudClient> {
        &self.client
    }
}

fn harness_error(error: ApiError) -> HarnessError {
    match error {
        ApiError::Credentials(error) => HarnessError::NotSignedIn(error.to_string()),
        other => HarnessError::Protocol(other.to_string()),
    }
}

fn provider_error(error: ApiError) -> ProviderCloudError {
    match error {
        ApiError::Credentials(CredentialError::AccountMismatch) => {
            ProviderCloudError::AccountMismatch(CredentialError::AccountMismatch.to_string())
        }
        ApiError::Credentials(error) => ProviderCloudError::SignInRequired(error.to_string()),
        ApiError::Status { status, message } => ProviderCloudError::Provider {
            status: Some(status),
            message: format!("Claude cloud returned {status}: {message}"),
        },
        other => ProviderCloudError::Other(other.to_string()),
    }
}

#[async_trait]
impl ProviderCloud for ClaudeCloudHarness {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<ProviderSession, ProviderCloudError> {
        let account = self
            .client
            .credentials()
            .account()
            .await
            .map_err(|e| provider_error(ApiError::Credentials(e)))?;
        // Zeron never calls the environment endpoints: a new session goes in
        // the account's existing cloud environment, the one its sessions use.
        let page = self
            .client
            .list_sessions(&account, 50, None)
            .await
            .map_err(provider_error)?;
        let environment = page
            .data
            .iter()
            .find(|s| s.environment_kind.as_deref() == Some("anthropic_cloud"))
            .and_then(|s| s.environment_id.clone())
            .ok_or_else(|| {
                ProviderCloudError::Other(
                    "This Claude account has no cloud environment yet. Open claude.ai/code once, then try again."
                        .into(),
                )
            })?;
        let mut config = json!({ "sources": [], "outcomes": [] });
        if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
            config["model"] = json!(model);
        }
        let body = json!({
            "title": request.title.clone().unwrap_or_else(|| zeron_proto::provider_cloud::title_from_prompt(&request.prompt)),
            "environment_id": environment,
            "events": [{ "payload": session::user_payload(&request.prompt_uuid, "", &request.prompt) }],
            "config": config,
        });
        let created = self
            .client
            .create_session(&account, &body)
            .await
            .map_err(provider_error)?;
        let session = ProviderSession {
            provider: zeron_proto::provider_cloud::CLAUDE.to_owned(),
            session_id: created.id,
            account_uuid: account.account_uuid,
            account_email: account.email,
            org_id: account.org_id,
        };
        self.bindings
            .created(session.clone(), request.prompt_uuid, request.model);
        Ok(session)
    }

    fn bind_run(&self, binding: RunBinding) {
        self.bindings.bind(binding);
    }
}

#[async_trait]
impl Harness for ClaudeCloudHarness {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCodeCloud
    }
    fn display_name(&self) -> &str {
        "Claude Code cloud"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    /// A message sent during a turn goes to the session at once; the worker
    /// reads it at its next step boundary, or runs it right after the turn.
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    /// Model, permission mode and plan mode change on the live session with
    /// control requests (`set_model`, `set_permission_mode`) sent ahead of
    /// the message that carries them; nothing about a session is launched.
    fn reconfigures_in_place(&self, live: &RunRequest, next: &RunRequest) -> bool {
        live.cwd == next.cwd
    }
    fn access_modes(&self) -> &[AccessMode] {
        &AccessMode::ALL
    }
    /// The session's own plan mode (`set_permission_mode` `plan`);
    /// approving `ExitPlanMode` ends it.
    fn supports_plan(&self) -> bool {
        true
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    /// Every turn ends with the session's own stored `result`.
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    fn remote_workspace(&self) -> bool {
        true
    }
    fn provider_cloud(&self) -> Option<&dyn ProviderCloud> {
        Some(self)
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(self.fallback_models())
    }
    /// The workspace (and its skills) is at the provider.
    async fn skills(
        &self,
        _cwd: &std::path::Path,
    ) -> Result<Option<Vec<zeron_proto::invocation::Skill>>, HarnessError> {
        Ok(None)
    }
    /// The session runs whatever model its account's cloud offers; the picker
    /// (ticket #4) lists Claude models only. No model means the default.
    fn fallback_models(&self) -> Vec<Model> {
        vec![Model {
            id: "claude-opus-5-5".into(),
            label: "Claude Opus 5.5".into(),
            description: None,
            reasoning_levels: Vec::new(),
            options: Vec::new(),
        }]
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let session_id = zeron_proto::provider_cloud::claude_session_from_cwd(&request.cwd)
            .ok_or_else(|| {
                HarnessError::Protocol(
                    "This chat has no Claude cloud session: it can't run here.".into(),
                )
            })?
            .to_owned();
        let start = self.bindings.take_run(&session_id).ok_or_else(|| {
            HarnessError::Protocol(
                "This chat's Claude cloud session isn't known on this device.".into(),
            )
        })?;
        // Open the stream before sending, so nothing the session does in
        // reply is missed; a login that can't act for the chat's account
        // refuses here, before anything is sent.
        let account = session::account_of(&start.session);
        let stream = self
            .client
            .stream(&account, &session_id, start.from_seq)
            .await
            .map_err(harness_error)?;
        let (events, receiver) = mpsc::channel(256);
        // A configuration this device didn't leave the session in goes
        // first, under ids derived from the opening prompt's.
        let opening_controls = session::changes(&start.prompt_uuid, &start.applied, &request);
        let opening = (!start.delivered && !request.prompt.trim().is_empty())
            .then(|| (start.prompt_uuid, request.prompt.clone()));
        tokio::spawn(
            session::RunSession {
                client: self.client.clone(),
                bindings: self.bindings.clone(),
                session: start.session,
                opening,
                opening_controls,
                mirror: mirror::Mirror::new(request.cwd.clone(), start.from_seq),
                config: request,
                stream,
                controls,
                events,
            }
            .run(),
        );
        Ok(futures::stream::unfold(receiver, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}
