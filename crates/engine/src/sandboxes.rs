//! Chats on places: the user's own cloud (spec #37,
//! docs/design/cloud-byo.md, ADR 0004). Providers are Rust, built in and
//! registered here ([`Sandboxes::register`]); a chat created on one gets a
//! sandbox from it and runs there, on a Zeron engine of its own signed in on
//! tokens this device lends it (ADR 0003).
//!
//! This device, the one that keeps the provider, runs every step
//! ([`steps::Step`]) itself, through the provider's few operations
//! ([`SandboxProvider`]), skipping what the provider can't do. Creating the
//! chat returns at once; its first message waits in the chat and goes to
//! the sandbox's engine when the sandbox is Ready.

pub mod docker;
mod keys;
mod provider;
mod recovery;
mod steps;

pub use keys::KeySlot;
pub use provider::{
    Account, Command, CreateSandbox, Credential, DeviceCode, ErrorKind, Exit, Finished, Ops,
    Placement, ProviderError, RunEvent, RunStream, SandboxProvider, SandboxStatus, Template,
    run_to_end,
};
pub use steps::{SandboxState, SandboxSteps, Step, StepProgress, StepState};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::auth::{Auth, LentSignIn};
use crate::doc_host::DocHost;
use crate::workspace_host::WorkspaceHost;

/// Zeron's engine in a sandbox: run as `zeron-engine watchdog`, which keeps
/// `zeron-engine headless` running (no systemd there).
pub const ENGINE_PROGRAM: &str = "zeron-engine";
/// The device id a sandbox's engine takes on its first start, minted here
/// so the chat names its host from the start ([`take_device_id`]).
pub const DEVICE_ID_ENV: &str = "ZERON_SANDBOX_DEVICE_ID";
/// The edge the sandbox's engine dials (`zeron_cli::edge_url_from_env`).
const EDGE_URL_ENV: &str = "ZERON_EDGE_URL";
/// The chats on places, kept across restarts (`{data}/sandboxes.json`). No
/// secret goes in it: lent sign-ins stay in memory.
const RECORDS_FILE: &str = "sandboxes.json";
/// How long Ready waits for the sandbox's engine to answer.
const READY_DEADLINE: Duration = Duration::from_secs(180);
/// A lent token older than this is lent afresh before the engine starts.
const LENT_FRESH: Duration = Duration::from_secs(30 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const CLONE_TIMEOUT: Duration = Duration::from_secs(900);

/// Take `device_id` as this data dir's device id when it has none yet: a
/// sandbox's engine on its first start ([`DEVICE_ID_ENV`]).
pub fn take_device_id(data_dir: &Path, device_id: &str) -> std::io::Result<()> {
    let path = data_dir.join("device-id");
    if std::fs::read_to_string(&path).is_ok_and(|id| !id.trim().is_empty()) {
        return Ok(());
    }
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(path, device_id.trim())
}

/// A provider as the device picker and Settings list it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub ops: Vec<&'static str>,
    /// The account it's connected to here; `None` until the user connects.
    pub account: Option<Account>,
}

/// What `createChat` on a place answers: the chat's host and folder.
#[derive(Clone, Debug)]
pub(crate) struct Placed {
    pub device_id: String,
    pub cwd: String,
}

/// What the steps need of this engine.
#[derive(Clone)]
pub(crate) struct Host {
    pub workspace: WorkspaceHost,
    pub doc_host: DocHost,
    pub auth: Option<Auth>,
    pub links: Option<Arc<zeron_rpc::LinkCache>>,
}

/// A chat on a place, as kept on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(flatten)]
    steps: SandboxSteps,
    #[serde(default)]
    space_id: Option<String>,
    /// Where the checkout goes in the sandbox.
    workdir: String,
    /// The checkout's folder in `workdir`, when there's a project.
    #[serde(default)]
    checkout: Option<String>,
    /// Zeron already runs there, signed in itself (a PC at home).
    #[serde(default)]
    resident: bool,
}

impl Record {
    fn cwd(&self) -> String {
        match &self.checkout {
            Some(folder) => join(&self.workdir, folder),
            None => self.workdir.clone(),
        }
    }
}

