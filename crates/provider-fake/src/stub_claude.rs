//! A stub `claude` CLI that answers `auth status --json` from files the test
//! writes, so it can play renewal, a logged-out login, another account or a
//! CLI that hangs:
//!
//! - `status.json`: what it prints (default: logged in as the given account;
//!   [`StubClaude::report_account`], [`StubClaude::log_out`]);
//! - `exit_code`: its exit code (default 0);
//! - `delay_secs`: whole seconds it waits before doing anything else
//!   ([`StubClaude::set_delay`]);
//! - `renew.credentials.json`: if present, copied over
//!   `$CLAUDE_CONFIG_DIR/.credentials.json` after the wait (the CLI
//!   refreshing; [`StubClaude::renew_with`]).
//!
//! - `login.claude.json` / `login.credentials.json`: what `auth login`
//!   writes into `$CLAUDE_CONFIG_DIR` (the person finishing the sign-in in
//!   the browser; [`StubClaude::sign_in_as`]). Without them it fails.
//!
//! Every call is logged to `calls.log` (`<args>|<CLAUDE_CONFIG_DIR>`) as it
//! starts ([`StubClaude::calls`], [`StubClaude::call_count`]). Windows gets a
//! `claude.cmd`, everything else a `claude` sh script.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
if \"%~1 %~2\"==\"auth login\" goto login\r\n\
if not \"%~1 %~2 %~3\"==\"auth status --json\" (\r\n\
  echo unsupported 1>&2\r\n\
  exit /b 2\r\n\
)\r\n\
set DELAY=0\r\n\
if exist \"%STUB%delay_secs\" set /p DELAY=<\"%STUB%delay_secs\"\r\n\
set /a PINGS=DELAY+1\r\n\
if not \"%DELAY%\"==\"0\" ping -n %PINGS% 127.0.0.1 >nul\r\n\
if exist \"%STUB%renew.credentials.json\" if defined CLAUDE_CONFIG_DIR copy /y \"%STUB%renew.credentials.json\" \"%CLAUDE_CONFIG_DIR%\\.credentials.json\" >nul\r\n\
type \"%STUB%status.json\"\r\n\
set CODE=0\r\n\
if exist \"%STUB%exit_code\" set /p CODE=<\"%STUB%exit_code\"\r\n\
exit /b %CODE%\r\n\
:login\r\n\
if not defined CLAUDE_CONFIG_DIR exit /b 3\r\n\
if not exist \"%STUB%login.claude.json\" exit /b 1\r\n\
if not exist \"%CLAUDE_CONFIG_DIR%\" mkdir \"%CLAUDE_CONFIG_DIR%\"\r\n\
copy /y \"%STUB%login.claude.json\" \"%CLAUDE_CONFIG_DIR%\\.claude.json\" >nul\r\n\
copy /y \"%STUB%login.credentials.json\" \"%CLAUDE_CONFIG_DIR%\\.credentials.json\" >nul\r\n\
exit /b 0\r\n";

#[cfg(not(windows))]
const SCRIPT: &str = r#"#!/bin/sh
STUB="$(dirname "$0")"
printf '%s|%s\n' "$*" "$CLAUDE_CONFIG_DIR" >> "$STUB/calls.log"
if [ "$1 $2" = "auth login" ]; then
  [ -n "$CLAUDE_CONFIG_DIR" ] || exit 3
  [ -f "$STUB/login.claude.json" ] || exit 1
  mkdir -p "$CLAUDE_CONFIG_DIR"
  cp "$STUB/login.claude.json" "$CLAUDE_CONFIG_DIR/.claude.json"
  cp "$STUB/login.credentials.json" "$CLAUDE_CONFIG_DIR/.credentials.json"
  exit 0
fi
if [ "$1 $2 $3" != "auth status --json" ]; then
  echo unsupported >&2
  exit 2
fi
if [ -f "$STUB/delay_secs" ]; then sleep "$(cat "$STUB/delay_secs")"; fi
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
        stub.report_account(account);
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

    /// Report a login as `account`, exit code 0 (what a signed-in CLI
    /// prints; another account than the chat's plays a mismatch).
    pub fn report_account(&self, account: &FakeAccount) {
        self.set_status(json!({
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": account.email,
            "orgId": account.org_id,
            "orgName": "Fake Org",
            "subscriptionType": "team",
        }));
        self.set_exit_code(0);
    }

    /// Report a signed-out login, as the CLI does: `loggedIn: false`, exit 1.
    pub fn log_out(&self) {
        self.set_status(json!({
            "loggedIn": false,
            "authMethod": "none",
            "apiProvider": "firstParty",
        }));
        self.set_exit_code(1);
    }

    /// Wait `delay` (rounded up to whole seconds) before answering, as a CLI
    /// stuck on a slow refresh would.
    pub fn set_delay(&self, delay: Duration) {
        let secs = delay.as_millis().div_ceil(1000);
        std::fs::write(self.dir.join("delay_secs"), secs.to_string()).unwrap();
    }

    /// On its next runs the stub "refreshes" the login: it writes these
    /// bytes as the folder's `.credentials.json`.
    pub fn renew_with(&self, credentials_json: &str) {
        std::fs::write(self.dir.join("renew.credentials.json"), credentials_json).unwrap();
    }

    /// From now on `auth login` signs its folder in as `account` with
    /// `token` (the person finishing the sign-in in the browser).
    pub fn sign_in_as(&self, account: &FakeAccount, token: &str) {
        let staged = crate::ClaudeLogin::create(self.dir.join("staged-login"), account, token);
        std::fs::copy(
            staged.dir().join(".claude.json"),
            self.dir.join("login.claude.json"),
        )
        .unwrap();
        std::fs::copy(
            staged.credentials_file(),
            self.dir.join("login.credentials.json"),
        )
        .unwrap();
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

    /// How many times the stub has been run.
    pub fn call_count(&self) -> usize {
        self.calls().len()
    }
}
