//! One run of a provider-run chat: a persistent runtime over the session
//! stream. It mirrors the session into agent events until the turn's
//! `result`, ends the turn with `Done`, and stays up for the next message.
//!
//! Every command goes to the session as an event whose `uuid` the provider
//! dedupes, so a command executed twice (another host, a retry) acts once:
//!
//! - A message (a follow-up, or a steer during a turn) goes at once under
//!   its Zeron message id ([`zeron_proto::provider_cloud::event_uuid`]). The
//!   worker reads a steer at its next step boundary, or runs it as its own
//!   turn right after (the turn's `result` then counts it in
//!   `queued_turn_count`, and the mirror doesn't end the turn there).
//! - A changed model, permission mode or plan mode rides the message that
//!   carries it, as `set_model` / `set_permission_mode` control requests
//!   ahead of it, under ids derived from the message's
//!   ([`zeron_proto::provider_cloud::control_uuid`]); the mapping is the
//!   local Claude Code harness's (`model_arg`, `permission_mode`).
//! - Stop ([`RunControls::interrupt`]) sends `interrupt` (with
//!   `cancel_queued`) under the Stop command's id
//!   ([`crate::TurnControl::take_stop_command`]), then waits, bounded, for
//!   the turn's own interrupted `result`. Closing the stream alone never
//!   stops anything at the provider.
//! - A prompt (`can_use_tool`: a permission prompt or AskUserQuestion) is
//!   asked the way the local Claude Code harness asks it, through
//!   [`RunControls::request_input`] and the same access-mode rules. Its
//!   answer goes under a UUIDv5 of the prompt's `request_id`
//!   ([`zeron_proto::provider_cloud::answer_uuid`]), and only while the
//!   mirrored history has no `control_response` for it: once another client
//!   answers, the question closes here and nothing is sent (the provider
//!   keeps the first answer anyway).
//!
//! A person's message, from this device or elsewhere (claude.ai, the
//! phone), rides its event's marker ([`AgentEvent::ProviderEvent`]), which
//! the run sends after everything each stored event mapped to: the engine
//! writes the message unless the transcript has it, and advances the
//! chat's replay watermark. A fresh run picks up from the chat document
//! ([`crate::provider_cloud::ProviderResume`]); a `result` stored before
//! the run's opening message belongs to an earlier turn and doesn't end
//! this one. A dropped stream is reconnected a bounded number of times with
//! backoff ([`RECONNECTS`]), then the run ends errored.
//!
//! Not here (later tickets): takeover checks before each request (#12).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zeron_proto::provider_cloud::{answer_uuid, control_uuid, event_uuid};
use zeron_proto::{AgentEvent, DoneStatus, ProviderSession, RunRequest};

use super::api::{ApiError, CloudClient, EventAck, StreamItem};
use super::credentials::ClaudeAccount;
use super::mirror::{Control, Mirror};
use super::{Applied, Bindings};
use crate::approval;
use crate::claude::wire::{ControlRequestBody, allow_response, deny_response, exit_plan_response};
use crate::claude::{
    APPROVAL_DENIED, QUESTIONS_DISMISSED, access, access_permission_mode, approval_question,
    model_arg, parse_questions, permission_mode, updated_input_with_answers,
};
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

const CONNECTION_LOST: &str = "The connection to the Claude cloud session was lost.";

