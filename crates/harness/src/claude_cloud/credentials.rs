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
//! auth status --json` in the folder ([`Credentials::renew_rejected`]). At
//! most one runs per folder in this process at a time, and callers that
//! arrive while it runs share its outcome (single-flight). It has 10 s; a run
//! that overstays is left to finish rather than killed mid-refresh (that
//! would lose the CLI's rotated token, see [`crate::claude::refresh_lock`]).
//! The output's `loggedIn`, `email` and `orgId` are checked against the
//! chat's account, then the files are read again. Each failure says what the
//! chat shows ([`CredentialError::chat_state`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncReadExt;
use zeron_proto::provider_cloud::ProviderChatState;

/// The token file the CLI keeps in its config folder.
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
    #[error(
        "This device isn't signed in to Claude (no login in {0}). Sign in again with `claude`."
    )]
    NoLogin(String),
    #[error("The Claude login in {0} could not be read.")]
    Unreadable(String),
    /// The folder (or `auth status`) names another account than the chat's:
    /// this device can't host the chat. `email` is the chat's account.
    #[error(
        "This device is signed in to Claude as a different account than this chat's. Sign in as {email} to continue."
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
}

impl CredentialError {
    /// What a chat owned by the account with `email` shows for this
    /// failure; `None` for a passing problem (a file unreadable twice).
    pub fn chat_state(&self, email: &str) -> Option<ProviderChatState> {
        let email = email.to_owned();
        match self {
            Self::Unreadable(_) => None,
            Self::AccountMismatch { .. } => Some(ProviderChatState::SignInAs { email }),
            Self::NoLogin(_)
            | Self::Expired
            | Self::LoggedOut
            | Self::RenewFailed(_)
            | Self::Rejected => Some(ProviderChatState::SignInAgain { email }),
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
/// `{"loggedIn":true,"authMethod":"claude.ai","email":…,"orgId":…,…}`;
/// signed out: `{"loggedIn":false,"authMethod":"none",…}`, exit code 1.
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
    /// Whether the status allows acting for `expected`.
    fn verdict(&self, expected: &ClaudeAccount) -> Result<(), CredentialError> {
        if self.logged_in == Some(false) {
            return Err(CredentialError::LoggedOut);
        }
        let differs = |reported: &Option<String>, wanted: &str| {
            reported
                .as_deref()
                .is_some_and(|value| !wanted.is_empty() && !value.eq_ignore_ascii_case(wanted))
        };
        if differs(&self.email, &expected.email) || differs(&self.org_id, &expected.org_id) {
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

/// What one `claude auth status --json` run said: the status it printed
/// (`None`: nothing readable), or why it gave no answer.
type RunOutcome = Result<Option<AuthStatus>, CredentialError>;

/// The renewal of one folder, shared by every [`Credentials`] in the
/// process that reads it (two engines in one process, say).
#[derive(Default)]
struct Flight {
    /// Bumped after each run, so a caller that waited can tell a run
    /// finished meanwhile and take that run's outcome.
    finished: AtomicU64,
    /// Held while a run is in flight; keeps the last run's outcome.
    last: tokio::sync::Mutex<Option<RunOutcome>>,
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

    async fn read_token(&self) -> Result<AccessToken, CredentialError> {
        let file: CredentialsFile = read_json(&self.folder.credentials_file())
            .await
            .map_err(|()| CredentialError::Unreadable(self.label()))?
            .ok_or_else(|| CredentialError::NoLogin(self.label()))?;
        let entry = file
            .oauth
            .ok_or_else(|| CredentialError::NoLogin(self.label()))?;
        let value = entry
            .access_token
            .filter(|t| !t.is_empty())
            .ok_or_else(|| CredentialError::NoLogin(self.label()))?;
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
        self.renew_and_read(expected, stale).await
    }

    /// The API refused the token with fingerprint `rejected` (401): have the
    /// CLI renew the login, check the account again and read the token anew.
    /// Whether the API takes that one is the caller's retry to find out.
    pub async fn renew_rejected(
        &self,
        expected: &ClaudeAccount,
        rejected: u64,
    ) -> Result<AccessToken, CredentialError> {
        self.renew_and_read(expected, rejected).await
    }

    async fn renew_and_read(
        &self,
        expected: &ClaudeAccount,
        stale: u64,
    ) -> Result<AccessToken, CredentialError> {
        self.renew(expected, stale).await?;
        self.check_account(expected).await?;
        let token = self.read_token().await?;
        if token.expiring(now_ms()) {
            return Err(CredentialError::Expired);
        }
        Ok(token)
    }

    /// Have the CLI refresh the folder's token, unless the file already
    /// holds a fresh token other than `stale` (someone renewed it). A caller
    /// arriving while a run is in flight waits for it and takes its outcome.
    async fn renew(&self, expected: &ClaudeAccount, stale: u64) -> Result<(), CredentialError> {
        let seen = self.flight.finished.load(Ordering::SeqCst);
        let mut last = self.flight.last.lock().await;
        if self.flight.finished.load(Ordering::SeqCst) != seen
            && let Some(outcome) = last.as_ref()
        {
            return judge(outcome, expected);
        }
        if let Ok(token) = self.read_token().await
            && token.fingerprint() != stale
            && !token.expiring(now_ms())
        {
            return Ok(());
        }
        let outcome = self.run_auth_status().await;
        let verdict = judge(&outcome, expected);
        *last = Some(outcome);
        self.flight.finished.fetch_add(1, Ordering::SeqCst);
        verdict
    }

    /// One `claude auth status --json` in the folder, given at most
    /// [`RENEW_TIMEOUT`].
    async fn run_auth_status(&self) -> RunOutcome {
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
            .stderr(std::process::Stdio::null())
            // Never killed mid-refresh: see `finish_in_background`.
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
        let mut child = command.spawn().map_err(|error| {
            tracing::warn!(target: "zeron_harness::claude_cloud", %error, "claude auth status could not run");
            CredentialError::RenewFailed("`claude auth status` could not run".into())
        })?;
        let mut stdout = child.stdout.take();
        let finished = tokio::time::timeout(RENEW_TIMEOUT, async {
            let mut out = Vec::new();
            if let Some(stdout) = stdout.as_mut() {
                let _ = stdout.read_to_end(&mut out).await;
            }
            let _ = child.wait().await;
            out
        })
        .await;
        match finished {
            Ok(out) => Ok(serde_json::from_slice(&out).ok()),
            Err(_) => {
                tracing::warn!(target: "zeron_harness::claude_cloud", "claude auth status timed out");
                drop(stdout);
                tokio::spawn(finish_in_background(child, self.folder.dir.clone()));
                Err(CredentialError::RenewFailed(
                    "`claude auth status` didn't answer in time".into(),
                ))
            }
        }
    }
}

/// A run's outcome for one caller's account.
fn judge(outcome: &RunOutcome, expected: &ClaudeAccount) -> Result<(), CredentialError> {
    match outcome {
        Ok(Some(status)) => status.verdict(expected),
        // Nothing readable: the file read that follows decides.
        Ok(None) => Ok(()),
        Err(error) => Err(error.clone()),
    }
}

/// Let an overdue `claude auth status` finish: killing it mid-refresh would
/// lose the rotated token the CLI is saving. One still running after
/// [`crate::claude::refresh_lock::SETTLE_MAX`] is stopped once no refresh
/// is in flight in its folder.
async fn finish_in_background(mut child: tokio::process::Child, dir: PathBuf) {
    use crate::claude::refresh_lock;
    if tokio::time::timeout(refresh_lock::SETTLE_MAX, child.wait())
        .await
        .is_ok()
    {
        return;
    }
    refresh_lock::settle(&dir, refresh_lock::SETTLE_MAX).await;
    let _ = child.kill().await;
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
