//! Read-only access to a Claude CLI login (docs/design/provider-run-chats.md
//! §3). The official CLI owns every login: it signs in, keeps the folder's
//! `.credentials.json` and `.claude.json`, and refreshes the token there.
//! Zeron only reads:
//!
//! - the access token, from the folder, before every request — held in
//!   memory for that request only, never written, copied, cached or logged
//!   ([`AccessToken`] has no `Display` and a redacted `Debug`);
//! - the account (`oauthAccount.accountUuid`) from the same folder on every
//!   read, refusing a folder signed in to another account than the chat's
//!   (Zeron's account switcher can replace `~/.claude` at any time).
//!
//! A file that doesn't parse is read once more after 200 ms (the CLI can fall
//! back to writing in place).
//!
//! Renewal is the CLI's, triggered but never done here: a token within 5
//! minutes of expiry, or one the API refused (401), makes Zeron run `claude
//! auth status --json` in the folder ([`Credentials::renew`]). At most one
//! runs per folder in this process at a time, from spawn until the CLI
//! exits, and callers that arrive while it runs share its outcome
//! (single-flight). Its outcome is given up on after 10 s, but the CLI is
//! never killed mid-refresh (that would lose its rotated token, see
//! [`crate::claude::refresh_lock`]) and the folder stays busy until it is
//! reaped. The output must say the login is signed in, as the chat
//! account's email and org; then the files are read again. Each failure
//! says what it means for this device's login ([`CredentialError::account_login`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use crate::claude::refresh_lock;
use crate::provider_cloud::AccountLogin;

// The token file the CLI keeps in its config folder.
pub const CREDENTIALS_FILE: &str = ".credentials.json";
/// The CLI's global config, which names the signed-in account.
pub const ACCOUNT_FILE: &str = ".claude.json";

/// Wait before the one re-read of a file that didn't parse.
pub const PARSE_RETRY: Duration = Duration::from_millis(200);
/// A token this close to expiry is renewed (by the CLI) before use.
pub const RENEW_WITHIN: Duration = Duration::from_secs(5 * 60);
/// The longest a renewal (`claude auth status --json`) may take.
pub const RENEW_TIMEOUT: Duration = Duration::from_secs(10);

/// A Claude CLI config folder: where one login's files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeConfigDir {
    /// Holds `.credentials.json`.
    dir: PathBuf,
    /// The `.claude.json` naming the account: inside `dir` for a folder
    /// named by `CLAUDE_CONFIG_DIR`, `~/.claude.json` for the default login.
    account_file: PathBuf,
    /// What `CLAUDE_CONFIG_DIR` must be for the CLI to use this login
    /// (`None`: unset, the default `~/.claude`).
    env: Option<PathBuf>,
}