struct ChatSandbox {
    record: Mutex<Record>,
    progress: tokio::sync::watch::Sender<SandboxSteps>,
    /// The steps are running (one run at a time).
    busy: Mutex<bool>,
    /// The sign-in lent to it, and when: kept here only.
    lent: Mutex<Option<(LentSignIn, Instant)>>,
    /// Its chat was deleted: steps still running stop and discard it.
    deleted: AtomicBool,
    /// What it left was discarded ([`ChatSandbox::discard`]).
    discarded: AtomicBool,
}

#[derive(Default)]
struct Inner {
    records_path: Option<PathBuf>,
    providers: Mutex<Vec<Arc<dyn SandboxProvider>>>,
    chats: Mutex<HashMap<String, Arc<ChatSandbox>>>,
    /// Chat deletions are followed ([`Sandboxes::follow_deletions`]).
    following: AtomicBool,
}

/// The providers this device keeps and the chats on them.
#[derive(Clone, Default)]
pub struct Sandboxes {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Sandboxes {
    /// The chats on places this device ran the steps for, from `data_dir`.
    pub fn open(data_dir: &Path) -> Self {
        let records_path = data_dir.join(RECORDS_FILE);
        let records: Vec<Record> = std::fs::read(&records_path)
            .ok()
            .and_then(|bytes| match serde_json::from_slice(&bytes) {
                Ok(records) => Some(records),
                Err(err) => {
                    tracing::warn!(error = %err, "sandboxes: unreadable {RECORDS_FILE}; starting empty");
                    None
                }
            })
            .unwrap_or_default();
        let chats = records
            .into_iter()
            .map(|mut record| {
                record.steps.interrupted();
                (
                    record.steps.chat_id.clone(),
                    Arc::new(ChatSandbox::new(record)),
                )
            })
            .collect();
        Self {
            inner: Arc::new(Inner {
                records_path: Some(records_path),
                providers: Mutex::default(),
                chats: Mutex::new(chats),
                following: AtomicBool::new(false),
            }),
        }
    }

    /// Offer `provider` (replacing one with the same id).
    pub fn register(&self, provider: Arc<dyn SandboxProvider>) {
        let mut providers = lock(&self.inner.providers);
        providers.retain(|p| p.id() != provider.id());
        providers.push(provider);
    }

