//! The provider interface (spec #37, docs/design/cloud-byo.md "The
//! provider's part"): what a cloud supplies so a chat can run on it. A
//! provider supplies only the operations that need it (create, start, stop,
//! delete, run a command, and optionally templates and save-template), plus
//! its name and icon. Zeron runs every step itself ([`super::steps`]) and
//! skips the ones a provider can't do.
//!
//! Places that aren't sandboxes fit too: an SSH box supplies only `run`, a
//! PC woken with Wake-on-LAN only `start`.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Serialize;

/// The optional operations a provider supplies, as a set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ops(u16);

impl Ops {
    pub const NONE: Ops = Ops(0);
    pub const CREATE: Ops = Ops(1);
    pub const START: Ops = Ops(1 << 1);
    pub const STOP: Ops = Ops(1 << 2);
    pub const DELETE: Ops = Ops(1 << 3);
    pub const RUN: Ops = Ops(1 << 4);
    pub const TEMPLATES: Ops = Ops(1 << 5);
    pub const SAVE_TEMPLATE: Ops = Ops(1 << 6);
    /// The user connects an account to it ([`SandboxProvider::connect`]).
    pub const CONNECT: Ops = Ops(1 << 7);
    /// It can push a sandbox's time limit out ([`SandboxProvider::extend`]).
    pub const EXTEND: Ops = Ops(1 << 8);
    /// Every operation.
    pub const ALL: Ops = Ops(0x1ff);

    pub const fn contains(self, other: Ops) -> bool {
        self.0 & other.0 == other.0
    }

    /// Their names, as `ListSandboxProviders` reports them.
    pub fn names(self) -> Vec<&'static str> {
        [
            (Ops::CREATE, "create"),
            (Ops::START, "start"),
            (Ops::STOP, "stop"),
            (Ops::DELETE, "delete"),
            (Ops::RUN, "run"),
            (Ops::TEMPLATES, "templates"),
            (Ops::SAVE_TEMPLATE, "saveTemplate"),
            (Ops::CONNECT, "connect"),
            (Ops::EXTEND, "extend"),
        ]
        .into_iter()
        .filter(|(op, _)| self.contains(*op))
        .map(|(_, name)| name)
        .collect()
    }
}

impl std::ops::BitOr for Ops {
    type Output = Ops;
    fn bitor(self, other: Ops) -> Ops {
        Ops(self.0 | other.0)
    }
}

/// How the provider maps chats to sandboxes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A new sandbox for each chat (Docker Sandbox, Boat).
    PerChat,
    /// Every chat runs on this one place: an SSH box, a PC at home. Zeron
    /// doesn't create it.
    Shared {
        sandbox: String,
        /// The Zeron device already running there, signed in itself (a PC
        /// at home): Install Zeron and Sign in are skipped, and Ready waits
        /// for it. `None`: Zeron is started there like in a sandbox.
        device_id: Option<String>,
    },
}

/// `create`'s request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CreateSandbox {
    /// Idempotency key: the chat it's for. Asking again with the same key
    /// should find the same sandbox, where the provider can.
    pub key: String,
    /// The picked template, or `None` for the provider's plain one (Blank).
    pub template: Option<String>,
    /// Environment the sandbox's commands see.
    pub env: BTreeMap<String, String>,
}

impl CreateSandbox {
    pub fn new(key: impl Into<String>, template: Option<String>) -> Self {
        Self {
            key: key.into(),
            template,
            env: BTreeMap::new(),
        }
    }
}

/// A command `run` runs in a sandbox: a program and its arguments (no shell
/// unless `args` asks for one), with environment and working folder.
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Command {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// `None`: the provider's own default folder.
    pub cwd: Option<String>,
    /// How long it may run; `None` is the provider's default.
    pub timeout: Option<Duration>,
    /// Keep it running after `run` returns (Zeron's engine). The provider
    /// starts it detached from the call and reports only whether it started:
    /// the stream ends with an exit code of 0 once it has.
    pub detach: bool,
}

/// The environment's names only: a value may be a secret (a lent sign-in).
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("args", &self.args)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .field("detach", &self.detach)
            .finish()
    }
}

impl Command {
    pub fn new<S: Into<String>>(args: impl IntoIterator<Item = S>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn detached(mut self) -> Self {
        self.detach = true;
        self
    }
}

/// What a running command reports, in order: output as it comes, then how
/// it ended (always last).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(Exit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// `None` when it was killed or the provider doesn't say.
    pub code: Option<i32>,
    pub timed_out: bool,
}

impl Exit {
    pub fn code(code: i32) -> Self {
        Self {
            code: Some(code),
            timed_out: false,
        }
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// `run`'s answer: the command's events as they happen. A stream that ends
/// without [`RunEvent::Exit`] means the connection to it was lost.
pub type RunStream = BoxStream<'static, Result<RunEvent, ProviderError>>;

/// One of the account's templates (Docker: an image).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub name: String,
}

/// What the user gives to connect an account ([`SandboxProvider::connect`]).
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Credential {
    /// A pasted API key or token.
    Token(String),
    /// A sign-in started with [`SandboxProvider::begin_connect`]: `connect`
    /// waits until the user approves it on the provider's page (or it
    /// expires).
    DeviceCode(DeviceCode),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Token(_) => f.write_str("Token(..)"),
            Credential::DeviceCode(code) => write!(f, "DeviceCode({})", code.user_code),
        }
    }
}