impl ClaudeConfigDir {
    /// A folder the CLI uses through `CLAUDE_CONFIG_DIR` (an added account,
    /// or a test's): both files inside it.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            account_file: dir.join(ACCOUNT_FILE),
            env: Some(dir.clone()),
            dir,
        }
    }

    /// The device's own login: `$CLAUDE_CONFIG_DIR` when set, else
    /// `~/.claude` with its account in `~/.claude.json`. `None` without a
    /// home directory.
    pub fn device_default() -> Option<Self> {
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return Some(Self::at(PathBuf::from(dir)));
        }
        let home = home_dir()?;
        Some(Self {
            dir: home.join(".claude"),
            account_file: home.join(ACCOUNT_FILE),
            env: None,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.dir.join(CREDENTIALS_FILE)
    }

    pub fn account_file(&self) -> &Path {
        &self.account_file
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let names = ["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    let names = ["HOME", "USERPROFILE"];
    names
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The Claude account a folder is signed in to (`.claude.json`
/// `oauthAccount`), or the one a chat's session belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAccount {
    pub account_uuid: String,
    pub email: String,
    pub org_id: String,
}

/// An access token, read for one request. No `Display`, no `Serialize`, and
/// a redacted `Debug`, so it can't end up in a log line or a file by
/// accident. It is held only as its `Authorization` header value, marked
/// sensitive, so the HTTP stack redacts it too.
pub struct AccessToken {
    authorization: reqwest::header::HeaderValue,
    expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl AccessToken {
    /// `None` for a token that can't travel in a header.
    fn new(value: &str, expires_at_ms: Option<i64>) -> Option<Self> {
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {value}")).ok()?;
        authorization.set_sensitive(true);
        Some(Self {
            authorization,
            expires_at_ms,
        })
    }

    /// The `Authorization` header value (`Bearer …`), marked sensitive.
    pub(crate) fn authorization(&self) -> reqwest::header::HeaderValue {
        self.authorization.clone()
    }

    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    /// Tells this token from another one in memory (a request refused with
    /// it, then the file re-read) without keeping the token itself.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.authorization.as_bytes().hash(&mut hasher);
        hasher.finish()
    }

    fn expiring(&self, now_ms: i64) -> bool {
        self.expires_at_ms
            .is_some_and(|at| at - now_ms < RENEW_WITHIN.as_millis() as i64)
    }
}

/// Why no token could be read. Never carries a credential.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// The folder isn't signed in to any account: this device has no login
    /// for the chat's account.
    #[error("This device isn't signed in to Claude (no login in {0}).")]
    NoLogin(String),
    #[error("The Claude login in {0} could not be read.")]
    Unreadable(String),
    /// The folder (or `auth status`) names another account than the chat's:
    /// this device has no login for the chat's account. `email` is the
    /// chat's account.
    #[error(
        "This device is signed in to Claude as a different account than this chat's ({email}), so it can't continue the chat here."
    )]
    AccountMismatch { email: String },
    #[error("The Claude login has expired. Sign in again with `claude`.")]
    Expired,
    #[error("The Claude login on this device is signed out. Sign in again with `claude`.")]
    LoggedOut,
    #[error("Claude couldn't renew the login: {0}. Sign in again with `claude`.")]
    RenewFailed(String),
    /// The API refused the login (401) again right after the CLI renewed it.
    #[error(
        "Claude refused this device's login even after renewing it. Sign in again with `claude`."
    )]
    Rejected,
    /// Adding an account: the CLI's sign-in didn't leave the new folder
    /// signed in.
    #[error("The Claude sign-in didn't finish: {0}")]
    SignInFailed(String),
}

impl CredentialError {
    /// What this failure says about this device's login for the account;
    /// `None` for a passing problem (a file unreadable twice).
    pub fn account_login(&self) -> Option<AccountLogin> {
        match self {
            Self::Unreadable(_) | Self::SignInFailed(_) => None,
            Self::NoLogin(_) | Self::AccountMismatch { .. } => Some(AccountLogin::CantHost),
            Self::Expired | Self::LoggedOut | Self::RenewFailed(_) | Self::Rejected => {
                Some(AccountLogin::SignInAgain)
            }
        }
    }

    fn mismatch(expected: &ClaudeAccount) -> Self {
        Self::AccountMismatch {
            email: expected.email.clone(),
        }
    }
}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OauthEntry>,
}

/// Only the two fields Zeron reads; the refresh token is never deserialized.
#[derive(Deserialize)]
struct OauthEntry {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
struct AccountFile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

#[derive(Deserialize)]
struct OauthAccount {
    #[serde(rename = "accountUuid")]
    account_uuid: Option<String>,
    #[serde(rename = "emailAddress", default)]
    email: Option<String>,
    #[serde(rename = "organizationUuid", default)]
    org_id: Option<String>,
}

/// `claude auth status --json`, the fields the renewal reads. Signed in:
/// `{"loggedIn":true,"authMethod":"claude.ai","email":…,"orgId":…,…}`, exit
/// code 0; signed out: `{"loggedIn":false,"authMethod":"none",…}`, exit 1.
#[derive(Debug, Clone, Default, Deserialize)]
struct AuthStatus {
    #[serde(rename = "loggedIn", default)]
    logged_in: Option<bool>,
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "orgId", default)]
    org_id: Option<String>,
}

