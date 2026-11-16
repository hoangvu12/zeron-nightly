//! Sleep and wake (docs/design/cloud-byo.md "Lifecycle" and "Waking"):
//! the host device puts a sandbox to sleep when its chats are idle (or the
//! chat is archived), keeps its time limit pushed out while a chat is busy,
//! and stands in for it in its device room while it sleeps.
//!
//! - **Sleep** ([`Step::Sleep`]): `before-sleep` scripts, Zeron's engine
//!   winds down (`zeron-engine quiesce`; busy means not now), then the
//!   provider stops it (or `instead-of-sleep` scripts do), `after-sleep`.
//!   The engine isn't stopped on its own: its watchdog would start it again.
//! - **Standing in**: a send nudges the chat's host, which is the sandbox.
//!   While it sleeps, this device joins its room as host (the room checks
//!   only the user), wakes it **only if the chat has a queued send** (a
//!   reader opening the chat nudges too), and never fights the 4409 the
//!   room gives it when the sandbox's engine comes back. The front worker
//!   drops `nudgeAck`, so the nudges it got are spent: they're posted
//!   again once the sandbox is awake. A relayed send reaching the room
//!   wakes it as well.
//! - **Wake** ([`Step::Wake`]): `before-wake`, the provider starts it (or
//!   `instead-of-wake`), its engine starts again on a freshly lent token (a
//!   lent token over an hour old can't be refreshed), `after-wake`.
//!
//! The provider's own time limit (Docker: 30 minutes, then it stops) covers
//! the host device being gone: nothing extends it then.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::json;
use zeron_doc::{SessionCommandEntry, SessionCommandPayload, SessionCommandStatus};
use zeron_rpc::device_room::{StandInEnd, is_liveness_probe, stand_in_session};
use zeron_rpc::{NudgeHandler, RpcError, RpcReply, RpcService, methods};

use super::{
    ChatSandbox, Command, ENGINE_PROGRAM, Hook, Host, Ops, Record, SandboxProvider,
    SandboxState, SandboxStatus, SandboxSteps, Sandboxes, Step, StepState, Timing, WeakHost,
    answers, lock, run_to_end,
};

/// When sandboxes sleep, and how often the host device looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepTimings {
    /// Idle this long, no turn running: it sleeps.
    pub idle: Duration,
    /// The time limit the host device sets while a chat is busy: with the
    /// host device gone, the provider stops the sandbox at its end.
    pub time_limit: Duration,
    /// How often the host device looks at its sandboxes (and at once when
    /// a chat changes: archived).
    pub check_every: Duration,
}

impl Default for SleepTimings {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(10 * 60),
            time_limit: Duration::from_secs(30 * 60),
            check_every: Duration::from_secs(30),
        }
    }
}

/// How long `zeron-engine quiesce` may take (it waits up to 10 s for work
/// in flight; the edge gives Zeron Cloud's 60).
const QUIESCE_TIMEOUT: Duration = Duration::from_secs(60);
/// `zeron quiesce`'s exit when the engine is busy (EX_TEMPFAIL).
const QUIESCE_BUSY: i32 = 75;
/// What the stand-in answers a client asking anything of the sandbox.
const ASLEEP: &str = "The sandbox is asleep. Wake it, or send a message to it.";
/// A stand-in that another stand-in superseded waits this long before it
/// tries again (two host devices for a moment, while one hands over).
const OTHER_STAND_IN: Duration = Duration::from_secs(60);

/// What the host device does with a sandbox it looks at.
enum Look {
    Nothing,
    Sleep,
    StandIn,
}

impl Sandboxes {
    /// Set when sandboxes sleep (tests make it seconds).
    pub fn set_sleep_timings(&self, timings: SleepTimings) {
        *lock(&self.inner.timings) = timings;
    }

    fn timings(&self) -> SleepTimings {
        *lock(&self.inner.timings)
    }

    /// Look after the sandboxes this device keeps, from now until the engine
    /// goes: sleep the idle ones, extend the busy ones, stand in for the
    /// stopped ones. Once per engine.
    pub(crate) fn keep(&self, host: WeakHost) {
        if tokio::runtime::Handle::try_current().is_err()
            || self.inner.keeping.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let sandboxes = self.clone();
        tokio::spawn(async move {
            let Some(mut chats) = host.upgrade().map(|h| h.workspace.watch_chats()) else {
                return;
            };
            loop {
                let Some(strong) = host.upgrade() else { return };
                sandboxes.look(&strong).await;
                drop(strong);
                let every = sandboxes.timings().check_every;
                tokio::select! {
                    () = tokio::time::sleep(every) => {}
                    changed = chats.changed() => if changed.is_err() { return },
                }
            }
        });
    }

