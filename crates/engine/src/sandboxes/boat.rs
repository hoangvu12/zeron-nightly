//! Boat (boat.dev) as a provider, on **the user's own** Boat account and API
//! key (spec #37, docs/design/cloud-byo.md "Boat specifics"). The only module
//! that knows Boat's URLs, fields, states, headers and error codes. It
//! mirrors the edge's Boat provider (`edge/src/cloud/providers/boat.ts`),
//! which Zeron Cloud uses with its operator account:
//!
//! - the lifecycle field is `sandbox.state`; `ready|idle|running` is up,
//!   `archiving` stopping, `archived` stopped, `error|cancelled` failed, the
//!   rest still coming up; an unknown sandbox (404) is gone;
//! - sandboxes are always created `noEnv: true`, so nothing of the account
//!   reaches them, with a time limit (`ttlSeconds`) after which Boat stops
//!   (never deletes) them; `PATCH` pushes it out ([`SandboxProvider::extend`]);
//! - `Idempotency-Key` makes create safe to ask again (same key, same body);
//! - `DELETE` needs `X-Ascii-Confirm-Delete: {id}`;
//! - commands (`POST …/commands`) take one shell line, answer when they end
//!   and are never retried by Boat;
//! - errors sort as the edge's do: retryable, config (the key or the
//!   account), not found, conflict, fatal; Boat's own `retryable` hint wins.
//!
//! Boat machines have systemd, so a detached command (Zeron's engine, from
//! the Ready step) runs as a system unit, as Zeron Cloud's installer does it
//! (`edge/src/cloud/install-script.ts`): it restarts when it stops and the
//! updater can restart it into a new version. Its environment, the lent
//! sign-in among it, lives in `/run` (memory), never on the sandbox's disk,
//! so a stopped sandbox's snapshot holds none of it; the unit starts only
//! when that file is there (a resumed sandbox waits for Ready again).
//!
//! The API key lives in this provider's [`KeySlot`] on the host device. It
//! is sent to Boat as the bearer and nowhere else: never logged, never in a
//! sandbox.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Value, json};

use super::KeySlot;
use super::provider::{
    Account, Command, CreateSandbox, Credential, ErrorKind, Exit, Ops, ProviderError, RunEvent,
    RunStream, SandboxProvider,
};

pub const BOAT_ID: &str = "boat";
const DEFAULT_BASE: &str = "https://boat.dev/api/v1";
/// The time limit a sandbox is created and resumed with: it stops after an
/// hour unless the host device pushes it out while a chat is busy. A trial
/// account allows at most two hours.
const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
/// How long create and start wait for the sandbox to come up. Boat has been
/// seen holding a resume in `provisioning` for 7 to 11 minutes.
const READY_DEADLINE: Duration = Duration::from_secs(12 * 60);
const POLL: Duration = Duration::from_secs(2);
/// A command's limit when it names none.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
/// Boat's own calls (not commands).
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Where the checkouts go: the sandbox user's home, as Zeron Cloud's.
const WORKDIR: &str = "/home/user";

/// The system unit Zeron's engine runs as on Boat; the updater restarts it
/// with sudo.
const SERVICE_UNIT: &str = zeron_update::SANDBOX_SERVICE_UNIT;
/// Its environment: in memory (tmpfs), never on the sandbox's disk.
const RUN_DIR: &str = "/run/zeron-sandbox";

pub struct BoatProvider {
    base: String,
    slot: KeySlot,
    http: reqwest::Client,
    ttl: Duration,
    poll: Duration,
    ready_deadline: Duration,
}

impl BoatProvider {
    /// Boat on the key kept in `slot` (none yet: not connected).
    pub fn new(slot: KeySlot) -> Self {
        Self {
            base: DEFAULT_BASE.to_owned(),
            slot,
            http: reqwest::Client::new(),
            ttl: DEFAULT_TTL,
            poll: POLL,
            ready_deadline: READY_DEADLINE,
        }
    }