impl AuthStatus {
    /// Whether the status allows acting for `expected`: signed in, as the
    /// account's email and org. Anything it doesn't say is a failed renewal.
    fn verdict(&self, expected: &ClaudeAccount) -> Result<(), CredentialError> {
        match self.logged_in {
            Some(true) => {}
            Some(false) => return Err(CredentialError::LoggedOut),
            None => {
                return Err(CredentialError::RenewFailed(
                    "`claude auth status` didn't say whether it's signed in".into(),
                ));
            }
        }
        let (Some(email), Some(org_id)) = (
            self.email.as_deref().filter(|e| !e.is_empty()),
            self.org_id.as_deref().filter(|o| !o.is_empty()),
        ) else {
            return Err(CredentialError::RenewFailed(
                "`claude auth status` didn't say which account it's signed in to".into(),
            ));
        };
        if !email.eq_ignore_ascii_case(&expected.email) || org_id != expected.org_id {
            return Err(CredentialError::mismatch(expected));
        }
        Ok(())
    }
}

/// Read and parse `path`, re-reading once after [`PARSE_RETRY`] when it
/// doesn't parse. `Ok(None)`: no such file.
async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, ()> {
    for attempt in 0..2 {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) if attempt == 0 => {
                tokio::time::sleep(PARSE_RETRY).await;
                continue;
            }
            Err(_) => return Err(()),
        };
        match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => return Ok(Some(value)),
            Err(_) if attempt == 0 => tokio::time::sleep(PARSE_RETRY).await,
            Err(_) => return Err(()),
        }
    }
    Err(())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What one `claude auth status --json` run said, or why it said nothing
/// usable.
type RunOutcome = Result<AuthStatus, CredentialError>;

/// The renewal state of one folder.
#[derive(Debug, Clone, Default)]
struct FlightState {
    /// Bumped with every published outcome.
    generation: u64,
    /// A `claude auth status` is running in the folder (its reaper holds
    /// it): no other may start.
    running: bool,
    /// The running one overstayed [`RENEW_TIMEOUT`] and its outcome was
    /// given up on; callers fail fast until it is reaped.
    overrun: bool,
    /// The latest published outcome.
    last: Option<RunOutcome>,
}

/// The renewal of one folder, shared by every [`Credentials`] in the
/// process that reads it (two engines in one process, say).
struct Flight {
    state: watch::Sender<FlightState>,
}

impl Default for Flight {
    fn default() -> Self {
        Self {
            state: watch::Sender::new(FlightState::default()),
        }
    }
}

impl Flight {
    /// Publish `outcome`; `done`: the run has been reaped.
    fn publish(&self, outcome: RunOutcome, done: bool) {
        self.state.send_modify(|state| {
            state.generation += 1;
            state.last = Some(outcome);
            if done {
                state.running = false;
                state.overrun = false;
            } else {
                state.overrun = true;
            }
        });
    }
}

static FLIGHTS: LazyLock<Mutex<HashMap<PathBuf, Arc<Flight>>>> = LazyLock::new(Default::default);

fn flight_for(dir: &Path) -> Arc<Flight> {
    FLIGHTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(dir.to_path_buf())
        .or_default()
        .clone()
}

