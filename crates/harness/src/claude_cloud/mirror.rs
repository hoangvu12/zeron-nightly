//! Session stream → [`AgentEvent`]s. The payloads are Claude Code
//! stream-json, so they go through the Claude Code harness's own
//! [`Normalizer`]: tool calls, results, errors and usage render exactly as
//! they do for a local Claude Code chat. What differs from the CLI's stdout:
//!
//! - A replay (and a stream opened after a message began) brings stored
//!   events only: the live text deltas (`ephemeral_event`) are gone, so an
//!   assistant message whose text never streamed has it taken from the
//!   stored message instead.
//! - The session echoes every user message as a stored `user` event. The
//!   host already wrote the messages it sent, so their echoes show nothing.
//! - A stored event already seen (a reconnect replaying from an earlier
//!   number) shows nothing again.
//! - `SessionStarted` names this harness and the chat's pseudo `cwd`, and no
//!   session id: a provider-run chat never records a local resume id
//!   (older engines would resume it as a local Claude session).

use std::collections::HashMap;

use serde_json::Value;
use zeron_proto::{AgentEvent, HarnessId};

use super::api::StreamItem;
use crate::claude::normalize::Normalizer;
use crate::claude::wire::parse_frame;

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
}

impl Mirror {
    /// A mirror for a stream that starts after `last_seq`.
    pub fn new(cwd: String, last_seq: u64) -> Self {
        Self {
            norm: Normalizer::new(),
            cwd,
            streamed: HashMap::new(),
            last_seq,
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// A steer boundary: (previous, next) assistant message ids.
    pub fn rotate_for_steer(&mut self) -> (String, String) {
        self.norm.rotate_for_steer()
    }

    pub fn map(&mut self, item: StreamItem) -> Vec<AgentEvent> {
        let payload = match item {
            StreamItem::Stored(stored) => {
                if let Some(seq) = stored.sequence_num {
                    if seq <= self.last_seq {
                        return Vec::new();
                    }
                    self.last_seq = seq;
                }
                stored.payload
            }
            StreamItem::Ephemeral(payload) => payload,
            StreamItem::Other { .. } => return Vec::new(),
        };
        self.map_payload(payload)
    }

    fn map_payload(&mut self, payload: Value) -> Vec<AgentEvent> {
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let main_feed = payload.get("parent_tool_use_id").is_none_or(Value::is_null);
        let mut out = Vec::new();
        match kind {
            // Prompts and their answers are ticket #5's; the echo of a
            // message (plain text, no tool results) was written by its sender.
            "control_request" | "control_response" => return out,
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
        out.extend(
            self.norm
                .normalize(frame, false)
                .into_iter()
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
