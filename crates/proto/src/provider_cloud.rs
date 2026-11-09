//! Provider-run chats: the identities every device must derive the same way
//! (docs/design/provider-run-chats.md §5). Pure.
//!
//! - A provider-run chat's id is a UUIDv5 of `<provider>:<session id>`, so two
//!   devices that import the same session write the same row.
//! - Its `cwd` is the pseudo path `claude-cloud://<session id>`: no device has
//!   it on disk, so an older engine that tries to launch it locally fails.
//! - Each message Zeron sends carries a provider event `uuid` derived from the
//!   Zeron message id, which the provider uses to ignore repeats. Interrupts
//!   and setting changes derive theirs from the command that caused them, and
//!   a prompt's answer from the prompt's `request_id`.

use crate::ProviderSession;

/// The Claude Code cloud (claude.ai/code).
pub const CLAUDE: &str = "claude";

/// The pseudo working directory scheme of a Claude provider-run chat.
pub const CLAUDE_CWD_SCHEME: &str = "claude-cloud://";

/// Namespace of the UUIDv5 chat ids and event uuids (fixed forever: every
/// device must derive the same ids).
const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x5a8e_7c1d_2b4f_4e0a_9c6d_8f31_a2b7_c5e4);

/// The chat id of `provider`'s session `session_id`: UUIDv5 of
/// `<provider>:<session id>`.
pub fn chat_id(provider: &str, session_id: &str) -> String {
    uuid::Uuid::new_v5(&NAMESPACE, format!("{provider}:{session_id}").as_bytes()).to_string()
}

/// [`chat_id`] for a recorded session.
pub fn chat_id_for(session: &ProviderSession) -> String {
    chat_id(&session.provider, &session.session_id)
}

/// The pseudo `cwd` of a Claude provider-run chat.
pub fn claude_cwd(session_id: &str) -> String {
    format!("{CLAUDE_CWD_SCHEME}{session_id}")
}

/// The session a Claude provider-run chat's pseudo `cwd` names; `None` for
/// any other path.
pub fn claude_session_from_cwd(cwd: &str) -> Option<&str> {
    cwd.strip_prefix(CLAUDE_CWD_SCHEME)
        .map(|rest| rest.trim_end_matches('/'))
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

/// Whether `cwd` is a provider-run chat's pseudo path (never a folder).
pub fn is_pseudo_cwd(cwd: &str) -> bool {
    claude_session_from_cwd(cwd).is_some()
}

/// The provider event `uuid` a Zeron message travels under. A message id that
/// already is a UUID is used as is; any other id maps to a UUIDv5 of it, so a
/// retried send always carries the same uuid and the provider ignores the
/// repeat.
pub fn event_uuid(message_id: &str) -> String {
    match uuid::Uuid::parse_str(message_id) {
        Ok(id) => id.hyphenated().to_string(),
        Err(_) => {
            uuid::Uuid::new_v5(&NAMESPACE, format!("message:{message_id}").as_bytes()).to_string()
        }
    }
}

/// The provider event `uuid` of a control request (`kind`: `set_model`,
/// `set_permission_mode`) that rides Zeron command `command_id` (a message
/// whose own event takes [`event_uuid`]): a UUIDv5 of both, so re-executing
/// the command (another host, a retry) sends the same uuid and the provider
/// ignores the repeat. A Stop is its own command: its interrupt goes under
/// [`event_uuid`] of the command id.
pub fn control_uuid(command_id: &str, kind: &str) -> String {
    uuid::Uuid::new_v5(
        &NAMESPACE,
        format!("control:{kind}:{command_id}").as_bytes(),
    )
    .to_string()
}

/// The provider event `uuid` of the answer to a prompt (a `control_request`
/// the session asked): a UUIDv5 of its `request_id`, the same on every
/// device, so two hosts answering the same prompt send one event.
pub fn answer_uuid(request_id: &str) -> String {
    uuid::Uuid::new_v5(&NAMESPACE, format!("answer:{request_id}").as_bytes()).to_string()
}

/// What a provider-run chat shows when its host can't act for the chat's
/// account (docs/design/provider-run-chats.md §3). The host records it in
/// the chat document's metadata (`providerState`) and clears it once a
/// request for that account works again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProviderChatState {
    /// The login for the chat's account is signed out, expired past the
    /// CLI's renewal, or refused even after a renewal: "Sign in again".
    #[serde(rename_all = "camelCase")]
    SignInAgain { email: String },
    /// No eligible device (one with a login for the chat's account) is
    /// online: "Sign in as <email> to continue". Decided by hosting, never
    /// by one device finding it has no such login; never a reason to turn
    /// the feature off.
    #[serde(rename_all = "camelCase")]
    SignInAs { email: String },
}