/// A sign-in on the provider's own page (OAuth device flow): what to show
/// the user, and the handle `connect` waits on.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCode {
    /// The code the user checks on the page (`ABCD-EFGH`).
    pub user_code: String,
    /// The page to open.
    pub verification_uri: String,
    /// The page with the code filled in, when the provider has one.
    pub verification_uri_complete: Option<String>,
    /// How long the code lasts, in seconds.
    pub expires_in: u64,
    /// How often to ask whether it was approved, in seconds.
    #[serde(skip)]
    pub interval: u64,
    /// What `connect` polls with. Stays on this device.
    #[serde(skip)]
    pub device_code: String,
}

/// The account a provider is connected to, as Settings shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Its email, organization or other name the user knows it by.
    pub name: String,
}

/// Where a sandbox is, as its provider sees it ([`SandboxProvider::status`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SandboxStatus {
    Running,
    Stopped,
    /// Deleted, outside Zeron or not.
    Gone,
}

/// What kind of failure, so steps can say and decide what to do alike for
/// every provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// Worth trying again (network, rate limit, the provider busy).
    Retryable,
    /// The account or its key needs the user (not connected, no credit).
    Config,
    /// The sandbox isn't there any more.
    NotFound,
    /// Not in a state that allows it.
    Conflict,
    /// The provider doesn't supply this operation.
    Unsupported,
    Fatal,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn unsupported(op: &str) -> Self {
        Self::new(ErrorKind::Unsupported, format!("this provider can't {op}"))
    }
}

/// A cloud (or any place) chats can run on. Implementations keep every call
/// to their service in their own module. All five operations are optional:
/// say which it supplies in [`Self::ops`], and leave the rest to the
/// defaults, which refuse.
#[async_trait]
pub trait SandboxProvider: Send + Sync + 'static {
    /// Stable id, persisted with each sandbox (`"docker"`); never renamed.
    fn id(&self) -> &str;
    /// Its name in the device picker and Settings (`"Docker Sandbox"`).
    fn name(&self) -> &str;
    /// Its mark: the name of a glyph in Zeron's icon set.
    fn icon(&self) -> &str;
    /// The operations it supplies.
    fn ops(&self) -> Ops;
    /// Which sandbox `chat_id` runs on. Default: one per chat.
    fn placement(&self, _chat_id: &str) -> Placement {
        Placement::PerChat
    }
    /// The folder in the sandbox `chat_id`'s checkout goes in (absolute,
    /// on the sandbox's file system): Get the code clones the project there.
    fn workdir(&self, chat_id: &str) -> String;

    /// A new sandbox from `request.template`, started; its id.
    async fn create(&self, _request: CreateSandbox) -> Result<String, ProviderError> {
        Err(ProviderError::unsupported("create sandboxes"))
    }
    /// Start (wake) a stopped sandbox; returns once it runs.
    async fn start(&self, _sandbox: &str) -> Result<(), ProviderError> {
        Err(ProviderError::unsupported("start sandboxes"))
    }
    /// Stop it, keeping its disk.
    async fn stop(&self, _sandbox: &str) -> Result<(), ProviderError> {
        Err(ProviderError::unsupported("stop sandboxes"))
    }
    /// Delete it and its disk, for good.
    async fn delete(&self, _sandbox: &str) -> Result<(), ProviderError> {
        Err(ProviderError::unsupported("delete sandboxes"))
    }
    /// Whether it's still there, and running. Optional (not one of
    /// [`Ops`]): without it, Zeron learns a sandbox is gone only when a step
    /// finds it so.
    async fn status(&self, _sandbox: &str) -> Result<SandboxStatus, ProviderError> {
        Err(ProviderError::unsupported("tell how a sandbox is"))
    }
    /// Run a command in it.
    async fn run(&self, _sandbox: &str, _command: Command) -> Result<RunStream, ProviderError> {
        Err(ProviderError::unsupported("run commands"))
    }
    /// The account's templates.
    async fn templates(&self) -> Result<Vec<Template>, ProviderError> {
        Ok(Vec::new())
    }
    /// Save a sandbox as a new template named `name`.
    async fn save_template(&self, _sandbox: &str, _name: &str) -> Result<Template, ProviderError> {
        Err(ProviderError::unsupported("save templates"))
    }
    /// Check `credential` with the provider and keep it in the provider's
    /// [`super::KeySlot`] on this device; the account it's for.
    async fn connect(&self, _credential: Credential) -> Result<Account, ProviderError> {
        Err(ProviderError::unsupported("connect accounts"))
    }
    /// Start connecting on the provider's own sign-in page (OAuth device
    /// flow): the code to show; then [`Self::connect`] with
    /// [`Credential::DeviceCode`]. `None`: it only takes a pasted credential.
    async fn begin_connect(&self) -> Result<Option<DeviceCode>, ProviderError> {
        Ok(None)
    }
    /// The account it's connected to on this device, if any.
    fn account(&self) -> Option<Account> {
        None
    }
    /// Set the sandbox's time limit to `ttl` from now. When it runs out the
    /// sandbox stops (never deletes); the host device pushes it out while a
    /// chat there is busy.
    async fn extend(&self, _sandbox: &str, _ttl: Duration) -> Result<(), ProviderError> {
        Err(ProviderError::unsupported("extend time limits"))
    }
    /// What its key slot holds, for the next host device (spec #37): moved
    /// over the relay once, never logged. `None`: not connected here.
    fn export_key(&self) -> Option<String> {
        None
    }
    /// Keep `raw`, what another host device's [`Self::export_key`] gave, as
    /// this device's sign-in; the account it's for.
    async fn adopt_key(&self, _raw: String) -> Result<Account, ProviderError> {
        Err(ProviderError::unsupported("move to another device"))
    }
    /// Let go of the sign-in on this device: another device keeps it now.
    async fn forget_key(&self) {}
}

