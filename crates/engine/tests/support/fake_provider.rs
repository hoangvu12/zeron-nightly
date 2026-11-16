//! A fake sandbox provider (spec #37, Seam 1): each sandbox is a folder under
//! the test's directory, and its "machine" runs real commands there. Running
//! Zeron's engine in it (`zeron-engine headless`, detached) starts a second
//! real engine in this process, configured from the command's environment
//! the way `zeron-engine` reads its own: on the sign-in the host device lent
//! it, with the device id it was given.
//!
//! What it supplies is configurable, so steps a provider can't do are
//! exercised: everything (like Docker Sandbox), `run` only (an SSH box), or
//! `start` only (a PC at home that already runs Zeron).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use tokio::io::AsyncReadExt;
use zeron_engine::sandboxes::{
    Account, Command, CreateSandbox, Credential, ErrorKind, Exit, KeySlot, Ops, Placement,
    ProviderError, RunEvent, RunStream, SandboxProvider, SandboxStatus, Template,
};
use zeron_engine::{Engine, EngineConfig, EngineRuntime, HarnessId, LentSignIn};

/// An engine as `zeron-engine headless` brings one up: auth, its refresh
/// loop, then the runtime (device room host relay included).
pub async fn start_engine(config: &EngineConfig) -> EngineRuntime {
    let auth = Engine::build_auth(config).await;
    let scope = Engine::initial_workspace_scope(&auth);
    let profile = Engine::resolve_profile(config, &auth, scope)
        .expect("profile resolves")
        .expect("a signed-in profile");
    drop(auth.spawn_refresh_loop());
    Engine::assemble_runtime(config, auth, profile)
        .await
        .expect("runtime assembles")
}

pub fn engine_config(data_dir: &Path, edge_url: &str, lent: Option<LentSignIn>) -> EngineConfig {
    EngineConfig {
        data_dir: data_dir.to_path_buf(),
        edge_url: edge_url.to_owned(),
        edge_token: None,
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        org_id: None,
        // As production: a WorkOS client id is configured everywhere.
        workos_client_id: Some("client_test".into()),
        lent_sign_in: lent,
    }
}

/// The engine a PC at home runs, already signed in: the start-only place
/// starts it.
pub struct Resident {
    pub device_id: String,
    pub edge_url: String,
    pub sign_in: LentSignIn,
}

struct FakeBox {
    dir: PathBuf,
    running: bool,
    engine: Option<EngineRuntime>,
    /// The key of the sign-in its engine was lent.
    lent_key: Option<String>,
    /// Its engine's data, kept across stops and starts like its disk.
    data_dir: Option<PathBuf>,
}

impl FakeBox {
    fn new(dir: PathBuf, running: bool) -> Self {
        Self {
            dir,
            running,
            engine: None,
            lent_key: None,
            data_dir: None,
        }
    }
}

#[derive(Default)]
struct State {
    calls: Vec<String>,
    boxes: BTreeMap<String, FakeBox>,
    /// Create waits while this is set.
    create_gate: Option<tokio::sync::watch::Receiver<bool>>,
    /// Commands find the disk full (Zeron's own engine aside).
    disk_full: bool,
    /// Sandboxes made so far: each one's engine keeps its data apart.
    made: usize,
    /// Time limits pushed out: `<box> <seconds>`.
    extends: Vec<String>,
}

pub struct FakeProvider {
    id: String,
    ops: Ops,
    root: PathBuf,
    /// The template has Zeron on it (Install Zeron finds it).
    has_zeron: bool,
    /// Every chat on this one place.
    shared: Option<String>,
    resident: Option<Resident>,
    /// The agents its template has (`None`: whatever this computer has).
    agents: Option<Vec<HarnessId>>,
    state: Arc<Mutex<State>>,
    hold: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    /// Where one device keeps the account's key ([`Self::on_device`]).
    slot: Option<KeySlot>,
}

impl FakeProvider {
    /// A full cloud: one sandbox per chat, every sandbox operation,
    /// templates with Zeron on them (no account to connect, no time limit).
    pub fn cloud(root: &Path) -> Arc<Self> {
        let ops = Ops::CREATE
            | Ops::START
            | Ops::STOP
            | Ops::DELETE
            | Ops::RUN
            | Ops::TEMPLATES
            | Ops::SAVE_TEMPLATE
            | Ops::EXTEND;
        Arc::new(Self::new("fake", ops, root))
    }