    /// Another API root (tests, a Boat staging).
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }

    /// How often it reads a sandbox's state while waiting for it.
    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    fn key(&self) -> Result<String, ProviderError> {
        self.slot
            .get()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                ProviderError::new(
                    ErrorKind::Config,
                    "Boat isn't connected on this device: add a Boat API key in Settings › Sandboxes.",
                )
            })
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Value, ProviderError> {
        self.call_with(&self.key()?, method, path, body, headers, timeout)
            .await
    }

    async fn call_with(
        &self,
        key: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Value, ProviderError> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(key)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(timeout);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|err| {
            ProviderError::new(
                ErrorKind::Retryable,
                format!("Boat unreachable: {}", without_url(err)),
            )
        })?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|err| {
            ProviderError::new(
                ErrorKind::Retryable,
                format!("Boat's reply was cut off: {}", without_url(err)),
            )
        })?;
        let parsed: Option<Value> = if text.trim().is_empty() {
            Some(json!({}))
        } else {
            serde_json::from_str(&text).ok()
        };
        let refused = parsed.as_ref().is_some_and(|v| v["ok"] == json!(false));
        if !(200..300).contains(&status) || refused {
            let envelope = parsed.unwrap_or(Value::Null);
            let status = if (200..300).contains(&status) {
                502
            } else {
                status
            };
            return Err(boat_error(status, &envelope, &text));
        }
        parsed.ok_or_else(|| {
            ProviderError::new(
                ErrorKind::Retryable,
                "Boat answered something that isn't JSON",
            )
        })
    }

    async fn state(&self, sandbox: &str) -> Result<(State, Option<String>), ProviderError> {
        let reply = self
            .call(
                reqwest::Method::GET,
                &path(sandbox, ""),
                None,
                &[],
                CALL_TIMEOUT,
            )
            .await;
        let reply = match reply {
            Err(err) if err.kind == ErrorKind::NotFound => return Ok((State::Gone, None)),
            other => other?,
        };
        let sandbox = sandbox_of(&reply)?;
        let error = sandbox["error"]
            .as_str()
            .or(sandbox["healthReason"].as_str())
            .map(str::to_owned);
        Ok((
            State::of(sandbox["state"].as_str().unwrap_or_default()),
            error,
        ))
    }

    /// Wait until it's up.
    async fn ready(&self, sandbox: &str) -> Result<(), ProviderError> {
        let deadline = tokio::time::Instant::now() + self.ready_deadline;
        loop {
            match self.state(sandbox).await? {
                (State::Ready, _) => return Ok(()),
                (State::Gone, _) => return Err(gone(sandbox)),
                (State::Failed, error) => {
                    return Err(ProviderError::new(
                        ErrorKind::Conflict,
                        format!(
                            "the Boat sandbox failed: {}",
                            error.as_deref().unwrap_or("Boat didn't say why")
                        ),
                    ));
                }
                (State::Stopping | State::Stopped, _) => {
                    return Err(ProviderError::new(
                        ErrorKind::Conflict,
                        "the Boat sandbox stopped while it was starting",
                    ));
                }
                (State::Starting, _) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ProviderError::new(
                    ErrorKind::Retryable,
                    format!(
                        "the Boat sandbox didn't come up within {} minutes",
                        self.ready_deadline.as_secs() / 60
                    ),
                ));
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

/// A sandbox's state, as the edge normalizes Boat's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
    Gone,
}

impl State {
    fn of(boat: &str) -> Self {
        match boat {
            // `idle`/`running` reflect only Boat's own prompt queue: the
            // machine is up.
            "ready" | "idle" | "running" => State::Ready,
            "archiving" => State::Stopping,
            "archived" => State::Stopped,
            "error" | "cancelled" => State::Failed,
            // init, provisioning, provisioned, cloning, and anything new.
            _ => State::Starting,
        }
    }
}

#[async_trait]
impl SandboxProvider for BoatProvider {
    fn id(&self) -> &str {
        BOAT_ID
    }
    fn name(&self) -> &str {
        "Boat"
    }
    fn icon(&self) -> &str {
        "boat"
    }
    fn ops(&self) -> Ops {
        Ops::CREATE | Ops::START | Ops::STOP | Ops::DELETE | Ops::RUN | Ops::CONNECT | Ops::EXTEND
    }
    fn workdir(&self, _chat_id: &str) -> String {
        WORKDIR.to_owned()
    }

    async fn connect(&self, credential: Credential) -> Result<Account, ProviderError> {
        // Boat takes a pasted key only (no device sign-in).
        #[allow(unreachable_patterns)]
        let key = match &credential {
            Credential::Token(key) => key.trim(),
            _ => {
                return Err(ProviderError::new(
                    ErrorKind::Config,
                    "Boat connects with an API key",
                ));
            }
        };
        if key.is_empty() {
            return Err(ProviderError::new(
                ErrorKind::Config,
                "paste a Boat API key",
            ));
        }
        // Listing one sandbox needs only a key Boat accepts.
        self.call_with(
            key,
            reqwest::Method::GET,
            "/sandboxes?limit=1",
            None,
            &[],
            CALL_TIMEOUT,
        )
        .await?;
        self.slot.set(key).map_err(|err| {
            ProviderError::new(
                ErrorKind::Fatal,
                format!("couldn't keep the Boat key on this device: {err}"),
            )
        })?;
        Ok(Account {
            name: "Boat account".into(),
        })
    }

    async fn create(&self, request: CreateSandbox) -> Result<String, ProviderError> {
        let mut body = json!({
            "ttlSeconds": self.ttl.as_secs(),
            // Boat's rule, and ours: nothing of the account reaches the sandbox.
            "noEnv": true,
            "env": request.env,
        });
        if let Some(template) = &request.template {
            body["from"] = json!(template);
        }
        let reply = self
            .call(
                reqwest::Method::POST,
                "/sandboxes",
                Some(body),
                &[("idempotency-key", request.key.as_str())],
                CALL_TIMEOUT,
            )
            .await?;
        let id = sandbox_of(&reply)?["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        self.ready(&id).await?;
        Ok(id)
    }

    async fn start(&self, sandbox: &str) -> Result<(), ProviderError> {
        match self.state(sandbox).await?.0 {
            State::Ready => return Ok(()),
            State::Gone => return Err(gone(sandbox)),
            State::Starting => {}
            State::Stopping | State::Stopped | State::Failed => {
                let resumed = self
                    .call(
                        reqwest::Method::POST,
                        &path(sandbox, "/resume"),
                        Some(json!({ "ttlSeconds": self.ttl.as_secs() })),
                        &[],
                        CALL_TIMEOUT,
                    )
                    .await;
                match resumed {
                    // Already on its way up (a retry whose first reply was lost).
                    Err(err) if err.message.contains("sandbox_resuming") => {}
                    other => {
                        other?;
                    }
                }
            }
        }
        self.ready(sandbox).await
    }

    /// Never forced: a forced stop throws away what changed since the last
    /// good snapshot.
    async fn stop(&self, sandbox: &str) -> Result<(), ProviderError> {
        self.call(
            reqwest::Method::POST,
            &path(sandbox, "/stop"),
            Some(json!({})),
            &[],
            CALL_TIMEOUT,
        )
        .await
        .map(drop)
    }

    async fn delete(&self, sandbox: &str) -> Result<(), ProviderError> {
        self.call(
            reqwest::Method::DELETE,
            &path(sandbox, ""),
            None,
            &[("x-ascii-confirm-delete", sandbox)],
            CALL_TIMEOUT,
        )
        .await
        .map(drop)
    }

    async fn extend(&self, sandbox: &str, ttl: Duration) -> Result<(), ProviderError> {
        self.call(
            reqwest::Method::PATCH,
            &path(sandbox, ""),
            Some(json!({ "ttlSeconds": ttl.as_secs().max(1) })),
            &[],
            CALL_TIMEOUT,
        )
        .await
        .map(drop)
    }

    async fn run(&self, sandbox: &str, command: Command) -> Result<RunStream, ProviderError> {
        let line = if command.detach {
            service_line(&command)?
        } else {
            shell_line(&command)
        };
        let timeout = command.timeout.unwrap_or(COMMAND_TIMEOUT).as_secs().max(1);
        let reply = self
            .call(
                reqwest::Method::POST,
                &path(sandbox, "/commands"),
                Some(json!({ "command": line, "timeoutSeconds": timeout })),
                &[],
                Duration::from_secs(timeout) + CALL_TIMEOUT,
            )
            .await?;
        let mut events = Vec::new();
        for (field, event) in [
            ("stdout", RunEvent::Stdout as fn(Vec<u8>) -> RunEvent),
            ("stderr", RunEvent::Stderr),
        ] {
            if let Some(text) = reply[field].as_str().filter(|t| !t.is_empty()) {
                events.push(Ok(event(text.as_bytes().to_vec())));
            }
        }
        events.push(Ok(RunEvent::Exit(Exit {
            code: reply["exitCode"].as_i64().map(|c| c as i32),
            timed_out: reply["timedOut"] == json!(true),
        })));
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn path(sandbox: &str, suffix: &str) -> String {
    let id: String = sandbox
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("/sandboxes/{id}{suffix}")
}

fn gone(sandbox: &str) -> ProviderError {
    ProviderError::new(
        ErrorKind::NotFound,
        format!("the Boat sandbox {sandbox} is gone"),
    )
}

fn sandbox_of(reply: &Value) -> Result<&Value, ProviderError> {
    let sandbox = &reply["sandbox"];
    if sandbox["id"].is_string() {
        Ok(sandbox)
    } else {
        Err(ProviderError::new(
            ErrorKind::Retryable,
            "Boat's reply had no sandbox",
        ))
    }
}

/// reqwest's errors name the URL; ours carry no secret, but keep them short.
fn without_url(err: reqwest::Error) -> String {
    let timed_out = err.is_timeout();
    let text = err.without_url().to_string();
    if timed_out {
        format!("timed out ({text})")
    } else {
        text
    }
}

const CONFIG: &[&str] = &[
    "unauthorized",
    "forbidden",
    "api_key_action_forbidden",
    "trial_auto_stop_required",
    "trial_machine_class_not_allowed",
    "billing_required",
    "org_suspended",
];
const CAPACITY: &[&str] = &[
    "limit_reached",
    "member_limit_reached",
    "daily_limit_reached",
];

/// Boat's HTTP status and code → kind (the edge's `boatErrorKind`).
fn error_kind(status: u16, code: &str) -> ErrorKind {
    match (status, code) {
        (0 | 408 | 425, _) | (500.., _) => ErrorKind::Retryable,
        (409, "idempotency_in_progress" | "boat_starting" | "sandbox_starting") => {
            ErrorKind::Retryable
        }
        (400, "machine_not_running") | (429, "rate_limited") => ErrorKind::Retryable,
        (404, _) => ErrorKind::NotFound,
        (401..=403, _) => ErrorKind::Config,
        _ if CONFIG.contains(&code) => ErrorKind::Config,
        (409, _) => ErrorKind::Conflict,
        _ => ErrorKind::Fatal,
    }
}

/// A refusal no retry fixes, whatever else the reply hints: an expired disk,
/// or one too big for the machine (the edge's `boatFailure`).
fn lasting_failure(code: &str, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    code == "snapshot_data_expired"
        || code == "type_too_small"
        || message.contains("data was deleted")
        || (message.contains("of disk")
            && (message.contains("needs ") || message.contains("uses ")))
}

fn boat_error(status: u16, envelope: &Value, text: &str) -> ProviderError {
    let field = |name: &str| {
        envelope[name]
            .as_str()
            .or(envelope["error"][name].as_str())
            .map(str::to_owned)
    };
    let code = field("code").unwrap_or_else(|| format!("http_{status}"));
    let message = field("message").unwrap_or_else(|| {
        let text: String = text.chars().take(200).collect();
        if text.trim().is_empty() {
            format!("HTTP {status}")
        } else {
            text
        }
    });
    let action = envelope["error"]["details"]["action"].as_str();
    let hinted = envelope
        .get("retryable")
        .or(envelope["error"].get("retryable"))
        .and_then(Value::as_bool);
    let mut kind = error_kind(status, &code);
    match hinted {
        Some(true) => kind = ErrorKind::Retryable,
        Some(false) if kind == ErrorKind::Retryable => kind = ErrorKind::Conflict,
        _ => {}
    }
    if lasting_failure(&code, &message) {
        kind = ErrorKind::Conflict;
    }
    let said = match code.as_str() {
        "api_key_action_forbidden" => format!(
            "the Boat API key lacks {}",
            action.unwrap_or("a permission this needs")
        ),
        "unauthorized" => "Boat rejected the API key".to_owned(),
        "trial_auto_stop_required" => {
            "this Boat account needs sandboxes to stop within two hours (trial)".to_owned()
        }
        "billing_required" | "org_suspended" => format!("Boat billing: {message}"),
        c if CAPACITY.contains(&c) => format!("Boat is at capacity ({c}); try again later"),
        _ => message,
    };
    // The code stays in the message, so callers can match on it.
    ProviderError::new(kind, format!("{said} ({code})"))
}

/// `'…'` for POSIX sh.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// A command as one shell line: its folder, its environment, its words.
fn shell_line(command: &Command) -> String {
    let mut line = String::new();
    if let Some(cwd) = &command.cwd {
        line.push_str(&format!("cd {} && ", quote(cwd)));
    }
    line.push_str("exec ");
    if !command.env.is_empty() {
        line.push_str("env ");
        for (name, value) in &command.env {
            line.push_str(&quote(&format!("{name}={value}")));
            line.push(' ');
        }
    }
    let words: Vec<String> = command.args.iter().map(|a| quote(a)).collect();
    line.push_str(&words.join(" "));
    line
}

/// `"…"` for systemd (unit lines and environment files).
fn systemd_quote(value: &str) -> Result<String, ProviderError> {
    if value.contains(['\n', '\r', '\0']) {
        return Err(ProviderError::new(
            ErrorKind::Fatal,
            "a service's settings can't hold a line break",
        ));
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', r"\\")
            .replace('"', "\\\"")
            .replace('$', "$$")
            .replace('%', "%%")
    ))
}

fn environment_file(env: &BTreeMap<String, String>) -> Result<String, ProviderError> {
    let mut file = String::new();
    for (name, value) in env
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .chain([("ZERON_SERVICE_UNIT", SERVICE_UNIT)])
    {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(ProviderError::new(
                ErrorKind::Fatal,
                format!("{name:?} isn't an environment variable's name"),
            ));
        }
        // An environment file takes no `$`/`%` specifiers; only quotes and
        // backslashes are special inside its double quotes.
        let value = value.replace('\\', r"\\").replace('"', "\\\"");
        if value.contains(['\n', '\r', '\0']) {
            return Err(ProviderError::new(
                ErrorKind::Fatal,
                "an environment value can't hold a line break",
            ));
        }
        file.push_str(&format!("{name}=\"{value}\"\n"));
    }
    Ok(file)
}

/// A detached command as a systemd system unit, as Zeron Cloud runs its
/// engine: written, reloaded and (re)started, answering once it has.
/// Everything travels base64-encoded (no quoting hazards, and no secret in
/// clear on the command line); the environment goes to `/run`.
fn service_line(command: &Command) -> Result<String, ProviderError> {
    let Some((program, rest)) = command.args.split_first() else {
        return Err(ProviderError::new(ErrorKind::Fatal, "no command to run"));
    };
    let env = environment_file(&command.env)?;
    let args = rest
        .iter()
        .map(|a| systemd_quote(a))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let workdir = match &command.cwd {
        Some(cwd) => systemd_quote(cwd)?,
        None => "~".to_owned(),
    };
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let script = format!(
        r#"set -eu
unit={SERVICE_UNIT}
run={RUN_DIR}
user="$(id -un)"
prog="$(command -v {program})" || {{ echo "{program_shown} isn't installed in the sandbox" >&2; exit 127; }}
sudo install -d -m 0700 -o "$user" "$run"
umask 077
printf %s '{env}' | base64 -d >"$run/env.new"
mv -f "$run/env.new" "$run/env"
args="$(printf %s '{args}' | base64 -d)"
t="$(mktemp)"
{{
  echo "[Unit]"
  echo "Description=Zeron in this sandbox"
  echo "StartLimitIntervalSec=0"
  echo "ConditionPathExists=$run/env"
  echo
  echo "[Service]"
  echo "User=$user"
  echo "WorkingDirectory={workdir}"
  echo "Environment=HOME=$HOME"
  echo "EnvironmentFile=$run/env"
  echo "ExecStart=$prog $args"
  echo "Restart=always"
  echo "RestartSec=1"
}} >"$t"
sudo install -m 0644 "$t" "/etc/systemd/system/$unit"
rm -f "$t"
sudo systemctl daemon-reload
sudo systemctl restart "$unit"
echo "started $unit""#,
        program = quote(program),
        program_shown = program.replace(['"', '\\', '$', '`'], ""),
        env = b64(&env),
        args = b64(&args),
    );
    Ok(format!(
        "printf %s {} | base64 -d | sh",
        quote(&b64(&script))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_map_as_the_edges() {
        for (boat, want) in [
            ("ready", State::Ready),
            ("idle", State::Ready),
            ("running", State::Ready),
            ("archiving", State::Stopping),
            ("archived", State::Stopped),
            ("error", State::Failed),
            ("cancelled", State::Failed),
            ("provisioning", State::Starting),
            ("cloning", State::Starting),
            ("something-new", State::Starting),
        ] {
            assert_eq!(State::of(boat), want, "{boat}");
        }
    }

    #[test]
    fn a_shell_line_quotes_every_word() {
        let line = shell_line(
            &Command::new(["git", "clone", "it's"])
                .cwd("/home/user")
                .env("GIT_TERMINAL_PROMPT", "0"),
        );
        assert_eq!(
            line,
            r"cd '/home/user' && exec env 'GIT_TERMINAL_PROMPT=0' 'git' 'clone' 'it'\''s'"
        );
        assert_eq!(shell_line(&Command::new(["true"])), "exec 'true'");
    }

    #[test]
    fn an_environment_file_holds_values_as_they_are() {
        let env = BTreeMap::from([("A".to_owned(), r#"x "y" \ $z %n"#.to_owned())]);
        assert_eq!(
            environment_file(&env).unwrap(),
            "A=\"x \\\"y\\\" \\\\ $z %n\"\nZERON_SERVICE_UNIT=\"zeron-sandbox.service\"\n"
        );
        let bad = BTreeMap::from([("A".to_owned(), "two\nlines".to_owned())]);
        assert!(environment_file(&bad).is_err());
        let bad = BTreeMap::from([("A B".to_owned(), "x".to_owned())]);
        assert!(environment_file(&bad).is_err());
    }

    #[test]
    fn systemd_words_escape_specifiers() {
        assert_eq!(systemd_quote("a $b %c").unwrap(), r#""a $$b %%c""#);
    }

    #[test]
    fn a_sandbox_id_stays_one_path_segment() {
        assert_eq!(path("bx_ab12", "/stop"), "/sandboxes/bx_ab12/stop");
        assert_eq!(path("../x", ""), "/sandboxes/%2E%2E%2Fx");
    }

    #[test]
    fn lasting_failures_are_told_apart() {
        assert!(lasting_failure("snapshot_data_expired", ""));
        assert!(lasting_failure(
            "x",
            "This Sandbox needs 118.3 GiB of disk but the largest 'large' machine holds 111.8 GiB."
        ));
        assert!(lasting_failure(
            "x",
            "This sandbox uses 77.7 GB of disk, which does not fit a 'default' machine (76.0 GB)."
        ));
        assert!(!lasting_failure("rate_limited", "slow down"));
    }
}
