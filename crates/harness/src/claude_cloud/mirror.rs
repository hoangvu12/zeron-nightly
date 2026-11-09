//! Session stream → [`AgentEvent`]s. The payloads are Claude Code
//! stream-json, so they go through the Claude Code harness's own
//! [`Normalizer`]: tool calls, results, errors and usage render exactly as
//! they do for a local Claude Code chat. What differs from the CLI's stdout:
//!
//! - A replay (and a stream opened after a message began) brings stored
//!   events only: the live text deltas (`ephemeral_event`) are gone, so an
//!   assistant message whose text never streamed has it taken from the
//!   stored message instead.
//! - The session stores every person's message as a `user` event from a
//!   client. The mirror hands those to the run ([`PersonMessage`]), which
//!   drops the ones this device sent (it wrote them already) and shows the
//!   rest: messages typed on claude.ai or the phone. The worker's own plain
//!   `user` events (`[Request interrupted by user]`) show nothing.
//! - Each turn's `system/init` says the session's model and permission mode
//!   ([`Mapped::settings`]), which is how the run knows what a setting change
//!   would change.
//! - A stored event already seen (a reconnect replaying from an earlier
//!   number) shows nothing again.
//! - `SessionStarted` names this harness and the chat's pseudo `cwd`, and no
//!   session id: a provider-run chat never records a local resume id
//!   (older engines would resume it as a local Claude session).
//! - Prompts (`control_request` `can_use_tool`) and every `control_response`
//!   come out as [`Control`]s for the run to act on, not as events: the
//!   mirrored history is what says whether a prompt is still open.
//! - A `result` with `queued_turn_count` above zero isn't the turn's end: a
//!   message sent during the turn runs next (a steer the worker didn't fold
//!   in), and that turn's `result` ends it.

use std::collections::HashMap;

use serde_json::Value;
use zeron_proto::{AgentEvent, HarnessId};

use super::api::StreamItem;
use super::{PermissionMode, Settings};
use crate::claude::normalize::Normalizer;
use crate::claude::wire::{ControlRequestBody, ControlRequestFrame, parse_frame};

/// What the session's control traffic means for the run.
#[derive(Debug)]
pub(crate) enum Control {
    /// The session asks: a permission prompt or AskUserQuestion, stored at
    /// sequence number `seq`.
    Prompt {
        request_id: String,
        request: ControlRequestBody,
        seq: Option<u64>,
    },
    /// A `control_response` for `request_id` is in the history: a prompt's
    /// answer (from any client), or the worker's reply to a control request.
    Responded { request_id: String },
}

/// A person's message, sent to the session by some client: this device's
/// own (an echo) or another's.
#[derive(Debug)]
pub(crate) struct PersonMessage {
    pub uuid: String,
    pub text: String,
}

/// One stream item, mapped.
#[derive(Debug, Default)]
pub(crate) struct Mapped {
    pub events: Vec<AgentEvent>,
    pub control: Option<Control>,
    pub person: Option<PersonMessage>,
    /// The session's settings, from a `system/init`.
    pub settings: Option<Settings>,
}

/// What a payload is, as far as control traffic goes.
enum Traffic {
    /// Not control traffic: mirrored as events.
    Other,
    /// Control traffic that means nothing to the run (a client's own
    /// request as the history stores it).
    Ignored,
    Control(Control),
}

/// Which kinds of delta streamed for one API message.
#[derive(Default, Clone, Copy)]
struct Streamed {
    text: bool,
    thinking: bool,
}

pub(crate) struct Mirror {
    norm: Normalizer,
    cwd: String,
    streamed: HashMap<String, Streamed>,
    /// The highest stored sequence number mapped (0: none yet).
    last_seq: u64,
    /// The turn is being interrupted: its end is an interrupted one.
    interrupted: bool,
}

