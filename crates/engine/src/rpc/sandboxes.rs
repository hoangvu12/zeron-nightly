//! Places chats run on (docs/design/cloud-byo.md): `createChat` with a
//! `place`, and the Sandbox* methods the device keeping the provider answers
//! ([`crate::sandboxes`]).

use serde::Deserialize;
use zeron_proto::ChatConfig;
use zeron_rpc::{RpcError, RpcReply, RpcService, methods};

use super::{EngineRpc, parse_params, watch_stream};
use crate::sandboxes::scripts::{self, Scope};
use crate::sandboxes::{Credential, Hook, Host, Sandboxes, Step, repos_here};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_SANDBOX_PROVIDERS
            | methods::CONNECT_SANDBOX_PROVIDER
            | methods::SANDBOX_STEPS
            | methods::WATCH_SANDBOX_STEPS
            | methods::RUN_SANDBOX_STEPS_AGAIN
            | methods::LIST_STEP_SCRIPTS
            | methods::READ_STEP_SCRIPT
            | methods::WRITE_STEP_SCRIPT
            | methods::DELETE_STEP_SCRIPT
            | methods::CHECK_SANDBOX
            | methods::START_SANDBOX_FRESH
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatParams {
    chat_id: String,
}

/// A step script, as Settings names it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScriptParams {
    #[serde(rename = "for")]
    scope: Scope,
    hook: String,
    name: String,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectParams {
    provider: String,
    /// A pasted key; without one, the provider's own sign-in page.
    #[serde(default)]
    token: Option<String>,
}

impl EngineRpc {
    fn sandboxes(&self) -> Result<&Sandboxes, RpcError> {
        self.sandboxes
            .as_ref()
            .ok_or_else(|| RpcError::Failed("Sandboxes aren't available here".into()))
    }

    fn sandbox_host(&self) -> Host {
        Host {
            workspace: self.workspace.clone(),
            doc_host: self.doc_host.clone(),
            auth: self.auth.clone(),
            links: self.links.clone(),
        }
    }

    pub(super) async fn sandboxes_rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let sandboxes = self.sandboxes()?;
        match method {
            methods::LIST_SANDBOX_PROVIDERS => RpcReply::value(&sandboxes.providers()),
            methods::CONNECT_SANDBOX_PROVIDER => {
                let p: ConnectParams = parse_params(params)?;
                let provider = sandboxes.provider_named(&p.provider).ok_or_else(|| {
                    RpcError::Failed(format!("{} isn't on this device.", p.provider))
                })?;
                let failed = |e: crate::sandboxes::ProviderError| RpcError::Failed(e.message);
                if let Some(token) = p.token.filter(|t| !t.trim().is_empty()) {
                    let account = provider
                        .connect(Credential::Token(token))
                        .await
                        .map_err(failed)?;
                    return RpcReply::value(&serde_json::json!({ "account": account }));
                }
                let code = provider.begin_connect().await.map_err(failed)?.ok_or_else(|| {
                    RpcError::Failed(format!("{} connects with a pasted key.", provider.name()))
                })?;
                let shown = serde_json::to_value(&code).unwrap_or_default();
                // Approved on the provider's page, in the user's time.
                tokio::spawn(async move {
                    if let Err(err) = provider.connect(Credential::DeviceCode(code)).await {
                        tracing::warn!(provider = provider.id(), %err, "sandboxes: connect didn't finish");
                    }
                });
                RpcReply::value(&serde_json::json!({ "deviceCode": shown }))
            }
            methods::SANDBOX_STEPS => {
                let p: ChatParams = parse_params(params)?;
                RpcReply::value(&sandboxes.steps(&p.chat_id))
            }
            methods::WATCH_SANDBOX_STEPS => {
                let p: ChatParams = parse_params(params)?;
                let steps = sandboxes.watch(&p.chat_id).ok_or_else(|| {
                    RpcError::Failed("This chat doesn't run on a sandbox from this device.".into())
                })?;
                Ok(RpcReply::Stream(watch_stream(steps)))
            }
            methods::RUN_SANDBOX_STEPS_AGAIN => {
                let p: ChatParams = parse_params(params)?;
                sandboxes
                    .run_again(self.sandbox_host(), &p.chat_id)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LIST_STEP_SCRIPTS => {
                let repos = repos_here(&self.workspace, self.doc_host.device_id());
                let rows: Vec<serde_json::Value> = scripts::all(sandboxes.steps_dir(), &repos)
                    .into_iter()
                    .map(|script| script_row(&script))
                    .collect();
                RpcReply::value(&rows)
            }
            methods::READ_STEP_SCRIPT => {
                let p: ScriptParams = parse_params(params)?;
                let path = self.script_path(sandboxes, &p)?;
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| RpcError::Failed(format!("Couldn't read {}: {e}", p.name)))?;
                RpcReply::value(&serde_json::json!({ "content": content }))
            }
            methods::WRITE_STEP_SCRIPT => {
                let p: ScriptParams = parse_params(params)?;
                let path = self.script_path(sandboxes, &p)?;
                write_script(&path, p.content.as_deref().unwrap_or_default())
                    .map_err(|e| RpcError::Failed(format!("Couldn't save {}: {e}", p.name)))?;
                RpcReply::value(&serde_json::json!({ "ok": true, "path": path }))
            }
            methods::DELETE_STEP_SCRIPT => {
                let p: ScriptParams = parse_params(params)?;
                let path = self.script_path(sandboxes, &p)?;
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        // An emptied hook folder goes too (never `.zeron`).
                        if p.hook != scripts::SETUP_HOOK
                            && let Some(dir) = path.parent()
                        {
                            let _ = std::fs::remove_dir(dir);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(RpcError::Failed(format!("Couldn't delete {}: {e}", p.name)));
                    }
                }
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::CHECK_SANDBOX => {
                let p: ChatParams = parse_params(params)?;
                RpcReply::value(&sandboxes.check(self.auth.as_ref(), &p.chat_id).await)
            }
            methods::START_SANDBOX_FRESH => {
                let p: ChatParams = parse_params(params)?;
                sandboxes
                    .start_fresh(self.sandbox_host(), &p.chat_id)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            _ => Err(RpcError::UnknownMethod(method.to_owned())),
        }
    }