pub(crate) struct RunSession {
    pub client: Arc<CloudClient>,
    pub bindings: Arc<Bindings>,
    pub session: ProviderSession,
    /// The opening prompt and its event uuid; `None` when the session
    /// already has it (it rode the create request).
    pub opening: Option<(String, String)>,
    /// Control requests that go before the opening prompt ([`changes`]).
    pub opening_controls: Vec<Value>,
    pub mirror: Mirror,
    /// The configuration the session is in, as this run put it.
    pub config: RunRequest,
    pub stream: BoxStream<'static, Result<StreamItem, ApiError>>,
    pub controls: RunControls,
    pub events: Events,
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

/// What a session in `applied` needs to be in `next`'s configuration: the
/// control requests, under ids derived from `command` (the message they
/// ride).
pub(crate) fn changes(command: &str, applied: &Applied, next: &RunRequest) -> Vec<Value> {
    let mut out = Vec::new();
    let model = model_arg(next);
    let model_differs = match &applied.model {
        Some(have) => *have != model,
        None => model.is_some(),
    };
    if model_differs {
        out.push(control_request(
            &control_uuid(command, "set_model"),
            "set-model",
            json!({ "subtype": "set_model", "model": model }),
        ));
    }
    let mode = permission_mode(next);
    if applied.mode != mode {
        out.push(control_request(
            &control_uuid(command, "set_permission_mode"),
            "set-mode",
            json!({ "subtype": "set_permission_mode", "mode": mode }),
        ));
    }
    out
}

fn applied_by(config: &RunRequest) -> Applied {
    Applied {
        model: Some(model_arg(config)),
        mode: permission_mode(config).to_owned(),
    }
}

async fn send(events: &Events, event: AgentEvent) -> bool {
    events.send(Ok(event)).await.is_ok()
}

fn done(status: DoneStatus, error: Option<String>) -> AgentEvent {
    AgentEvent::Done {
        status,
        result: None,
        error,
        session_id: None,
    }
}

/// A prompt's answer, from the task that asked the person.
type Answer = (String, Value);

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

impl RunSession {
    async fn post(&mut self, payloads: Vec<Value>) -> Result<Vec<EventAck>, ApiError> {
        if payloads.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<String> = payloads
            .iter()
            .filter_map(|p| p.get("uuid").and_then(Value::as_str).map(str::to_owned))
            .collect();
        let account = account_of(&self.session);
        let acks = self
            .client
            .send_events(&account, &self.session.session_id, payloads)
            .await?;
        if acks.iter().any(|ack| ack.duplicate) {
            tracing::info!(target: "zeron_harness::claude_cloud", "already delivered (duplicate uuid)");
        }
        for uuid in uuids {
            self.bindings
                .mark_delivered(&self.session.session_id, &uuid);
        }
        Ok(acks)
    }

    /// Send `message` now (with any configuration it carries ahead of it),
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
        let mut payloads = Vec::new();
        if let Some(next) = message.config {
            payloads = changes(&uuid, &applied_by(&self.config), &next);
            self.config = *next;
        }
        payloads.push(user_payload(
            &uuid,
            &self.session.session_id,
            &message.prompt,
        ));
        self.post(payloads).await.map_err(|e| e.to_string())?;
        self.bindings
            .note_applied(&self.session.session_id, applied_by(&self.config));
        Ok(())
    }

    /// Send the run's opening (its settings controls, then its prompt).
    /// Returns the prompt's sequence number: a turn end stored before it is
    /// an earlier turn's (a replay, or turns run elsewhere meanwhile).
    async fn open(&mut self, opening: Vec<Value>) -> Result<Option<u64>, ApiError> {
        let prompt = opening.iter().any(|p| p["type"] == "user");
        let acks = self.post(opening).await?;
        Ok(prompt
            .then(|| acks.last().and_then(|ack| ack.sequence_num))
            .flatten())
    }

