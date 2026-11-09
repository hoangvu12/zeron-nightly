//! A provider-run chat's mirror bookkeeping in its chat document
//! (docs/design/provider-run-chats.md §5, "Mirrored entries", "Replay
//! watermark", "Deduplication").
//!
//! - **Entry metadata.** Every mirrored entry carries the provider event it
//!   came from as two additive scalar fields on its map: `providerUuid` (the
//!   event's `uuid`, which is also its `event_id`) and `providerSeq` (its
//!   `sequence_num`). A person's message carries its own event; an assistant
//!   entry carries the first stored event whose output it shows. Older
//!   readers ignore both.
//! - **Metadata.** `meta.providerWatermark` (a number) is the highest
//!   `sequence_num` whose events, including those that map to no entry, are
//!   committed. `meta.providerTurn` (`{entryId, fromSeq}`, one JSON value)
//!   names the entry of a turn still being mirrored and the first event that
//!   can have written to it; it goes when the turn's end is committed.
//!   `meta.providerPrompts` (`{requestId: seq}`, one JSON value) lists the
//!   prompts the session asked that have no answer yet. Only the host writes
//!   them.
//! - **Deduplication.** For each provider uuid the entry whose Loro element
//!   id (peer, counter) is lowest is kept, the same choice on every device.
//!   [`SessionDoc::read_entries`] already leaves the others out (render-time
//!   hiding); the host's [`SessionDoc::sweep_provider_duplicates`] deletes
//!   them.

use std::collections::{BTreeMap, HashMap, HashSet};

use loro::{Container, ContainerID, ContainerTrait, LoroMap, LoroValue, ValueOrContainer};
use serde::{Deserialize, Serialize};
use zeron_proto::ProviderPrompt;

use crate::schema::{DocError, SessionDoc, SessionMessageEntry};

/// Entry field: the provider event `uuid` the entry came from.
pub const PROVIDER_UUID: &str = "providerUuid";
/// Entry field: that event's `sequence_num`.
pub const PROVIDER_SEQ: &str = "providerSeq";
/// Chat document metadata: the provider session a provider-run chat is
/// tied to (also what marks a document as a provider-run chat's).
pub const PROVIDER_SESSION: &str = "providerSession";
const WATERMARK: &str = "providerWatermark";
const TURN: &str = "providerTurn";
const PROMPTS: &str = "providerPrompts";

/// The turn a provider-run chat's host is still mirroring: the entry it
/// writes, and the first provider event that can have written to it. A
/// replay from just before `from_seq` writes that entry over again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub entry_id: String,
    pub from_seq: u64,
}

/// What a committed provider event does to [`ProviderTurn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnUpdate {
    Keep,
    Open(ProviderTurn),
    /// The turn's end is committed.
    Close,
}

/// One entry's provider identity, as stored (duplicates included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEntryMeta {
    /// Position in the `messages` list.
    pub index: usize,
    pub id: String,
    pub provider_uuid: Option<String>,
    pub provider_seq: Option<u64>,
    /// The entry map's Loro element id (peer, counter).
    pub element: (u64, i32),
}

pub(crate) fn stamp_provider(map: &LoroMap, uuid: &str, sequence_num: u64) -> Result<(), DocError> {
    map.insert(PROVIDER_UUID, uuid)?;
    map.insert(PROVIDER_SEQ, sequence_num as i64)?;
    Ok(())
}

fn string_of(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

fn number_of(map: &LoroMap, key: &str) -> Option<u64> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::I64(n))) => u64::try_from(n).ok(),
        Some(ValueOrContainer::Value(LoroValue::Double(n))) if n >= 0.0 => Some(n as u64),
        _ => None,
    }
}

fn element_of(map: &LoroMap) -> (u64, i32) {
    element_of_id(&map.id())
}

/// An entry map's Loro element id (peer, counter), what the keep-rule
/// orders by.
pub fn element_of_id(id: &ContainerID) -> (u64, i32) {
    match id {
        ContainerID::Normal { peer, counter, .. } => (*peer, *counter),
        ContainerID::Root { .. } => (u64::MAX, i32::MAX),
    }
}

/// The provider uuid an entry map carries, if any.
pub fn provider_uuid_of(map: &LoroMap) -> Option<String> {
    string_of(map, PROVIDER_UUID)
}

