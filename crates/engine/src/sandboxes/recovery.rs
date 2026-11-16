//! When things go wrong, and when a chat goes (docs/design/cloud-byo.md
//! "Lifecycle"): a deleted chat takes its sandbox with it (tokens stop, then
//! the provider deletes it; the transcript stays), a sandbox deleted outside
//! Zeron reads as gone, and Start fresh makes the chat a new one with its
//! pushed work.

use std::sync::atomic::Ordering;

use super::{
    ChatSandbox, ErrorKind, Host, Hook, Placement, Record, SandboxProvider, SandboxSteps,
    Sandboxes, Timing, lock,
};
use crate::auth::Auth;
use crate::sandboxes::{Ops, SandboxState, SandboxStatus, Step, StepState};

impl Sandboxes {
    /// Whether `device_id` is the engine of a sandbox this device keeps.
    pub fn hosts_device(&self, device_id: &str) -> bool {
        lock(&self.inner.chats)
            .values()
            .any(|chat| lock(&chat.record).steps.device_id == device_id)
    }

    /// Delete the sandbox of every chat whose row goes, whichever device
    /// deleted it: deletes arrive here by sync too. Only a row seen and then
    /// gone counts (a row not synced in yet is no deletion). Once per engine.
    pub(crate) fn follow_deletions(
        &self,
        mut chats: tokio::sync::watch::Receiver<Vec<zeron_proto::Chat>>,
        host: Host,
    ) {
        if tokio::runtime::Handle::try_current().is_err()
            || self.inner.following.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let sandboxes = self.clone();
        let ids = |chats: &[zeron_proto::Chat]| -> std::collections::HashSet<String> {
            chats.iter().map(|c| c.id.clone()).collect()
        };
        let mut known = ids(&chats.borrow_and_update());
        tokio::spawn(async move {
            while chats.changed().await.is_ok() {
                let now = ids(&chats.borrow_and_update());
                for gone in known.difference(&now) {
                    if sandboxes.chat(gone).is_none() {
                        continue;
                    }
                    tracing::info!(chat = %gone, "chat deleted; deleting its sandbox");
                    let sandboxes = sandboxes.clone();
                    let (chat_id, host) = (gone.clone(), host.clone());
                    tokio::spawn(async move { sandboxes.delete(&chat_id, &host).await });
                }
                known = now;
            }
        });
    }

    /// The chat is deleted: stop lending its sandbox tokens, then delete the
    /// sandbox (one Zeron made). Steps still running stop at the next one
    /// and do this themselves.
    pub(crate) async fn delete(&self, chat_id: &str, host: &Host) {
        let Some(chat) = lock(&self.inner.chats).remove(chat_id) else {
            return;
        };
        self.persist();
        chat.deleted.store(true, Ordering::SeqCst);
        if *lock(&chat.busy) {
            // Lending stops now all the same; the steps delete the rest.
            stop_lending(&lock(&chat.record), host.auth.as_ref());
            return;
        }
        let provider = self.provider(&lock(&chat.record).steps.provider);
        self.discard(&chat, provider.as_deref(), host).await;
    }

    /// `CheckSandbox`: ask the provider whether the chat's sandbox is still
    /// there. Never wakes it.
    pub(crate) async fn check(&self, auth: Option<&Auth>, chat_id: &str) -> Option<SandboxSteps> {
        let chat = self.chat(chat_id)?;
        let (checks, provider_id) = {
            let record = lock(&chat.record);
            let checks = matches!(
                record.steps.state,
                SandboxState::Ready | SandboxState::Failed
            ) && record.steps.sandbox_id.is_some();
            (checks, record.steps.provider.clone())
        };
        let busy = *lock(&chat.busy);
        if checks
            && !busy
            && let Some(provider) = self.provider(&provider_id)
        {
            self.found_gone(&chat, provider.as_ref(), auth).await;
        }
        self.steps(chat_id)
    }

