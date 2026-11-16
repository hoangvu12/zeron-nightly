//! Each sandbox provider's host device (spec #37): one `sandboxHosts` row per
//! provider, keyed by its id, so every device names the same one. The host
//! device writes it when it takes the provider's key; a later write wins.

use super::*;
use zeron_proto::SandboxHost;

impl RegistryDoc {
    /// Every connected provider's host device.
    pub fn sandbox_hosts(&self) -> Vec<SandboxHost> {
        let mut hosts: Vec<SandboxHost> = self
            .overlay_rows(KIND_SANDBOX_HOSTS)
            .iter()
            .filter_map(row_to::<SandboxHost>)
            .collect();
        hosts.sort_by(|a, b| a.provider.cmp(&b.provider));
        hosts
    }

    pub fn sandbox_host(&self, provider: &str) -> Option<SandboxHost> {
        self.overlay_row(KIND_SANDBOX_HOSTS, provider)
            .as_ref()
            .and_then(row_to::<SandboxHost>)
    }

    pub fn set_sandbox_host(&mut self, host: &SandboxHost) {
        let set = fields([
            ("provider", json!(host.provider)),
            ("hostDeviceId", json!(host.host_device_id)),
            ("connectedOn", opt_str(host.connected_on.as_deref())),
        ]);
        self.write(KIND_SANDBOX_HOSTS, &host.provider, OpKind::Upsert, set);
    }

    /// `provider` was disconnected: no device is its host device any more
    /// (a later connect names one again, its write the newer).
    pub fn clear_sandbox_host(&mut self, provider: &str) {
        if self.row_exists(KIND_SANDBOX_HOSTS, provider) {
            self.delete_row_ops(&[(KIND_SANDBOX_HOSTS, provider)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_providers_host_device_reads_back_and_the_last_write_wins() {
        let mut doc = RegistryDoc::new("laptop");
        assert!(doc.sandbox_hosts().is_empty());
        let laptop = SandboxHost {
            provider: "docker".into(),
            host_device_id: "laptop".into(),
            connected_on: Some("laptop".into()),
        };
        doc.set_sandbox_host(&laptop);
        assert_eq!(doc.sandbox_host("docker"), Some(laptop.clone()));
        let server = SandboxHost {
            host_device_id: "server".into(),
            ..laptop
        };
        doc.set_sandbox_host(&server);
        assert_eq!(doc.sandbox_hosts(), [server.clone()]);
        assert_eq!(doc.sandbox_host("boat"), None);
    }

    #[test]
    fn a_disconnected_provider_names_no_host_device_until_it_connects_again() {
        let mut doc = RegistryDoc::new("laptop");
        let server = SandboxHost {
            provider: "docker".into(),
            host_device_id: "server".into(),
            connected_on: Some("laptop".into()),
        };
        doc.set_sandbox_host(&server);
        doc.clear_sandbox_host("docker");
        assert_eq!(doc.sandbox_host("docker"), None);
        assert!(doc.sandbox_hosts().is_empty());
        // (Clearing one that isn't there is nothing.)
        doc.clear_sandbox_host("boat");
        // Connected again, it's named again.
        doc.set_sandbox_host(&server);
        assert_eq!(doc.sandbox_host("docker"), Some(server));
    }
}
