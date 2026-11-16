//! The host device (spec #37, docs/design/cloud-byo.md "Picking the host
//! device"): one of the user's devices keeps a provider's key and does every
//! provider call; the others ask it through the engine's request forwarding
//! (`targetDeviceId`, ADR 0004). The registry names it ([`zeron_proto::SandboxHost`]).
//!
//! Here: the providers built in ([`ProviderFactory`]), made from a key when
//! connected; the key on this device's disk (0600, never answered back);
//! picking the host device ([`pick_host`]); and what one host device hands the
//! next ([`Handover`]): the key, the sign-ins it lends running sandboxes, and
//! their chats' steps, so changing it interrupts nothing.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zeron_proto::Device;
use zeron_proto::capabilities::{HEADLESS_V1, SANDBOX_HOST_V1};

use super::{ChatSandbox, Host, Ops, Record, SandboxProvider, Sandboxes, lock};
use crate::auth::{Auth, LentGrant};

/// The providers' keys, one file each: `{data}/sandbox-keys/{provider}.json`.
pub(super) const KEYS_DIR: &str = "sandbox-keys";

/// A provider built into the engine (Docker Sandbox, Boat), made from the
/// key its sign-in gave: the provider, or why the key won't do.
pub type ProviderFactory = Arc<
    dyn Fn(&serde_json::Value) -> Result<Arc<dyn SandboxProvider>, String> + Send + Sync,
>;

/// What a host device hands the next (`AdoptSandboxProvider`): the key, the
/// sign-ins it lends the provider's sandboxes, and their chats. A first
/// connect hands the key alone.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Handover {
    pub provider: String,
    pub key: serde_json::Value,
    #[serde(default)]
    pub connected_on: Option<String>,
    #[serde(default)]
    pub grants: Vec<LentGrant>,
    /// Each chat's record, as kept in `sandboxes.json`.
    #[serde(default)]
    pub chats: Vec<serde_json::Value>,
}

impl std::fmt::Debug for Handover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handover")
            .field("provider", &self.provider)
            .field("grants", &self.grants.len())
            .field("chats", &self.chats.len())
            .finish_non_exhaustive()
    }
}

/// The host device for a provider being connected on `connected_on`: of the
/// devices online that can be one, a `zeron headless` server, the one online
/// longest; with none, `connected_on`.
pub fn pick_host(devices: &[Device], online: impl Fn(&str) -> bool, connected_on: &str) -> String {
    devices
        .iter()
        .filter(|d| d.supports(SANDBOX_HOST_V1) && d.supports(HEADLESS_V1) && online(&d.id))
        // (One that doesn't say since when goes last; ties by id, so every
        // device picks the same.)
        .min_by(|a, b| {
            (a.online_since.is_none(), a.online_since, &a.id).cmp(&(
                b.online_since.is_none(),
                b.online_since,
                &b.id,
            ))
        })
        .map_or_else(|| connected_on.to_owned(), |d| d.id.clone())
}