    /// Ask the person what prompt `request_id` asks, the way the local
    /// Claude Code harness does; the answer comes back on `answers`. A
    /// prompt the access mode already allows is answered at once.
    fn ask(
        &self,
        request_id: String,
        request: ControlRequestBody,
        answers: &mpsc::UnboundedSender<Answer>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let answers = answers.clone();
        if request.tool_name == "AskUserQuestion" {
            let input = request.input;
            let questions = parse_questions(&input);
            let picked = (self.controls.request_input)(questions.clone());
            return Some(tokio::spawn(async move {
                // No answers (closed, or the run stopped waiting): a decline.
                let picked = picked.await.unwrap_or_default();
                let response = if picked.is_empty() {
                    deny_response(QUESTIONS_DISMISSED)
                } else {
                    allow_response(updated_input_with_answers(&input, &questions, &picked))
                };
                let _ = answers.send((request_id, response));
            }));
        }
        // Approving the plan leaves plan mode for the run's access mode.
        let after_plan = access_permission_mode(&self.config);
        let exit_plan = request.tool_name == "ExitPlanMode";
        let allow = move |input: Value| {
            if exit_plan {
                exit_plan_response(input, after_plan)
            } else {
                allow_response(input)
            }
        };
        let question = approval_question(
            &request.tool_name,
            request.display_name.as_deref(),
            request.description.as_deref(),
            &request.input,
            access(&self.config),
        );
        let Some(question) = question else {
            let _ = answers.send((request_id, allow(request.input)));
            return None;
        };
        let picked = (self.controls.request_input)(vec![question.clone()]);
        let input = request.input;
        Some(tokio::spawn(async move {
            let picked = picked.await.unwrap_or_default();
            let response = if approval::approved(&question, &picked) {
                allow(input)
            } else {
                deny_response(APPROVAL_DENIED)
            };
            let _ = answers.send((request_id, response));
        }))
    }

