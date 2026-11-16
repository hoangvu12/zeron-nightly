//! Places chats run on (a sandbox provider a device keeps; spec #37): how a
//! composer names one to ask what agents it offers, and how a sandbox says
//! the agent picked isn't on it. Pure; every client reads the same words.

use crate::HarnessId;

/// A place's catalog target: `place:<provider>` or
/// `place:<provider>:<template>`, passed as `targetDeviceId` to
/// `ListHarnesses` / `ListModels`. The device keeping the provider answers
/// it itself, never asking a sandbox.
pub const PLACE_TARGET_PREFIX: &str = "place:";

pub fn place_target(provider: &str, template: Option<&str>) -> String {
    match template {
        Some(template) => format!("{PLACE_TARGET_PREFIX}{provider}:{template}"),
        None => format!("{PLACE_TARGET_PREFIX}{provider}"),
    }
}

/// `(provider, template)` of a place target; `None` for any other target.
/// A template may hold `:` (an image reference); a provider id can't.
pub fn parse_place_target(target: &str) -> Option<(&str, Option<&str>)> {
    let rest = target.strip_prefix(PLACE_TARGET_PREFIX)?;
    let (provider, template) = match rest.split_once(':') {
        Some((provider, template)) => (provider, Some(template).filter(|t| !t.is_empty())),
        None => (rest, None),
    };
    (!provider.is_empty()).then_some((provider, template))
}

/// The reason a sandbox refuses a send whose agent isn't there:
/// `agent_not_on_template:<harness>: <Agent> isn't on this template`. The
/// message is kept (it waits, refused, until it's sent again) and no other
/// agent runs it.
pub const AGENT_MISSING_PREFIX: &str = "agent_not_on_template:";

pub fn agent_missing_reason(harness: HarnessId, name: &str) -> String {
    format!(
        "{AGENT_MISSING_PREFIX}{}: {name} isn't on this template",
        harness.as_str()
    )
}

/// The agent a refusal names and the words to show for it.
pub fn agent_missing(reason: &str) -> Option<(HarnessId, &str)> {
    let rest = reason.strip_prefix(AGENT_MISSING_PREFIX)?;
    let (slug, words) = rest.split_once(':')?;
    Some((HarnessId::from_slug(slug)?, words.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_targets_round_trip_with_or_without_a_template() {
        assert_eq!(place_target("docker", Some("zeron-dev")), "place:docker:zeron-dev");
        assert_eq!(
            parse_place_target("place:docker:docker/sandbox:claude"),
            Some(("docker", Some("docker/sandbox:claude")))
        );
        assert_eq!(parse_place_target(&place_target("boat", None)), Some(("boat", None)));
        assert_eq!(parse_place_target("place:"), None);
        assert_eq!(parse_place_target("device-1"), None);
    }

    #[test]
    fn a_missing_agent_reads_back_as_its_agent_and_words() {
        let reason = agent_missing_reason(HarnessId::Codex, "Codex");
        assert_eq!(
            agent_missing(&reason),
            Some((HarnessId::Codex, "Codex isn't on this template"))
        );
        assert_eq!(agent_missing("harness binary not found: Codex"), None);
    }
}
