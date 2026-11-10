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
    CreateSession, HostCheck, ListedSession, ProviderCloud, ProviderCloudError, ProviderResume,
    RunBinding, SessionActivity,
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

/// Whether the provider keeps the custom tag a create sends
/// (`zeron:<first prompt uuid>`), so a create whose answer was lost can be
/// found by it. Without it, such a create goes straight to "Creation
/// uncertain". Checked against the real API once (ticket #11).
const TAGS_KEPT: bool = true;

/// The session list's page size, and the most pages one read follows.
const LIST_PAGE_SIZE: usize = 20;
const LIST_PAGES: usize = 25;

/// What the list's status says the session is doing. Archived and plain
/// lifecycle states read as idle; the working and waiting words are not
/// verified against the real API yet (the live smoke test checks them).
fn session_activity(info: &api::SessionInfo) -> SessionActivity {
    let says = |word: &str| {
        [info.worker_status.as_deref(), info.status.as_deref()]
            .into_iter()
            .flatten()
            .any(|s| s.eq_ignore_ascii_case(word))
    };
    if ["requires_action", "waiting", "waiting_for_input", "awaiting_input"]
        .into_iter()
        .any(says)
    {
        SessionActivity::WaitingForYou
    } else if ["running", "working", "busy"].into_iter().any(says) {
        SessionActivity::Working
    } else {
        SessionActivity::Idle
    }
}

