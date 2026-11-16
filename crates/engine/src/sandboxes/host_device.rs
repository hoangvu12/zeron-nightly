//! The host device (spec #37, docs/design/cloud-byo.md "Picking the host
//! device"): one of the user's devices keeps a provider's key and does every
//! provider call; the others ask it through the engine's request forwarding
//! (`targetDeviceId`, ADR 0004). The registry names it ([`zeron_proto::SandboxHost`]).
//!
//! Here: picking the host device ([`pick_host`]), and what one host device
//! hands the next ([`Handover`]): the key from the provider's key slot
//! ([`super::SandboxProvider::export_key`]), the sign-ins it lends running
//! sandboxes, and their chats' steps, so changing it interrupts nothing.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zeron_proto::Device;
use zeron_proto::capabilities::{HEADLESS_V1, SANDBOX_HOST_V1};

use super::{ChatSandbox, Record, Sandboxes, lock};
use crate::auth::{Auth, LentGrant};

/// What a host device hands the next (`AdoptSandboxProvider`): the key, the
/// sign-ins it lends the provider's sandboxes, and their chats. A first
/// connect hands the key alone.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Handover {
    pub provider: String,
    /// What the provider's key slot holds. Never logged.
    pub key: String,
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

impl Sandboxes {
    /// The provider whose chat `chat_id` is, when this device keeps it.
    pub(crate) fn provider_of(&self, chat_id: &str) -> Option<String> {
        self.chat(chat_id)
            .map(|chat| lock(&chat.record).steps.provider.clone())
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
            .provider(provider)
            .and_then(|p| p.export_key())
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
    pub(crate) async fn adopt(
        &self,
        handover: Handover,
        auth: Option<&Auth>,
    ) -> Result<(), String> {
        let provider = self
            .provider(&handover.provider)
            .ok_or_else(|| format!("{} isn't built into Zeron on this device.", handover.provider))?;
        let records: Vec<Record> = handover
            .chats
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| format!("Unreadable chats from the last host device: {e}"))?;
        let auth = match auth {
            Some(auth) => Some(auth),
            None if handover.grants.is_empty() => None,
            None => {
                return Err(
                    "This device isn't signed in, so it can't lend sandboxes their sign-in.".into(),
                );
            }
        };
        provider
            .adopt_key(handover.key)
            .await
            .map_err(|e| format!("Couldn't keep the key: {e}"))?;
        if let Some(auth) = auth
            && !handover.grants.is_empty()
        {
            auth.take_over_lending(handover.grants)
                .map_err(|e| e.to_string())?;
        }
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
    pub(crate) async fn forget(&self, provider: &str, auth: Option<&Auth>) {
        if let Some(built) = self.provider(provider) {
            built.forget_key().await;
        }
        let gone: Vec<Record> = {
            let mut chats = lock(&self.inner.chats);
            let ids: Vec<String> = chats
                .iter()
                .filter(|(_, chat)| lock(&chat.record).steps.provider == provider)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| chats.remove(id))
                .map(|chat| {
                    // The next host device stands in for it now.
                    chat.leave_room();
                    lock(&chat.record).clone()
                })
                .collect()
        };
        if let Some(auth) = auth {
            for record in &gone {
                auth.stop_lending(&record.steps.device_id);
            }
        }
        self.persist();
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
}