/// The longest an account's sign-in may take (the person finishes it in the
/// browser).
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Add an account: run the official `claude auth login` with
/// `CLAUDE_CONFIG_DIR` naming `folder` (created), and say which account
/// the folder holds once it exits. The CLI signs in and writes the files;
/// nothing here reads or writes a token.
pub async fn sign_in(
    folder: &ClaudeConfigDir,
    claude: Option<&Path>,
) -> Result<ClaudeAccount, CredentialError> {
    let claude = match claude {
        Some(path) => path.to_path_buf(),
        None => crate::claude::resolve_claude_executable().ok_or_else(|| {
            CredentialError::SignInFailed("the `claude` command isn't installed".into())
        })?,
    };
    tokio::fs::create_dir_all(&folder.dir)
        .await
        .map_err(|error| CredentialError::SignInFailed(error.to_string()))?;
    let mut command = tokio::process::Command::new(&claude);
    command
        .args(["auth", "login"])
        .env("CLAUDE_CONFIG_DIR", &folder.dir)
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        tracing::warn!(target: "zeron_harness::claude_cloud", %error, "claude auth login could not run");
        CredentialError::SignInFailed("`claude auth login` could not run".into())
    })?;
    let status = match tokio::time::timeout(SIGN_IN_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(CredentialError::SignInFailed(error.to_string())),
        Err(_) => {
            let _ = child.kill().await;
            return Err(CredentialError::SignInFailed("it took too long".into()));
        }
    };
    if !status.success() {
        return Err(CredentialError::SignInFailed(format!(
            "`claude auth login` exited with {status}"
        )));
    }
    Credentials::new(folder.clone(), None)
        .account()
        .await
        .map_err(|_| CredentialError::SignInFailed("no account was signed in".into()))
}

/// What a caller sees when it arrives while an overdue run is being reaped.
const STILL_RUNNING: &str = "an earlier `claude auth status` is still running";

/// One login folder, read on demand. Holds no credential itself.
pub struct Credentials {
    folder: ClaudeConfigDir,
    /// The `claude` the renewal runs; `None` resolves it like the Claude
    /// Code harness does.
    claude: Option<PathBuf>,
    /// Single-flight renewal per folder.
    flight: Arc<Flight>,
}

impl Credentials {
    pub fn new(folder: ClaudeConfigDir, claude: Option<PathBuf>) -> Self {
        let flight = flight_for(&folder.dir);
        Self {
            folder,
            claude,
            flight,
        }
    }

    pub fn folder(&self) -> &ClaudeConfigDir {
        &self.folder
    }

    fn label(&self) -> String {
        self.folder.dir.display().to_string()
    }

    /// The account the folder is signed in to now (a file read).
    pub async fn account(&self) -> Result<ClaudeAccount, CredentialError> {
        let file: AccountFile = read_json(&self.folder.account_file)
            .await
            .map_err(|()| CredentialError::Unreadable(self.label()))?
            .ok_or_else(|| CredentialError::NoLogin(self.label()))?;
        let account = file
            .oauth_account
            .ok_or_else(|| CredentialError::NoLogin(self.label()))?;
        let account_uuid = account
            .account_uuid
            .filter(|id| !id.is_empty())
            .ok_or_else(|| CredentialError::NoLogin(self.label()))?;
        Ok(ClaudeAccount {
            account_uuid,
            email: account.email.unwrap_or_default(),
            org_id: account.org_id.unwrap_or_default(),
        })
    }

    /// The folder's token. Its account was checked already, so a missing
    /// token means that login is signed out.
    async fn read_token(&self) -> Result<AccessToken, CredentialError> {
        let file: CredentialsFile = read_json(&self.folder.credentials_file())
            .await
            .map_err(|()| CredentialError::Unreadable(self.label()))?
            .ok_or(CredentialError::LoggedOut)?;
        let entry = file.oauth.ok_or(CredentialError::LoggedOut)?;
        let value = entry
            .access_token
            .filter(|t| !t.is_empty())
            .ok_or(CredentialError::LoggedOut)?;
        AccessToken::new(&value, entry.expires_at)
            .ok_or_else(|| CredentialError::Unreadable(self.label()))
    }

    async fn check_account(&self, expected: &ClaudeAccount) -> Result<(), CredentialError> {
        let signed_in = self.account().await?;
        if signed_in.account_uuid != expected.account_uuid {
            return Err(CredentialError::mismatch(expected));
        }
        Ok(())
    }