    pub fn providers(&self) -> Vec<ProviderInfo> {
        lock(&self.inner.providers)
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().to_owned(),
                name: p.name().to_owned(),
                icon: p.icon().to_owned(),
                ops: p.ops().names(),
                account: p.account(),
            })
            .collect()
    }

    /// The provider with id `id`, as registered.
    pub fn provider_named(&self, id: &str) -> Option<Arc<dyn SandboxProvider>> {
        self.provider(id)
    }

    fn provider(&self, id: &str) -> Option<Arc<dyn SandboxProvider>> {
        lock(&self.inner.providers)
            .iter()
            .find(|p| p.id() == id)
            .cloned()
    }

    fn chat(&self, chat_id: &str) -> Option<Arc<ChatSandbox>> {
        lock(&self.inner.chats).get(chat_id).cloned()
    }

    /// How far `chat_id`'s sandbox got (`None`: it isn't on a place this
    /// device keeps).
    pub fn steps(&self, chat_id: &str) -> Option<SandboxSteps> {
        self.chat(chat_id)
            .map(|chat| chat.progress.borrow().clone())
    }

    /// Each change to [`Self::steps`].
    pub fn watch(&self, chat_id: &str) -> Option<tokio::sync::watch::Receiver<SandboxSteps>> {
        self.chat(chat_id).map(|chat| chat.progress.subscribe())
    }

    /// Where `chat_id` runs on `provider_id`: its sandbox's device and the
    /// folder its checkout goes in. Nothing runs yet ([`Self::start`]).
    pub(crate) fn place(
        &self,
        chat_id: &str,
        space: Option<&zeron_proto::Space>,
        provider_id: &str,
        template: Option<String>,
    ) -> Result<Placed, String> {
        if let Some(chat) = self.chat(chat_id) {
            let record = lock(&chat.record);
            return Ok(Placed {
                device_id: record.steps.device_id.clone(),
                cwd: record.cwd(),
            });
        }
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| format!("{provider_id} isn't connected on this device."))?;
        let (sandbox, device_id, resident) = match provider.placement(chat_id) {
            Placement::PerChat => (None, crate::new_id(), false),
            Placement::Shared {
                sandbox,
                device_id: Some(device_id),
            } => (Some(sandbox), device_id, true),
            Placement::Shared {
                sandbox,
                device_id: None,
            } => (Some(sandbox), crate::new_id(), false),
        };
        let mut steps = SandboxSteps::new(chat_id, provider.id(), template, &device_id);
        steps.sandbox_id = sandbox;
        let record = Record {
            steps,
            space_id: space.map(|s| s.id.clone()),
            workdir: provider.workdir(chat_id),
            checkout: space.and_then(checkout_folder),
            resident,
        };
        let placed = Placed {
            device_id,
            cwd: record.cwd(),
        };
        lock(&self.inner.chats).insert(chat_id.to_owned(), Arc::new(ChatSandbox::new(record)));
        self.persist();
        Ok(placed)
    }

    /// Run `chat_id`'s steps in the background, from the first not done.
    pub(crate) fn start(&self, host: Host, chat_id: &str) -> Result<(), String> {
        let chat = self
            .chat(chat_id)
            .ok_or_else(|| "This chat doesn't run on a sandbox from this device.".to_string())?;
        let provider_id = lock(&chat.record).steps.provider.clone();
        let provider = self
            .provider(&provider_id)
            .ok_or_else(|| format!("{provider_id} isn't connected on this device."))?;
        {
            let mut busy = lock(&chat.busy);
            if *busy {
                return Ok(());
            }
            *busy = true;
        }
        let sandboxes = self.clone();
        tokio::spawn(async move {
            sandboxes.run_steps(&chat, provider.clone(), &host).await;
            let deleted = {
                let mut busy = lock(&chat.busy);
                *busy = false;
                chat.deleted.load(Ordering::SeqCst)
            };
            if deleted {
                chat.discard(Some(provider.as_ref()), host.auth.as_ref())
                    .await;
            }
        });
        Ok(())
    }

    /// Run again: a failed step, and the ones after it.
    pub(crate) fn run_again(&self, host: Host, chat_id: &str) -> Result<(), String> {
        let chat = self
            .chat(chat_id)
            .ok_or_else(|| "This chat doesn't run on a sandbox from this device.".to_string())?;
        if *lock(&chat.busy) {
            return Ok(());
        }
        chat.update(|steps| steps.retry());
        self.persist();
        self.start(host, chat_id)
    }

    async fn run_steps(&self, chat: &ChatSandbox, provider: Arc<dyn SandboxProvider>, host: &Host) {
        for step in Step::STARTING {
            if chat.deleted.load(Ordering::SeqCst) {
                return;
            }
            if matches!(
                chat.progress.borrow().state_of(step),
                StepState::Done | StepState::Skipped
            ) {
                continue;
            }
            chat.update(|steps| steps.begin(step));
            let outcome = self.run_step(step, chat, provider.as_ref(), host).await;
            match outcome {
                Ok(state) => chat.update(|steps| steps.finish(step, state)),
                Err(error) => {
                    let chat_id = lock(&chat.record).steps.chat_id.clone();
                    tracing::warn!(chat = %chat_id, ?step, %error, "sandbox step failed");
                    chat.update(|steps| steps.fail(step, error));
                    self.persist();
                    // It failed because the sandbox was deleted outside Zeron.
                    self.found_gone(chat, provider.as_ref(), host.auth.as_ref())
                        .await;
                    return;
                }
            }
            self.persist();
        }
        chat.update(SandboxSteps::ready);
        self.persist();
    }

    async fn run_step(
        &self,
        step: Step,
        chat: &ChatSandbox,
        provider: &dyn SandboxProvider,
        host: &Host,
    ) -> Result<StepState, String> {
        let record = lock(&chat.record).clone();
        let ops = provider.ops();
        // Zeron is started there by Zeron: through `run`, unless it already
        // runs there signed in itself.
        let starts_zeron = ops.contains(Ops::RUN) && !record.resident;
        let sandbox = || {
            record
                .steps
                .sandbox_id
                .clone()
                .ok_or_else(|| "The sandbox wasn't created.".to_string())
        };
        match step {
            Step::Create => {
                if record.steps.sandbox_id.is_some() {
                    return Ok(StepState::Skipped);
                }
                if !ops.contains(Ops::CREATE) {
                    return Err(format!("{} can't create sandboxes.", provider.name()));
                }
                let request =
                    CreateSandbox::new(&record.steps.chat_id, record.steps.template.clone());
                let id = provider
                    .create(request)
                    .await
                    .map_err(|e| format!("Couldn't create the sandbox: {e}"))?;
                chat.update(|steps| steps.sandbox_id = Some(id));
                Ok(StepState::Done)
            }
            Step::Start => {
                if !ops.contains(Ops::START) {
                    return Ok(StepState::Skipped);
                }
                provider
                    .start(&sandbox()?)
                    .await
                    .map_err(|e| format!("Couldn't start the sandbox: {e}"))?;
                Ok(StepState::Done)
            }
            Step::InstallZeron => {
                if !starts_zeron {
                    return Ok(StepState::Skipped);
                }
                let sandbox = sandbox()?;
                let probe = Command::new([ENGINE_PROGRAM, "--version"]);
                if run_to_end(provider, &sandbox, probe)
                    .await
                    .is_ok_and(|done| done.exit.success())
                {
                    // The template already has it.
                    return Ok(StepState::Skipped);
                }
                let install = install_command(&edge_url(host)?);
                let done = run_to_end(provider, &sandbox, install)
                    .await
                    .map_err(|e| format!("Couldn't install Zeron: {e}"))?;
                if !done.exit.success() {
                    return Err(format!("Couldn't install Zeron: {}", done.failure()));
                }
                Ok(StepState::Done)
            }
            Step::SignIn => {
                if !starts_zeron {
                    return Ok(StepState::Skipped);
                }
                chat.lend(host, &record.steps.device_id).await?;
                Ok(StepState::Done)
            }
            // Provider secrets (Docker's proxy secrets) need nothing from
            // Zeron; the user's own come with step scripts.
            Step::Secrets => Ok(StepState::Skipped),
            Step::GetCode => {
                if !ops.contains(Ops::RUN) {
                    return Ok(StepState::Skipped);
                }
                let (Some(folder), Some(url)) = (&record.checkout, repo_url(host, &record).await)
                else {
                    return Ok(StepState::Skipped);
                };
                let dir = join(&record.workdir, folder);
                let branch = format!("zeron/{}", record.steps.chat_id);
                get_code(provider, &sandbox()?, &record.workdir, &dir, &url, &branch).await?;
                if let Err(err) = host
                    .workspace
                    .set_chat_branch(&record.steps.chat_id, &branch)
                {
                    tracing::warn!(error = %err, "sandbox: couldn't name the chat's branch");
                }
                Ok(StepState::Done)
            }
            // Step scripts and the repo's `.zeron/setup.sh` come later.
            Step::Setup => Ok(StepState::Skipped),
            Step::Ready => {
                if starts_zeron {
                    let lent = chat.lend(host, &record.steps.device_id).await?;
                    // Under its watchdog, which restarts it when it stops.
                    let mut command = Command::new([ENGINE_PROGRAM, "watchdog"])
                        .env(EDGE_URL_ENV, edge_url(host)?)
                        .env(DEVICE_ID_ENV, &record.steps.device_id)
                        .detached();
                    for (name, value) in lent.to_env() {
                        command = command.env(name, value);
                    }
                    let started = run_to_end(provider, &sandbox()?, command)
                        .await
                        .map_err(|e| format!("Couldn't start Zeron in the sandbox: {e}"))?;
                    if !started.exit.success() {
                        return Err(format!(
                            "Couldn't start Zeron in the sandbox: {}",
                            started.failure()
                        ));
                    }
                }
                answers(host, &record.steps.device_id).await?;
                // What waited for it goes now, not at the next retry.
                if let Err(err) = host.doc_host.retry_delivery(&record.steps.chat_id) {
                    tracing::warn!(error = %err, "sandbox: couldn't hand the waiting messages over");
                }
                Ok(StepState::Done)
            }
        }
    }

    fn persist(&self) {
        let Some(path) = &self.inner.records_path else {
            return;
        };
        let records: Vec<Record> = lock(&self.inner.chats)
            .values()
            .map(|chat| lock(&chat.record).clone())
            .collect();
        let outcome = serde_json::to_vec_pretty(&records)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, path)
            });
        if let Err(err) = outcome {
            tracing::warn!(error = %err, "sandboxes: couldn't save {RECORDS_FILE}");
        }
    }
}

