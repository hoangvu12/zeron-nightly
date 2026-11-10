//! One run of a provider-run chat: a persistent runtime over the session
//! stream. It mirrors the session into agent events until the turn's
//! `result`, ends the turn with `Done`, and stays up for the next message.
//!
//! Zeron sends the session only what the person sent (design §7). Every
//! command goes as an event whose `uuid` the provider dedupes, so a command
//! executed twice (another host, a retry) acts once, and a send whose
//! delivery is uncertain is retried under its original uuid, a bounded
//! number of times with backoff, honouring `Retry-After`:
//!
//! - A message (a follow-up, or a steer during a turn) goes at once under
//!   its Zeron message id ([`zeron_proto::provider_cloud::event_uuid`]). The
//!   worker reads a steer at its next step boundary, or runs it as its own
//!   turn right after (the turn's `result` then counts it in
//!   `queued_turn_count`, and the mirror doesn't end the turn there).
//! - A model, permission mode or plan mode the person changed here rides the
//!   message that carries it, as `set_model` / `set_permission_mode` control
//!   requests ahead of it, under ids derived from the message's
//!   ([`zeron_proto::provider_cloud::control_uuid`]). Only a setting that
//!   changed here and that the session, as its last `system/init` says, isn't
//!   already in goes ([`changes`]); none goes while either is unknown, or for
//!   a message with no id.
//! - The person's Stop ([`RunControls::interrupt`] with the Stop command
//!   noted, [`crate::TurnControl::take_stop_command`]) sends `interrupt` (with
//!   `cancel_queued`) under the Stop command's id, then waits, bounded, for
//!   the turn's own interrupted `result`. Every other teardown (shutdown,
//!   sign-out, a replaced runtime) only closes the stream: the session keeps
//!   working at the provider.
//! - A prompt (`can_use_tool`: a permission prompt or AskUserQuestion) is
//!   decided the way the local Claude Code harness decides it
//!   ([`decide_prompt`]). It is surfaced or answered only once the history
//!   after it shows no `control_response` for it (a replay brings prompts
//!   long answered), and its answer goes under a UUIDv5 of its `request_id`
//!   ([`zeron_proto::provider_cloud::answer_uuid`]) only while the mirrored
//!   history still has no response: once another client answers, the
//!   question closes here and nothing is sent (the provider keeps the first
//!   answer anyway). A question whose answer couldn't be delivered stays
//!   answerable.
//!
//! Every stored event the run mirrors is followed by its marker
//! ([`AgentEvent::ProviderEvent`]), which the engine uses to stamp entries,
//! advance the chat's replay watermark and track open prompts. A person's
//! message, from this device or elsewhere (claude.ai, the phone), rides its
//! marker: the engine writes it unless the transcript already has it (it
//! matches by transcript ids, so this survives restarts). A fresh run picks
//! up where the chat document says the transcript got to
//! ([`crate::provider_cloud::ProviderResume`]). A run started with no
//! message ([`RunSession::idle`], a host resuming the chat) sends nothing and
//! mirrors whatever the session does. A dropped stream is reconnected a
//! bounded number of times with backoff ([`RECONNECTS`]), then the run ends
//! errored.
//!
//! Before every request it sends, and before reconnecting, the run asks
//! whether this device still hosts the chat
//! ([`crate::provider_cloud::ProviderCloud::check_host`]); once another
//! device does, it sends nothing more and ends quietly, the session working
//! on (design §4, "Demotion").

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;
use zeron_proto::provider_cloud::{answer_uuid, control_uuid, event_uuid};
use zeron_proto::{AgentEvent, DoneStatus, ProviderSession, RunRequest};

use super::api::{ApiError, CloudClient, EventAck, StreamItem};
use super::credentials::ClaudeAccount;
use super::mirror::{Control, Mirror};
use super::{Bindings, PermissionMode, Settings};
use crate::claude::wire::ControlRequestBody;
use crate::claude::{PromptDecision, decide_prompt, model_arg, permission_mode};
use crate::{HarnessError, RunControls, SteerMessage};

