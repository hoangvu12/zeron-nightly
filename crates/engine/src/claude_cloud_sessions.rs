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
//!
//! The kill switch (ticket #15) is the file `claude-cloud/DISABLED` in the
//! engine's data folder: while it is in place the feature is off on this
//! device, saying what the file says (checked before every request, so
//! placing it acts at once), and the device doesn't offer to host. When the
//! feature turns itself off (a refused login, list or create, an event in a
//! shape Zeron doesn't know), the reason is written there, so it stays off
//! across restarts until the person removes the file.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use zeron_proto::HarnessId;

use crate::EngineError;
use crate::registry::HarnessRegistry;

const FILE_NAME: &str = "claude-cloud-sessions.json";
/// The feature's data folder, and its kill switch in it.
const DATA_FOLDER: &str = "claude-cloud";
const KILL_SWITCH: &str = "DISABLED";

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
    /// Why the feature is off on this device although it's on in the
    /// settings: its kill switch (see the module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<String>,
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
    /// Held across a whole change (read, save, apply, capability), so two
    /// changes at once can't leave the harness or capability out of step
    /// with the saved setting.
    changing: Mutex<()>,
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
            changing: Mutex::new(()),
            discovery: Default::default(),
        }
    }

    /// The kill switch: `claude-cloud/DISABLED` next to the settings file.
    pub fn kill_switch(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(DATA_FOLDER)
            .join(KILL_SWITCH)
    }

    /// Hold while making a change, from reading the current settings through
    /// applying them. Never across an `.await`.
    pub fn changing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.changing.lock().unwrap_or_else(PoisonError::into_inner)
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
pub const API_OVERRIDE_ENV: &str = "ZERON_CLAUDE_CLOUD_API";

/// The base URL [`API_OVERRIDE_ENV`] asks for: `Ok(None)` when it's unset
/// (the production API), `Err` when it names anything but an
/// `http(s)://` loopback address (the same hosts the API client treats as
/// loopback).
fn api_override(value: Option<&str>) -> Result<Option<String>, ()> {
    let Some(url) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let url = url.trim_end_matches('/');
    let schemed = url.starts_with("http://") || url.starts_with("https://");
    if schemed && zeron_harness::claude_cloud::api::is_loopback(url) {
        Ok(Some(url.to_owned()))
    } else {
        Err(())
    }
}

/// The device's `claude-code-cloud` harness: the device's own login, on the
/// production API unless [`API_OVERRIDE_ENV`] names a loopback fake. `None`
/// when it can't be built, and when the override names anything else: a
/// rejected override never falls back to the production API, or a hand
/// check's fake login would go to Anthropic.
pub(crate) fn device_harness() -> Option<zeron_harness::ClaudeCloudHarness> {
    let Some(mut config) = zeron_harness::claude_cloud::ClaudeCloudConfig::device_default() else {
        tracing::warn!("Claude cloud sessions: no home directory for the device's login");
        return None;
    };
    match api_override(std::env::var(API_OVERRIDE_ENV).ok().as_deref()) {
        Ok(Some(url)) => config.base_url = url,
        Ok(None) => {}
        Err(()) => {
            tracing::error!(
                "{API_OVERRIDE_ENV} names no loopback address; Claude cloud sessions stay off"
            );
            return None;
        }
    }
    zeron_harness::ClaudeCloudHarness::new(config)
        .inspect_err(
            |error| tracing::error!(%error, "Claude cloud sessions are unavailable on this device"),
        )
        .ok()
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
        if let Some(harness) = device_harness() {
            registry.register(Arc::new(harness));
        }
    } else if !wanted && registered {
        registry.remove_provider_cloud(HarnessId::ClaudeCodeCloud);
    }
    if let (Some(store), Ok(harness)) = (
        registry.claude_cloud(),
        registry.resolve(HarnessId::ClaudeCodeCloud),
    ) && let Some(cloud) = harness.provider_cloud()
    {
        cloud.set_kill_switch(store.kill_switch());
    }
    registry.provider_run_active()
}

/// Why the registered harness turned the feature off, if it did.
pub fn disabled(registry: &HarnessRegistry) -> Option<String> {
    let harness = registry.resolve(HarnessId::ClaudeCodeCloud).ok()?;
    let cloud = harness.provider_cloud()?;
    cloud.status().borrow().disabled.clone()
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
        active: available() && registry.provider_run_active(),
        forced: zeron_harness::claude_cloud::enabled(),
        disabled: disabled(registry),
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
    fn the_api_override_takes_loopback_addresses_only() {
        assert_eq!(api_override(None), Ok(None));
        assert_eq!(api_override(Some("  ")), Ok(None));
        for url in [
            "http://127.0.0.1:4100",
            "http://localhost:4100/",
            "http://[::1]:4100",
            "https://127.0.0.1",
        ] {
            assert_eq!(
                api_override(Some(url)),
                Ok(Some(url.trim_end_matches('/').to_owned())),
                "{url}"
            );
        }
        for url in [
            "http://example.test",
            "http://127.0.0.1.example.test",
            "ftp://127.0.0.1",
            "127.0.0.1:4100",
            "https://api.anthropic.com",
        ] {
            assert_eq!(api_override(Some(url)), Err(()), "{url}");
        }
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