    /// If the chat's sandbox is gone, say so: the chat offers Start fresh.
    /// Lending stops, as there's nothing left to lend to.
    pub(super) async fn found_gone(
        &self,
        chat: &ChatSandbox,
        provider: &dyn SandboxProvider,
        auth: Option<&Auth>,
    ) -> bool {
        let Some(sandbox) = lock(&chat.record).steps.sandbox_id.clone() else {
            return false;
        };
        match provider.status(&sandbox).await {
            Ok(SandboxStatus::Gone) => {}
            Ok(_) => return false,
            Err(err) => {
                if err.kind != ErrorKind::Unsupported {
                    tracing::debug!(%sandbox, error = %err, "sandbox: couldn't tell how it is");
                }
                return false;
            }
        }
        tracing::info!(%sandbox, "sandbox deleted outside Zeron");
        chat.update(SandboxSteps::gone);
        stop_lending(&lock(&chat.record), auth);
        *lock(&chat.lent) = None;
        self.persist();
        true
    }

    /// What a deleted chat leaves: its grant, and the sandbox Zeron made,
    /// with the user's delete scripts around it (`before-delete`,
    /// `instead-of-delete`, `after-delete`, on this device). Once only.
    pub(super) async fn discard(
        &self,
        chat: &ChatSandbox,
        provider: Option<&dyn SandboxProvider>,
        host: &Host,
    ) {
        if chat.discarded.swap(true, Ordering::SeqCst) {
            return;
        }
        let record = lock(&chat.record).clone();
        stop_lending(&record, host.auth.as_ref());
        let Some(provider) = provider else {
            tracing::warn!(
                chat = %record.steps.chat_id,
                provider = %record.steps.provider,
                "sandbox not deleted: its provider isn't connected here"
            );
            return;
        };
        let hook = |timing| Hook::new(timing, Step::Delete);
        self.run_hook(chat, provider, host, hook(Timing::Before))
            .await;
        let instead = self
            .scripts_for(chat, provider, host, hook(Timing::InsteadOf))
            .await;
        let replaced = !instead.is_empty()
            && self
                .run_scripts(chat, provider, host, Step::Delete, instead)
                .await
                .any;
        if !replaced && record.steps.state != SandboxState::Gone {
            delete_sandbox(&record, provider).await;
        }
        self.run_hook(chat, provider, host, hook(Timing::After))
            .await;
    }

    /// Start fresh: a new sandbox for the chat, every step again; Get the
    /// code brings back what was pushed to the chat's branch. The old
    /// sandbox, if still there, is deleted first. The chat keeps its device.
    pub(crate) async fn start_fresh(&self, host: Host, chat_id: &str) -> Result<(), String> {
        let chat = self
            .chat(chat_id)
            .ok_or_else(|| "This chat doesn't run on a sandbox from this device.".to_string())?;
        let provider_id = lock(&chat.record).steps.provider.clone();
        let provider = self
            .provider(&provider_id)
            .ok_or_else(|| format!("{provider_id} isn't connected on this device."))?;
        if *lock(&chat.busy) {
            return Err("Its sandbox is still being set up.".into());
        }
        let record = lock(&chat.record).clone();
        if record.steps.state != SandboxState::Gone {
            delete_sandbox(&record, provider.as_ref()).await;
        }
        let shared = match provider.placement(chat_id) {
            Placement::PerChat => None,
            Placement::Shared { sandbox, .. } => Some(sandbox),
        };
        chat.update(|steps| steps.fresh(shared));
        *lock(&chat.lent) = None;
        self.persist();
        self.start(host, chat_id)
    }
}

/// Lending stops, unless the place runs a Zeron signed in itself.
fn stop_lending(record: &Record, auth: Option<&Auth>) {
    if let Some(auth) = auth
        && !record.resident
    {
        auth.stop_lending(&record.steps.device_id);
    }
}

/// Delete the sandbox, if Zeron made it (a shared place isn't Zeron's).
async fn delete_sandbox(record: &Record, provider: &dyn SandboxProvider) {
    let Some(sandbox) = &record.steps.sandbox_id else {
        return;
    };
    if record.steps.state_of(Step::Create) != StepState::Done
        || !provider.ops().contains(Ops::DELETE)
    {
        return;
    }
    match provider.delete(sandbox).await {
        Ok(()) => tracing::info!(%sandbox, "sandbox deleted"),
        Err(err) if err.kind == ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%sandbox, error = %err, "sandbox not deleted"),
    }
}