    /// The access token for `expected`'s account, read from the folder now.
    /// The folder's account is checked first; a token near expiry is renewed
    /// by the CLI and read again.
    pub async fn token_for(
        &self,
        expected: &ClaudeAccount,
    ) -> Result<AccessToken, CredentialError> {
        self.check_account(expected).await?;
        let token = self.read_token().await?;
        if !token.expiring(now_ms()) {
            return Ok(token);
        }
        let stale = token.fingerprint();
        drop(token);
        self.renew(expected, stale).await
    }

    /// Have the CLI renew the login whose token has fingerprint `stale`
    /// (near expiry, or refused with a 401), check the account again and
    /// read the token anew. Whether the API takes the new one is the
    /// caller's to find out.
    pub async fn renew(
        &self,
        expected: &ClaudeAccount,
        stale: u64,
    ) -> Result<AccessToken, CredentialError> {
        self.renewal(expected, stale).await?;
        self.check_account(expected).await?;
        let token = self.read_token().await?;
        if token.expiring(now_ms()) {
            return Err(CredentialError::Expired);
        }
        Ok(token)
    }

    /// Have the CLI refresh the folder's token, unless the file already
    /// holds a fresh token other than `stale` (someone renewed it). A caller
    /// arriving while a run is in flight waits for its outcome; one arriving
    /// while an overdue run is being reaped fails at once.
    async fn renewal(&self, expected: &ClaudeAccount, stale: u64) -> Result<(), CredentialError> {
        let mut state = self.flight.state.subscribe();
        loop {
            let now = state.borrow_and_update().clone();
            if now.overrun {
                return Err(CredentialError::RenewFailed(STILL_RUNNING.into()));
            }
            if now.running {
                return self
                    .outcome_after(&mut state, now.generation, expected)
                    .await;
            }
            if let Ok(token) = self.read_token().await
                && token.fingerprint() != stale
                && !token.expiring(now_ms())
            {
                return Ok(());
            }
            // Claim the folder, then start the run with no await between,
            // so a cancelled caller never strands a claim or a child.
            let mut claimed = None;
            self.flight.state.send_if_modified(|flight| {
                if flight.running || flight.overrun {
                    return false;
                }
                flight.running = true;
                claimed = Some(flight.generation);
                true
            });
            let Some(generation) = claimed else {
                continue;
            };
            if let Err(error) = self.start_run() {
                self.flight.publish(Err(error), true);
            }
            return self.outcome_after(&mut state, generation, expected).await;
        }
    }

    /// The first outcome published after `generation`, for `expected`.
    async fn outcome_after(
        &self,
        state: &mut watch::Receiver<FlightState>,
        generation: u64,
        expected: &ClaudeAccount,
    ) -> Result<(), CredentialError> {
        loop {
            {
                let now = state.borrow_and_update();
                if now.generation > generation {
                    return match &now.last {
                        Some(Ok(status)) => status.verdict(expected),
                        Some(Err(error)) => Err(error.clone()),
                        None => Err(CredentialError::RenewFailed(STILL_RUNNING.into())),
                    };
                }
            }
            if state.changed().await.is_err() {
                return Err(CredentialError::RenewFailed(STILL_RUNNING.into()));
            }
        }
    }