/// The keep-rule over entries given in list order as (provider uuid,
/// element id): for each uuid, every entry but the one with the lowest
/// element id is dropped, the same choice on every device. Returns the
/// dropped positions, ascending. Every view of a provider-run chat's
/// transcript filters with this.
pub fn duplicate_positions<'a>(
    entries: impl IntoIterator<Item = (Option<&'a str>, (u64, i32))>,
) -> Vec<usize> {
    let mut kept: HashMap<&'a str, ((u64, i32), usize)> = HashMap::new();
    let mut dropped = Vec::new();
    for (index, (uuid, element)) in entries.into_iter().enumerate() {
        let Some(uuid) = uuid else {
            continue;
        };
        match kept.get_mut(uuid) {
            None => {
                kept.insert(uuid, (element, index));
            }
            Some(keep) if element < keep.0 => {
                dropped.push(keep.1);
                *keep = (element, index);
            }
            Some(_) => dropped.push(index),
        }
    }
    dropped.sort_unstable();
    dropped
}

/// Whether a chat document is a provider-run chat's.
pub fn is_provider_run(doc: &loro::LoroDoc) -> bool {
    doc.get_map("meta").get(PROVIDER_SESSION).is_some()
}

impl SessionDoc {
    /// The replay watermark (0: nothing committed yet).
    pub fn provider_watermark(&self) -> u64 {
        number_of(&self.doc().get_map("meta"), WATERMARK).unwrap_or(0)
    }

    /// The turn still being mirrored, if any.
    pub fn provider_turn(&self) -> Option<ProviderTurn> {
        serde_json::from_str(&string_of(&self.doc().get_map("meta"), TURN)?).ok()
    }

