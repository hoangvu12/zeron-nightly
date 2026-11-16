//! Save as template (spec #37, docs/design/cloud-byo.md "What's in a
//! sandbox"): a chat's sandbox saved through its provider, safe to reuse.
//!
//! Before the provider saves it, Zeron in the sandbox forgets who it is
//! (`zeron-engine forget-device`, [`super::forget`]): its device id and
//! data go, so sandboxes from the template never share a device id, and
//! nothing of its sign-in is on the disk (the lent token only ever lived in
//! the engine's memory, which isn't saved). Then the engine is started again
//! the way Ready starts it, on the same device id, and the chat goes on.

use super::{
    ChatSandbox, Host, Ops, Record, SandboxProvider, SandboxState, SandboxStatus, Sandboxes,
    Template, answers, lock, run_to_end,
};
use crate::sandboxes::{Command, ENGINE_PROGRAM, FORGET_BUSY_EXIT, FORGET_DEVICE};

/// How long `forget-device` may take (stopping the engine, then the wipe).
const FORGET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Hands the chat's sandbox back when the save ends, however it ends.
struct Busy<'a>(&'a ChatSandbox);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) = false;
    }
}

impl Sandboxes {
    /// `SaveSandboxTemplate`: save `chat_id`'s sandbox as a template named
    /// `name`, Zeron's identity wiped from it first.
    pub(crate) async fn save_template(
        &self,
        host: Host,
        chat_id: &str,
        name: &str,
    ) -> Result<Template, String> {
        let name = template_name(name)?;
        let chat = self
            .chat(chat_id)
            .ok_or_else(|| "This chat doesn't run on a sandbox from this device.".to_string())?;
        let record = lock(&chat.record).clone();
        let provider = self
            .provider(&record.steps.provider)
            .ok_or_else(|| format!("{} isn't connected on this device.", record.steps.provider))?;
        let ops = provider.ops();
        if !ops.contains(Ops::SAVE_TEMPLATE) {
            return Err(format!("{} can't save templates.", provider.name()));
        }
        // Zeron's identity can only be wiped where Zeron started it.
        if record.resident || !ops.contains(Ops::RUN) {
            return Err(format!(
                "Zeron can't clear its identity on {}, so it won't save it as a template.",
                provider.name()
            ));
        }
        let sandbox = match (&record.steps.sandbox_id, record.steps.state) {
            (Some(sandbox), SandboxState::Ready) => sandbox.clone(),
            (_, SandboxState::Gone) => return Err("Its sandbox was deleted.".into()),
            _ => return Err("Its sandbox isn't ready yet.".into()),
        };
        {
            let mut busy = lock(&chat.busy);
            if *busy {
                return Err("Its sandbox is busy; try again in a moment.".into());
            }
            *busy = true;
        }
        let _busy = Busy(&chat);

        // Asleep: woken to be wiped (the provider saves a running one).
        if ops.contains(Ops::START)
            && let Ok(SandboxStatus::Stopped) = provider.status(&sandbox).await
        {
            provider
                .start(&sandbox)
                .await
                .map_err(|e| format!("Couldn't wake the sandbox: {e}"))?;
        }
        forget(provider.as_ref(), &sandbox).await?;
        let saved = provider
            .save_template(&sandbox, &name)
            .await
            .map_err(|e| format!("Couldn't save the template: {e}"));
        // Zeron goes on in the chat's sandbox whether or not it was saved.
        let restarted = restart(&chat, provider.as_ref(), &host, &record, &sandbox).await;
        let saved = saved?;
        tracing::info!(chat = %chat_id, template = %saved.id, "sandbox saved as a template");
        if let Err(err) = restarted {
            tracing::warn!(chat = %chat_id, %err, "sandbox: Zeron didn't come back after the save");
            chat.update(|steps| steps.fail(super::Step::Ready, err));
            self.persist();
        }
        Ok(saved)
    }

    /// `ListSandboxTemplates`: `provider_id`'s templates (the picker's).
    pub(crate) async fn templates(&self, provider_id: &str) -> Result<Vec<Template>, String> {
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| format!("{provider_id} isn't on this device."))?;
        if !provider.ops().contains(Ops::TEMPLATES) {
            return Ok(Vec::new());
        }
        provider.templates().await.map_err(|e| e.message)
    }
}

/// The name as saved: trimmed; never blank, never a path.
fn template_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the template a name.".into());
    }
    if name.contains(['/', '\\']) || name == "." || name == ".." || name.chars().any(char::is_control)
    {
        return Err("A template's name can't have / in it.".into());
    }
    Ok(name.to_owned())
}

/// Zeron in the sandbox stops and wipes its identity.
async fn forget(provider: &dyn SandboxProvider, sandbox: &str) -> Result<(), String> {
    let command = Command::new([ENGINE_PROGRAM, FORGET_DEVICE]).timeout(FORGET_TIMEOUT);
    let done = run_to_end(provider, sandbox, command)
        .await
        .map_err(|e| format!("Couldn't clear Zeron's identity in the sandbox: {e}"))?;
    match done.exit.code {
        Some(0) => Ok(()),
        Some(FORGET_BUSY_EXIT) => Err(
            "The chat is still working. Save it as a template once it's done.".into(),
        ),
        _ => Err(format!(
            "Couldn't clear Zeron's identity in the sandbox: {}",
            done.failure()
        )),
    }
}

/// Zeron started again in the sandbox, and answering, as Ready leaves it.
async fn restart(
    chat: &ChatSandbox,
    provider: &dyn SandboxProvider,
    host: &Host,
    record: &Record,
    sandbox: &str,
) -> Result<(), String> {
    chat.start_zeron(provider, host, record, sandbox).await?;
    answers(host, &record.steps.device_id).await?;
    if let Err(err) = host.doc_host.retry_delivery(&record.steps.chat_id) {
        tracing::warn!(error = %err, "sandbox: couldn't hand the waiting messages over");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_is_named_plainly() {
        assert_eq!(template_name("  reportly-dev ").unwrap(), "reportly-dev");
        assert_eq!(template_name("Reportly dev box").unwrap(), "Reportly dev box");
        for bad in ["", "   ", "a/b", r"a\b", "..", "a\nb"] {
            assert!(template_name(bad).is_err(), "{bad:?}");
        }
    }
}