impl ChatSandbox {
    fn new(record: Record) -> Self {
        let (progress, _) = tokio::sync::watch::channel(record.steps.clone());
        Self {
            record: Mutex::new(record),
            progress,
            busy: Mutex::new(false),
            lent: Mutex::new(None),
            deleted: AtomicBool::new(false),
            discarded: AtomicBool::new(false),
        }
    }

    fn update(&self, change: impl FnOnce(&mut SandboxSteps)) {
        let steps = {
            let mut record = lock(&self.record);
            change(&mut record.steps);
            record.steps.clone()
        };
        self.progress.send_replace(steps);
    }

    /// The sign-in its engine starts on: lent once, again when stale.
    async fn lend(&self, host: &Host, device_id: &str) -> Result<LentSignIn, String> {
        if let Some((lent, at)) = lock(&self.lent).clone()
            && at.elapsed() < LENT_FRESH
        {
            return Ok(lent);
        }
        let auth = host.auth.as_ref().ok_or_else(|| {
            "This device isn't signed in, so it can't sign the sandbox in.".to_string()
        })?;
        let (key, token) = auth
            .lend(device_id, host.workspace.org_id())
            .await
            .map_err(|e| format!("Couldn't sign the sandbox in: {e}"))?;
        let lent = LentSignIn {
            lender: host.doc_host.device_id().to_owned(),
            key,
            token,
        };
        *lock(&self.lent) = Some((lent.clone(), Instant::now()));
        Ok(lent)
    }
}