    /// An SSH box: `run` only, every chat on the one box.
    pub fn run_only(root: &Path) -> Arc<Self> {
        let mut provider = Self::new("fake-ssh", Ops::RUN, root);
        provider.shared = Some("box".into());
        provider
            .state
            .lock()
            .unwrap()
            .boxes
            .insert("box".into(), FakeBox::new(root.join("box"), true));
        Arc::new(provider)
    }

    /// A PC at home: `start` only, already running Zeron signed in.
    pub fn start_only(root: &Path, resident: Resident) -> Arc<Self> {
        let mut provider = Self::new("fake-pc", Ops::START, root);
        provider.shared = Some("pc".into());
        provider
            .state
            .lock()
            .unwrap()
            .boxes
            .insert("pc".into(), FakeBox::new(root.join("pc"), false));
        provider.resident = Some(resident);
        Arc::new(provider)
    }

    fn new(id: &str, ops: Ops, root: &Path) -> Self {
        Self {
            id: id.into(),
            ops,
            root: root.to_path_buf(),
            has_zeron: true,
            shared: None,
            resident: None,
            agents: None,
            state: Arc::default(),
            hold: Mutex::new(None),
            slot: None,
        }
    }

    /// The same cloud as one of the user's devices builds it in: connected
    /// with a key kept in that device's data dir, as Docker Sandbox is.
    pub fn on_device(&self, data_dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            id: self.id.clone(),
            ops: self.ops | Ops::CONNECT,
            root: self.root.clone(),
            has_zeron: self.has_zeron,
            shared: self.shared.clone(),
            resident: None,
            state: self.state.clone(),
            hold: Mutex::new(None),
            slot: Some(KeySlot::open(data_dir, &self.id)),
            agents: self.agents.clone(),
        })
    }

    /// A full cloud whose template has only `agents` on it: its engine
    /// knows no other.
    pub fn cloud_with_agents(root: &Path, agents: &[HarnessId]) -> Arc<Self> {
        let mut provider = Self::new("fake", Ops::ALL, root);
        provider.agents = Some(agents.to_vec());
        Arc::new(provider)
    }

    /// The sandbox goes to sleep: its engine stops and nothing runs there
    /// until it's started again.
    pub async fn sleep(&self, sandbox: &str) {
        let engine = {
            let mut state = self.state.lock().unwrap();
            let Some(fake) = state.boxes.get_mut(sandbox) else {
                return;
            };
            fake.running = false;
            fake.engine.take()
        };
        if let Some(engine) = engine {
            engine.shutdown().await;
        }
    }

    /// Hold `create` until [`Self::release_create`].
    pub fn hold_create(&self) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        self.state.lock().unwrap().create_gate = Some(rx);
        *self.hold.lock().unwrap() = Some(tx);
    }

    pub fn release_create(&self) {
        if let Some(tx) = self.hold.lock().unwrap().take() {
            let _ = tx.send(true);
        }
    }

    /// What it was asked to do, in order: `create`, `start <box>`,
    /// `run <box>: <args>`.
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Each time limit pushed out, in order: `<box> <seconds>`.
    pub fn extends(&self) -> Vec<String> {
        self.state.lock().unwrap().extends.clone()
    }

    pub fn sandboxes(&self) -> Vec<String> {
        self.state.lock().unwrap().boxes.keys().cloned().collect()
    }

    /// The user deletes `sandbox` in the provider's own console: its engine
    /// goes with it.
    pub async fn delete_outside(&self, sandbox: &str) {
        self.discard(sandbox).await;
    }

    /// Commands in its sandboxes find the disk full (or not, again).
    pub fn set_disk_full(&self, full: bool) {
        self.state.lock().unwrap().disk_full = full;
    }

    /// The key of the sign-in `sandbox`'s engine was lent, once it started.
    pub fn lent_key(&self, sandbox: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .boxes
            .get(sandbox)?
            .lent_key
            .clone()
    }

    async fn discard(&self, sandbox: &str) -> bool {
        let removed = self.state.lock().unwrap().boxes.remove(sandbox);
        let Some(fake) = removed else {
            return false;
        };
        if let Some(engine) = fake.engine {
            engine.shutdown().await;
        }
        // Its checkout goes. (Its engine's data stays: on Windows a file may
        // still be held a moment, and each sandbox's engine has a data
        // folder of its own.)
        let home = fake.dir.join("home");
        writable(&home);
        if let Err(err) = std::fs::remove_dir_all(&home)
            && home.exists()
        {
            panic!("the fake couldn't delete {}: {err}", home.display());
        }
        true
    }

    /// Look inside the engine running in `sandbox`, once it has started.
    pub fn with_engine<T>(
        &self,
        sandbox: &str,
        look: impl FnOnce(&zeron_engine::EngineCore) -> T,
    ) -> Option<T> {
        let state = self.state.lock().unwrap();
        let engine = state.boxes.get(sandbox)?.engine.as_ref()?;
        Some(look(engine.core()))
    }

    fn record(&self, call: String) {
        self.state.lock().unwrap().calls.push(call);
    }

    fn dir(&self, sandbox: &str) -> Result<PathBuf, ProviderError> {
        let state = self.state.lock().unwrap();
        let fake = state.boxes.get(sandbox).ok_or_else(|| {
            ProviderError::new(ErrorKind::NotFound, format!("no sandbox {sandbox}"))
        })?;
        if !fake.running {
            return Err(ProviderError::new(
                ErrorKind::Conflict,
                format!("{sandbox} isn't running"),
            ));
        }
        Ok(fake.dir.clone())
    }

    /// `zeron-engine headless`, detached: boot an engine from the command's
    /// environment, as the real binary reads its own.
    fn boot(&self, sandbox: &str, dir: &Path, env: &BTreeMap<String, String>) -> RunStream {
        let var = |name: &str| env.get(name).cloned();
        let [lender, key, token] = LentSignIn::ENV.map(var);
        let lent = match (lender, key, token) {
            (Some(lender), Some(key), Some(token)) => LentSignIn { lender, key, token },
            _ => return exit_with(1, "no lent sign-in in the environment"),
        };
        let Some(edge_url) = var("ZERON_EDGE_URL") else {
            return exit_with(1, "ZERON_EDGE_URL isn't set");
        };
        let data_dir = {
            let mut state = self.state.lock().unwrap();
            let Some(fake) = state.boxes.get_mut(sandbox) else {
                return exit_with(1, "the sandbox is gone");
            };
            fake.lent_key = Some(lent.key.clone());
            let made = state.made;
            let Some(fake) = state.boxes.get_mut(sandbox) else {
                return exit_with(1, "the sandbox is gone");
            };
            fake.data_dir
                .get_or_insert_with(|| dir.join(format!("zeron-data-{made}")))
                .clone()
        };
        std::fs::create_dir_all(&data_dir).unwrap();
        if let Some(device) = var(zeron_engine::sandboxes::DEVICE_ID_ENV) {
            zeron_engine::sandboxes::take_device_id(&data_dir, &device).unwrap();
        }
        if let Some(provider) = var(zeron_engine::sandboxes::PROVIDER_ENV) {
            zeron_engine::sandboxes::take_provider(&data_dir, &provider).unwrap();
        }
        let config = engine_config(&data_dir, &edge_url, Some(lent));
        let up = self.spawn_engine(sandbox, config);
        if self.agents.is_none() {
            return futures::stream::iter([Ok(RunEvent::Exit(Exit::code(0)))]).boxed();
        }
        // A template's agents are there before anyone can ask its engine.
        futures::stream::once(async move {
            let _ = up.await;
            Ok(RunEvent::Exit(Exit::code(0)))
        })
        .boxed()
    }

    /// `zeron-engine quiesce`: the engine winds down for a sleep; 75 when
    /// it's busy (a turn running, a message waiting).
    fn quiesce(&self, sandbox: &str) -> RunStream {
        let doc_host = {
            let state = self.state.lock().unwrap();
            state
                .boxes
                .get(sandbox)
                .and_then(|fake| fake.engine.as_ref())
                .map(|engine| engine.core().doc_host.clone())
        };
        let Some(doc_host) = doc_host else {
            return exit_with(1, "zeron-engine isn't running");
        };
        futures::stream::once(async move {
            Ok(RunEvent::Exit(Exit::code(
                match doc_host.quiesce_for_sleep().await {
                    Ok(()) => 0,
                    Err(_) => 75,
                },
            )))
        })
        .boxed()
    }

    /// Start an engine in `sandbox`; the receiver fires once it's up.
    fn spawn_engine(
        &self,
        sandbox: &str,
        config: EngineConfig,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let state = self.state.clone();
        let sandbox = sandbox.to_owned();
        let agents = self.agents.clone();
        let (up_tx, up) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let engine = start_engine(&config).await;
            if let Some(agents) = agents {
                engine.core().registry.retain(|id| agents.contains(&id));
            }
            if let Some(fake) = state.lock().unwrap().boxes.get_mut(&sandbox) {
                fake.engine = Some(engine);
            }
            let _ = up_tx.send(());
        });
        up
    }
}