impl ProviderChatState {
    /// The words the chat shows.
    pub fn label(&self) -> String {
        match self {
            Self::SignInAgain { .. } => "Sign in again".to_owned(),
            Self::SignInAs { email } => format!("Sign in as {email} to continue"),
        }
    }
}

/// A provider session's title from its first prompt: the first non-empty
/// line, at most 80 characters.
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut title: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        title.push('…');
    }
    if title.is_empty() {
        title = "Zeron chat".into();
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_the_first_line() {
        assert_eq!(
            title_from_prompt("\n  Fix the build\nthen test"),
            "Fix the build"
        );
        assert_eq!(title_from_prompt(""), "Zeron chat");
        assert_eq!(title_from_prompt(&"x".repeat(90)).chars().count(), 81);
    }

    #[test]
    fn chat_states_read_as_the_ui_shows_them() {
        let again = ProviderChatState::SignInAgain {
            email: "a@example.test".into(),
        };
        assert_eq!(again.label(), "Sign in again");
        assert_eq!(
            serde_json::to_value(&again).unwrap(),
            serde_json::json!({"kind": "signInAgain", "email": "a@example.test"})
        );
        let elsewhere: ProviderChatState =
            serde_json::from_str(r#"{"kind":"signInAs","email":"a@example.test"}"#).unwrap();
        assert_eq!(elsewhere.label(), "Sign in as a@example.test to continue");
    }

    #[test]
    fn chat_ids_are_deterministic_per_provider_session() {
        let a = chat_id(CLAUDE, "cse_01ABC");
        assert_eq!(a, chat_id(CLAUDE, "cse_01ABC"));
        assert_ne!(a, chat_id(CLAUDE, "cse_01ABD"));
        let parsed = uuid::Uuid::parse_str(&a).unwrap();
        assert_eq!(parsed.get_version_num(), 5);
        let expected = uuid::Uuid::new_v5(&NAMESPACE, b"claude:cse_01ABC").to_string();
        assert_eq!(a, expected);
    }

    #[test]
    fn pseudo_cwd_round_trips_and_rejects_folders() {
        let cwd = claude_cwd("cse_01ABC");
        assert_eq!(cwd, "claude-cloud://cse_01ABC");
        assert_eq!(claude_session_from_cwd(&cwd), Some("cse_01ABC"));
        assert!(is_pseudo_cwd(&cwd));
        for folder in [
            "/home/me/proj",
            "~",
            "C:\\Users\\me",
            "claude-cloud://",
            "claude-cloud://a/b",
        ] {
            assert!(!is_pseudo_cwd(folder), "{folder}");
        }
    }

    #[test]
    fn event_uuids_keep_uuid_message_ids_and_derive_the_rest() {
        let id = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
        assert_eq!(event_uuid(id), id);
        let derived = event_uuid("msg-np-1");
        assert_eq!(derived, event_uuid("msg-np-1"));
        assert!(uuid::Uuid::parse_str(&derived).is_ok());
        assert_ne!(derived, event_uuid("msg-np-2"));
    }

    #[test]
    fn control_and_answer_uuids_are_deterministic_v5s() {
        let id = "6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70";
        let model = control_uuid(id, "set_model");
        assert_eq!(model, control_uuid(id, "set_model"));
        assert_ne!(model, control_uuid(id, "set_permission_mode"));
        assert_ne!(model, event_uuid(id));
        assert_eq!(uuid::Uuid::parse_str(&model).unwrap().get_version_num(), 5);
        let answer = answer_uuid("req-1");
        assert_eq!(answer, answer_uuid("req-1"));
        assert_ne!(answer, answer_uuid("req-2"));
        assert_eq!(
            answer,
            uuid::Uuid::new_v5(&NAMESPACE, b"answer:req-1").to_string()
        );
    }
}