    /// The prompts asked and not answered yet: request id → `sequence_num`.
    pub fn provider_open_prompts(&self) -> BTreeMap<String, u64> {
        string_of(&self.doc().get_map("meta"), PROMPTS)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Record (or with `None` clear) the turn being mirrored. No-op when it
    /// already reads that way.
    pub fn set_provider_turn(&self, turn: Option<&ProviderTurn>) -> Result<(), DocError> {
        if self.provider_turn().as_ref() == turn {
            return Ok(());
        }
        let meta = self.doc().get_map("meta");
        match turn {
            Some(turn) => meta.insert(TURN, serde_json::to_string(turn)?)?,
            None => meta.delete(TURN)?,
        }
        self.doc().commit();
        Ok(())
    }

    /// Provider event `sequence_num` is committed (everything it wrote is in
    /// the document already): advance the watermark, apply its turn and
    /// prompt bookkeeping, in one commit. The watermark never goes back.
    pub fn commit_provider_event(
        &self,
        sequence_num: u64,
        turn: TurnUpdate,
        prompt: Option<&ProviderPrompt>,
    ) -> Result<(), DocError> {
        let meta = self.doc().get_map("meta");
        if sequence_num > self.provider_watermark() {
            meta.insert(WATERMARK, sequence_num as i64)?;
        }
        match turn {
            TurnUpdate::Keep => {}
            TurnUpdate::Open(turn) => {
                if self.provider_turn().as_ref() != Some(&turn) {
                    meta.insert(TURN, serde_json::to_string(&turn)?)?;
                }
            }
            TurnUpdate::Close => {
                if self.provider_turn().is_some() {
                    meta.delete(TURN)?;
                }
                // A turn's prompts end with it (an interrupted turn's never
                // get a response): none is asked again after this.
                if meta.get(PROMPTS).is_some() {
                    meta.delete(PROMPTS)?;
                }
            }
        }
        if let Some(prompt) = prompt {
            let mut open = self.provider_open_prompts();
            let changed = match prompt {
                ProviderPrompt::Asked { request_id } => open
                    .insert(request_id.clone(), sequence_num)
                    .is_none_or(|was| was != sequence_num),
                ProviderPrompt::Answered { request_id } => open.remove(request_id).is_some(),
            };
            if changed {
                if open.is_empty() {
                    meta.delete(PROMPTS)?;
                } else {
                    meta.insert(PROMPTS, serde_json::to_string(&open)?)?;
                }
            }
        }
        self.doc().commit();
        Ok(())
    }

    /// Every entry's provider identity, duplicates included, in list order.
    pub fn provider_entries(&self) -> Vec<ProviderEntryMeta> {
        let messages = self.doc().get_list("messages");
        (0..messages.len())
            .filter_map(|index| match messages.get(index) {
                Some(ValueOrContainer::Container(Container::Map(map))) => Some(ProviderEntryMeta {
                    index,
                    id: string_of(&map, "id").unwrap_or_default(),
                    provider_uuid: string_of(&map, PROVIDER_UUID),
                    provider_seq: number_of(&map, PROVIDER_SEQ),
                    element: element_of(&map),
                }),
                _ => None,
            })
            .collect()
    }

    /// Where entry `id` is in the `messages` list (the first such entry).
    pub fn entry_index(&self, id: &str) -> Option<usize> {
        let messages = self.doc().get_list("messages");
        (0..messages.len()).find(|&index| {
            matches!(messages.get(index),
                Some(ValueOrContainer::Container(Container::Map(map)))
                    if string_of(&map, "id").as_deref() == Some(id))
        })
    }

    /// Push a person's message mirrored from provider event `uuid`.
    pub fn push_provider_message(
        &self,
        entry: &SessionMessageEntry,
        uuid: &str,
        sequence_num: u64,
    ) -> Result<(), DocError> {
        self.push_message(entry)?;
        let messages = self.doc().get_list("messages");
        if let Some(ValueOrContainer::Container(Container::Map(map))) =
            messages.get(messages.len().saturating_sub(1))
        {
            stamp_provider(&map, uuid, sequence_num)?;
            self.doc().commit();
        }
        Ok(())
    }

    /// A person's message, provider event `uuid`, is already in the
    /// transcript: an entry carries that uuid, or is this device's own send
    /// of it (its id is the event uuid, or maps to it,
    /// [`zeron_proto::provider_cloud::event_uuid`]). That entry gets the
    /// event's identity if it lacked it. `false` when there is none.
    pub fn stamp_provider_echo(&self, uuid: &str, sequence_num: u64) -> Result<bool, DocError> {
        let messages = self.doc().get_list("messages");
        for index in 0..messages.len() {
            let Some(ValueOrContainer::Container(Container::Map(map))) = messages.get(index) else {
                continue;
            };
            if string_of(&map, PROVIDER_UUID).as_deref() == Some(uuid) {
                return Ok(true);
            }
            let own = string_of(&map, "role").as_deref() == Some("user")
                && string_of(&map, "id")
                    .is_some_and(|id| zeron_proto::provider_cloud::event_uuid(&id) == uuid);
            if own {
                if string_of(&map, PROVIDER_UUID).is_none() {
                    stamp_provider(&map, uuid, sequence_num)?;
                    self.doc().commit();
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The entries the keep-rule drops ([`duplicate_positions`]): for each
    /// provider uuid, all but the one with the lowest element id. Positions
    /// in the `messages` list, ascending.
    pub fn provider_duplicates(&self) -> Vec<usize> {
        // Positions here are over every list item; `provider_entries` skips
        // non-map items, so map back through its `index`.
        let entries = self.provider_entries();
        duplicate_positions(
            entries
                .iter()
                .map(|e| (e.provider_uuid.as_deref(), e.element)),
        )
        .into_iter()
        .map(|i| entries[i].index)
        .collect()
    }

    /// Delete [`Self::provider_duplicates`] (the host's sweep). Returns how
    /// many went.
    pub fn sweep_provider_duplicates(&self) -> Result<usize, DocError> {
        let dropped = self.provider_duplicates();
        if dropped.is_empty() {
            return Ok(0);
        }
        let messages = self.doc().get_list("messages");
        for &index in dropped.iter().rev() {
            messages.delete(index, 1)?;
        }
        self.doc().commit();
        Ok(dropped.len())
    }

    /// Positions [`Self::read_entries`] leaves out: duplicates by provider
    /// uuid, in a provider-run chat's document only.
    pub(crate) fn hidden_duplicates(&self) -> HashSet<usize> {
        if !is_provider_run(self.doc()) {
            return HashSet::new();
        }
        self.provider_duplicates().into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::{MessagePart, MessageStatus};
    use crate::schema::MessageRole;

    fn message(id: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: "hi".into(),
            }],
            created_at: 0,
            device_id: "dev".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            duration_ms: None,
        }
    }

    fn provider_doc(peer: u64) -> SessionDoc {
        let doc = SessionDoc::init("chat").unwrap();
        doc.doc().set_peer_id(peer).unwrap();
        doc.doc()
            .get_map("meta")
            .insert(PROVIDER_SESSION, "{}")
            .unwrap();
        doc.doc().commit();
        doc
    }

    #[test]
    fn every_device_keeps_the_same_copy_and_hides_the_rest_until_swept() {
        let a = provider_doc(7);
        let b = provider_doc(3);
        b.doc()
            .import(&a.doc().export(loro::ExportMode::all_updates()).unwrap())
            .unwrap();
        a.push_provider_message(&message("on-a"), "evt-1", 5).unwrap();
        b.push_provider_message(&message("on-b"), "evt-1", 5).unwrap();
        let (from_a, from_b) = (
            a.doc().export(loro::ExportMode::all_updates()).unwrap(),
            b.doc().export(loro::ExportMode::all_updates()).unwrap(),
        );
        a.doc().import(&from_b).unwrap();
        b.doc().import(&from_a).unwrap();
        // Shown once, the same copy on both (the lowest element id: peer 3).
        for doc in [&a, &b] {
            let ids: Vec<_> = doc.read_entries().unwrap().into_iter().map(|e| e.id).collect();
            assert_eq!(ids, vec!["on-b".to_owned()]);
            assert_eq!(doc.provider_entries().len(), 2);
        }
        assert_eq!(a.sweep_provider_duplicates().unwrap(), 1);
        b.doc()
            .import(&a.doc().export(loro::ExportMode::all_updates()).unwrap())
            .unwrap();
        for doc in [&a, &b] {
            let ids: Vec<_> = doc.provider_entries().into_iter().map(|e| e.id).collect();
            assert_eq!(ids, vec!["on-b".to_owned()]);
        }
        // Concurrent sweeps on both devices converge too.
        assert_eq!(b.sweep_provider_duplicates().unwrap(), 0);
    }

    #[test]
    fn a_chat_that_isnt_provider_run_hides_nothing() {
        let doc = SessionDoc::init("local").unwrap();
        doc.push_provider_message(&message("x"), "evt", 1).unwrap();
        doc.push_provider_message(&message("y"), "evt", 1).unwrap();
        assert_eq!(doc.read_entries().unwrap().len(), 2);
    }

    #[test]
    fn the_watermark_only_moves_forward_and_a_turns_end_closes_its_prompts() {
        let doc = provider_doc(1);
        let asked = |id: &str| ProviderPrompt::Asked {
            request_id: id.into(),
        };
        doc.commit_provider_event(4, TurnUpdate::Keep, Some(&asked("p1")))
            .unwrap();
        doc.commit_provider_event(6, TurnUpdate::Keep, Some(&asked("p2")))
            .unwrap();
        doc.commit_provider_event(
            7,
            TurnUpdate::Keep,
            Some(&ProviderPrompt::Answered {
                request_id: "p1".into(),
            }),
        )
        .unwrap();
        assert_eq!(doc.provider_watermark(), 7);
        assert_eq!(
            doc.provider_open_prompts(),
            BTreeMap::from([("p2".to_owned(), 6)])
        );
        doc.commit_provider_event(3, TurnUpdate::Keep, None).unwrap();
        assert_eq!(doc.provider_watermark(), 7);
        let turn = ProviderTurn {
            entry_id: "e".into(),
            from_seq: 5,
        };
        doc.commit_provider_event(8, TurnUpdate::Open(turn.clone()), None)
            .unwrap();
        assert_eq!(doc.provider_turn(), Some(turn));
        doc.commit_provider_event(9, TurnUpdate::Close, None).unwrap();
        assert_eq!(doc.provider_turn(), None);
        assert!(doc.provider_open_prompts().is_empty());
        assert_eq!(doc.provider_watermark(), 9);
    }

    #[test]
    fn an_echo_is_matched_by_its_entry_id_and_stamped() {
        let doc = provider_doc(1);
        doc.push_message(&message("6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70"))
            .unwrap();
        assert!(
            doc.stamp_provider_echo("6f1f8a3e-3c3b-4f7e-9d1a-2b8c4d5e6f70", 2)
                .unwrap()
        );
        assert_eq!(doc.provider_entries()[0].provider_seq, Some(2));
        // A message id that isn't a uuid travels under its UUIDv5.
        doc.push_message(&message("msg-2")).unwrap();
        let uuid = zeron_proto::provider_cloud::event_uuid("msg-2");
        assert!(doc.stamp_provider_echo(&uuid, 3).unwrap());
        assert!(!doc.stamp_provider_echo("someone-elses", 4).unwrap());
    }
}
