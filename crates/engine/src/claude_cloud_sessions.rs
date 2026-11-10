//! The "Claude cloud sessions" setting (docs/design/provider-run-chats.md
//! §6 "Settings"): whether this device offers provider-run chats, and which
//! of its Claude logins act for them.
//!
//! Off by default. While it's on and a login is enabled, the
//! `claude-code-cloud` harness is registered (so the device's catalog lists
//! it, `remote_workspace` and all) and the device row advertises
//! [`zeron_proto::capabilities::PROVIDER_RUN_V1`], which is what makes the
//! device eligible to create and host these chats. Turning it off removes
//! both again. `ZERON_CLAUDE_CLOUD_SESSIONS=1` keeps the harness on whatever
//! the setting says (tests and the dev rig).
//!
//! Only the device's own login (`~/.claude`) exists until more accounts land
//! (ticket #14); it is enabled by default.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use zeron_proto::HarnessId;

use crate::EngineError;
use crate::registry::HarnessRegistry;

const FILE_NAME: &str = "claude-cloud-sessions.json";

/// The account id of the device's own Claude login.
pub const DEVICE_LOGIN: &str = "device";

/// What the user chose, persisted per device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaudeCloudSettings {
    /// The feature itself. Off by default.
    pub enabled: bool,
    /// Whether the device's own Claude login shows its sessions and starts
    /// chats. On by default, so the common case needs no setup.
    pub device_login: bool,
}

impl Default for ClaudeCloudSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            device_login: true,
        }
    }
}

/// One Claude login on this device, as the settings list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCloudAccount {
    /// [`DEVICE_LOGIN`] for the device's own login.
    pub id: String,
    /// The signed-in account's email, when the login folder names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub enabled: bool,
}

/// `GetClaudeCloudSessions` / `SetClaudeCloudSessions` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCloudStatus {
    pub settings: ClaudeCloudSettings,
    /// The device can create and host provider-run chats right now.
    pub active: bool,
    /// `ZERON_CLAUDE_CLOUD_SESSIONS=1` keeps the feature on.
    #[serde(default)]
    pub forced: bool,
    /// False on a Cloud machine, which never offers it.
    #[serde(default = "yes")]
    pub available: bool,
    #[serde(default)]
    pub accounts: Vec<ClaudeCloudAccount>,
}

fn yes() -> bool {
    true
}

/// A partial update: absent fields keep their value.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCloudChange {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub device_login: Option<bool>,
}

impl ClaudeCloudSettings {
    /// Apply a partial update.
    pub fn with(mut self, change: &ClaudeCloudChange) -> Self {
        if let Some(enabled) = change.enabled {
            self.enabled = enabled;
        }
        if let Some(device_login) = change.device_login {
            self.device_login = device_login;
        }
        self
    }

    /// Whether these settings put the harness on: the feature is on and a
    /// login acts for it.
    pub fn wants_harness(&self) -> bool {
        self.enabled && self.device_login
    }
}

/// The device's setting, loaded from and saved to its data dir.
pub struct ClaudeCloudSessions {
    path: PathBuf,
    stored: Mutex<ClaudeCloudSettings>,
    /// The sidebar's session list: its throttle and latest read.
    discovery: crate::provider_discovery::ProviderDiscovery,
}

impl ClaudeCloudSessions {
    /// Load `{data_dir}/claude-cloud-sessions.json`; missing or corrupt reads
    /// as the defaults (off).
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE_NAME);
        let stored = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            stored: Mutex::new(stored),
            discovery: Default::default(),
        }
    }

    pub(crate) fn discovery(&self) -> &crate::provider_discovery::ProviderDiscovery {
        &self.discovery
    }

    pub fn settings(&self) -> ClaudeCloudSettings {
        self.stored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Persist `next` (atomic replace) and keep it.
    pub fn save(&self, next: ClaudeCloudSettings) -> Result<(), EngineError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| EngineError::Other("settings have no folder".into()))?;
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, &next)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        temp.write_all(b"\n")?;
        temp.persist(&self.path)
            .map_err(|e| EngineError::Io(e.error))?;
        *self.stored.lock().unwrap_or_else(PoisonError::into_inner) = next;
        Ok(())
    }
}

/// Whether this engine may offer provider-run chats at all: never on a Cloud
/// machine.
fn available() -> bool {
    !crate::runner::is_cloud_platform()
}

/// Hand checks point the harness at the fake session server
/// (`zeron-provider-fake`'s `serve` example) through
/// `ZERON_CLAUDE_CLOUD_API`. Loopback addresses only, so no setting of it
/// can send a real login's token anywhere but Anthropic.
const API_OVERRIDE_ENV: &str = "ZERON_CLAUDE_CLOUD_API";

