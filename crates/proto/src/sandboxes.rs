//! Sandbox devices (spec #37): the engine in one chat's sandbox, on a
//! provider the user brought (Docker Sandbox, Boat, …). Its device row says
//! which provider with a capability of its own, `sandbox:<provider>`. Device
//! lists and pickers hide those rows and show one entry per provider instead.
//!
//! Zeron Cloud's [`crate::CLOUD_SESSION_CAPABILITY`] is not reused: clients
//! send a device with it to Zeron Cloud to wake and sleep.

/// Capabilities naming a sandbox's provider start with this.
pub const SANDBOX_CAPABILITY_PREFIX: &str = "sandbox:";

/// The capability a sandbox on `provider` advertises.
pub fn sandbox_capability(provider: &str) -> String {
    format!("{SANDBOX_CAPABILITY_PREFIX}{provider}")
}

/// The provider a device's capabilities name, when it is a sandbox.
pub fn sandbox_provider<S: AsRef<str>>(capabilities: &[S]) -> Option<&str> {
    capabilities.iter().find_map(|c| {
        c.as_ref()
            .strip_prefix(SANDBOX_CAPABILITY_PREFIX)
            .filter(|provider| !provider.is_empty())
    })
}

/// Whether a device is one chat's machine (a sandbox, or a Zeron Cloud
/// session's): never listed or picked as a device, but still found by id
/// for its chats.
pub fn is_one_chats_machine<S: AsRef<str>>(capabilities: &[S]) -> bool {
    sandbox_provider(capabilities).is_some()
        || capabilities
            .iter()
            .any(|c| c.as_ref() == crate::CLOUD_SESSION_CAPABILITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sandbox_names_its_provider_in_its_capabilities() {
        let caps = vec![
            "message-queue-v1".to_string(),
            sandbox_capability("docker"),
        ];
        assert_eq!(caps[1], "sandbox:docker");
        assert_eq!(sandbox_provider(&caps), Some("docker"));
    }

    #[test]
    fn a_machine_of_one_chats_is_a_sandbox_or_a_cloud_session() {
        assert!(is_one_chats_machine(&["sandbox:docker"]));
        assert!(is_one_chats_machine(&[crate::CLOUD_SESSION_CAPABILITY]));
        assert!(!is_one_chats_machine(&["sandbox-host-v1", "sandbox:"]));
        assert!(!is_one_chats_machine::<&str>(&[]));
    }

    #[test]
    fn other_devices_name_no_provider() {
        assert_eq!(sandbox_provider::<&str>(&[]), None);
        assert_eq!(sandbox_provider(&["cloud-session", "sandbox-host-v1"]), None);
        assert_eq!(sandbox_provider(&["sandbox:"]), None);
    }
}