impl Mirror {
    /// A mirror for a stream that starts after `last_seq`.
    pub fn new(cwd: String, last_seq: u64) -> Self {
        Self {
            norm: Normalizer::new(),
            cwd,
            streamed: HashMap::new(),
            last_seq,
            interrupted: false,
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// The run interrupted the turn: it ends `Interrupted`, whatever its
    /// `result` says.
    pub fn set_interrupted(&mut self) {
        self.interrupted = true;
    }

    /// A steer boundary: (previous, next) assistant message ids.
    pub fn rotate_for_steer(&mut self) -> (String, String) {
        self.norm.rotate_for_steer()
    }

    pub fn map(&mut self, item: StreamItem) -> Mapped {
        let (payload, source, seq) = match item {
            StreamItem::Stored(stored) => {
                if let Some(seq) = stored.sequence_num {
                    if seq <= self.last_seq {
                        return Mapped::default();
                    }
                    self.last_seq = seq;
                }
                (stored.payload, stored.source, stored.sequence_num)
            }
            StreamItem::Ephemeral(payload) => (payload, None, None),
            StreamItem::Other { .. } => return Mapped::default(),
        };
        match traffic(&payload, seq) {
            Traffic::Control(control) => {
                return Mapped {
                    control: Some(control),
                    ..Mapped::default()
                };
            }
            Traffic::Ignored => return Mapped::default(),
            Traffic::Other => {}
        }
        if let Some(person) = person_message(&payload, source.as_deref()) {
            return Mapped {
                person: Some(person),
                ..Mapped::default()
            };
        }
        let settings = init_settings(&payload);
        Mapped {
            events: self.map_payload(payload),
            settings,
            ..Mapped::default()
        }
    }

    fn map_payload(&mut self, payload: Value) -> Vec<AgentEvent> {
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let main_feed = payload.get("parent_tool_use_id").is_none_or(Value::is_null);
        let mut out = Vec::new();
        // More of the session's turns are queued: this result is a boundary.
        let continues = kind == "result"
            && !self.interrupted
            && payload
                .get("queued_turn_count")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0);
        match kind {
            // A plain `user` event that isn't a person's message (those are
            // [`PersonMessage`]s): the worker's own markers.
            "user" if main_feed && !carries_tool_results(&payload) => return out,
            "stream_event" => {
                if let Some(message) = payload.get("api_message_id").and_then(Value::as_str) {
                    let delta = payload
                        .pointer("/event/delta/type")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let entry = self.streamed.entry(message.to_owned()).or_default();
                    match delta {
                        "text_delta" => entry.text = true,
                        "thinking_delta" => entry.thinking = true,
                        _ => {}
                    }
                }
            }
            "assistant" if main_feed => out.extend(self.unstreamed_blocks(&payload)),
            _ => {}
        }
        let Ok(frame) = parse_frame(&payload.to_string()) else {
            return out;
        };
        let interrupted = self.interrupted;
        out.extend(
            self.norm
                .normalize(frame, interrupted)
                .into_iter()
                .filter(|event| !(continues && matches!(event, AgentEvent::Done { .. })))
                .map(|event| self.own(event)),
        );
        out
    }

    /// The text and thinking of a stored assistant message whose deltas
    /// never reached this stream.
    fn unstreamed_blocks(&self, payload: &Value) -> Vec<AgentEvent> {
        let streamed = payload
            .pointer("/message/id")
            .and_then(Value::as_str)
            .and_then(|id| self.streamed.get(id).copied())
            .unwrap_or_default();
        let Some(blocks) = payload
            .pointer("/message/content")
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") if !streamed.text => block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(|text| AgentEvent::TextDelta {
                        text: text.to_owned(),
                    }),
                Some("thinking") if !streamed.thinking => block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(|text| AgentEvent::ReasoningDelta {
                        text: text.to_owned(),
                    }),
                _ => None,
            })
            .collect()
    }

    /// The normalizer speaks for the local Claude Code harness; these
    /// events are this harness's.
    fn own(&self, event: AgentEvent) -> AgentEvent {
        match event {
            AgentEvent::SessionStarted {
                model,
                tools,
                assistant_message_id,
                ..
            } => AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCodeCloud,
                model,
                tools,
                cwd: self.cwd.clone(),
                session_id: String::new(),
                assistant_message_id,
            },
            AgentEvent::Done {
                status,
                result,
                error,
                ..
            } => AgentEvent::Done {
                status,
                result,
                error,
                session_id: None,
            },
            other => other,
        }
    }
}

/// What `payload` (stored at `seq`) is as control traffic. Only
/// `can_use_tool` is a prompt; any other control request is a client's own
/// (an interrupt, a mode change) as the history stores them.
fn traffic(payload: &Value, seq: Option<u64>) -> Traffic {
    match payload.get("type").and_then(Value::as_str) {
        Some("control_request") => {
            let Ok(frame) = serde_json::from_value::<ControlRequestFrame>(payload.clone()) else {
                return Traffic::Ignored;
            };
            if frame.request.subtype != "can_use_tool" || frame.request_id.is_empty() {
                return Traffic::Ignored;
            }
            Traffic::Control(Control::Prompt {
                request_id: frame.request_id,
                request: frame.request,
                seq,
            })
        }
        Some("control_response") => {
            let response = payload.get("response");
            let field = |key: &str| {
                response
                    .and_then(|r| r.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            if field("subtype") == "error" {
                tracing::warn!(
                    target: "zeron_harness::claude_cloud",
                    request = field("request_id"),
                    error = %response.and_then(|r| r.get("error")).cloned().unwrap_or_default(),
                    "the session refused a control request"
                );
            }
            let request_id = field("request_id").to_owned();
            if request_id.is_empty() {
                return Traffic::Ignored;
            }
            Traffic::Control(Control::Responded { request_id })
        }
        _ => Traffic::Other,
    }
}

/// The session's settings, from a `system/init`.
fn init_settings(payload: &Value) -> Option<Settings> {
    let field = |key: &str| payload.get(key).and_then(Value::as_str);
    if field("type") != Some("system") || field("subtype") != Some("init") {
        return None;
    }
    Some(Settings {
        model: field("model").map(str::to_owned),
        mode: PermissionMode::from_wire(field("permissionMode")?)?,
    })
}

/// A person's message: a main-feed `user` event a client sent (not the
/// worker's), with text and no tool results.
fn person_message(payload: &Value, source: Option<&str>) -> Option<PersonMessage> {
    if payload.get("type").and_then(Value::as_str) != Some("user")
        || source == Some("worker")
        || !payload.get("parent_tool_use_id").is_none_or(Value::is_null)
        || carries_tool_results(payload)
    {
        return None;
    }
    let text = match payload.pointer("/message/content")? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    if text.trim().is_empty() || text.starts_with("[Request interrupted") {
        return None;
    }
    let uuid = payload.get("uuid").and_then(Value::as_str)?.to_owned();
    Some(PersonMessage { uuid, text })
}

/// Whether a `user` payload carries tool results (the worker's own
/// bookkeeping), rather than being a person's message.
fn carries_tool_results(payload: &Value) -> bool {
    payload
        .pointer("/message/content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}
