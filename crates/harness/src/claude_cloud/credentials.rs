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
//! back to writing in place). A token within 5 minutes of expiry triggers the
//! CLI's own refresh by running `claude auth status --json` in the folder
//! (single-flight per folder, 10 s timeout), then the files are read again.
//! The sign-in states built on that (expired, logged out, mismatched status)
//! belong to ticket #7; this module keeps the hook minimal.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

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
/// accident.
pub struct AccessToken {
    value: String,
    expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl AccessToken {
    /// The `Authorization` header value.
    pub(crate) fn bearer(&self) -> String {
        format!("Bearer {}", self.value)
    }

    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    fn expiring(&self, now_ms: i64) -> bool {
        self.expires_at_ms
            .is_some_and(|at| at - now_ms < RENEW_WITHIN.as_millis() as i64)
    }
}

/// Why no token could be read. Never carries a credential.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("This device isn't signed in to Claude (no login in {0}). Sign in with `claude`.")]
    NoLogin(String),
    #[error("The Claude login in {0} could not be read.")]
    Unreadable(String),
    #[error("This device is signed in to Claude as a different account than this chat's.")]
    AccountMismatch,
    #[error("The Claude login has expired. Sign in again with `claude`.")]
    Expired,
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

/// `claude auth status --json`, the fields the renewal reads.
#[derive(Deserialize)]
struct AuthStatus {
    #[serde(rename = "loggedIn", default)]
    logged_in: Option<bool>,
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "orgId", default)]
    org_id: Option<String>,
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

/// One login folder, read on demand. Holds no credential itself.
pub struct Credentials {
    folder: ClaudeConfigDir,
    /// The `claude` the renewal runs; `None` resolves it like the Claude
    /// Code harness does.
    claude: Option<PathBuf>,
    /// Single-flight renewal per folder.
    renewing: tokio::sync::Mutex<()>,
}

impl Credentials {
    pub fn new(folder: ClaudeConfigDir, claude: Option<PathBuf>) -> Self {
        Self {
            folder,
            claude,
            renewing: tokio::sync::Mutex::new(()),
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
        Ok(AccessToken {
            value,
            expires_at_ms: entry.expires_at,
        })
    }

    async fn check_account(&self, expected: &ClaudeAccount) -> Result<(), CredentialError> {
        let signed_in = self.account().await?;
        if signed_in.account_uuid != expected.account_uuid {
            return Err(CredentialError::AccountMismatch);
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
        drop(token);
        self.renew(expected).await?;
        self.check_account(expected).await?;
        let token = self.read_token().await?;
        if token.expiring(now_ms()) {
            return Err(CredentialError::Expired);
        }
        Ok(token)
    }

    /// Have the CLI refresh the folder's token (`claude auth status --json`
    /// in the folder). Single-flight: a caller arriving while another runs
    /// waits for it, then finds the fresh token on its own read.
    async fn renew(&self, expected: &ClaudeAccount) -> Result<(), CredentialError> {
        let _guard = self.renewing.lock().await;
        // Someone else's renewal may already have done it.
        if let Ok(token) = self.read_token().await
            && !token.expiring(now_ms())
        {
            return Ok(());
        }
        let claude = match &self.claude {
            Some(path) => path.clone(),
            None => crate::claude::resolve_claude_executable().ok_or(CredentialError::Expired)?,
        };
        let mut command = tokio::process::Command::new(&claude);
        command
            .args(["auth", "status", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match &self.folder.env {
            Some(dir) => command.env("CLAUDE_CONFIG_DIR", dir),
            None => command.env_remove("CLAUDE_CONFIG_DIR"),
        };
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = match tokio::time::timeout(RENEW_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                tracing::warn!(target: "zeron_harness::claude_cloud", %error, "claude auth status could not run");
                return Err(CredentialError::Expired);
            }
            Err(_) => {
                tracing::warn!(target: "zeron_harness::claude_cloud", "claude auth status timed out");
                return Err(CredentialError::Expired);
            }
        };
        let status: Option<AuthStatus> = serde_json::from_slice(&output.stdout).ok();
        match status {
            Some(status) if status.logged_in == Some(false) => Err(CredentialError::Expired),
            Some(status)
                if status
                    .email
                    .as_deref()
                    .is_some_and(|email| !expected.email.is_empty() && email != expected.email)
                    || status.org_id.as_deref().is_some_and(|org| {
                        !expected.org_id.is_empty() && org != expected.org_id
                    }) =>
            {
                Err(CredentialError::AccountMismatch)
            }
            _ => Ok(()),
        }
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
        assert_eq!(token.bearer(), "Bearer token-one");
        // A new token on disk is what the next read returns: nothing cached.
        write_login(dir.path(), "acct-a", "token-two", later());
        let token = creds.token_for(&account("acct-a")).await.unwrap();
        assert_eq!(token.bearer(), "Bearer token-two");
        assert_eq!(format!("{token:?}"), "AccessToken(<redacted>)");
    }

    #[tokio::test]
    async fn refuses_a_folder_signed_in_to_another_account() {
        let dir = tempfile::tempdir().unwrap();
        write_login(dir.path(), "acct-b", "token-b", later());
        let creds = Credentials::new(ClaudeConfigDir::at(dir.path()), None);
        assert_eq!(
            creds.token_for(&account("acct-a")).await.unwrap_err(),
            CredentialError::AccountMismatch
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
        assert_eq!(token.bearer(), "Bearer token-fixed");

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
