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

/// The tag a session Zeron creates carries: `zeron:<first prompt uuid>`.
/// The first prompt's uuid is chosen before the create (the chat id, a
/// UUIDv5 of the session id, can't be), so a create whose answer was lost
/// can be found in the session list by it.
pub fn create_tag(prompt_uuid: &str) -> String {
    format!("zeron:{prompt_uuid}")
}

/// Where the person sees a session on claude.ai.
pub fn claude_session_url(session_id: &str) -> String {
    format!("https://claude.ai/code/{session_id}")
}

/// Where the person sees their Claude cloud sessions on claude.ai.
pub const CLAUDE_SESSIONS_URL: &str = "https://claude.ai/code";

/// What a provider-run chat shows about its session beyond its transcript
/// (docs/design/provider-run-chats.md §3, §6), recorded in the chat
/// document's metadata (`providerState`):
///
/// - sign-in states, which the host records when it can't act for the
///   chat's account and clears once a request for that account works again;
/// - "Archived on claude.ai", recorded when the session is archived at the
///   provider while the chat is kept (archived in Zeron, or unarchived after
///   the session was archived), and cleared once the session is active again.
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
    /// The session is archived at the provider (claude.ai, the phone): it
    /// takes no messages. `url` opens it on claude.ai.
    #[serde(rename_all = "camelCase")]
    ArchivedOnProvider { url: String },
}

impl ProviderChatState {
    /// The words the chat shows.
    pub fn label(&self) -> String {
        match self {
            Self::SignInAgain { .. } => "Sign in again".to_owned(),
            Self::SignInAs { email } => format!("Sign in as {email} to continue"),
            Self::ArchivedOnProvider { .. } => "Archived on claude.ai".to_owned(),
        }
    }

    /// A sign-in state (which the host's login decides), as opposed to a
    /// fact about the session.
    pub fn is_sign_in(&self) -> bool {
        matches!(self, Self::SignInAgain { .. } | Self::SignInAs { .. })
    }

    /// The page on claude.ai the chat links to, if it shows one.
    pub fn link(&self) -> Option<&str> {
        match self {
            Self::ArchivedOnProvider { url } => Some(url),
            _ => None,
        }
    }
}

/// A provider-run chat's host saying it can't host the chat right now
/// (docs/design/provider-run-chats.md §3): the row's `hostUnavailable`,
/// written by that host as a field-only update, cleared when its check
/// passes again or a new host takes the chat over. It carries no account or
/// credential detail beyond the reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostUnavailable {
    pub device_id: String,
    pub reason: HostUnavailableReason,
}

/// Why a host can't host a provider-run chat ([`HostUnavailable::reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostUnavailableReason {
    /// The host's login isn't the chat's account (switched, or signed out
    /// of it).
    Account,
    /// A reason from a newer build.
    #[serde(other)]
    Other,
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

/// The logical device that owns projects living only at a provider cloud:
/// a GitHub repository no device has cloned ("New project from GitHub…" under
/// Anthropic cloud). Like Zeron Cloud's logical device it has no engine and
/// is never listed, dialed or forwarded to; its projects run provider-run
/// chats only.
pub const PROJECTS_DEVICE: &str = "provider-cloud";

/// Whether `device_id` owns projects that are only a GitHub repository, with
/// no folder on any device: Zeron Cloud's and [`PROJECTS_DEVICE`]'s.
pub fn is_repository_only_device(device_id: &str) -> bool {
    device_id == PROJECTS_DEVICE || device_id.starts_with(crate::CLOUD_DEVICE_PREFIX)
}

/// The GitHub repository (`owner/name`) that `input` names: `owner/name`, or
/// a github.com remote (https, `git@github.com:`, `ssh://`), with or
/// without `.git` and a trailing slash. `None` for anything else.
pub fn github_repo_from_input(input: &str) -> Option<String> {
    let input = input.trim();
    let rest = [
        "https://github.com/",
        "http://github.com/",
        "https://www.github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
        "github.com/",
    ]
    .iter()
    .find_map(|prefix| {
        input
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &input[prefix.len()..])
    })
    .unwrap_or(input);
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repos_read_from_what_people_paste() {
        for input in [
            "acme/app",
            " acme/app ",
            "https://github.com/acme/app",
            "https://github.com/acme/app.git",
            "https://github.com/acme/app/",
            "git@github.com:acme/app.git",
            "ssh://git@github.com/acme/app",
            "github.com/acme/app",
        ] {
            assert_eq!(
                github_repo_from_input(input).as_deref(),
                Some("acme/app"),
                "{input}"
            );
        }
        for input in [
            "app",
            "",
            "https://github.com/acme",
            "https://gitlab.com/acme/app",
            "acme/app/tree/main",
            "a b/c",
            "../app",
        ] {
            assert_eq!(github_repo_from_input(input), None, "{input}");
        }
        assert!(is_repository_only_device(PROJECTS_DEVICE));
        assert!(is_repository_only_device("cloud-org1"));
        assert!(!is_repository_only_device("laptop"));
    }

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
    fn archived_on_claude_reads_with_its_link() {
        let state = ProviderChatState::ArchivedOnProvider {
            url: claude_session_url("cse_01ABC"),
        };
        assert_eq!(state.label(), "Archived on claude.ai");
        assert_eq!(state.link(), Some("https://claude.ai/code/cse_01ABC"));
        assert!(!state.is_sign_in());
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::json!({"kind": "archivedOnProvider", "url": "https://claude.ai/code/cse_01ABC"})
        );
        assert_eq!(
            create_tag("6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70"),
            "zeron:6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70"
        );
    }

    #[test]
    fn host_unavailable_reads_reasons_it_doesnt_know() {
        let known: HostUnavailable =
            serde_json::from_str(r#"{"deviceId":"pc","reason":"account"}"#).unwrap();
        assert_eq!(known.reason, HostUnavailableReason::Account);
        let newer: HostUnavailable =
            serde_json::from_str(r#"{"deviceId":"pc","reason":"quota"}"#).unwrap();
        assert_eq!(newer.reason, HostUnavailableReason::Other);
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
