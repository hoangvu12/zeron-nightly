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

use crate::provider_cloud::{
    CreateSession, HostCheck, ProviderCloud, ProviderCloudError, ProviderResume, RunBinding,
};
use crate::{Harness, HarnessError, RunControls};
use api::{ApiError, CloudClient};
use credentials::{ClaudeConfigDir, CredentialError, Credentials};

/// The switch that turns provider-run chats on for this device, until the
/// "Claude cloud sessions" setting replaces it.
pub const ENABLE_ENV: &str = "ZERON_CLAUDE_CLOUD_SESSIONS";

/// Whether provider-run chats are on: off unless [`ENABLE_ENV`] is `1`.
pub fn enabled() -> bool {
    std::env::var(ENABLE_ENV).is_ok_and(|v| v.trim() == "1")
}

/// Session-list pages read, at most, looking for the account's cloud
/// environment before concluding it has none.
const ENVIRONMENT_PAGES: usize = 10;
const ENVIRONMENT_PAGE_SIZE: usize = 50;

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
    /// The highest stored sequence number mirrored so far in this process.
    last_seq: Option<u64>,
    /// Where the next fresh run picks up, from the chat document (the
    /// engine's [`RunBinding::resume`]); wins over `last_seq`.
    resume: Option<ProviderResume>,
    /// The settings the session is in, as its last `system/init` (or this
    /// device's acknowledged change) said; `None` until known.
    actual: Option<Settings>,
    /// The settings this device's last run of the chat asked for; `None`
    /// until one ran here (another device's session, or after a restart).
    ran_with: Option<Settings>,
}

/// A Claude Code permission mode (`--permission-mode`, `set_permission_mode`,
/// `system/init`'s `permissionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    Plan,
}

impl PermissionMode {
    pub fn from_wire(mode: &str) -> Option<Self> {
        Some(match mode {
            "default" => Self::Default,
            "acceptEdits" => Self::AcceptEdits,
            "bypassPermissions" => Self::BypassPermissions,
            "plan" => Self::Plan,
            _ => return None,
        })
    }

    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
            Self::Plan => "plan",
        }
    }
}

/// A session's model and permission mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    /// `None`: the account's default model.
    pub model: Option<String>,
    pub mode: PermissionMode,
}

/// What a fresh run starts from.
pub(crate) struct RunStart {
    pub session: ProviderSession,
    /// The opening prompt's event uuid, when the engine named its message
    /// (no id: a new message, and no setting change goes with it).
    pub prompt_uuid: Option<String>,
    /// Whether the session already has the opening prompt.
    pub delivered: bool,
    /// Where the stream starts, and what the transcript already has.
    pub resume: ProviderResume,
    pub actual: Option<Settings>,
    pub ran_with: Option<Settings>,
}

/// The sessions runs may use, by session id.
#[derive(Default)]
pub(crate) struct Bindings {
    sessions: Mutex<HashMap<String, SessionState>>,
    /// Whether this device still hosts each session's chat
    /// ([`ProviderCloud::check_host`]).
    hosts: Mutex<HashMap<String, HostCheck>>,
}