/// A provider id is a file name here.
fn valid_id(provider: &str) -> bool {
    !provider.is_empty()
        && provider.len() <= 64
        && provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Sandboxes {
    /// Build `provider` in: offered once connected (its key on this device),
    /// which it is at once when the key is already here.
    pub fn add_factory(&self, provider: &str, factory: ProviderFactory) {
        lock(&self.inner.factories).insert(provider.to_owned(), factory.clone());
        let Some(key) = self.stored_key(provider) else {
            return;
        };
        match factory(&key) {
            Ok(built) => self.register(built),
            Err(err) => tracing::warn!(provider, error = %err, "sandboxes: the kept key no longer works"),
        }
    }

    /// Whether this device keeps `provider` (it's its host device).
    pub(crate) fn keeps(&self, provider: &str) -> bool {
        self.provider(provider).is_some()
    }

    /// The provider whose chat `chat_id` is, when this device keeps it.
    pub(crate) fn provider_of(&self, chat_id: &str) -> Option<String> {
        self.chat(chat_id)
            .map(|chat| lock(&chat.record).steps.provider.clone())
    }

    fn key_path(&self, provider: &str) -> Option<PathBuf> {
        valid_id(provider)
            .then(|| self.inner.keys_dir.as_ref())
            .flatten()
            .map(|dir| dir.join(format!("{provider}.json")))
    }

    fn stored_key(&self, provider: &str) -> Option<serde_json::Value> {
        let bytes = std::fs::read(self.key_path(provider)?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Keep `provider`'s key here (replacing one), once it makes a provider.
    fn connect_here(&self, provider: &str, key: &serde_json::Value) -> Result<(), String> {
        let factory = lock(&self.inner.factories)
            .get(provider)
            .cloned()
            .ok_or_else(|| format!("{provider} isn't built into Zeron on this device."))?;
        let built = factory(key)?;
        let path = self
            .key_path(provider)
            .ok_or_else(|| format!("{provider} can't be kept on this device."))?;
        let bytes = serde_json::to_vec(key).map_err(|e| e.to_string())?;
        crate::runner::write_private_atomic(&path, &bytes)
            .map_err(|e| format!("Couldn't keep the key: {e}"))?;
        self.register(built);
        Ok(())
    }

    /// Everything the next host device needs to go on for `provider`. Only
    /// while no sandbox of it is mid-steps: those would be cut off.
    pub(crate) fn handover(
        &self,
        provider: &str,
        auth: Option<&Auth>,
        connected_on: Option<String>,
    ) -> Result<Handover, String> {
        let key = self
            .stored_key(provider)
            .ok_or_else(|| format!("This device doesn't keep the key for {provider}."))?;
        let chats: Vec<Arc<ChatSandbox>> = lock(&self.inner.chats)
            .values()
            .filter(|chat| lock(&chat.record).steps.provider == provider)
            .cloned()
            .collect();
        if chats.iter().any(|chat| *lock(&chat.busy)) {
            return Err("A sandbox is starting. Try again once it's ready.".into());
        }
        let records: Vec<Record> = chats.iter().map(|chat| lock(&chat.record).clone()).collect();
        let devices: Vec<String> = records.iter().map(|r| r.steps.device_id.clone()).collect();
        Ok(Handover {
            provider: provider.to_owned(),
            key,
            connected_on,
            grants: auth.map(|auth| auth.lending_to(&devices)).unwrap_or_default(),
            chats: records
                .iter()
                .filter_map(|record| serde_json::to_value(record).ok())
                .collect(),
        })
    }

    /// Become `handover.provider`'s host device: keep its key, lend what the
    /// last one lent, and answer for its chats.
    pub(crate) fn adopt(&self, handover: Handover, auth: Option<&Auth>) -> Result<(), String> {
        let records: Vec<Record> = handover
            .chats
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| format!("Unreadable chats from the last host device: {e}"))?;
        if !handover.grants.is_empty() {
            let auth = auth.ok_or("This device isn't signed in, so it can't lend sandboxes their sign-in.")?;
            auth.take_over_lending(handover.grants)
                .map_err(|e| e.to_string())?;
        }
        self.connect_here(&handover.provider, &handover.key)?;
        {
            let mut chats = lock(&self.inner.chats);
            for mut record in records {
                record.steps.interrupted();
                let chat_id = record.steps.chat_id.clone();
                chats.insert(chat_id, Arc::new(ChatSandbox::new(record)));
            }
        }
        self.persist();
        Ok(())
    }

    /// No longer `provider`'s host device: its key, its chats and the
    /// sign-ins lent to their sandboxes go (the next host device has them).
    pub(crate) fn forget(&self, provider: &str, auth: Option<&Auth>) {
        if let Some(path) = self.key_path(provider)
            && let Err(err) = std::fs::remove_file(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(provider, error = %err, "sandboxes: couldn't remove the key");
        }
        lock(&self.inner.providers).retain(|p| p.id() != provider);
        let gone: Vec<Record> = {
            let mut chats = lock(&self.inner.chats);
            let ids: Vec<String> = chats
                .iter()
                .filter(|(_, chat)| lock(&chat.record).steps.provider == provider)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| chats.remove(id))
                .map(|chat| lock(&chat.record).clone())
                .collect()
        };
        if let Some(auth) = auth {
            for record in &gone {
                auth.stop_lending(&record.steps.device_id);
            }
        }
        self.persist();
    }

    /// Delete `chat_id`'s sandbox: the provider deletes it (or, without
    /// delete, stops it) and its engine isn't lent a sign-in any more. A
    /// shared place (an SSH box, a PC at home) is left as it is.
    pub(crate) async fn delete_sandbox(&self, host: &Host, chat_id: &str) -> Result<(), String> {
        let chat = self
            .chat(chat_id)
            .ok_or_else(|| "This chat doesn't run on a sandbox from this device.".to_string())?;
        let record = lock(&chat.record).clone();
        let provider = self.provider(&record.steps.provider).ok_or_else(|| {
            format!("{} isn't connected on this device.", record.steps.provider)
        })?;
        let per_chat = matches!(provider.placement(chat_id), super::Placement::PerChat);
        if let Some(sandbox) = record.steps.sandbox_id.as_deref().filter(|_| per_chat) {
            let ops = provider.ops();
            let outcome = if ops.contains(Ops::DELETE) {
                provider.delete(sandbox).await
            } else if ops.contains(Ops::STOP) {
                provider.stop(sandbox).await
            } else {
                Ok(())
            };
            if let Err(err) = outcome
                && err.kind != super::ErrorKind::NotFound
            {
                return Err(format!("Couldn't delete the sandbox: {err}"));
            }
        }
        if let Some(auth) = &host.auth {
            auth.stop_lending(&record.steps.device_id);
        }
        lock(&self.inner.chats).remove(chat_id);
        self.persist();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, headless: bool, since_s: Option<i64>) -> Device {
        let mut capabilities = zeron_proto::capabilities::current();
        if headless {
            capabilities.push(HEADLESS_V1.into());
        }
        Device {
            id: id.into(),
            name: id.into(),
            platform: "linux".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
            online_since: since_s.and_then(|s| chrono::DateTime::from_timestamp(s, 0)),
            cursor_sdk_version: None,
            capabilities,
        }
    }

    #[test]
    fn the_online_headless_device_online_longest_is_picked_else_where_it_was_connected() {
        let devices = [
            device("laptop", false, Some(10)),
            device("server-new", true, Some(300)),
            device("server-old", true, Some(200)),
            device("server-off", true, Some(100)),
            device("server-unsaid", true, None),
        ];
        let online = |id: &str| id != "server-off";
        assert_eq!(pick_host(&devices, online, "laptop"), "server-old");
        // Nothing headless online: where the user connected it.
        let online = |id: &str| id == "laptop";
        assert_eq!(pick_host(&devices, online, "laptop"), "laptop");
        // An older engine can't be one.
        let mut old = device("server-old", true, Some(1));
        old.capabilities.retain(|c| c != SANDBOX_HOST_V1);
        assert_eq!(pick_host(&[old], |_| true, "laptop"), "laptop");
    }

    #[test]
    fn a_provider_id_is_a_plain_file_name() {
        assert!(valid_id("docker"));
        assert!(valid_id("fake-ssh"));
        assert!(!valid_id("../docker"));
        assert!(!valid_id(""));
    }
}
