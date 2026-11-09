//! A Claude CLI login folder with fake values, laid out the way the CLI
//! keeps one named by `CLAUDE_CONFIG_DIR`: `.credentials.json` (the OAuth
//! token) and `.claude.json` (`oauthAccount`, the signed-in account).

use std::path::{Path, PathBuf};

use serde_json::json;

/// A fake Claude account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeAccount {
    pub account_uuid: String,
    pub email: String,
    pub org_id: String,
}

impl FakeAccount {
    /// Account "a" (`a@example.test`).
    pub fn a() -> Self {
        Self {
            account_uuid: "11111111-1111-4111-8111-111111111111".into(),
            email: "a@example.test".into(),
            org_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        }
    }

    /// Account "b" (`b@example.test`), a different person.
    pub fn b() -> Self {
        Self {
            account_uuid: "22222222-2222-4222-8222-222222222222".into(),
            email: "b@example.test".into(),
            org_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
        }
    }
}

/// A login folder on disk.
#[derive(Debug, Clone)]
pub struct ClaudeLogin {
    dir: PathBuf,
}

impl ClaudeLogin {
    /// Sign `dir` in as `account` with `token`, valid for an hour.
    pub fn create(dir: impl Into<PathBuf>, account: &FakeAccount, token: &str) -> Self {
        let login = Self { dir: dir.into() };
        std::fs::create_dir_all(&login.dir).expect("login folder");
        login.set_account(account);
        login.set_token(token, in_ms(3_600_000));
        login
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.dir.join(".credentials.json")
    }

    /// Replace the token (as the CLI's refresh would).
    pub fn set_token(&self, token: &str, expires_at_ms: i64) {
        std::fs::write(
            self.credentials_file(),
            Self::credentials_json(token, expires_at_ms),
        )
        .expect("write credentials");
    }

    /// The bytes `.credentials.json` holds for `token`.
    pub fn credentials_json(token: &str, expires_at_ms: i64) -> String {
        json!({"claudeAiOauth": {
            "accessToken": token,
            "refreshToken": "fake-refresh-token-never-read",
            "expiresAt": expires_at_ms,
            "scopes": ["user:inference", "user:sessions:claude_code"],
            "subscriptionType": "team",
        }})
        .to_string()
    }

    /// Switch the folder to another account (as an account switcher would).
    pub fn set_account(&self, account: &FakeAccount) {
        std::fs::write(
            self.dir.join(".claude.json"),
            json!({
                "numStartups": 3,
                "oauthAccount": {
                    "accountUuid": account.account_uuid,
                    "emailAddress": account.email,
                    "organizationUuid": account.org_id,
                    "displayName": "Fake",
                },
                "projects": {},
            })
            .to_string(),
        )
        .expect("write account");
    }
}

/// Epoch ms, `delta` from now.
pub fn in_ms(delta: i64) -> i64 {
    chrono::Utc::now().timestamp_millis() + delta
}