fn parse_time(at: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(at?.trim())
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// One list entry, as `account`'s session. `None` without an id or any
/// time to place it by.
fn listed_session(
    info: api::SessionInfo,
    account: &credentials::ClaudeAccount,
) -> Option<ListedSession> {
    if info.id.is_empty() {
        return None;
    }
    let last_event_at = parse_time(info.last_event_at.as_deref())
        .or_else(|| parse_time(info.created_at.as_deref()))?;
    let created_at = parse_time(info.created_at.as_deref()).unwrap_or(last_event_at);
    Some(ListedSession {
        activity: session_activity(&info),
        archived: info.archived(),
        title: info.title.clone().filter(|t| !t.trim().is_empty()),
        created_by: info
            .tags
            .iter()
            .find_map(|tag| zeron_proto::provider_cloud::prompt_of_create_tag(tag))
            .map(str::to_owned),
        session: session_of(info.id, account),
        created_at,
        last_event_at,
    })
}

/// Read the device's own login's session list from its first page, newest
/// activity first, handing each entry to `visit` until it says to stop
/// (`false`), the list ends, or [`LIST_PAGES`] pages were read. Every page
/// read is bounded by the client's list timeout.
async fn walk_list(
    client: &CloudClient,
    account: &credentials::ClaudeAccount,
    mut visit: impl FnMut(api::SessionInfo) -> bool,
) -> Result<(), ProviderCloudError> {
    let mut cursor: Option<String> = None;
    for _ in 0..LIST_PAGES {
        let page = client
            .list_sessions(account, LIST_PAGE_SIZE, cursor.as_deref())
            .await
            .map_err(provider_error)?;
        if page.data.is_empty() {
            return Ok(());
        }
        for info in page.data {
            if !visit(info) {
                return Ok(());
            }
        }
        cursor = page.next_cursor.filter(|c| !c.is_empty());
        if cursor.is_none() {
            return Ok(());
        }
    }
    Ok(())
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

/// A create's failure: [`ProviderCloudError::Uncertain`] when the session
/// may exist (the request went out and no answer said what happened: a
/// dropped connection or timeout, a server failure, or an answer that
/// couldn't be read); otherwise what [`provider_error`] says, nothing having
/// been created (a refusal, a login problem, or no connection at all).
fn create_error(error: ApiError) -> ProviderCloudError {
    let uncertain = match &error {
        ApiError::Transport(_) | ApiError::Decode(_) => true,
        ApiError::Status { status, .. } => *status == 408 || (500..600).contains(status),
        _ => false,
    };
    if uncertain {
        ProviderCloudError::Uncertain(format!(
            "Zeron couldn't tell whether Claude created the session ({error})."
        ))
    } else {
        provider_error(error)
    }
}

/// Session `id` of `account`.
fn session_of(id: String, account: &credentials::ClaudeAccount) -> ProviderSession {
    ProviderSession {
        provider: zeron_proto::provider_cloud::CLAUDE.to_owned(),
        session_id: id,
        account_uuid: account.account_uuid.clone(),
        account_email: account.email.clone(),
        org_id: account.org_id.clone(),
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
        // Bounded as a whole: nothing has been created yet, so running out of
        // time here is a plain failure.
        let timeouts = self.client.timeouts();
        let environment = tokio::time::timeout(timeouts.lookup, async {
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
            Ok::<_, ProviderCloudError>(environment)
        })
        .await
        .map_err(|_| {
            ProviderCloudError::Other(
                "Claude cloud didn't answer in time; nothing was created.".into(),
            )
        })??;
        let environment = environment.ok_or_else(|| {
                ProviderCloudError::Other(
                    "This Claude account has no cloud environment yet. Open claude.ai/code once, then try again."
                        .into(),
                )
            })?;
        // A GitHub project's session clones its repository at the branch
        // (no revision: the default branch). Where the session pushes its
        // work (`outcomes`) is left to the session, as "No project" leaves it.
        let sources: Vec<serde_json::Value> = request
            .source
            .iter()
            .map(|source| {
                let mut repo = json!({
                    "type": "git_repository",
                    "url": format!("https://github.com/{}", source.github_repo),
                });
                if let Some(branch) = source.branch.as_deref().filter(|b| !b.is_empty()) {
                    repo["revision"] = json!(branch);
                }
                repo
            })
            .collect();
        let mut config = json!({ "sources": sources, "outcomes": [] });
        if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
            config["model"] = json!(model);
        }
        let mut body = json!({
            "title": request.title.clone().unwrap_or_else(|| zeron_proto::provider_cloud::title_from_prompt(&request.prompt)),
            "environment_id": environment,
            "events": [{ "payload": session::user_payload(&request.prompt_uuid, "", &request.prompt) }],
            "config": config,
        });
        if TAGS_KEPT {
            body["tags"] = json!([zeron_proto::provider_cloud::create_tag(&request.prompt_uuid)]);
        }
        // One bound over the create, a CLI renewal and its one resend
        // included: past it, the outcome is unknown.
        let created = tokio::time::timeout(timeouts.create, self.client.create_session(&account, &body))
            .await
            .map_err(|_| {
                ProviderCloudError::Uncertain(
                    "Zeron couldn't tell whether Claude created the session (no answer in time)."
                        .into(),
                )
            })?
            .map_err(create_error)?;
        let session = session_of(created.id, &account);
        self.bindings
            .created(session.clone(), request.prompt_uuid, request.model);
        Ok(session)
    }

    async fn find_created(
        &self,
        prompt_uuid: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<ProviderSession>, ProviderCloudError> {
        if !TAGS_KEPT {
            return Ok(None);
        }
        let account = self
            .client
            .credentials()
            .account()
            .await
            .map_err(|e| provider_error(ApiError::Credentials(e)))?;
        let mut found = None;
        walk_list(&self.client, &account, |info| {
            let Some(listed) = listed_session(info, &account) else {
                return true;
            };
            if listed.created_by.as_deref() == Some(prompt_uuid) {
                found = Some(listed.session);
                return false;
            }
            // Newest activity first: the create's session (whose first event
            // is no older than the attempt) can't come later.
            listed.last_event_at >= since
        })
        .await?;
        if let Some(session) = &found {
            // Every session starts on the model it was created with; which
            // one is unknown here, so no model change is assumed.
            self.bindings
                .created(session.clone(), prompt_uuid.to_owned(), None);
        }
        Ok(found)
    }

    async fn archive_session(&self, session: &ProviderSession) -> Result<(), ProviderCloudError> {
        self.client
            .archive(&session::account_of(session), &session.session_id)
            .await
            .map_err(provider_error)
    }

    async fn read_session(
        &self,
        session: &ProviderSession,
    ) -> Result<ListedSession, ProviderCloudError> {
        let account = session::account_of(session);
        let info = self
            .client
            .session(&account, &session.session_id)
            .await
            .map_err(provider_error)?;
        listed_session(info, &account).ok_or_else(|| {
            ProviderCloudError::Other("Claude cloud described the session without its times.".into())
        })
    }

    async fn list_sessions(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ListedSession>, ProviderCloudError> {
        // Only the device's own login for now; added accounts (ticket #14)
        // each get their own read.
        let account = self
            .client
            .credentials()
            .account()
            .await
            .map_err(|e| provider_error(ApiError::Credentials(e)))?;
        let mut listed = Vec::new();
        walk_list(&self.client, &account, |info| {
            let Some(found) = listed_session(info, &account) else {
                return true;
            };
            if found.last_event_at < since {
                // Newest activity first: everything after is older.
                return false;
            }
            listed.push(found);
            true
        })
        .await?;
        Ok(listed)
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
