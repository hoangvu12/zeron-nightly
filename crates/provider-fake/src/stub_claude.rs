//! A stub `claude` CLI that answers `auth status --json` from files the test
//! writes, so it can play renewal, a logged-out login or another account:
//!
//! - `status.json`: what it prints (default: logged in as the given account);
//! - `exit_code`: its exit code (default 0);
//! - `renew.credentials.json`: if present, copied over
//!   `$CLAUDE_CONFIG_DIR/.credentials.json` first (the CLI refreshing).
//!
//! Every call is logged to `calls.log` (`<args>|<CLAUDE_CONFIG_DIR>`).
//! Windows gets a `claude.cmd`, everything else a `claude` sh script.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::FakeAccount;

/// One recorded invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubCall {
    pub args: String,
    /// `CLAUDE_CONFIG_DIR` as the stub saw it (empty when unset).
    pub config_dir: String,
}

pub struct StubClaude {
    dir: PathBuf,
    path: PathBuf,
}

#[cfg(windows)]
const SCRIPT: &str = "@echo off\r\n\
setlocal\r\n\
set \"STUB=%~dp0\"\r\n\
>>\"%STUB%calls.log\" echo %*^|%CLAUDE_CONFIG_DIR%\r\n\
if not \"%~1 %~2 %~3\"==\"auth status --json\" (\r\n\
  echo unsupported 1>&2\r\n\
  exit /b 2\r\n\
)\r\n\
if exist \"%STUB%renew.credentials.json\" if defined CLAUDE_CONFIG_DIR copy /y \"%STUB%renew.credentials.json\" \"%CLAUDE_CONFIG_DIR%\\.credentials.json\" >nul\r\n\
type \"%STUB%status.json\"\r\n\
set CODE=0\r\n\
if exist \"%STUB%exit_code\" set /p CODE=<\"%STUB%exit_code\"\r\n\
exit /b %CODE%\r\n";

#[cfg(not(windows))]
const SCRIPT: &str = r#"#!/bin/sh
STUB="$(dirname "$0")"
printf '%s|%s\n' "$*" "$CLAUDE_CONFIG_DIR" >> "$STUB/calls.log"
if [ "$1 $2 $3" != "auth status --json" ]; then
  echo unsupported >&2
  exit 2
fi
if [ -f "$STUB/renew.credentials.json" ] && [ -n "$CLAUDE_CONFIG_DIR" ]; then
  cp "$STUB/renew.credentials.json" "$CLAUDE_CONFIG_DIR/.credentials.json"
fi
cat "$STUB/status.json"
if [ -f "$STUB/exit_code" ]; then exit "$(cat "$STUB/exit_code")"; fi
exit 0
"#;

impl StubClaude {
    /// Install the stub in `dir` (created), logged in as `account`.
    pub fn install(dir: impl Into<PathBuf>, account: &FakeAccount) -> Self {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).expect("stub dir");
        let path = if cfg!(windows) {
            dir.join("claude.cmd")
        } else {
            dir.join("claude")
        };
        std::fs::write(&path, SCRIPT).expect("write stub claude");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let stub = Self { dir, path };
        stub.set_status(json!({
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": account.email,
            "orgId": account.org_id,
            "orgName": "Fake Org",
            "subscriptionType": "team",
        }));
        stub
    }

    /// The executable to run.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_status(&self, status: Value) {
        std::fs::write(self.dir.join("status.json"), status.to_string()).unwrap();
    }

    pub fn set_exit_code(&self, code: i32) {
        std::fs::write(self.dir.join("exit_code"), code.to_string()).unwrap();
    }

    /// On its next runs the stub "refreshes" the login: it writes these
    /// bytes as the folder's `.credentials.json`.
    pub fn renew_with(&self, credentials_json: &str) {
        std::fs::write(self.dir.join("renew.credentials.json"), credentials_json).unwrap();
    }

    pub fn calls(&self) -> Vec<StubCall> {
        std::fs::read_to_string(self.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let (args, dir) = line.rsplit_once('|').unwrap_or((line, ""));
                StubCall {
                    args: args.trim().to_owned(),
                    config_dir: dir.trim().to_owned(),
                }
            })
            .collect()
    }
}