impl Bindings {
    /// Whether this device may still send to session `session_id` (it
    /// hosts the chat, or nothing said otherwise).
    pub(crate) fn hosts(&self, session_id: &str) -> bool {
        let check = self
            .hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
            .cloned();
        check.is_none_or(|check| check.hosts())
    }

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
                resume: None,
                // Every session starts in `default` (each recorded
                // `system/init`), on the model it was created with.
                actual: Some(Settings {
                    model,
                    mode: PermissionMode::Default,
                }),
                ran_with: None,
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
                state.resume = binding.resume;
            }
            None => {
                sessions.insert(
                    id,
                    SessionState {
                        session: binding.session,
                        prompt_uuid: binding.prompt_uuid,
                        delivered: HashSet::new(),
                        last_seq: None,
                        resume: binding.resume,
                        actual: None,
                        ran_with: None,
                    },
                );
            }
        }
    }

    /// What the next fresh run for the session starts from.
    fn take_run(&self, session_id: &str) -> Option<RunStart> {
        let mut sessions = self.lock();
        let state = sessions.get_mut(session_id)?;
        let uuid = state.prompt_uuid.take();
        let resume = state.resume.take().unwrap_or_else(|| {
            let seq = state.last_seq.unwrap_or(0);
            ProviderResume {
                from_sequence_num: seq,
                mirrored_through: seq,
                watermark: seq,
                open_prompts: Vec::new(),
            }
        });
        Some(RunStart {
            session: state.session.clone(),
            delivered: uuid.as_ref().is_some_and(|u| state.delivered.contains(u)),
            prompt_uuid: uuid,
            resume,
            actual: state.actual.clone(),
            ran_with: state.ran_with.clone(),
        })
    }

    /// The settings the session is in now (its `system/init`, or a change
    /// it acknowledged).
    pub(crate) fn note_actual(&self, session_id: &str, actual: Settings) {
        if let Some(state) = self.lock().get_mut(session_id) {
            state.actual = Some(actual);
        }
    }

    /// The settings a run of the chat on this device asked for.
    pub(crate) fn note_ran_with(&self, session_id: &str, settings: Settings) {
        if let Some(state) = self.lock().get_mut(session_id) {
            state.ran_with = Some(settings);
        }
    }

    pub(crate) fn actual(&self, session_id: &str) -> Option<Settings> {
        self.lock()
            .get(session_id)
            .and_then(|state| state.actual.clone())
    }

    pub(crate) fn mark_delivered(&self, session_id: &str, uuid: &str) {
        if let Some(state) = self.lock().get_mut(session_id) {
            state.delivered.insert(uuid.to_owned());
        }
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
    /// Fails when its HTTP client (which names Zeron on every request) can't
    /// be built; there is no unnamed fallback.
    pub fn new(config: ClaudeCloudConfig) -> Result<Self, HarnessError> {
        let credentials = Arc::new(Credentials::new(config.login, config.claude_executable));
        let client = CloudClient::new(&config.base_url, credentials).map_err(harness_error)?;
        Ok(Self {
            client: Arc::new(client),
            bindings: Arc::new(Bindings::default()),
        })
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
        ApiError::Credentials(error @ CredentialError::AccountMismatch { .. }) => {
            ProviderCloudError::AccountMismatch(error.to_string())
        }
        ApiError::Credentials(error) => ProviderCloudError::SignInRequired(error.to_string()),
        ApiError::Disabled(reason) => ProviderCloudError::Disabled(reason),
        ApiError::Status {
            status, message, ..
        } => ProviderCloudError::Provider {
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
        let mut environment = None;
        let mut cursor: Option<String> = None;
        for _ in 0..ENVIRONMENT_PAGES {
            let page = self
                .client
                .list_sessions(&account, ENVIRONMENT_PAGE_SIZE, cursor.as_deref())
                .await
                .map_err(provider_error)?;
            environment = page
                .data
                .iter()
                .find(|s| s.environment_kind.as_deref() == Some("anthropic_cloud"))
                .and_then(|s| s.environment_id.clone());
            cursor = page.next_cursor.filter(|c| !c.is_empty());
            if environment.is_some() || cursor.is_none() {
                break;
            }
        }
        let environment = environment.ok_or_else(|| {
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

    fn check_host(&self, session_id: &str, check: HostCheck) {
        self.bindings
            .hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session_id.to_owned(), check);
    }

    fn status(&self) -> tokio::sync::watch::Receiver<crate::provider_cloud::ProviderCloudStatus> {
        self.client.status()
    }

    /// The folder's signed-in account, read from its file now.
    async fn has_login(&self, account_uuid: &str) -> bool {
        self.client
            .credentials()
            .account()
            .await
            .is_ok_and(|account| account.account_uuid == account_uuid)
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
    /// lists Claude models only. No model means the default.
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
        self.start(request, controls, false).await
    }

    /// A host resuming the chat with no message (after a restart, or taking
    /// it over): the run sends nothing, catches the transcript up from where
    /// its document says it got to, asks again the prompts still open, and
    /// mirrors whatever the session does next.
    async fn start_idle(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.start(request, controls, true).await
    }
}

impl ClaudeCloudHarness {
    async fn start(
        &self,
        request: RunRequest,
        controls: RunControls,
        idle: bool,
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
            .stream(&account, &session_id, start.resume.from_sequence_num)
            .await
            .map_err(harness_error)?;
        let (events, receiver) = mpsc::channel(256);
        // A setting changed here since this device's last run, that the
        // session isn't in, goes first, under ids derived from the opening
        // prompt's ([`session::changes`]).
        let opening_changes = session::changes(
            start.prompt_uuid.as_deref(),
            start.ran_with.as_ref(),
            start.actual.as_ref(),
            &session::settings_of(&request),
        );
        // A message with no id is a new one: its own uuid is minted here.
        let opening_changes = if idle {
            (Vec::new(), None)
        } else {
            opening_changes
        };
        let opening = (!idle && !start.delivered && !request.prompt.trim().is_empty()).then(|| {
            (
                start
                    .prompt_uuid
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                request.prompt.clone(),
            )
        });
        tokio::spawn(
            session::RunSession {
                client: self.client.clone(),
                bindings: self.bindings.clone(),
                session: start.session,
                opening,
                opening_changes,
                mirror: mirror::Mirror::new(request.cwd.clone(), start.resume),
                config: request,
                stream,
                controls,
                events,
                idle,
            }
            .run(),
        );
        Ok(futures::stream::unfold(receiver, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}
