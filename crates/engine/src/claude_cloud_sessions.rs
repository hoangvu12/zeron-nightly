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
//! The logins (docs/design/provider-run-chats.md §3, ticket #14):
//!
//! - **The device's own login** (`~/.claude`), on by default.
//! - **Added accounts.** "Add account" runs the official `claude auth login`
//!   into a new folder of this device's data dir (`claude-accounts/<id>`),
//!   so another account's sessions show and run here too. Each is switched
//!   on or off per device.
//! - **Every request** finds the login signed in to its chat's account when
//!   it's sent. When none is (the device's login was switched, say), this
//!   device can't host the chat: it publishes `hostUnavailable` and stops,
//!   and another device takes over on use. That never turns the feature
//!   off.
//! - **A switched device login.** The account the device's login was last
//!   seen in is remembered; once it changes, Zeron offers to add the
//!   previous account ("Keep <email>'s cloud chats working on this
//!   device?"), until it's added or the offer is declined.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use zeron_proto::HarnessId;

use crate::EngineError;
use crate::registry::HarnessRegistry;

const FILE_NAME: &str = "claude-cloud-sessions.json";
/// Where added accounts' login folders live, in the data dir.
const ACCOUNTS_DIR: &str = "claude-accounts";

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
    /// Accounts added to this device, each signed in through the CLI into a
    /// folder of its own.
    pub accounts: Vec<AddedAccount>,
    /// The account the device's own login was last seen signed in to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_account: Option<KnownAccount>,
    /// The device login's account before it was switched, which Zeron
    /// offers to add.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<KnownAccount>,
}

impl Default for ClaudeCloudSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            device_login: true,
            accounts: Vec::new(),
            device_account: None,
            offer: None,
        }
    }
}

/// An account added to this device: its login folder is
/// `claude-accounts/<id>` in the data dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedAccount {
    pub id: String,
    pub enabled: bool,
}

/// A Claude account, as a login was seen signed in to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownAccount {
    pub account_uuid: String,
    pub email: String,
}