/// A command that ran to its end: its exit and its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub exit: Exit,
    pub stdout: String,
    pub stderr: String,
}

impl Finished {
    /// What to tell the user when it failed: its last words.
    pub fn failure(&self) -> String {
        let said = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let said = said.trim();
        let tail: String = {
            let lines: Vec<&str> = said.lines().collect();
            lines[lines.len().saturating_sub(MAX_FAILURE_LINES)..].join("\n")
        };
        match (self.exit.timed_out, self.exit.code) {
            (true, _) => format!("timed out\n{tail}").trim().to_owned(),
            (_, Some(code)) if tail.is_empty() => format!("exited with {code}"),
            (_, None) if tail.is_empty() => "was stopped".into(),
            _ => tail,
        }
    }
}

/// The most of a failed command's output an error keeps.
const MAX_FAILURE_LINES: usize = 12;
/// The most of each output stream a finished command keeps.
const MAX_OUTPUT: usize = 64 * 1024;

/// Run `command` in `sandbox` and wait for its end.
pub async fn run_to_end(
    provider: &dyn SandboxProvider,
    sandbox: &str,
    command: Command,
) -> Result<Finished, ProviderError> {
    use futures::StreamExt;
    let mut events = provider.run(sandbox, command).await?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    while let Some(event) = events.next().await {
        match event? {
            RunEvent::Stdout(bytes) => keep(&mut stdout, &bytes),
            RunEvent::Stderr(bytes) => keep(&mut stderr, &bytes),
            RunEvent::Exit(exit) => {
                return Ok(Finished {
                    exit,
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                });
            }
        }
    }
    Err(ProviderError::new(
        ErrorKind::Retryable,
        "lost the connection to the command before it ended",
    ))
}

/// Keep the tail of a stream within [`MAX_OUTPUT`].
fn keep(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
    if out.len() > MAX_OUTPUT {
        out.drain(..out.len() - MAX_OUTPUT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_says_the_commands_last_words() {
        let finished = |code, stdout: &str, stderr: &str| Finished {
            exit: Exit::code(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        assert_eq!(
            finished(
                128,
                "",
                "Cloning into 'x'...\nfatal: repository 'y' does not exist\n"
            )
            .failure(),
            "Cloning into 'x'...\nfatal: repository 'y' does not exist"
        );
        assert_eq!(finished(1, "only stdout", "").failure(), "only stdout");
        assert_eq!(finished(3, "", " ").failure(), "exited with 3");
        let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let said = finished(1, "", &long).failure();
        assert_eq!(said.lines().count(), MAX_FAILURE_LINES);
        assert!(said.ends_with("line 39"));
    }

    #[test]
    fn ops_are_a_set() {
        let ops = Ops::START | Ops::RUN;
        assert!(ops.contains(Ops::RUN) && ops.contains(Ops::START));
        assert!(!ops.contains(Ops::CREATE));
        assert!(Ops::ALL.contains(ops));
        assert_eq!(ops.names(), ["start", "run"]);
        assert_eq!(Ops::ALL.names().len(), 9);
        assert_eq!((Ops::CONNECT | Ops::EXTEND).names(), ["connect", "extend"]);
    }

    #[test]
    fn a_credential_never_prints_its_secret() {
        let shown = format!("{:?}", Credential::Token("bk_secret".into()));
        assert!(!shown.contains("bk_secret"), "{shown}");
    }

    #[test]
    fn a_command_never_prints_its_environments_values() {
        let command = Command::new(["zeron-engine", "watchdog"])
            .env("ZERON_LENT_TOKEN", "eyJ-lent-secret")
            .cwd("/home/agent");
        let shown = format!("{command:?}");
        assert!(!shown.contains("eyJ-lent-secret"), "{shown}");
        assert!(shown.contains("ZERON_LENT_TOKEN"), "{shown}");
        assert!(shown.contains("watchdog") && shown.contains("/home/agent"), "{shown}");
    }
}
