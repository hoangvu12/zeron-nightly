//! One run of a provider-run chat: a persistent runtime over the session
//! stream. It mirrors the session into agent events until the turn's
//! `result`, ends the turn with `Done`, and stays up for the next message,
//! which arrives through the steering mailbox and is sent to the session
//! under the Zeron message id's event uuid (the provider ignores repeats).
//!
//! Not here yet (later tickets): interrupts and prompt answers (#5), the
//! durable replay watermark (#8), takeover checks before each request (#12).

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zeron_proto::{AgentEvent, DoneStatus, ProviderSession};

use super::Bindings;
use super::api::{ApiError, CloudClient, StreamItem};
use super::credentials::ClaudeAccount;
use super::mirror::Mirror;
use crate::{HarnessError, RunControls, SteerMessage};

type Events = mpsc::Sender<Result<AgentEvent, HarnessError>>;

pub(crate) struct RunSession {
    pub client: Arc<CloudClient>,
    pub bindings: Arc<Bindings>,
    pub session: ProviderSession,
    /// The opening prompt and its event uuid; `None` when the session
    /// already has it (it rode the create request).
    pub opening: Option<(String, String)>,
    pub mirror: Mirror,
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

impl RunSession {
    async fn post(&mut self, uuid: &str, text: &str) -> Result<(), ApiError> {
        let account = account_of(&self.session);
        let acks = self
            .client
            .send_events(
                &account,
                &self.session.session_id,
                vec![user_payload(uuid, &self.session.session_id, text)],
            )
            .await?;
        if acks.first().is_some_and(|ack| ack.duplicate) {
            tracing::info!(target: "zeron_harness::claude_cloud", "message already delivered (duplicate uuid)");
        }
        self.bindings.mark_delivered(&self.session.session_id, uuid);
        Ok(())
    }

    /// Start the next turn with `message`: a steer boundary, then the send.
    async fn begin(&mut self, message: SteerMessage) -> Result<(), String> {
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
            Some(id) => zeron_proto::provider_cloud::event_uuid(id),
            None => uuid::Uuid::new_v4().to_string(),
        };
        self.post(&uuid, &message.prompt)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn run(mut self) {
        let session_id = self.session.session_id.clone();
        if let Some((uuid, prompt)) = self.opening.take()
            && let Err(error) = self.post(&uuid, &prompt).await
        {
            let message = error.to_string();
            let _ = send(
                &self.events,
                AgentEvent::Error {
                    message: message.clone(),
                },
            )
            .await;
            let _ = send(&self.events, done(DoneStatus::Errored, Some(message))).await;
            return;
        }
        let interrupt = self.controls.interrupt.clone();
        let mut turn_active = true;
        let mut steering_open = true;
        let mut queued: VecDeque<SteerMessage> = VecDeque::new();
        let mut reconnected = false;
        'main: loop {
            tokio::select! {
                biased;
                _ = self.events.closed() => break 'main,
                _ = interrupt.cancelled() => {
                    // Closing the stream never interrupts the session at the
                    // provider; stopping the turn there is ticket #5's.
                    let _ = send(&self.events, done(DoneStatus::Interrupted, None)).await;
                    break 'main;
                }
                steer = self.controls.steering.recv(), if steering_open => match steer {
                    Some(message) if turn_active => queued.push_back(message),
                    Some(message) => {
                        if let Err(error) = self.begin(message).await {
                            if !error.is_empty() {
                                let _ = send(&self.events, AgentEvent::Error { message: error.clone() }).await;
                                let _ = send(&self.events, done(DoneStatus::Errored, Some(error))).await;
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
                item = self.stream.next() => match item {
                    Some(Ok(item)) => {
                        reconnected = false;
                        let events = self.mirror.map(item);
                        self.bindings.note_seq(&session_id, self.mirror.last_seq());
                        for event in events {
                            let turn_over = matches!(event, AgentEvent::Done { .. });
                            if !send(&self.events, event).await {
                                break 'main;
                            }
                            if !turn_over {
                                continue;
                            }
                            turn_active = false;
                            if let Some(next) = queued.pop_front() {
                                if let Err(error) = self.begin(next).await {
                                    if !error.is_empty() {
                                        let _ = send(&self.events, AgentEvent::Error { message: error.clone() }).await;
                                        let _ = send(&self.events, done(DoneStatus::Errored, Some(error))).await;
                                    }
                                    break 'main;
                                }
                                turn_active = true;
                            } else if !steering_open {
                                break 'main;
                            }
                        }
                    }
                    ended => {
                        // One reconnect, from where the mirror got to (the
                        // stream replays strictly after it). Ticket #8 owns
                        // backoff and the durable watermark.
                        if let Some(Err(error)) = &ended {
                            tracing::warn!(target: "zeron_harness::claude_cloud", %error, "session stream failed");
                        }
                        if !reconnected {
                            reconnected = true;
                            let account = account_of(&self.session);
                            match self.client.stream(&account, &session_id, self.mirror.last_seq()).await {
                                Ok(stream) => {
                                    self.stream = stream;
                                    continue 'main;
                                }
                                Err(error) => tracing::warn!(target: "zeron_harness::claude_cloud", %error, "session stream reconnect failed"),
                            }
                        }
                        let message = "The connection to the Claude cloud session was lost.".to_owned();
                        if turn_active {
                            let _ = send(&self.events, AgentEvent::Error { message: message.clone() }).await;
                        }
                        let _ = send(&self.events, done(DoneStatus::Errored, Some(message))).await;
                        break 'main;
                    }
                },
            }
        }
    }
}