type Events = mpsc::Sender<Result<AgentEvent, HarnessError>>;

/// How long a stopped turn has to report its own interrupted `result`
/// before the run ends it itself.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Reconnects a dropped stream gets before the run gives up (design §7:
/// nothing retries in a loop). The budget comes back when a turn ends, or
/// once a connection has stayed up for [`STABLE_CONNECTION`].
const RECONNECTS: u32 = 5;
/// The first reconnect's delay; each further one doubles it.
const RECONNECT_BACKOFF: Duration = Duration::from_millis(100);
const STABLE_CONNECTION: Duration = Duration::from_secs(30);

/// Tries a send (or a history read) gets when the failure may pass: the
/// first, then retries after a doubling delay from [`SEND_BACKOFF`].
const SEND_ATTEMPTS: u32 = 4;
const SEND_BACKOFF: Duration = Duration::from_millis(200);
/// The longest single wait, `Retry-After` included.
const LONGEST_WAIT: Duration = Duration::from_secs(10);

/// History pages read past a prompt for its answer.
const HISTORY_PAGES: usize = 20;

const CONNECTION_LOST: &str = "The connection to the Claude cloud session was lost.";

pub(crate) struct RunSession {
    pub client: Arc<CloudClient>,
    pub bindings: Arc<Bindings>,
    pub session: ProviderSession,
    /// The opening prompt and its event uuid; `None` when the session
    /// already has it (it rode the create request).
    pub opening: Option<(String, String)>,
    /// Setting changes that go before the opening prompt, and the settings
    /// the session is in once they apply ([`changes`]).
    pub opening_changes: (Vec<Value>, Option<Settings>),
    pub mirror: Mirror,
    /// The configuration the run works in (what the person chose).
    pub config: RunRequest,
    pub stream: BoxStream<'static, Result<StreamItem, ApiError>>,
    pub controls: RunControls,
    pub events: Events,
    /// Started with no message (a host resuming the chat): no turn is known
    /// to be running until the session shows one.
    pub idle: bool,
}

pub(crate) fn account_of(session: &ProviderSession) -> ClaudeAccount {
    ClaudeAccount {
        account_uuid: session.account_uuid.clone(),
        email: session.account_email.clone(),
        org_id: session.org_id.clone(),
    }
}

/// The `user` event a message travels as.
pub(crate) fn user_payload(uuid: &str, session_id: &str, text: &str) -> Value {
    json!({
        "uuid": uuid,
        "session_id": session_id,
        "type": "user",
        "parent_tool_use_id": null,
        "message": { "role": "user", "content": text },
    })
}

/// A client `control_request`, in the shape the spike recorded
/// (`request_id` = `<kind>-<uuid>`).
fn control_request(uuid: &str, prefix: &str, request: Value) -> Value {
    json!({
        "uuid": uuid,
        "type": "control_request",
        "request_id": format!("{prefix}-{uuid}"),
        "request": request,
    })
}

/// Stop the turn, and the messages queued behind it.
pub(crate) fn interrupt_payload(uuid: &str) -> Value {
    control_request(
        uuid,
        "interrupt",
        json!({ "subtype": "interrupt", "cancel_queued": true }),
    )
}