impl From<zeron_harness::provider_cloud::LoginAccount> for KnownAccount {
    fn from(account: zeron_harness::provider_cloud::LoginAccount) -> Self {
        Self {
            account_uuid: account.account_uuid,
            email: account.email,
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
    /// "Keep <email>'s cloud chats working on this device?": the device's
    /// login was switched away from this account, which isn't added here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<String>,
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
    /// Switch one added account on or off.
    #[serde(default)]
    pub account: Option<AddedAccount>,
    /// Decline the offer to add the device login's previous account.
    #[serde(default)]
    pub dismiss_offer: bool,
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
        if let Some(switch) = &change.account
            && let Some(account) = self.accounts.iter_mut().find(|a| a.id == switch.id)
        {
            account.enabled = switch.enabled;
        }
        if change.dismiss_offer {
            self.offer = None;
        }
        self
    }

    /// Whether these settings put the harness on: the feature is on and a
    /// login acts for it.
    pub fn wants_harness(&self) -> bool {
        self.enabled && (self.device_login || self.accounts.iter().any(|a| a.enabled))
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

    /// Hold while making a change, from reading the current settings through
    /// applying them. Never across an `.await`.
    pub fn changing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.changing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn discovery(&self) -> &crate::provider_discovery::ProviderDiscovery {
        &self.discovery
    }

    /// The login folder of added account `id`.
    pub fn account_folder(&self, id: &str) -> PathBuf {
        self.path
            .parent()
            .unwrap_or(Path::new("."))
            .join(ACCOUNTS_DIR)
            .join(id)
    }

    /// The folders of the added accounts switched on, in order.
    fn enabled_folders(&self, settings: &ClaudeCloudSettings) -> Vec<PathBuf> {
        settings
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .map(|a| self.account_folder(&a.id))
            .collect()
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
/// (the environment switch keeps it registered), and hand it the logins
/// switched on. Returns whether the device can create and host
/// provider-run chats now.
pub fn apply(
    registry: &HarnessRegistry,
    store: &ClaudeCloudSessions,
    settings: &ClaudeCloudSettings,
) -> bool {
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
    if let Some(harness) = provider_harness(registry) {
        let cloud = harness.provider_cloud().expect("a provider-cloud harness");
        cloud.set_logins(settings.device_login, store.enabled_folders(settings));
    }
    registry.is_registered(HarnessId::ClaudeCodeCloud)
}

/// The registered `claude-code-cloud` harness, if any.
fn provider_harness(registry: &HarnessRegistry) -> Option<Arc<dyn zeron_harness::Harness>> {
    if !registry.is_registered(HarnessId::ClaudeCodeCloud) {
        return None;
    }
    registry
        .resolve(HarnessId::ClaudeCodeCloud)
        .ok()
        .filter(|harness| harness.provider_cloud().is_some())
}

/// The account the device's own login is signed in to now: the harness's
/// device login while it's registered, else the CLI's default folder (a
/// file read; no CLI call, no token read).
async fn device_account(registry: &HarnessRegistry) -> Option<KnownAccount> {
    if let Some(harness) = provider_harness(registry) {
        let cloud = harness.provider_cloud()?;
        return cloud.device_account().await.map(KnownAccount::from);
    }
    let folder = zeron_harness::claude_cloud::credentials::ClaudeConfigDir::device_default()?;
    folder_account(folder).await
}

async fn folder_account(
    folder: zeron_harness::claude_cloud::credentials::ClaudeConfigDir,
) -> Option<KnownAccount> {
    let account = zeron_harness::claude_cloud::credentials::Credentials::new(folder, None)
        .account()
        .await
        .ok()?;
    Some(KnownAccount {
        account_uuid: account.account_uuid,
        email: account.email,
    })
}

/// Note the account the device's login is in now; when it differs from the
/// one last seen, the previous one becomes the offer (see module docs).
pub(crate) fn note_device_account(store: &ClaudeCloudSessions, now: Option<&KnownAccount>) {
    let Some(now) = now else {
        return; // signed out: nothing to compare
    };
    let _changing = store.changing();
    let mut next = store.settings();
    match &next.device_account {
        Some(seen) if seen == now => return,
        Some(seen) if seen.account_uuid != now.account_uuid => {
            next.offer = Some(seen.clone());
        }
        _ => {}
    }
    next.device_account = Some(now.clone());
    if next
        .offer
        .as_ref()
        .is_some_and(|offer| offer.account_uuid == now.account_uuid)
    {
        next.offer = None;
    }
    if let Err(error) = store.save(next) {
        tracing::warn!(%error, "couldn't remember the device's Claude login");
    }
}

/// The status the settings page shows. Reads the logins' account files for
/// their emails (no CLI call, no token read), and notices a switched device
/// login.
pub async fn status(registry: &HarnessRegistry, store: &ClaudeCloudSessions) -> ClaudeCloudStatus {
    let device = device_account(registry).await;
    note_device_account(store, device.as_ref());
    let settings = store.settings();
    let mut accounts = vec![ClaudeCloudAccount {
        id: DEVICE_LOGIN.to_owned(),
        email: device
            .as_ref()
            .map(|a| a.email.clone())
            .filter(|email| !email.is_empty()),
        enabled: settings.device_login,
    }];
    let mut held: Vec<String> = device.iter().map(|a| a.account_uuid.clone()).collect();
    for added in &settings.accounts {
        let folder = zeron_harness::claude_cloud::credentials::ClaudeConfigDir::at(
            store.account_folder(&added.id),
        );
        let account = folder_account(folder).await;
        held.extend(account.iter().map(|a| a.account_uuid.clone()));
        accounts.push(ClaudeCloudAccount {
            id: added.id.clone(),
            email: account.map(|a| a.email).filter(|email| !email.is_empty()),
            enabled: added.enabled,
        });
    }
    let offer = settings
        .offer
        .as_ref()
        .filter(|offer| !held.contains(&offer.account_uuid))
        .map(|offer| offer.email.clone());
    ClaudeCloudStatus {
        accounts,
        offer,
        active: available() && registry.is_registered(HarnessId::ClaudeCodeCloud),
        forced: zeron_harness::claude_cloud::enabled(),
        available: available(),
        settings,
    }
}

/// "Add account": sign a new folder in with the official CLI (the person
/// finishes in the browser) and add its account to this device. Fails,
/// leaving nothing behind, when the sign-in doesn't finish or names an
/// account this device already has.
pub async fn add_account(
    registry: &HarnessRegistry,
    store: &ClaudeCloudSessions,
) -> Result<KnownAccount, String> {
    let harness = provider_harness(registry)
        .ok_or_else(|| "Turn on Claude cloud sessions to add an account.".to_owned())?;
    let cloud = harness.provider_cloud().expect("a provider-cloud harness");
    let id = uuid::Uuid::new_v4().simple().to_string();
    let folder = store.account_folder(&id);
    let signed_in = cloud.sign_in(&folder).await;
    let forget = || {
        if let Err(error) = std::fs::remove_dir_all(&folder) {
            tracing::warn!(%error, "couldn't remove an unused Claude login folder");
        }
    };
    let account: KnownAccount = match signed_in {
        Ok(account) => account.into(),
        Err(error) => {
            forget();
            return Err(error.to_string());
        }
    };
    let mut held: Vec<String> = cloud
        .device_account()
        .await
        .map(|a| a.account_uuid)
        .into_iter()
        .collect();
    for added in store.settings().accounts {
        if let Some(found) = cloud.folder_account(&store.account_folder(&added.id)).await {
            held.push(found.account_uuid);
        }
    }
    if held.contains(&account.account_uuid) {
        forget();
        return Err(format!("{} is already on this device.", account.email));
    }
    let _changing = store.changing();
    let mut next = store.settings();
    next.accounts.push(AddedAccount { id, enabled: true });
    if next
        .offer
        .as_ref()
        .is_some_and(|offer| offer.account_uuid == account.account_uuid)
    {
        next.offer = None;
    }
    store.save(next.clone()).map_err(|e| e.to_string())?;
    apply(registry, store, &next);
    Ok(account)
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
            ..Default::default()
        });
        store.save(next).unwrap();
        let reopened = ClaudeCloudSessions::open(dir.path());
        assert_eq!(
            reopened.settings(),
            ClaudeCloudSettings {
                enabled: true,
                ..ClaudeCloudSettings::default()
            }
        );
        assert!(reopened.settings().wants_harness());
        let without_login = reopened.settings().with(&ClaudeCloudChange {
            device_login: Some(false),
            ..Default::default()
        });
        assert!(!without_login.wants_harness());
        // An added account switched on is a login acting for it too.
        let mut added = without_login.clone();
        added.accounts.push(AddedAccount {
            id: "x".into(),
            enabled: false,
        });
        assert!(!added.wants_harness());
        let added = added.with(&ClaudeCloudChange {
            account: Some(AddedAccount {
                id: "x".into(),
                enabled: true,
            }),
            ..Default::default()
        });
        assert!(added.wants_harness());
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
        let dir = tempfile::tempdir().unwrap();
        let store = ClaudeCloudSessions::open(dir.path());
        let on = ClaudeCloudSettings {
            enabled: true,
            ..ClaudeCloudSettings::default()
        };
        assert!(apply(&registry, &store, &on));
        let listed = registry.descriptors();
        let cloud = listed
            .iter()
            .find(|d| d.id == HarnessId::ClaudeCodeCloud)
            .expect("listed while on");
        assert!(cloud.remote_workspace);
        assert!(!apply(&registry, &store, &ClaudeCloudSettings::default()));
        assert!(!registry.is_registered(HarnessId::ClaudeCodeCloud));
        let no_login = ClaudeCloudSettings {
            enabled: true,
            device_login: false,
            ..ClaudeCloudSettings::default()
        };
        assert!(!apply(&registry, &store, &no_login));
    }
}