fn edge_url(host: &Host) -> Result<String, String> {
    host.workspace
        .edge()
        .map(|edge| edge.url.clone())
        .ok_or_else(|| "This device isn't connected to Zeron, so the sandbox can't be.".to_string())
}

/// The checkout's folder name: the project's repository's, else its folder's.
fn checkout_folder(space: &zeron_proto::Space) -> Option<String> {
    let name = match &space.github_repo {
        Some(repo) => repo.rsplit('/').next().unwrap_or(repo),
        None => space
            .path
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default(),
    };
    let name = name.trim_end_matches(".git").trim();
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

/// The project's repository: its folder's `origin` when it's on this device,
/// else its GitHub repository.
async fn repo_url(host: &Host, record: &Record) -> Option<String> {
    let space = host
        .workspace
        .space(record.space_id.as_deref()?)
        .ok()
        .flatten()?;
    if space.device_id == host.doc_host.device_id() {
        let origin = tokio::process::Command::new("git")
            .args(["-C", &space.path, "remote", "get-url", "origin"])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|url| !url.is_empty());
        if origin.is_some() {
            return origin;
        }
    }
    space
        .github_repo
        .map(|repo| format!("https://github.com/{repo}.git"))
}

/// Get the code: clone (unless a run before did) and check out the chat's
/// branch.
async fn get_code(
    provider: &dyn SandboxProvider,
    sandbox: &str,
    workdir: &str,
    dir: &str,
    url: &str,
    branch: &str,
) -> Result<(), String> {
    let cloned = Command::new(["git", "-C", dir, "rev-parse", "--is-inside-work-tree"]);
    let have = run_to_end(provider, sandbox, cloned)
        .await
        .is_ok_and(|done| done.exit.success());
    if !have {
        let clone = Command::new(["git", "clone", url, dir])
            .cwd(workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .timeout(CLONE_TIMEOUT);
        let done = run_to_end(provider, sandbox, clone)
            .await
            .map_err(|e| format!("Couldn't clone {url}: {e}"))?;
        if !done.exit.success() {
            return Err(format!("Couldn't clone {url}:\n{}", done.failure()));
        }
    }
    // The chat's branch as it is here (Run again), else as it was pushed (a
    // fresh sandbox: Start fresh), else new.
    let has = async |reference: String| {
        let probe = Command::new([
            "git",
            "-C",
            dir,
            "rev-parse",
            "--verify",
            "--quiet",
            reference.as_str(),
        ]);
        run_to_end(provider, sandbox, probe)
            .await
            .is_ok_and(|done| done.exit.success())
    };
    let pushed = format!("refs/remotes/origin/{branch}");
    let checkout = if has(format!("refs/heads/{branch}")).await {
        Command::new(["git", "-C", dir, "checkout", branch])
    } else if has(pushed.clone()).await {
        Command::new(["git", "-C", dir, "checkout", "-B", branch, pushed.as_str()])
    } else {
        Command::new(["git", "-C", dir, "checkout", "-B", branch])
    };
    let done = run_to_end(provider, sandbox, checkout)
        .await
        .map_err(|e| format!("Couldn't check out {branch}: {e}"))?;
    if !done.exit.success() {
        return Err(format!("Couldn't check out {branch}:\n{}", done.failure()));
    }
    Ok(())
}

/// Wait for the sandbox's engine to answer through its device room.
async fn answers(host: &Host, device_id: &str) -> Result<(), String> {
    let links = host
        .links
        .as_ref()
        .ok_or_else(|| "This device isn't connected, so it can't reach the sandbox.".to_string())?;
    let deadline = tokio::time::Instant::now() + READY_DEADLINE;
    loop {
        let asked = tokio::time::timeout(Duration::from_secs(10), async {
            let client = links.client(device_id).await.ok()?;
            client
                .call(zeron_rpc::methods::ENGINE_INFO, serde_json::json!({}))
                .await
                .ok()
        })
        .await;
        if let Ok(Some(info)) = asked
            && info["deviceId"] == device_id
        {
            return Ok(());
        }
        links.invalidate(device_id);
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Zeron in the sandbox didn't come online within {} s.",
                READY_DEADLINE.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Install Zeron's engine from this edge's releases into the managed layout
/// the updater knows (`~/.zeron/app/current`), and on the PATH.
fn install_command(edge_url: &str) -> Command {
    const SCRIPT: &str = r#"set -eu
edge="$ZERON_EDGE_URL"
version=$(curl -fsSL "$edge/releases/latest.txt")
case "$(uname -m)" in aarch64|arm64) arch=aarch64 ;; *) arch=x86_64 ;; esac
dir="$HOME/.zeron/app/$version"
mkdir -p "$dir"
curl -fsSL "$edge/releases/zeron-$version-linux-$arch.tar.gz" | tar -xz --strip-components=1 -C "$dir"
ln -sfn "$dir" "$HOME/.zeron/app/current"
sudo ln -sf "$HOME/.zeron/app/current/zeron-engine" /usr/local/bin/zeron-engine
zeron-engine --version"#;
    Command::new(["sh", "-c", SCRIPT])
        .env(EDGE_URL_ENV, edge_url)
        .timeout(INSTALL_TIMEOUT)
}

/// `folder` in `dir`, with `dir`'s own separator.
fn join(dir: &str, folder: &str) -> String {
    let separator = if dir.contains('\\') && !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{separator}{folder}", dir.trim_end_matches(['/', '\\']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(path: &str, github_repo: Option<&str>) -> zeron_proto::Space {
        serde_json::from_value(serde_json::json!({
            "id": "s", "deviceId": "d", "path": path, "githubRepo": github_repo,
            "createdAt": "2026-11-16T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn the_checkout_is_named_after_the_repository_else_the_folder() {
        assert_eq!(
            checkout_folder(&space("/home/u/src/app", Some("acme/reportly"))).as_deref(),
            Some("reportly")
        );
        assert_eq!(
            checkout_folder(&space("/home/u/src/app/", None)).as_deref(),
            Some("app")
        );
        assert_eq!(
            checkout_folder(&space(r"C:\src\app.git", None)).as_deref(),
            Some("app")
        );
        assert_eq!(checkout_folder(&space("/", None)), None);
    }

    #[test]
    fn folders_join_with_the_sandboxs_separator() {
        assert_eq!(
            join("/home/agent/workspace", "app"),
            "/home/agent/workspace/app"
        );
        assert_eq!(
            join("/home/agent/workspace/", "app"),
            "/home/agent/workspace/app"
        );
        assert_eq!(join(r"C:\box\home", "app"), r"C:\box\home\app");
    }

    #[test]
    fn a_sandbox_takes_its_device_id_only_on_its_first_start() {
        let dir = tempfile::tempdir().unwrap();
        take_device_id(dir.path(), "sandbox-1").unwrap();
        take_device_id(dir.path(), "sandbox-2").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("device-id")).unwrap(),
            "sandbox-1"
        );
    }
}