    pub async fn run(mut self) {
        let session_id = self.session.session_id.clone();
        let mut opening = std::mem::take(&mut self.opening_controls);
        if let Some((uuid, prompt)) = self.opening.take() {
            opening.push(user_payload(&uuid, &session_id, &prompt));
        }
        // A turn end stored before the opening prompt isn't this turn's.
        let mut floor = 0u64;
        if !opening.is_empty() {
            match self.open(opening).await {
                Ok(seq) => floor = seq.unwrap_or(0),
                Err(error) => {
                    fail(&self.events, error.to_string(), true).await;
                    return;
                }
            }
            self.bindings
                .note_applied(&session_id, applied_by(&self.config));
        }
        let interrupt = self.controls.interrupt.clone();
        let (answer_tx, mut answer_rx) = mpsc::unbounded_channel::<Answer>();
        // Prompts asked of the person and not yet answered, by request id.
        let mut open: HashMap<String, Option<tokio::task::JoinHandle<()>>> = HashMap::new();
        // Request ids with a `control_response` in the mirrored history.
        let mut responded: HashSet<String> = HashSet::new();
        let mut turn_active = true;
        let mut steering_open = true;
        let mut stop_deadline: Option<tokio::time::Instant> = None;
        // Reconnects spent, when the stream last came up, and when the next
        // reconnect is due (the stream is down until then).
        let mut reconnects = 0u32;
        let mut connected_at = tokio::time::Instant::now();
        let mut reconnect_at: Option<tokio::time::Instant> = None;
        'main: loop {
            tokio::select! {
                biased;
                _ = self.events.closed() => break 'main,
                _ = interrupt.cancelled(), if stop_deadline.is_none() => {
                    let command = self.controls.turn.take_stop_command();
                    // No Stop behind it (the engine shutting down, the run
                    // replaced): the session goes on at the provider, and a
                    // later run picks it back up.
                    if command.is_none() {
                        break 'main;
                    }
                    if !turn_active {
                        let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                        break 'main;
                    }
                    let uuid = command
                        .map(|id| event_uuid(&id))
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    if let Err(error) = self.post(vec![interrupt_payload(&uuid)]).await {
                        tracing::warn!(target: "zeron_harness::claude_cloud", %error, "interrupt not sent");
                        let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                        break 'main;
                    }
                    // The stopped turn's questions can't be answered now.
                    for (_, task) in open.drain() {
                        if let Some(task) = task {
                            task.abort();
                        }
                    }
                    self.mirror.set_interrupted();
                    stop_deadline = Some(tokio::time::Instant::now() + STOP_GRACE);
                }
                _ = tokio::time::sleep_until(stop_deadline.unwrap_or_else(tokio::time::Instant::now)),
                    if stop_deadline.is_some() => {
                    let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                    break 'main;
                }
                Some((request_id, response)) = answer_rx.recv(), if stop_deadline.is_none() => {
                    // Only while the prompt is still open in the history.
                    if responded.contains(&request_id) || open.remove(&request_id).is_none() {
                        continue 'main;
                    }
                    if let Err(error) = self.post(vec![answer_payload(&request_id, response)]).await {
                        let message = format!("Your answer didn't reach the Claude cloud session: {error}");
                        if !send(&self.events, AgentEvent::Error { message }).await {
                            break 'main;
                        }
                        continue 'main;
                    }
                    responded.insert(request_id);
                }
                _ = tokio::time::sleep_until(reconnect_at.unwrap_or_else(tokio::time::Instant::now)),
                    if reconnect_at.is_some() => {
                    // Reconnect from where the mirror got to (the stream
                    // replays strictly after it). Ticket #8 owns the durable
                    // watermark.
                    reconnect_at = None;
                    let account = account_of(&self.session);
                    match self.client.stream(&account, &session_id, self.mirror.last_seq()).await {
                        Ok(stream) => {
                            self.stream = stream;
                            connected_at = tokio::time::Instant::now();
                        }
                        Err(error) => {
                            tracing::warn!(target: "zeron_harness::claude_cloud", %error, "session stream reconnect failed");
                            if reconnects >= RECONNECTS {
                                fail(&self.events, CONNECTION_LOST.into(), turn_active).await;
                                break 'main;
                            }
                            reconnect_at = Some(tokio::time::Instant::now() + RECONNECT_BACKOFF * 2u32.pow(reconnects));
                            reconnects += 1;
                        }
                    }
                }
                item = self.stream.next(), if reconnect_at.is_none() => match item {
                    Some(Ok(item)) => {
                        let mapped = self.mirror.map(item);
                        self.bindings.note_seq(&session_id, self.mirror.last_seq());
                        for control in mapped.controls {
                            match control {
                                Control::Prompt { request_id, request } => {
                                    if stop_deadline.is_some()
                                        || responded.contains(&request_id)
                                        || open.contains_key(&request_id)
                                    {
                                        continue;
                                    }
                                    let task = self.ask(request_id.clone(), request, &answer_tx);
                                    open.insert(request_id, task);
                                }
                                Control::Responded { request_id } => {
                                    // Answered (here or by another client):
                                    // a question still up closes unanswered.
                                    if let Some(Some(task)) = open.remove(&request_id) {
                                        task.abort();
                                    }
                                    responded.insert(request_id);
                                }
                                // A person's message: its marker below
                                // carries it to the transcript. One sent from
                                // elsewhere (claude.ai, the phone) starts a
                                // turn.
                                Control::UserMessage { uuid, .. } => {
                                    if !self.bindings.was_delivered(&session_id, &uuid) {
                                        turn_active = true;
                                    }
                                }
                            }
                        }
                        // An earlier turn's end (stored before this run's
                        // opening prompt) doesn't end this run's turn.
                        let earlier = mapped.seq.is_some_and(|seq| seq < floor);
                        let mark = mapped.mark;
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
                                continue;
                            }
                            turn_active = false;
                            reconnects = 0;
                            if stop_deadline.is_some() || !steering_open {
                                // The turn's end is committed with its marker.
                                if let Some(mark) = mark {
                                    let _ = send(&self.events, mark).await;
                                }
                                break 'main;
                            }
                        }
                        if let Some(mark) = mark
                            && !send(&self.events, mark).await
                        {
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
                        if reconnects >= RECONNECTS {
                            fail(&self.events, CONNECTION_LOST.into(), turn_active).await;
                            break 'main;
                        }
                        // The stream is down until the reconnect.
                        self.stream = futures::stream::pending().boxed();
                        reconnect_at = Some(tokio::time::Instant::now() + RECONNECT_BACKOFF * 2u32.pow(reconnects));
                        reconnects += 1;
                    }
                },
                // After the stream: what the session produced before a steer is sent stays ahead of its boundary.
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
        for (_, task) in open.drain() {
            if let Some(task) = task {
                task.abort();
            }
        }
    }
}