/// Make everything under `dir` writable (git's objects are read-only, which
/// Windows won't delete).
fn writable(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            writable(&path);
        } else if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }
}

fn exit_with(code: i32, stderr: &str) -> RunStream {
    futures::stream::iter([
        Ok(RunEvent::Stderr(stderr.as_bytes().to_vec())),
        Ok(RunEvent::Exit(Exit::code(code))),
    ])
    .boxed()
}

#[async_trait]
impl SandboxProvider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Fake Sandbox"
    }

    fn icon(&self) -> &str {
        "box"
    }

    fn ops(&self) -> Ops {
        self.ops
    }

    fn account(&self) -> Option<Account> {
        self.slot.as_ref()?.get().map(|_| Account {
            name: "fake-account".into(),
        })
    }

    async fn connect(&self, credential: Credential) -> Result<Account, ProviderError> {
        let Credential::Token(token) = credential else {
            return Err(ProviderError::unsupported("sign in on a page"));
        };
        // The keys the fake cloud gives out start with `fake-`.
        if !token.starts_with("fake-") {
            return Err(ProviderError::new(
                ErrorKind::Config,
                "that isn't a fake cloud key",
            ));
        }
        self.adopt_key(serde_json::json!({ "token": token }).to_string())
            .await
    }

    fn export_key(&self) -> Option<String> {
        self.slot.as_ref()?.get()
    }

    async fn adopt_key(&self, raw: String) -> Result<Account, ProviderError> {
        let slot = self
            .slot
            .as_ref()
            .ok_or_else(|| ProviderError::unsupported("keep a key"))?;
        slot.set(&raw)
            .map_err(|e| ProviderError::new(ErrorKind::Fatal, e.to_string()))?;
        Ok(Account {
            name: "fake-account".into(),
        })
    }

    async fn forget_key(&self) {
        if let Some(slot) = &self.slot {
            slot.clear().unwrap();
        }
    }

    fn placement(&self, _chat_id: &str) -> Placement {
        match &self.shared {
            None => Placement::PerChat,
            Some(sandbox) => Placement::Shared {
                sandbox: sandbox.clone(),
                device_id: self.resident.as_ref().map(|r| r.device_id.clone()),
            },
        }
    }

    async fn templates(&self) -> Result<Vec<Template>, ProviderError> {
        Ok(vec![Template {
            id: "zeron-dev".into(),
            name: "zeron-dev".into(),
        }])
    }

    fn workdir(&self, chat_id: &str) -> String {
        // Each chat's sandbox is its own folder (`create` names it after
        // the chat); on a shared box every chat works in the same home.
        let sandbox = self.shared.clone().unwrap_or_else(|| chat_id.to_owned());
        self.root
            .join(sandbox)
            .join("home")
            .to_string_lossy()
            .into_owned()
    }

    async fn create(&self, request: CreateSandbox) -> Result<String, ProviderError> {
        self.record("create".into());
        let gate = self.state.lock().unwrap().create_gate.clone();
        if let Some(mut gate) = gate {
            let _ = gate.wait_for(|open| *open).await;
        }
        let sandbox = request.key.clone();
        let dir = self.root.join(&sandbox);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let mut state = self.state.lock().unwrap();
        state.made += 1;
        state.boxes.insert(sandbox.clone(), FakeBox::new(dir, true));
        Ok(sandbox)
    }

    async fn delete(&self, sandbox: &str) -> Result<(), ProviderError> {
        self.record(format!("delete {sandbox}"));
        if self.discard(sandbox).await {
            Ok(())
        } else {
            Err(ProviderError::new(
                ErrorKind::NotFound,
                format!("no sandbox {sandbox}"),
            ))
        }
    }

    async fn extend(&self, sandbox: &str, ttl: std::time::Duration) -> Result<(), ProviderError> {
        // Kept apart from `calls`: the host device extends whenever it
        // finds a chat busy, in any test.
        let extended = format!("{sandbox} {}", ttl.as_secs());
        self.state.lock().unwrap().extends.push(extended);
        self.dir(sandbox).map(drop)
    }

    async fn status(&self, sandbox: &str) -> Result<SandboxStatus, ProviderError> {
        Ok(match self.state.lock().unwrap().boxes.get(sandbox) {
            None => SandboxStatus::Gone,
            Some(fake) if fake.running => SandboxStatus::Running,
            Some(_) => SandboxStatus::Stopped,
        })
    }

    async fn start(&self, sandbox: &str) -> Result<(), ProviderError> {
        self.record(format!("start {sandbox}"));
        let config = {
            let mut state = self.state.lock().unwrap();
            let fake = state.boxes.get_mut(sandbox).ok_or_else(|| {
                ProviderError::new(ErrorKind::NotFound, format!("no sandbox {sandbox}"))
            })?;
            let woke = !fake.running;
            fake.running = true;
            // A PC at home: waking it starts the Zeron it runs.
            match &self.resident {
                Some(resident) if woke && fake.engine.is_none() => {
                    let data_dir = fake.dir.join("zeron-data");
                    std::fs::create_dir_all(&data_dir).unwrap();
                    std::fs::write(data_dir.join("device-id"), &resident.device_id).unwrap();
                    Some(engine_config(
                        &data_dir,
                        &resident.edge_url,
                        Some(resident.sign_in.clone()),
                    ))
                }
                _ => None,
            }
        };
        if let Some(config) = config {
            drop(self.spawn_engine(sandbox, config));
        }
        Ok(())
    }

    async fn stop(&self, sandbox: &str) -> Result<(), ProviderError> {
        self.record(format!("stop {sandbox}"));
        let engine = {
            let mut state = self.state.lock().unwrap();
            let fake = state.boxes.get_mut(sandbox).ok_or_else(|| {
                ProviderError::new(ErrorKind::NotFound, format!("no sandbox {sandbox}"))
            })?;
            fake.running = false;
            fake.engine.take()
        };
        if let Some(engine) = engine {
            engine.shutdown().await;
        }
        Ok(())
    }

    async fn run(&self, sandbox: &str, command: Command) -> Result<RunStream, ProviderError> {
        self.record(format!("run {sandbox}: {}", command.args.join(" ")));
        let dir = self.dir(sandbox)?;
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let Some(program) = command.args.first() else {
            return Ok(exit_with(2, "no program"));
        };
        if program == zeron_engine::sandboxes::ENGINE_PROGRAM {
            return Ok(match command.args.get(1).map(String::as_str) {
                Some("--version") if self.has_zeron => futures::stream::iter([
                    Ok(RunEvent::Stdout(b"zeron 0.0.0-fake\n".to_vec())),
                    Ok(RunEvent::Exit(Exit::code(0))),
                ])
                .boxed(),
                // Under its watchdog, as Zeron starts it, or bare.
                Some("watchdog" | "headless") if command.detach => {
                    // A rerun replaces the engine already there.
                    let old = self
                        .state
                        .lock()
                        .unwrap()
                        .boxes
                        .get_mut(sandbox)
                        .and_then(|fake| fake.engine.take());
                    if let Some(old) = old {
                        old.shutdown().await;
                    }
                    self.boot(sandbox, &dir, &command.env)
                }
                Some("quiesce") => self.quiesce(sandbox),
                _ => exit_with(127, "zeron-engine: not found"),
            });
        }
        if self.state.lock().unwrap().disk_full {
            return Ok(exit_with(
                1,
                &format!("{program}: write error: No space left on device"),
            ));
        }
        let cwd = command.cwd.map(PathBuf::from).unwrap_or(home.clone());
        // The sandbox's `sh` (step scripts): on Windows, Git's.
        let mut child = if program == "sh" {
            tokio::process::Command::from(zeron_engine::sandboxes::scripts::sh_command())
        } else {
            tokio::process::Command::new(program)
        };
        child
            .args(&command.args[1..])
            .current_dir(&cwd)
            .envs(&command.env)
            .env("HOME", &home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = match child.spawn() {
            Ok(child) => child,
            Err(err) => return Ok(exit_with(127, &format!("{program}: {err}"))),
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        tokio::spawn(async move {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let _ = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
            let status = child.wait().await;
            let _ = tx.send(Ok(RunEvent::Stdout(out)));
            let _ = tx.send(Ok(RunEvent::Stderr(err)));
            let code = status.ok().and_then(|s| s.code());
            let _ = tx.send(Ok(RunEvent::Exit(Exit {
                code,
                timed_out: false,
            })));
        });
        Ok(tokio_stream_from(rx))
    }
}

fn tokio_stream_from(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Result<RunEvent, ProviderError>>,
) -> RunStream {
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed()
}