/// The answer to prompt `request_id`.
pub(crate) fn answer_payload(request_id: &str, response: Value) -> Value {
    json!({
        "uuid": answer_uuid(request_id),
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
}

/// The settings a run in `request` asks for (the local Claude Code
/// harness's mapping: `model_arg`, `permission_mode`).
pub(crate) fn settings_of(request: &RunRequest) -> Settings {
    Settings {
        model: model_arg(request),
        mode: PermissionMode::from_wire(permission_mode(request))
            .unwrap_or(PermissionMode::Default),
    }
}

/// The control requests that put the session in `next`, under ids derived
/// from `command` (the message they ride), and the settings it is in once
/// they apply. A setting goes only when the person changed it here (it
/// differs from `previous`, what this device last asked for) and the
/// session isn't already in it (`actual`, from its `system/init`): a change
/// made on claude.ai stands, and nothing goes while either is unknown or the
/// message has no id.
pub(crate) fn changes(
    command: Option<&str>,
    previous: Option<&Settings>,
    actual: Option<&Settings>,
    next: &Settings,
) -> (Vec<Value>, Option<Settings>) {
    let (Some(command), Some(previous), Some(actual)) = (command, previous, actual) else {
        return (Vec::new(), None);
    };
    let mut out = Vec::new();
    let mut after = actual.clone();
    if next.model != previous.model && next.model != actual.model {
        out.push(control_request(
            &control_uuid(command, "set_model"),
            "set-model",
            json!({ "subtype": "set_model", "model": next.model }),
        ));
        after.model = next.model.clone();
    }
    if next.mode != previous.mode && next.mode != actual.mode {
        out.push(control_request(
            &control_uuid(command, "set_permission_mode"),
            "set-mode",
            json!({ "subtype": "set_permission_mode", "mode": next.mode.as_wire() }),
        ));
        after.mode = next.mode;
    }
    let after = (!out.is_empty()).then_some(after);
    (out, after)
}

async fn send(events: &Events, event: AgentEvent) -> bool {
    events.send(Ok(event)).await.is_ok()
}

/// Send a stored event's marker, if it has one. `false`: the run's reader
/// is gone.
async fn send_mark(events: &Events, mark: Option<AgentEvent>) -> bool {
    match mark {
        Some(mark) => send(events, mark).await,
        None => true,
    }
}

fn done(status: DoneStatus, error: Option<String>) -> AgentEvent {
    AgentEvent::Done {
        status,
        result: None,
        error,
        session_id: None,
    }
}

/// End the run errored: the error shown (when `shown`), then `Done`.
async fn fail(events: &Events, message: String, shown: bool) {
    if shown {
        let _ = send(
            events,
            AgentEvent::Error {
                message: message.clone(),
            },
        )
        .await;
    }
    let _ = send(events, done(DoneStatus::Errored, Some(message))).await;
}

/// The wait before retry number `attempt` (from 0): doubling from `base`,
/// or longer when the server asked, never above [`LONGEST_WAIT`].
fn backoff(base: Duration, attempt: u32, asked: Option<Duration>) -> Duration {
    (base * 2u32.saturating_pow(attempt))
        .max(asked.unwrap_or_default())
        .min(LONGEST_WAIT)
}

/// When the next reconnect is due, spending one of the budget; `None` when
/// the budget is spent.
fn next_reconnect(reconnects: &mut u32, asked: Option<Duration>) -> Option<Instant> {
    if *reconnects >= RECONNECTS {
        return None;
    }
    let at = Instant::now() + backoff(RECONNECT_BACKOFF, *reconnects, asked);
    *reconnects += 1;
    Some(at)
}

/// `call`, tried again while its failure may pass ([`ApiError::is_transient`]).
async fn with_retries<T, F: Future<Output = Result<T, ApiError>>>(
    mut call: impl FnMut() -> F,
) -> Result<T, ApiError> {
    let mut attempt = 0;
    loop {
        match call().await {
            Err(error) if error.is_transient() && attempt + 1 < SEND_ATTEMPTS => {
                tokio::time::sleep(backoff(SEND_BACKOFF, attempt, error.retry_after())).await;
                attempt += 1;
            }
            outcome => return outcome,
        }
    }
}

/// Send `payloads` to the session (retried under the same uuids), noting
/// each as delivered by this device.
async fn post(
    client: &CloudClient,
    bindings: &Bindings,
    session: &ProviderSession,
    payloads: Vec<Value>,
) -> Result<Vec<EventAck>, ApiError> {
    if payloads.is_empty() {
        return Ok(Vec::new());
    }
    let account = account_of(session);
    let acks = with_retries(|| client.send_events(&account, &session.session_id, payloads.clone()))
        .await?;
    if acks.iter().any(|ack| ack.duplicate) {
        tracing::info!(target: "zeron_harness::claude_cloud", "already delivered (duplicate uuid)");
    }
    for payload in &payloads {
        if let Some(uuid) = payload.get("uuid").and_then(Value::as_str) {
            bindings.mark_delivered(&session.session_id, uuid);
        }
    }
    Ok(acks)
}

/// Whether the session's history after `seq` holds a `control_response` for
/// prompt `request_id` (the history pages by sequence number). A prompt with
/// no sequence number has nothing stored after it to read.
async fn answered_in_history(
    client: &CloudClient,
    session: &ProviderSession,
    request_id: &str,
    seq: Option<u64>,
) -> Result<bool, ApiError> {
    let Some(seq) = seq else {
        return Ok(false);
    };
    let account = account_of(session);
    let mut cursor = seq.to_string();
    for _ in 0..HISTORY_PAGES {
        let page =
            with_retries(|| client.history(&account, &session.session_id, Some(&cursor))).await?;
        let answered = page.data.iter().any(|event| {
            event.payload.get("type").and_then(Value::as_str) == Some("control_response")
                && event
                    .payload
                    .pointer("/response/request_id")
                    .and_then(Value::as_str)
                    == Some(request_id)
        });
        if answered {
            return Ok(true);
        }
        match page.resume_cursor {
            Some(next) if !page.data.is_empty() && next != cursor => cursor = next,
            _ => return Ok(false),
        }
    }
    Ok(false)
}

/// End the run quietly: another device hosts the chat now, and the session
/// works on there.
async fn demoted(events: &Events) {
    let _ = send(events, done(DoneStatus::Interrupted, None)).await;
}

/// A prompt's answer, from the task that asked the person (or at once).
type Answer = (String, Value);

/// A prompt surfaced and not yet answered in the history.
struct OpenPrompt {
    request: ControlRequestBody,
    /// The person's question, while it is up; `None` for a prompt the
    /// access mode answered.
    asking: Option<tokio::task::JoinHandle<()>>,
}

/// Close every question still up (their answers can't be sent now).
fn close_all(open: &mut HashMap<String, OpenPrompt>) {
    for (_, prompt) in open.drain() {
        if let Some(task) = prompt.asking {
            task.abort();
        }
    }
}

impl RunSession {
    /// Send `payloads`, while this device still hosts the chat (asked
    /// before every request; design §4).
    async fn post(&mut self, payloads: Vec<Value>) -> Result<(), ApiError> {
        if !payloads.is_empty() && !self.bindings.hosts(&self.session.session_id) {
            return Err(ApiError::NotHost);
        }
        post(&self.client, &self.bindings, &self.session, payloads)
            .await
            .map(drop)
    }

    /// Send `message` now (with the settings it changed ahead of it),
    /// starting a new assistant message after it.
    async fn deliver(&mut self, message: SteerMessage) -> Result<(), String> {
        let (prev, next) = self.mirror.rotate_for_steer();
        if !send(
            &self.events,
            AgentEvent::Steered {
                assistant_message_id: Some(prev),
                next_assistant_message_id: Some(next),
            },
        )
        .await
        {
            return Err(String::new());
        }
        let uuid = match &message.message_id {
            Some(id) => event_uuid(id),
            None => uuid::Uuid::new_v4().to_string(),
        };
        let (mut payloads, after) = match message.config {
            Some(next) => {
                let found = changes(
                    message.message_id.as_ref().map(|_| uuid.as_str()),
                    Some(&settings_of(&self.config)),
                    self.bindings.actual(&self.session.session_id).as_ref(),
                    &settings_of(&next),
                );
                self.config = *next;
                found
            }
            None => (Vec::new(), None),
        };
        payloads.push(user_payload(
            &uuid,
            &self.session.session_id,
            &message.prompt,
        ));
        if let Err(error) = self.post(payloads).await {
            if matches!(error, ApiError::NotHost) {
                demoted(&self.events).await;
                return Err(String::new());
            }
            return Err(error.to_string());
        }
        let session_id = &self.session.session_id;
        if let Some(after) = after {
            self.bindings.note_actual(session_id, after);
        }
        self.bindings
            .note_ran_with(session_id, settings_of(&self.config));
        Ok(())
    }

    /// Decide prompt `request_id` the way the local Claude Code harness
    /// does; the answer comes back on `answers`, at once when the access
    /// mode allows it, else once the person answers. Returns the question
    /// while it is up.
    fn ask(
        &self,
        request_id: String,
        request: ControlRequestBody,
        answers: &mpsc::UnboundedSender<Answer>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        match decide_prompt(request, &self.config) {
            PromptDecision::Answer(response) => {
                let _ = answers.send((request_id, response));
                None
            }
            PromptDecision::Ask { questions, answer } => {
                let picked = (self.controls.request_input)(questions);
                let answers = answers.clone();
                Some(tokio::spawn(async move {
                    // No answers (closed, or the run stopped waiting): a
                    // decline, as `answer` says.
                    let picked = picked.await.unwrap_or_default();
                    let _ = answers.send((request_id, answer(picked)));
                }))
            }
        }
    }

    pub async fn run(mut self) {
        let session_id = self.session.session_id.clone();
        let (mut opening, opening_actual) = std::mem::take(&mut self.opening_changes);
        let prompted = self.opening.is_some();
        if let Some((uuid, prompt)) = self.opening.take() {
            opening.push(user_payload(&uuid, &session_id, &prompt));
        }
        // A turn whose end is stored before this run's opening prompt is an
        // earlier one (a replay catching the transcript up, or turns run
        // elsewhere meanwhile): its end closes its own entry (its marker)
        // and doesn't end this run's turn.
        if !opening.is_empty() && !self.bindings.hosts(&session_id) {
            demoted(&self.events).await;
            return;
        }
        let floor = match post(&self.client, &self.bindings, &self.session, opening).await {
            Ok(acks) if prompted => acks.last().and_then(|ack| ack.sequence_num).unwrap_or(0),
            Ok(_) => 0,
            Err(error) => {
                fail(&self.events, error.to_string(), true).await;
                return;
            }
        };
        if let Some(actual) = opening_actual {
            self.bindings.note_actual(&session_id, actual);
        }
        self.bindings
            .note_ran_with(&session_id, settings_of(&self.config));
        let interrupt = self.controls.interrupt.clone();
        let (answer_tx, mut answer_rx) = mpsc::unbounded_channel::<Answer>();
        // Prompts surfaced and not yet answered, by request id.
        let mut open: HashMap<String, OpenPrompt> = HashMap::new();
        // Request ids with a `control_response` in the mirrored history.
        let mut responded: HashSet<String> = HashSet::new();
        let mut turn_active = !self.idle;
        let mut steering_open = true;
        let mut stop_deadline: Option<Instant> = None;
        // Reconnects spent, when the stream last came up, and when the next
        // reconnect is due (the stream is down until then).
        let mut reconnects = 0u32;
        let mut connected_at = Instant::now();
        let mut reconnect_at: Option<Instant> = None;
        'main: loop {
            // Biased, in this order. The interrupt must stay ahead of the
            // answers: a Stop unparks the open questions with no answers,
            // which decide as a deny, and posting that deny first would use
            // the prompt up for every device (the first answer wins). The
            // stream goes before steers, so what the session produced before
            // a steer is sent stays ahead of its boundary.
            tokio::select! {
                biased;
                _ = self.events.closed() => break 'main,
                _ = interrupt.cancelled(), if stop_deadline.is_none() => {
                    // Only the person's Stop reaches the session. Any other
                    // teardown closes the stream; the session works on.
                    let Some(command) = self.controls.turn.take_stop_command() else {
                        let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                        break 'main;
                    };
                    if !turn_active {
                        let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                        break 'main;
                    }
                    if let Err(error) = self.post(vec![interrupt_payload(&event_uuid(&command))]).await {
                        if matches!(error, ApiError::NotHost) {
                            demoted(&self.events).await;
                            break 'main;
                        }
                        let message = format!(
                            "The stop didn't reach the Claude cloud session, so it may still be working: {error}"
                        );
                        fail(&self.events, message, true).await;
                        break 'main;
                    }
                    // The stopped turn's questions can't be answered now.
                    close_all(&mut open);
                    self.mirror.set_interrupted();
                    stop_deadline = Some(Instant::now() + STOP_GRACE);
                }
                _ = tokio::time::sleep_until(stop_deadline.unwrap_or_else(Instant::now)),
                    if stop_deadline.is_some() => {
                    let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                    break 'main;
                }
                Some((request_id, response)) = answer_rx.recv(), if stop_deadline.is_none() => {
                    // Only while the prompt is still open in the history.
                    if responded.contains(&request_id) || !open.contains_key(&request_id) {
                        continue 'main;
                    }
                    match self.post(vec![answer_payload(&request_id, response)]).await {
                        Ok(()) => {
                            open.remove(&request_id);
                            responded.insert(request_id);
                        }
                        Err(ApiError::NotHost) => {
                            demoted(&self.events).await;
                            break 'main;
                        }
                        Err(error) => {
                            // Not delivered: the question stays answerable.
                            let reasked = open.get(&request_id).filter(|p| p.asking.is_some()).map(|p| p.request.clone());
                            let message = match &reasked {
                                Some(_) => format!("Your answer didn't reach the Claude cloud session ({error}). Answer again."),
                                None => format!("Claude's request couldn't be answered: {error}"),
                            };
                            if !send(&self.events, AgentEvent::Error { message }).await {
                                break 'main;
                            }
                            if let Some(request) = reasked {
                                let asking = self.ask(request_id.clone(), request.clone(), &answer_tx);
                                open.insert(request_id, OpenPrompt { request, asking });
                            }
                        }
                    }
                }
                _ = tokio::time::sleep_until(reconnect_at.unwrap_or_else(Instant::now)),
                    if reconnect_at.is_some() => {
                    // Reconnect from where the mirror got to (the stream
                    // replays strictly after it). Ticket #8 owns the durable
                    // watermark.
                    reconnect_at = None;
                    if !self.bindings.hosts(&session_id) {
                        demoted(&self.events).await;
                        break 'main;
                    }
                    let account = account_of(&self.session);
                    match self.client.stream(&account, &session_id, self.mirror.last_seq()).await {
                        Ok(stream) => {
                            self.stream = stream;
                            connected_at = Instant::now();
                        }
                        Err(error) => {
                            tracing::warn!(target: "zeron_harness::claude_cloud", %error, "session stream reconnect failed");
                            reconnect_at = next_reconnect(&mut reconnects, error.retry_after());
                            if reconnect_at.is_none() {
                                fail(&self.events, CONNECTION_LOST.into(), turn_active).await;
                                break 'main;
                            }
                        }
                    }
                }
                item = self.stream.next(), if reconnect_at.is_none() => match item {
                    Some(Ok(item)) => {
                        let mapped = self.mirror.map(item);
                        self.bindings.note_seq(&session_id, self.mirror.last_seq());
                        let earlier = mapped.seq().is_some_and(|seq| seq < floor);
                        if let Some(settings) = mapped.settings {
                            self.bindings.note_actual(&session_id, settings);
                        }
                        // The stored event's marker goes after everything it
                        // mapped to (and after a prompt is up).
                        let mark = mapped.mark;
                        match mapped.control {
                            Some(Control::Prompt { request_id, request, seq }) => {
                                if stop_deadline.is_some()
                                    || responded.contains(&request_id)
                                    || open.contains_key(&request_id)
                                {
                                    if !send_mark(&self.events, mark).await {
                                        break 'main;
                                    }
                                    continue 'main;
                                }
                                turn_active = true;
                                // A replay brings prompts long answered: the
                                // history past this one says.
                                match answered_in_history(&self.client, &self.session, &request_id, seq).await {
                                    Ok(true) => {
                                        responded.insert(request_id);
                                    }
                                    Ok(false) => {
                                        let asking = self.ask(request_id.clone(), request.clone(), &answer_tx);
                                        open.insert(request_id, OpenPrompt { request, asking });
                                    }
                                    Err(error) => {
                                        tracing::warn!(target: "zeron_harness::claude_cloud", %error, "prompt state unknown");
                                        let message = format!("Couldn't read whether Claude's request was already answered: {error}");
                                        if !send(&self.events, AgentEvent::Error { message }).await {
                                            break 'main;
                                        }
                                    }
                                }
                                if !send_mark(&self.events, mark).await {
                                    break 'main;
                                }
                                continue 'main;
                            }
                            Some(Control::Responded { request_id }) => {
                                // Answered (here or by another client): a
                                // question still up closes unanswered.
                                if let Some(prompt) = open.remove(&request_id)
                                    && let Some(task) = prompt.asking
                                {
                                    task.abort();
                                }
                                responded.insert(request_id);
                                if !send_mark(&self.events, mark).await {
                                    break 'main;
                                }
                                continue 'main;
                            }
                            None => {}
                        }
                        // A person's message (from here or elsewhere) rides
                        // its marker: the engine writes it unless the
                        // transcript has it. Either way a turn runs.
                        if mapped.person.is_some() {
                            turn_active = true;
                        }
                        for mut event in mapped.events {
                            let turn_over = matches!(event, AgentEvent::Done { .. });
                            if turn_over && earlier {
                                continue;
                            }
                            if turn_over && stop_deadline.is_some()
                                && let AgentEvent::Done { status, .. } = &mut event
                            {
                                *status = DoneStatus::Interrupted;
                            }
                            if !send(&self.events, event).await {
                                break 'main;
                            }
                            if !turn_over {
                                turn_active = true;
                                continue;
                            }
                            turn_active = false;
                            reconnects = 0;
                            if stop_deadline.is_some() || !steering_open {
                                // The turn's end is committed with its marker.
                                send_mark(&self.events, mark).await;
                                break 'main;
                            }
                        }
                        if !send_mark(&self.events, mark).await {
                            break 'main;
                        }
                    }
                    ended => {
                        if let Some(Err(error)) = &ended {
                            tracing::warn!(target: "zeron_harness::claude_cloud", %error, "session stream failed");
                        }
                        if connected_at.elapsed() >= STABLE_CONNECTION {
                            reconnects = 0;
                        }
                        // The stream is down until the reconnect.
                        self.stream = futures::stream::pending().boxed();
                        reconnect_at = next_reconnect(&mut reconnects, None);
                        if reconnect_at.is_none() {
                            fail(&self.events, CONNECTION_LOST.into(), turn_active).await;
                            break 'main;
                        }
                    }
                },
                steer = self.controls.steering.recv(), if steering_open && stop_deadline.is_none() => match steer {
                    Some(message) => {
                        if let Err(error) = self.deliver(message).await {
                            if !error.is_empty() {
                                fail(&self.events, error, true).await;
                            }
                            break 'main;
                        }
                        turn_active = true;
                    }
                    None => {
                        steering_open = false;
                        if !turn_active {
                            break 'main;
                        }
                    }
                },
            }
        }
        // Questions still up close with the run.
        close_all(&mut open);
    }
}