    /// One look at every sandbox.
    async fn look(&self, host: &Host) {
        let chats: Vec<Arc<ChatSandbox>> = lock(&self.inner.chats).values().cloned().collect();
        for chat in chats {
            let Some(provider) = self.provider(&lock(&chat.record).steps.provider) else {
                continue;
            };
            match self.look_at(&chat, provider.as_ref(), host).await {
                Look::Nothing => {}
                Look::StandIn => self.stand_in(&chat, host),
                Look::Sleep => {
                    let sandboxes = self.clone();
                    let host = host.clone();
                    tokio::spawn(async move { sandboxes.sleep(chat, provider, host).await });
                }
            }
        }
    }

    async fn look_at(&self, chat: &Arc<ChatSandbox>, provider: &dyn SandboxProvider, host: &Host) -> Look {
        let (steps, record) = {
            let record = lock(&chat.record).clone();
            (chat.progress.borrow().clone(), record)
        };
        if !sleeps(&record, provider) || *lock(&chat.busy) {
            return Look::Nothing;
        }
        if steps.stopped() {
            return Look::StandIn;
        }
        if steps.state != SandboxState::Ready {
            return Look::Nothing;
        }
        let Some(sandbox) = steps.sandbox_id.clone() else {
            return Look::Nothing;
        };
        // It stopped by itself (its time limit), or outside Zeron.
        match provider.status(&sandbox).await {
            Ok(SandboxStatus::Stopped) if !*lock(&chat.busy) => {
                tracing::info!(%sandbox, "sandbox stopped by itself; standing in for it");
                chat.update(SandboxSteps::asleep);
                self.persist();
                return Look::StandIn;
            }
            Ok(SandboxStatus::Gone) => {
                self.found_gone(chat, provider, host.auth.as_ref()).await;
                return Look::Nothing;
            }
            _ => {}
        }
        let timings = self.timings();
        let now = chrono::Utc::now();
        let device = &record.steps.device_id;
        let sessions: Vec<zeron_proto::Session> = host
            .workspace
            .read_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| &s.device_id == device)
            .collect();
        let busy = sessions
            .iter()
            .any(|s| zeron_proto::view::session_expires(s) && !stale(s, now));
        if busy {
            self.extend(chat, provider, &sandbox, timings).await;
            return Look::Nothing;
        }
        let chats = host.workspace.watch_chats().borrow().clone();
        let archived = chats
            .iter()
            .any(|c| c.id == record.steps.chat_id && c.archived);
        // Its last sign of life: a message, a turn's end, its waking.
        let last = sessions
            .iter()
            .map(|s| s.updated_at)
            .chain(
                chats
                    .iter()
                    .filter(|c| &c.device_id == device)
                    .filter_map(|c| c.last_message_at),
            )
            .max();
        let quiet_since_activity = last.map_or(Duration::MAX, |at| {
            now.signed_duration_since(at).to_std().unwrap_or_default()
        });
        let idle = lock(&chat.awake_since).elapsed().min(quiet_since_activity);
        if archived || idle >= timings.idle {
            Look::Sleep
        } else {
            Look::Nothing
        }
    }

    /// Push the time limit out, now and then while a chat is busy.
    async fn extend(
        &self,
        chat: &ChatSandbox,
        provider: &dyn SandboxProvider,
        sandbox: &str,
        timings: SleepTimings,
    ) {
        if !provider.ops().contains(Ops::EXTEND) {
            return;
        }
        let due = lock(&chat.extended_at).is_none_or(|at| at.elapsed() >= timings.time_limit / 3);
        if !due {
            return;
        }
        match provider.extend(sandbox, timings.time_limit).await {
            Ok(()) => *lock(&chat.extended_at) = Some(Instant::now()),
            Err(err) => tracing::warn!(%sandbox, error = %err, "sandbox: couldn't extend its time limit"),
        }
    }

    /// Put `chat`'s sandbox to sleep, unless its engine is busy.
    pub(crate) async fn sleep(
        &self,
        chat: Arc<ChatSandbox>,
        provider: Arc<dyn SandboxProvider>,
        host: Host,
    ) {
        if !chat.claim() {
            return;
        }
        chat.update(SandboxSteps::sleeping);
        self.persist();
        let outcome = self.put_to_sleep(&chat, provider.as_ref(), &host).await;
        match outcome {
            Ok(true) => {
                chat.update(SandboxSteps::asleep);
                *lock(&chat.extended_at) = None;
                tracing::info!(chat = %chat.chat_id(), "sandbox asleep");
            }
            Ok(false) => {
                // A turn or a message got there first: idle starts again.
                chat.update(SandboxSteps::ready);
                *lock(&chat.awake_since) = Instant::now();
            }
            Err(error) => {
                tracing::warn!(chat = %chat.chat_id(), %error, "sandbox didn't go to sleep");
                chat.update(SandboxSteps::ready);
                *lock(&chat.awake_since) = Instant::now();
            }
        }
        self.persist();
        chat.release();
        if chat.progress.borrow().stopped() {
            lock(&chat.nudged).clear();
            self.stand_in(&chat, &host);
            // A send that reached its engine as it wound down, or a nudge
            // that went to its socket before the stand-in held the room:
            // looked for once the room is held.
            self.sweep(&chat, &host);
        }
        if chat.wake_wanted.swap(false, Ordering::SeqCst) {
            self.wake(chat, provider, host).await;
        }
    }

    /// `Ok(true)`: it's asleep. `Ok(false)`: its engine is busy.
    async fn put_to_sleep(
        &self,
        chat: &ChatSandbox,
        provider: &dyn SandboxProvider,
        host: &Host,
    ) -> Result<bool, String> {
        let record = lock(&chat.record).clone();
        let sandbox = record
            .steps
            .sandbox_id
            .clone()
            .ok_or("The sandbox wasn't created.")?;
        self.run_hook(chat, provider, host, Hook::new(Timing::Before, Step::Sleep))
            .await;
        if provider.ops().contains(Ops::RUN) && !record.resident {
            let quiesce = Command::new([ENGINE_PROGRAM, "quiesce"]).timeout(QUIESCE_TIMEOUT);
            match run_to_end(provider, &sandbox, quiesce).await {
                Ok(done) if done.exit.code == Some(QUIESCE_BUSY) => return Ok(false),
                Ok(done) if !done.exit.success() => {
                    // Its engine isn't answering: nothing to wind down.
                    tracing::info!(%sandbox, said = %done.failure(), "sandbox: quiesce didn't run");
                }
                Ok(_) => {}
                Err(err) => tracing::info!(%sandbox, error = %err, "sandbox: quiesce didn't run"),
            }
        }
        let instead = self
            .scripts_for(chat, provider, host, Hook::new(Timing::InsteadOf, Step::Sleep))
            .await;
        let replaced = !instead.is_empty()
            && self
                .run_scripts(chat, provider, host, Step::Sleep, instead)
                .await
                .any;
        if !replaced {
            provider
                .stop(&sandbox)
                .await
                .map_err(|e| format!("Couldn't stop the sandbox: {e}"))?;
        }
        self.run_hook(chat, provider, host, Hook::new(Timing::After, Step::Sleep))
            .await;
        Ok(true)
    }

    /// Wake: `chat_id`'s sandbox (Wake in Files, Terminal or Diffs).
    pub(crate) fn wake_chat(&self, host: Host, chat_id: &str) -> Result<(), String> {
        let chat = self
            .chat(chat_id)
            .ok_or_else(|| "This chat doesn't run on a sandbox from this device.".to_string())?;
        let provider_id = lock(&chat.record).steps.provider.clone();
        let provider = self
            .provider(&provider_id)
            .ok_or_else(|| format!("{provider_id} isn't connected on this device."))?;
        let sandboxes = self.clone();
        tokio::spawn(async move { sandboxes.wake(chat, provider, host).await });
        Ok(())
    }

    /// Wake `chat`'s sandbox, if it's stopped. While it's going to sleep
    /// (or its steps run), it wakes as soon as that's done.
    pub(crate) async fn wake(
        &self,
        chat: Arc<ChatSandbox>,
        provider: Arc<dyn SandboxProvider>,
        host: Host,
    ) {
        if !chat.claim() {
            // Going to sleep: it wakes once that's done. (Anything else
            // running leaves it awake.)
            if chat.progress.borrow().state == SandboxState::Sleeping {
                tracing::info!(chat = %chat.chat_id(), "wake asked while it goes to sleep");
                chat.wake_wanted.store(true, Ordering::SeqCst);
            }
            return;
        }
        if !chat.progress.borrow().stopped() {
            chat.release();
            return;
        }
        tracing::info!(chat = %chat.chat_id(), "waking sandbox");
        chat.update(SandboxSteps::waking);
        self.persist();
        let outcome = self.wake_up(&chat, provider.as_ref(), &host).await;
        match outcome {
            Ok(()) => {
                chat.update(SandboxSteps::ready);
                *lock(&chat.awake_since) = Instant::now();
                tracing::info!(chat = %chat.chat_id(), "sandbox awake");
            }
            Err(error) => {
                tracing::warn!(chat = %chat.chat_id(), %error, "sandbox didn't wake");
                chat.update(|steps| steps.fail(Step::Wake, error));
            }
        }
        self.persist();
        chat.release();
        if chat.progress.borrow().state != SandboxState::Ready {
            // Deleted outside Zeron, maybe: then it reads as gone.
            self.found_gone(&chat, provider.as_ref(), host.auth.as_ref())
                .await;
            return;
        }
        // It holds its room now (its engine answered): the stand-in, if
        // still there, steps aside.
        if let Some(task) = lock(&chat.stand_in).take() {
            task.abort();
        }
        let timings = self.timings();
        let sandbox = lock(&chat.record).steps.sandbox_id.clone();
        if let Some(sandbox) = sandbox {
            self.extend(&chat, provider.as_ref(), &sandbox, timings).await;
        }
        self.handed_over(&chat, &host);
    }

    async fn wake_up(
        &self,
        chat: &ChatSandbox,
        provider: &dyn SandboxProvider,
        host: &Host,
    ) -> Result<(), String> {
        let record = lock(&chat.record).clone();
        let sandbox = record
            .steps
            .sandbox_id
            .clone()
            .ok_or("The sandbox wasn't created.")?;
        self.run_hook(chat, provider, host, Hook::new(Timing::Before, Step::Wake))
            .await;
        let instead = self
            .scripts_for(chat, provider, host, Hook::new(Timing::InsteadOf, Step::Wake))
            .await;
        let replaced = !instead.is_empty()
            && self
                .run_scripts(chat, provider, host, Step::Wake, instead)
                .await
                .any;
        if !replaced && provider.ops().contains(Ops::START) {
            provider
                .start(&sandbox)
                .await
                .map_err(|e| format!("Couldn't start the sandbox: {e}"))?;
        }
        if provider.ops().contains(Ops::RUN) && !record.resident {
            // Its engine again, on a token lent now: one lent over an hour
            // ago can't be refreshed by an engine that restarted.
            *lock(&chat.lent) = None;
            self.start_engine(chat, provider, host, &record).await?;
        }
        answers(host, &record.steps.device_id).await?;
        self.run_hook(chat, provider, host, Hook::new(Timing::After, Step::Wake))
            .await;
        Ok(())
    }

    /// The sandbox is awake: what waited for it goes now. The nudges the
    /// stand-in took in are spent, so they're posted again, and this
    /// device's own waiting messages go without waiting for a retry.
    fn handed_over(&self, chat: &ChatSandbox, host: &Host) {
        let owner = chat.chat_id();
        let nudged: BTreeSet<String> = std::mem::take(&mut *lock(&chat.nudged));
        for chat_id in nudged.iter().filter(|id| **id != owner) {
            host.doc_host.renudge_remote_host(chat_id);
        }
        if let Err(err) = host.doc_host.retry_delivery(&owner) {
            tracing::warn!(error = %err, "sandbox: couldn't hand the waiting messages over");
        }
    }

    /// Look for a queued send in every chat on `chat`'s sandbox; wake it
    /// for the first one found.
    fn sweep(&self, chat: &Arc<ChatSandbox>, host: &Host) {
        let device = lock(&chat.record).steps.device_id.clone();
        let chat_ids: Vec<String> = host
            .workspace
            .watch_chats()
            .borrow()
            .iter()
            .filter(|c| c.device_id == device)
            .map(|c| c.id.clone())
            .collect();
        for chat_id in chat_ids {
            self.check_send(chat, &chat_id, host.downgrade());
        }
    }

    /// A nudge (or a relayed send) for `chat_id` came to the room this
    /// device holds for `chat`'s sandbox: wake it if a send is queued.
    fn check_send(&self, chat: &Arc<ChatSandbox>, chat_id: &str, host: WeakHost) {
        let sandboxes = self.clone();
        let chat = chat.clone();
        let chat_id = chat_id.to_owned();
        tokio::spawn(async move {
            let Some(host) = host.upgrade() else { return };
            if !chat.progress.borrow().stopped() {
                return;
            }
            if host.doc_host.queued_send(&chat_id).await {
                tracing::info!(chat = %chat_id, "a send waits for a sleeping sandbox; waking it");
                sandboxes.wake_for_send(chat, host).await;
            } else {
                tracing::debug!(chat = %chat_id, "nudge with no queued send; the sandbox sleeps on");
            }
        });
    }

    async fn wake_for_send(&self, chat: Arc<ChatSandbox>, host: Host) {
        let Some(provider) = self.provider(&lock(&chat.record).steps.provider) else {
            return;
        };
        self.wake(chat, provider, host).await;
    }

    /// Hold `chat`'s sandbox's room while it's stopped (once).
    fn stand_in(&self, chat: &Arc<ChatSandbox>, host: &Host) {
        let mut slot = lock(&chat.stand_in);
        if slot.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let sandboxes = self.clone();
        let chat = chat.clone();
        let host = host.downgrade();
        *slot = Some(tokio::spawn(async move {
            sandboxes.hold_room(chat, host).await;
        }));
    }

    /// Whether `chat` is still one this device keeps (not deleted, not
    /// handed to another host device).
    fn keeps_chat(&self, chat: &Arc<ChatSandbox>) -> bool {
        lock(&self.inner.chats)
            .get(&chat.chat_id())
            .is_some_and(|kept| Arc::ptr_eq(kept, chat))
    }

    async fn hold_room(&self, chat: Arc<ChatSandbox>, host: WeakHost) {
        let mut backoff = Duration::from_secs(1);
        loop {
            if chat.deleted.load(Ordering::SeqCst)
                || !chat.progress.borrow().stopped()
                || !self.keeps_chat(&chat)
            {
                return;
            }
            let Some(strong) = host.upgrade() else { return };
            let (Some(edge_url), Some(auth)) =
                (strong.workspace.edge().map(|e| e.url.to_string()), strong.auth.clone())
            else {
                return;
            };
            let device = lock(&chat.record).steps.device_id.clone();
            let token = match zeron_rpc::TokenSource::token(&auth).await {
                Ok(token) => token,
                Err(err) => {
                    tracing::debug!(%device, error = %err, "stand-in: no token yet");
                    drop(strong);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            };
            drop(strong);
            let service: Arc<dyn RpcService> = Arc::new(StandIn {
                sandboxes: self.clone(),
                chat: Arc::downgrade(&chat),
                host: host.clone(),
            });
            let on_nudge: NudgeHandler = {
                let sandboxes = self.clone();
                let chat = Arc::downgrade(&chat);
                let host = host.clone();
                Arc::new(move |chat_id: String| {
                    if let Some(chat) = chat.upgrade() {
                        lock(&chat.nudged).insert(chat_id.clone());
                        sandboxes.check_send(&chat, &chat_id, host.clone());
                    }
                    // Never acknowledged: it's not this device's to take.
                    false
                })
            };
            let joined = Instant::now();
            let end = stand_in_session(&edge_url, &device, &token, service, on_nudge, || {
                tracing::info!(%device, "stand-in: holding the sleeping sandbox's room");
            })
            .await;
            if joined.elapsed() >= Duration::from_secs(30) {
                backoff = Duration::from_secs(1);
            }
            match end {
                StandInEnd::Superseded => {
                    if !chat.progress.borrow().stopped() {
                        // Its engine is back (it's waking): step aside.
                        return;
                    }
                    if self.came_back(&chat).await {
                        return;
                    }
                    // Another stand-in (another host device, for a moment).
                    tracing::info!(%device, "stand-in: another host holds the room; waiting");
                    tokio::time::sleep(OTHER_STAND_IN).await;
                }
                StandInEnd::Closed(why) => {
                    tracing::debug!(%device, %why, "stand-in: room closed; joining again");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// A host superseded the stand-in while Zeron thought it asleep: if
    /// the provider says it runs (started outside Zeron), it's awake.
    async fn came_back(&self, chat: &ChatSandbox) -> bool {
        let (provider, sandbox) = {
            let record = lock(&chat.record);
            (record.steps.provider.clone(), record.steps.sandbox_id.clone())
        };
        let (Some(provider), Some(sandbox)) = (self.provider(&provider), sandbox) else {
            return false;
        };
        if !matches!(provider.status(&sandbox).await, Ok(SandboxStatus::Running)) {
            return false;
        }
        if !chat.claim() {
            return true;
        }
        if chat.progress.borrow().stopped() {
            tracing::info!(%sandbox, "sandbox started outside Zeron; it's awake");
            chat.update(SandboxSteps::ready);
            *lock(&chat.awake_since) = Instant::now();
            self.persist();
        }
        chat.release();
        true
    }

    /// Run again after a failed wake: wake it again.
    pub(super) fn wake_again(&self, host: Host, chat: Arc<ChatSandbox>) -> Result<(), String> {
        let provider_id = lock(&chat.record).steps.provider.clone();
        let provider = self
            .provider(&provider_id)
            .ok_or_else(|| format!("{provider_id} isn't connected on this device."))?;
        let sandboxes = self.clone();
        tokio::spawn(async move { sandboxes.wake(chat, provider, host).await });
        Ok(())
    }
}

/// Whether Zeron puts this chat's sandbox to sleep: one it made (not a
/// shared place, not a PC running Zeron itself), that its provider can stop
/// and start again.
fn sleeps(record: &Record, provider: &dyn SandboxProvider) -> bool {
    let ops = provider.ops();
    !record.resident
        && ops.contains(Ops::STOP)
        && ops.contains(Ops::START)
        && record.steps.sandbox_id.is_some()
        && record.steps.state_of(Step::Create) == StepState::Done
}

fn stale(session: &zeron_proto::Session, now: chrono::DateTime<chrono::Utc>) -> bool {
    now.signed_duration_since(session.updated_at).num_milliseconds()
        > zeron_proto::view::SESSION_STALE_MS
}

/// What a sleeping sandbox's room answers while this device holds it: the
/// dial's readiness probe (so clients can reach it and send), a relayed
/// send (it wakes the sandbox; the sender tries again), and to anything
/// else, that it's asleep. Its catalog is answered by the host device
/// (`answer_for_place`), so asking never wakes it.
struct StandIn {
    sandboxes: Sandboxes,
    chat: Weak<ChatSandbox>,
    host: WeakHost,
}

#[async_trait::async_trait]
impl RpcService for StandIn {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if is_liveness_probe(method, &params) {
            return RpcReply::value(&json!([]));
        }
        if method == methods::RELAY_COMMAND {
            let chat_id = params["chatId"].as_str().unwrap_or_default().to_owned();
            let entry: Option<SessionCommandEntry> =
                serde_json::from_value(params["entry"].clone()).ok();
            let send = entry.is_some_and(|e| {
                e.status == SessionCommandStatus::Pending
                    && matches!(
                        e.payload,
                        SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
                    )
            });
            if send
                && !chat_id.is_empty()
                && let (Some(chat), Some(host)) = (self.chat.upgrade(), self.host.upgrade())
            {
                lock(&chat.nudged).insert(chat_id.clone());
                if chat.progress.borrow().stopped() {
                    tracing::info!(chat = %chat_id, "a send came to a sleeping sandbox; waking it");
                    let sandboxes = self.sandboxes.clone();
                    tokio::spawn(async move { sandboxes.wake_for_send(chat, host).await });
                }
            }
            return Err(RpcError::Failed(
                "The sandbox is asleep; it's waking up for this message.".into(),
            ));
        }
        Err(RpcError::Failed(ASLEEP.into()))
    }
}

impl ChatSandbox {
    /// Take the sandbox for one run of its steps, a sleep or a wake;
    /// `false` while another runs.
    pub(super) fn claim(&self) -> bool {
        let mut busy = lock(&self.busy);
        if *busy {
            return false;
        }
        *busy = true;
        true
    }

    pub(super) fn release(&self) {
        *lock(&self.busy) = false;
    }

    pub(super) fn chat_id(&self) -> String {
        lock(&self.record).steps.chat_id.clone()
    }
}