    /// A composer asking what agents a place offers (#46): a place target
    /// (`place:<provider>[:<template>]`) or a sandbox device of a chat this
    /// device keeps. Answered here, never by a sandbox, so asking never
    /// creates or wakes one: what the template had the last time it ran,
    /// else this device's own list. `None` = not a place: route as usual.
    pub(super) async fn answer_for_place(
        &self,
        target: &str,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<Result<RpcReply, RpcError>> {
        let named = zeron_proto::places::parse_place_target(target);
        if !matches!(method, methods::LIST_HARNESSES | methods::LIST_MODELS) {
            // A place names no device: nothing else can go there.
            return named.map(|_| {
                Err(RpcError::Failed(format!(
                    "{method}: {target} is a place, not a device"
                )))
            });
        }
        let (provider, template) = match (named, &self.sandboxes) {
            (Some((provider, template)), _) => (provider.to_owned(), template.map(str::to_owned)),
            (None, Some(sandboxes)) => sandboxes.place_of_device(target)?,
            (None, None) => return None,
        };
        let kept = self
            .sandboxes
            .as_ref()
            .and_then(|s| s.catalog(&provider, template.as_deref()));
        let mut local = params.clone();
        if let Some(map) = local.as_object_mut() {
            map.remove("targetDeviceId");
        }
        Some(match (method, kept) {
            (methods::LIST_HARNESSES, Some(kept)) => {
                let mut harnesses = kept.harnesses;
                if !super::include_registry(params) {
                    harnesses.retain(|d| d.id.registry().is_none());
                }
                RpcReply::value(&harnesses)
            }
            (methods::LIST_MODELS, Some(kept)) => {
                let p: super::ListModelsParams = match parse_params(local.clone()) {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                match kept.models.get(p.harness.as_str()) {
                    Some(models) => RpcReply::value(models),
                    None => self.handle(method, local).await,
                }
            }
            // The template hasn't run: every agent this device knows.
            _ => self.handle(method, local).await,
        })
    }

    fn script_path(
        &self,
        sandboxes: &Sandboxes,
        p: &ScriptParams,
    ) -> Result<std::path::PathBuf, RpcError> {
        let repo = match &p.scope {
            Scope::Project { space_id } => {
                let space = self
                    .workspace
                    .space(space_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .ok_or_else(|| RpcError::Failed("That project no longer exists.".into()))?;
                repos_here(&self.workspace, self.doc_host.device_id())
                    .into_iter()
                    .find(|repo| repo.space_id == space.id)
            }
            _ => None,
        };
        scripts::path_of(
            sandboxes.steps_dir(),
            repo.as_ref(),
            &p.scope,
            &p.hook,
            &p.name,
        )
        .map_err(RpcError::Failed)
    }

    /// A child (subagent, side chat) of a chat on a sandbox shares that
    /// sandbox, its device and checkout, unless it asks for a place of its
    /// own. `None`: the parent isn't on a sandbox; create as usual.
    pub(super) fn create_chat_in_parents_sandbox(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        parent_chat_id: &str,
        child_kind: Option<zeron_proto::ChildKind>,
    ) -> Option<Result<RpcReply, RpcError>> {
        let parent = self.workspace.chat(parent_chat_id).ok().flatten()?;
        // This device keeps its sandbox, or this engine is that sandbox's.
        let kept = self
            .sandboxes
            .as_ref()
            .is_some_and(|s| s.hosts_device(&parent.device_id));
        let here = self.auth.as_ref().is_some_and(crate::auth::Auth::is_lent)
            && parent.device_id == self.doc_host.device_id();
        if !kept && !here {
            return None;
        }
        let created = self
            .workspace
            .create_chat_hosted(
                chat_id,
                space_id,
                device_id,
                Some(&parent.device_id),
                config,
                parent.cwd.clone(),
                Some(parent_chat_id.to_owned()),
                child_kind,
            )
            .map_err(|e| RpcError::Failed(e.to_string()));
        Some(created.and_then(|_| {
            RpcReply::value(&serde_json::json!({
                "ok": true, "deviceId": parent.device_id, "cwd": parent.cwd,
            }))
        }))
    }

    /// `createChat` on a place: the chat's row names its sandbox's device and
    /// checkout from the start, and the steps run while this returns.
    #[allow(clippy::too_many_arguments)] // `createChat`'s fields, as given
    pub(super) async fn create_chat_on_place(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        parent_chat_id: Option<String>,
        child_kind: Option<zeron_proto::ChildKind>,
        provider: &str,
        template: Option<String>,
    ) -> Result<RpcReply, RpcError> {
        let failed = |e: crate::EngineError| RpcError::Failed(e.to_string());
        let sandboxes = self.sandboxes()?;
        let space = match space_id {
            Some(id) => Some(
                self.workspace
                    .space(id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::Failed("That project no longer exists.".into()))?,
            ),
            None => None,
        };
        let placed = sandboxes
            .place(chat_id, space.as_ref(), provider, template)
            .map_err(RpcError::Failed)?;
        self.workspace
            .create_chat_hosted(
                chat_id,
                space_id,
                device_id,
                Some(&placed.device_id),
                config,
                Some(placed.cwd.clone()),
                parent_chat_id,
                child_kind,
            )
            .map_err(failed)?;
        sandboxes
            .start(self.sandbox_host(), chat_id)
            .map_err(RpcError::Failed)?;
        RpcReply::value(&serde_json::json!({
            "ok": true, "deviceId": placed.device_id, "cwd": placed.cwd,
        }))
    }
}

/// A script as Settings lists it: when it runs (and where), what it's for.
fn script_row(script: &scripts::Script) -> serde_json::Value {
    let (timing, step, runs_on) = match Hook::parse(&script.hook) {
        Some(hook) => (
            serde_json::to_value(hook.timing).unwrap_or_default(),
            hook.step,
            hook.runs_in_sandbox(),
        ),
        // A project's setup.sh: the Setup step itself, in the sandbox.
        None => (serde_json::json!("step"), Step::Setup, true),
    };
    serde_json::json!({
        "for": script.scope,
        "hook": script.hook,
        "name": script.name,
        "timing": timing,
        "step": step,
        "runsOn": if runs_on { "sandbox" } else { "host" },
        "script": script.shown,
        "path": script.path,
    })
}

/// Save a script, making its folders; runnable where that means something.
fn write_script(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("zeron-tmp");
    std::fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, path)
}
