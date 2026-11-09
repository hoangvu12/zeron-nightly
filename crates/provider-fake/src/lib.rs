//! Test support for provider-run chats (docs/design/provider-run-chats.md):
//!
//! - [`FakeSessionServer`]: a fake of Anthropic's Claude Code session API
//!   (`/v1/code/sessions…`) on a loopback port. It replays turns recorded by
//!   the protocol spike ([`Turn::bash_tool`], [`Turn::text_zulu`]) and keeps
//!   the semantics the design relies on: event `uuid` dedupe answered
//!   `duplicate: true`, non-idempotent create, a stream that replays stored
//!   events strictly after `from_sequence_num` then follows live, history,
//!   list paging with `next_cursor`, archived status, and injected failures
//!   ([`Fault`]). Every request is recorded, headers included.
//! - [`StubClaude`]: a scriptable stand-in for the `claude` CLI's
//!   `auth status --json` (the renewal hook).
//! - [`ClaudeLogin`]: a CLI login folder (`.credentials.json` +
//!   `.claude.json`) with fake values.
//!
//! Room to grow: first answer wins per `request_id`, more injected failures,
//! list-driven discovery.

mod login;
mod server;
mod stub_claude;

pub use login::{ClaudeLogin, FakeAccount, in_ms};
pub use server::{FakeSessionServer, Fault, RecordedRequest, SeedSession, StoredEvent};
pub use stub_claude::{StubCall, StubClaude};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio::sync::Notify;

/// One step of a scripted worker turn.
#[derive(Clone, Debug)]
pub enum Step {
    /// A stored event (`client_event`): sequenced, replayed, in history.
    Stored(Value),
    /// A live-only event (`ephemeral_event`): text deltas and the like.
    Ephemeral(Value),
    /// Pause playback here until the test releases the gate.
    Hold(Gate),
}

/// The worker's side of one turn: what the session does after a message.
#[derive(Clone, Debug, Default)]
pub struct Turn {
    pub steps: Vec<Step>,
}

impl Turn {
    /// From a fixture: one `{"stream":"stored"|"ephemeral","payload":{…}}`
    /// per line.
    pub fn from_jsonl(text: &str) -> Self {
        let steps = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let mut value: Value = serde_json::from_str(line).expect("fixture line is JSON");
                let payload = value["payload"].take();
                match value["stream"].as_str() {
                    Some("ephemeral") => Step::Ephemeral(payload),
                    _ => Step::Stored(payload),
                }
            })
            .collect();
        Self { steps }
    }

    /// The first turn of the spike's no-repo session (recorded 2026-11-09):
    /// environment start-up, `system/init`, a `Bash` tool call
    /// (`uname -a; cat /tmp/zeron-init-marker 2>&1`) and its result, thinking,
    /// then a text reply, ending `result/success`.
    pub fn bash_tool() -> Self {
        Self::from_jsonl(include_str!("../fixtures/turn-bash-tool.jsonl"))
    }

    /// A plain-text turn from the spike's honest-identity session: the reply
    /// `ZULU`, streamed as a delta, then stored, then `result/success`.
    pub fn text_zulu() -> Self {
        Self::from_jsonl(include_str!("../fixtures/turn-text-zulu.jsonl"))
    }

    /// Pause playback right after the first step `at` matches; the returned
    /// gate releases it.
    pub fn hold_after(mut self, at: impl Fn(&Step) -> bool) -> (Self, Gate) {
        let gate = Gate::default();
        let index = self
            .steps
            .iter()
            .position(at)
            .expect("hold_after: no step matches");
        self.steps.insert(index + 1, Step::Hold(gate.clone()));
        (self, gate)
    }
}

/// Whether a step is a live text delta (for [`Turn::hold_after`]).
pub fn is_text_delta(step: &Step) -> bool {
    matches!(step, Step::Ephemeral(p)
        if p.pointer("/event/delta/type").and_then(Value::as_str) == Some("text_delta"))
}

/// A pause in a scripted turn.
#[derive(Clone, Debug, Default)]
pub struct Gate(Arc<GateInner>);

#[derive(Debug, Default)]
struct GateInner {
    reached: AtomicBool,
    reached_notify: Notify,
    released: AtomicBool,
    released_notify: Notify,
}

impl Gate {
    /// Let playback continue.
    pub fn release(&self) {
        self.0.released.store(true, Ordering::SeqCst);
        self.0.released_notify.notify_waiters();
    }

    /// Resolves once playback has reached the gate.
    pub async fn reached(&self) {
        loop {
            let notified = self.0.reached_notify.notified();
            if self.0.reached.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn pass(&self) {
        self.0.reached.store(true, Ordering::SeqCst);
        self.0.reached_notify.notify_waiters();
        loop {
            let notified = self.0.released_notify.notified();
            if self.0.released.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}