    /// Spawn `claude auth status --json` in the folder and hand it to its
    /// reaper at once.
    fn start_run(&self) -> Result<(), CredentialError> {
        let claude = match &self.claude {
            Some(path) => path.clone(),
            None => crate::claude::resolve_claude_executable().ok_or_else(|| {
                CredentialError::RenewFailed("the `claude` command isn't installed".into())
            })?,
        };
        let mut command = tokio::process::Command::new(&claude);
        command
            .args(["auth", "status", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Never killed mid-refresh: see `reap`.
            .kill_on_drop(false);
        match &self.folder.env {
            Some(dir) => command.env("CLAUDE_CONFIG_DIR", dir),
            None => command.env_remove("CLAUDE_CONFIG_DIR"),
        };
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command.spawn().map_err(|error| {
            tracing::warn!(target: "zeron_harness::claude_cloud", %error, "claude auth status could not run");
            CredentialError::RenewFailed("`claude auth status` could not run".into())
        })?;
        tokio::spawn(reap(child, self.folder.dir.clone(), self.flight.clone()));
        Ok(())
    }
}

/// Own one `claude auth status` from spawn to exit: drain its stdout and
/// stderr to the end (closing them early could fail the CLI's writes before
/// it saves the token) and publish its outcome. After [`RENEW_TIMEOUT`] the
/// outcome is given up on (callers get [`CredentialError::RenewFailed`])
/// but the folder stays claimed until the CLI exits, so no second run
/// starts beside it. It is never killed mid-refresh: one still running
/// after [`refresh_lock::SETTLE_MAX`] more is stopped only once no refresh
/// is in flight in its folder.
///
/// The reaper is a task detached on the runtime, independent of whichever
/// caller started the run (a caller that is cancelled leaves it running).
/// It outlives an engine's shutdown deliberately and ends with the runtime;
/// a CLI still running then is left to finish on its own, never killed.
async fn reap(mut child: tokio::process::Child, dir: PathBuf, flight: Arc<Flight>) {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let finished = {
        let work = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let (_, _, status) = tokio::join!(
                async {
                    if let Some(pipe) = stdout.as_mut() {
                        let _ = pipe.read_to_end(&mut out).await;
                    }
                },
                async {
                    if let Some(pipe) = stderr.as_mut() {
                        let _ = pipe.read_to_end(&mut err).await;
                    }
                },
                child.wait(),
            );
            (out, status)
        };
        tokio::pin!(work);
        let deadline = tokio::time::sleep(RENEW_TIMEOUT);
        tokio::pin!(deadline);
        let mut overrun = false;
        loop {
            tokio::select! {
                result = &mut work => break Some(result),
                () = &mut deadline => {
                    if overrun {
                        break None;
                    }
                    overrun = true;
                    tracing::warn!(target: "zeron_harness::claude_cloud", "claude auth status is taking too long; giving up on it");
                    flight.publish(
                        Err(CredentialError::RenewFailed(
                            "`claude auth status` didn't answer in time".into(),
                        )),
                        false,
                    );
                    deadline
                        .as_mut()
                        .reset(tokio::time::Instant::now() + refresh_lock::SETTLE_MAX);
                }
            }
        }
    };
    let outcome = match finished {
        Some((out, status)) => read_outcome(&out, status),
        None => {
            refresh_lock::settle(&dir, refresh_lock::SETTLE_MAX).await;
            let _ = child.kill().await;
            Err(CredentialError::RenewFailed(
                "`claude auth status` never finished".into(),
            ))
        }
    };
    flight.publish(outcome, true);
}

/// What a finished run printed and how it exited. The CLI exits 1 when it
/// is signed out (and says so); any other failure, or output Zeron can't
/// read, is a failed renewal.
fn read_outcome(out: &[u8], status: std::io::Result<std::process::ExitStatus>) -> RunOutcome {
    let parsed: Option<AuthStatus> = serde_json::from_slice(out).ok();
    let exit = match &status {
        Ok(status) if status.success() => None,
        Ok(status) => Some(
            status
                .code()
                .map_or("a signal".to_owned(), |c| format!("code {c}")),
        ),
        Err(error) => Some(error.to_string()),
    };
    match (parsed, exit) {
        (Some(status), _) if status.logged_in == Some(false) => Ok(status),
        (Some(status), None) => Ok(status),
        (_, Some(exit)) => Err(CredentialError::RenewFailed(format!(
            "`claude auth status` failed ({exit})"
        ))),
        (None, None) => Err(CredentialError::RenewFailed(
            "`claude auth status` printed nothing Zeron can read".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(uuid: &str) -> ClaudeAccount {
        ClaudeAccount {
            account_uuid: uuid.into(),
            email: "someone@example.test".into(),
            org_id: "org-test".into(),
        }
    }

    fn write_login(dir: &Path, uuid: &str, token: &str, expires_at: i64) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(CREDENTIALS_FILE),
            serde_json::json!({"claudeAiOauth": {
                "accessToken": token, "refreshToken": "never-read", "expiresAt": expires_at,
            }})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join(ACCOUNT_FILE),
            serde_json::json!({"oauthAccount": {
                "accountUuid": uuid, "emailAddress": "someone@example.test",
                "organizationUuid": "org-test",
            }, "projects": {}})
            .to_string(),
        )
        .unwrap();
    }

    fn later() -> i64 {
        now_ms() + 3_600_000
    }

    #[tokio::test]
    async fn reads_the_token_for_the_folders_own_account_on_every_call() {
        let dir = tempfile::tempdir().unwrap();
        write_login(dir.path(), "acct-a", "token-one", later());
        let creds = Credentials::new(ClaudeConfigDir::at(dir.path()), None);
        let token = creds.token_for(&account("acct-a")).await.unwrap();
        assert_eq!(token.authorization(), "Bearer token-one");
        // A new token on disk is what the next read returns: nothing cached.
        write_login(dir.path(), "acct-a", "token-two", later());
        let token = creds.token_for(&account("acct-a")).await.unwrap();
        assert_eq!(token.authorization(), "Bearer token-two");
        assert_eq!(format!("{token:?}"), "AccessToken(<redacted>)");
        // The header value is sensitive: the HTTP stack redacts it.
        let header = token.authorization();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("token-two"));
    }

    #[tokio::test]
    async fn refuses_a_folder_signed_in_to_another_account() {
        let dir = tempfile::tempdir().unwrap();
        write_login(dir.path(), "acct-b", "token-b", later());
        let creds = Credentials::new(ClaudeConfigDir::at(dir.path()), None);
        assert_eq!(
            creds.token_for(&account("acct-a")).await.unwrap_err(),
            CredentialError::AccountMismatch {
                email: "someone@example.test".into()
            }
        );
    }

    #[tokio::test]
    async fn rereads_a_torn_file_once_after_200ms() {
        let dir = tempfile::tempdir().unwrap();
        write_login(dir.path(), "acct-a", "token-one", later());
        let file = dir.path().join(CREDENTIALS_FILE);
        std::fs::write(&file, "{\"claudeAiOauth\": {\"accessT").unwrap();
        let fixed = file.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            std::fs::write(
                &fixed,
                serde_json::json!({"claudeAiOauth": {"accessToken": "token-fixed", "expiresAt": later()}})
                    .to_string(),
            )
            .unwrap();
        });
        let creds = Credentials::new(ClaudeConfigDir::at(dir.path()), None);
        let token = creds.token_for(&account("acct-a")).await.unwrap();
        writer.await.unwrap();
        assert_eq!(token.authorization(), "Bearer token-fixed");

        // Still torn on the second read: unreadable, not a guess.
        std::fs::write(&file, "not json").unwrap();
        assert!(matches!(
            creds.token_for(&account("acct-a")).await.unwrap_err(),
            CredentialError::Unreadable(_)
        ));
    }

    #[tokio::test]
    async fn a_missing_login_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let creds = Credentials::new(ClaudeConfigDir::at(dir.path()), None);
        assert!(matches!(
            creds.account().await.unwrap_err(),
            CredentialError::NoLogin(_)
        ));
    }

    #[test]
    fn the_default_login_keeps_its_account_beside_the_folder() {
        let folder = ClaudeConfigDir::at("/x/login");
        assert_eq!(
            folder.account_file(),
            Path::new("/x/login").join(ACCOUNT_FILE)
        );
        assert_eq!(
            folder.credentials_file(),
            Path::new("/x/login").join(CREDENTIALS_FILE)
        );
    }
}