pub(crate) fn local_api_override() -> Option<String> {
    let url = std::env::var(API_OVERRIDE_ENV).ok()?;
    let url = url.trim().trim_end_matches('/');
    let rest = url.strip_prefix("http://")?;
    let host = rest.split([':', '/']).next()?;
    matches!(host, "127.0.0.1" | "localhost").then(|| url.to_owned())
}

/// Register or remove the `claude-code-cloud` harness to match `settings`
/// (the environment switch keeps it registered). Returns whether the device
/// can create and host provider-run chats now.
pub fn apply(registry: &HarnessRegistry, settings: &ClaudeCloudSettings) -> bool {
    if !available() {
        return false;
    }
    let wanted = settings.wants_harness() || zeron_harness::claude_cloud::enabled();
    let registered = registry.is_registered(HarnessId::ClaudeCodeCloud);
    if wanted && !registered {
        match zeron_harness::claude_cloud::ClaudeCloudConfig::device_default() {
            Some(mut config) => {
                if let Some(url) = local_api_override() {
                    config.base_url = url;
                }
                match zeron_harness::ClaudeCloudHarness::new(config) {
                    Ok(harness) => registry.register(Arc::new(harness)),
                    Err(error) => {
                        tracing::error!(%error, "Claude cloud sessions are unavailable on this device")
                    }
                }
            }
            None => {
                tracing::warn!("Claude cloud sessions: no home directory for the device's login");
            }
        }
    } else if !wanted && registered {
        registry.remove_provider_cloud(HarnessId::ClaudeCodeCloud);
    }
    registry.is_registered(HarnessId::ClaudeCodeCloud)
}

/// The status the settings page shows. Reads the device login's account
/// file for its email (no CLI call, no token read).
pub async fn status(registry: &HarnessRegistry, settings: ClaudeCloudSettings) -> ClaudeCloudStatus {
    let email = match zeron_harness::claude_cloud::credentials::ClaudeConfigDir::device_default() {
        Some(folder) => zeron_harness::claude_cloud::credentials::Credentials::new(folder, None)
            .account()
            .await
            .ok()
            .map(|account| account.email)
            .filter(|email| !email.is_empty()),
        None => None,
    };
    ClaudeCloudStatus {
        accounts: vec![ClaudeCloudAccount {
            id: DEVICE_LOGIN.to_owned(),
            email,
            enabled: settings.device_login,
        }],
        active: available() && registry.is_registered(HarnessId::ClaudeCodeCloud),
        forced: zeron_harness::claude_cloud::enabled(),
        available: available(),
        settings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_with_the_device_login_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = ClaudeCloudSessions::open(dir.path());
        let settings = store.settings();
        assert!(!settings.enabled);
        assert!(settings.device_login);
        assert!(!settings.wants_harness());
    }

    #[test]
    fn changes_persist_and_merge() {
        let dir = tempfile::tempdir().unwrap();
        let store = ClaudeCloudSessions::open(dir.path());
        let next = store.settings().with(&ClaudeCloudChange {
            enabled: Some(true),
            device_login: None,
        });
        store.save(next).unwrap();
        let reopened = ClaudeCloudSessions::open(dir.path());
        assert_eq!(
            reopened.settings(),
            ClaudeCloudSettings {
                enabled: true,
                device_login: true,
            }
        );
        assert!(reopened.settings().wants_harness());
        let without_login = reopened.settings().with(&ClaudeCloudChange {
            enabled: None,
            device_login: Some(false),
        });
        assert!(!without_login.wants_harness());
    }

    #[test]
    fn the_setting_registers_and_removes_the_harness() {
        // The environment switch would keep it on regardless.
        if zeron_harness::claude_cloud::enabled() {
            return;
        }
        let registry = HarnessRegistry::new();
        let on = ClaudeCloudSettings {
            enabled: true,
            device_login: true,
        };
        assert!(apply(&registry, &on));
        let listed = registry.descriptors();
        let cloud = listed
            .iter()
            .find(|d| d.id == HarnessId::ClaudeCodeCloud)
            .expect("listed while on");
        assert!(cloud.remote_workspace);
        assert!(!apply(&registry, &ClaudeCloudSettings::default()));
        assert!(!registry.is_registered(HarnessId::ClaudeCodeCloud));
        let no_login = ClaudeCloudSettings {
            enabled: true,
            device_login: false,
        };
        assert!(!apply(&registry, &no_login));
    }
}
