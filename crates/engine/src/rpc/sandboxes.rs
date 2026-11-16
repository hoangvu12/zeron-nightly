//! Places chats run on (docs/design/cloud-byo.md): `createChat` with a
//! `place`, and the Sandbox* methods the device keeping the provider answers
//! ([`crate::sandboxes`]).
//!
//! That device is the provider's host device (spec #37): the registry names
//! it, and any other device asks it, forwarding the call over the relay as
//! `targetDeviceId` does. A forwarded call carries `routed`, so it's answered
//! where it lands and never sent on again.

use serde::Deserialize;
use serde_json::{Value, json};
use zeron_proto::{ChatConfig, SandboxHost};
use zeron_rpc::{RpcError, RpcReply, RpcService, methods};

use super::{EngineRpc, parse_params, watch_stream};
use crate::sandboxes::scripts::{self, Scope};
use crate::sandboxes::{Credential, Handover, Hook, Host, Sandboxes, Step, pick_host, repos_here};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_SANDBOX_PROVIDERS
            | methods::LIST_SANDBOX_TEMPLATES
            | methods::CONNECT_SANDBOX_PROVIDER
            | methods::DISCONNECT_SANDBOX_PROVIDER
            | methods::SANDBOX_STEPS
            | methods::WATCH_SANDBOX_STEPS
            | methods::RUN_SANDBOX_STEPS_AGAIN
            | methods::LIST_STEP_SCRIPTS
            | methods::READ_STEP_SCRIPT
            | methods::WRITE_STEP_SCRIPT
            | methods::DELETE_STEP_SCRIPT
            | methods::CHECK_SANDBOX
            | methods::START_SANDBOX_FRESH
            | methods::SAVE_SANDBOX_TEMPLATE
            | methods::WAKE_SANDBOX
            | methods::SANDBOX_HOST
            | methods::ADOPT_SANDBOX_PROVIDER
            | methods::SET_SANDBOX_HOST
    )
}

/// Set on a call one device sends another's way: answer it there.
const ROUTED: &str = "routed";
/// A device whose last presence beat is this recent is online (beats come
/// every 15 s; the engine's own `PRESENCE_FRESH_MS`).
const ONLINE_WITHIN_SECS: i64 = 45;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveTemplateParams {
    chat_id: String,
    name: String,
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
struct ProviderParams {
    provider: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectParams {
    provider: String,
    /// A pasted key; without one, the provider's own sign-in page.
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetHostParams {
    provider: String,
    device_id: String,
}

fn routed(params: &Value) -> bool {
    params.get(ROUTED).and_then(Value::as_bool) == Some(true)
}

/// `params` as sent on to the device that answers it.
fn route(mut params: Value) -> Value {
    if let Some(object) = params.as_object_mut() {
        object.remove("targetDeviceId");
        object.insert(ROUTED.into(), Value::Bool(true));
    }
    params
}

fn reply_value(reply: RpcReply, method: &str) -> Result<Value, RpcError> {
    match reply {
        RpcReply::Value(value) => Ok(value),
        RpcReply::Stream(_) => Err(RpcError::Failed(format!("{method} replied with a stream"))),
    }
}

/// The device couldn't be reached (as opposed to answering no).
fn unreachable(error: &RpcError) -> bool {
    match error {
        RpcError::Failed(message) => {
            let message = message.to_ascii_lowercase();
            ["offline", "unreachable", "timed out", "cannot reach", "can't be reached", "closed"]
                .iter()
                .any(|word| message.contains(word))
        }
        RpcError::BadParams(_) | RpcError::UnknownMethod(_) | RpcError::Capability(_) => false,
        RpcError::Transport(_) | RpcError::Closed => true,
    }
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

    fn this_device(&self) -> &str {
        self.doc_host.device_id()
    }

    /// The other host devices the registry names, each once.
    fn other_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .workspace
            .sandbox_hosts()
            .into_iter()
            .map(|host| host.host_device_id)
            .filter(|device| device != self.this_device())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// The host device that keeps `chat_id`, when it's another device: the
    /// one whose `SandboxSteps` knows it.
    async fn chat_keeper(&self, chat_id: &str) -> Option<String> {
        for device in self.other_hosts() {
            let asked = self
                .forward(
                    &device,
                    methods::SANDBOX_STEPS,
                    route(json!({ "chatId": chat_id })),
                )
                .await;
            if let Ok(RpcReply::Value(steps)) = asked
                && !steps.is_null()
            {
                return Some(device);
            }
        }
        None
    }

    pub(super) async fn sandboxes_rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let sandboxes = self.sandboxes()?;
        // A chat's methods go to the host device keeping it.
        if matches!(
            method,
            methods::SANDBOX_STEPS
                | methods::WATCH_SANDBOX_STEPS
                | methods::RUN_SANDBOX_STEPS_AGAIN
                | methods::CHECK_SANDBOX
                | methods::START_SANDBOX_FRESH
                | methods::SAVE_SANDBOX_TEMPLATE
                | methods::WAKE_SANDBOX
        ) && !routed(&params)
        {
            let p: ChatParams = parse_params(params.clone())?;
            if sandboxes.provider_of(&p.chat_id).is_none()
                && let Some(keeper) = self.chat_keeper(&p.chat_id).await
            {
                return self.forward(&keeper, method, route(params)).await;
            }
        }
        // A provider's templates come from its host device.
        if method == methods::LIST_SANDBOX_TEMPLATES && !routed(&params) {
            let p: ProviderParams = parse_params(params.clone())?;
            if !self.keeps(sandboxes, &p.provider)
                && let Some(host) = self
                    .workspace
                    .sandbox_host(&p.provider)
                    .map(|host| host.host_device_id)
                    .filter(|host| host != self.this_device())
            {
                return self.forward(&host, method, route(params)).await;
            }
        }
        match method {
            methods::LIST_SANDBOX_PROVIDERS => RpcReply::value(&sandboxes.providers()),
            methods::LIST_SANDBOX_TEMPLATES => {
                let p: ProviderParams = parse_params(params)?;
                let templates = sandboxes
                    .templates(&p.provider)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&templates)
            }
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
                    let host = self.settle_host(sandboxes, &p.provider).await?;
                    return RpcReply::value(&json!({ "account": account, "hostDeviceId": host }));
                }
                let code = provider.begin_connect().await.map_err(failed)?.ok_or_else(|| {
                    RpcError::Failed(format!("{} connects with a pasted key.", provider.name()))
                })?;
                let shown = serde_json::to_value(&code).unwrap_or_default();
                // Approved on the provider's page, in the user's time; then
                // the key goes to the host device.
                let rpc = self.clone();
                tokio::spawn(async move {
                    if let Err(err) = provider.connect(Credential::DeviceCode(code)).await {
                        tracing::warn!(provider = provider.id(), %err, "sandboxes: connect didn't finish");
                        return;
                    }
                    if let Some(sandboxes) = rpc.sandboxes.as_ref()
                        && let Err(err) = rpc.settle_host(sandboxes, provider.id()).await
                    {
                        tracing::warn!(provider = provider.id(), %err, "sandboxes: the key didn't reach the host device");
                    }
                });
                RpcReply::value(&serde_json::json!({ "deviceCode": shown }))
            }
            methods::DISCONNECT_SANDBOX_PROVIDER => {
                let p: ProviderParams = parse_params(params.clone())?;
                self.disconnect(sandboxes, &p.provider, params).await?;
                RpcReply::value(&json!({ "ok": true }))
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
                RpcReply::value(&json!({ "ok": true }))
            }
            methods::SANDBOX_HOST => {
                let p: ProviderParams = parse_params(params)?;
                RpcReply::value(&self.workspace.sandbox_host(&p.provider))
            }
            methods::ADOPT_SANDBOX_PROVIDER => {
                let mut params = params;
                if let Some(object) = params.as_object_mut() {
                    object.remove(ROUTED);
                    object.remove("targetDeviceId");
                }
                let handover: Handover = parse_params(params)?;
                self.adopt_provider(sandboxes, handover).await?;
                RpcReply::value(&json!({ "ok": true, "hostDeviceId": self.this_device() }))
            }
            methods::SET_SANDBOX_HOST => {
                let p: SetHostParams = parse_params(params.clone())?;
                let host = self.move_host(sandboxes, p, params).await?;
                RpcReply::value(&json!({ "ok": true, "hostDeviceId": host }))
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
            methods::SAVE_SANDBOX_TEMPLATE => {
                let p: SaveTemplateParams = parse_params(params)?;
                let template = sandboxes
                    .save_template(self.sandbox_host(), &p.chat_id, &p.name)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "template": template }))
            }
            methods::WAKE_SANDBOX => {
                let p: ChatParams = parse_params(params)?;
                sandboxes
                    .wake_chat(self.sandbox_host(), &p.chat_id)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            _ => Err(RpcError::UnknownMethod(method.to_owned())),
        }
    }

    /// Whether this device keeps `provider`: it's built in here and the
    /// registry names no other host device for it.
    fn keeps(&self, sandboxes: &Sandboxes, provider: &str) -> bool {
        sandboxes.provider_named(provider).is_some()
            && self
                .workspace
                .sandbox_host(provider)
                .is_none_or(|host| host.host_device_id == self.this_device())
    }

    /// `provider` was just connected on this device: its key goes to the
    /// host device (the one the registry names, else the one picked), once,
    /// and this device keeps it only when it's that one. The host device.
    async fn settle_host(&self, sandboxes: &Sandboxes, provider: &str) -> Result<String, RpcError> {
        let me = self.this_device().to_owned();
        let current = self.workspace.sandbox_host(provider);
        let target = match &current {
            Some(host) => host.host_device_id.clone(),
            None => {
                // The device list as it's shown: presence overlaid, so
                // "online" means what the Devices page says it does.
                let devices = self.workspace.watch_devices().borrow().clone();
                let now = chrono::Utc::now();
                let online = |id: &str| {
                    id == me
                        || devices.iter().any(|d| {
                            d.id == id
                                && d.last_seen_at.is_some_and(|seen| {
                                    now.signed_duration_since(seen)
                                        < chrono::Duration::seconds(ONLINE_WITHIN_SECS)
                                })
                        })
                };
                pick_host(&devices, online, &me)
            }
        };
        let built = sandboxes
            .provider_named(provider)
            .ok_or_else(|| RpcError::Failed(format!("{provider} isn't on this device.")))?;
        if target != me {
            let key = built.export_key().ok_or_else(|| {
                RpcError::Failed(format!("{} isn't connected on this device.", built.name()))
            })?;
            let handover = Handover {
                provider: provider.to_owned(),
                key,
                connected_on: Some(me.clone()),
                grants: Vec::new(),
                chats: Vec::new(),
            };
            let params = route(
                serde_json::to_value(&handover).map_err(|e| RpcError::Failed(e.to_string()))?,
            );
            let sent = self
                .forward(&target, methods::ADOPT_SANDBOX_PROVIDER, params)
                .await
                .and_then(|reply| reply_value(reply, methods::ADOPT_SANDBOX_PROVIDER));
            match sent {
                Ok(_) => {
                    // The host device keeps it; this one doesn't.
                    built.forget_key().await;
                    return Ok(target);
                }
                // A device picked just now that can't be reached: this one
                // keeps it instead.
                Err(err) if current.is_none() && unreachable(&err) => {
                    tracing::warn!(device = %target, error = %err,
                        "sandboxes: the picked host device can't be reached; this one keeps the key");
                }
                Err(err) => {
                    built.forget_key().await;
                    return Err(err);
                }
            }
        }
        self.workspace.set_sandbox_host(&SandboxHost {
            provider: provider.to_owned(),
            host_device_id: me.clone(),
            connected_on: current
                .and_then(|host| host.connected_on)
                .or_else(|| Some(me.clone())),
        });
        Ok(me)
    }

    /// `DisconnectSandboxProvider`: the host device forgets the key (its
    /// slot and session), and the registry names no host device for it.
    /// Asked of another device, it goes to the host device; one that can't
    /// be reached refuses, since its key would stay behind.
    async fn disconnect(
        &self,
        sandboxes: &Sandboxes,
        provider: &str,
        params: Value,
    ) -> Result<(), RpcError> {
        let built = sandboxes
            .provider_named(provider)
            .ok_or_else(|| RpcError::Failed(format!("{provider} isn't on this device.")))?;
        let host = self
            .workspace
            .sandbox_host(provider)
            .map(|host| host.host_device_id)
            .filter(|host| host != self.this_device());
        if let Some(host) = host
            && !routed(&params)
        {
            let reply = self
                .forward(&host, methods::DISCONNECT_SANDBOX_PROVIDER, route(params))
                .await
                .map_err(|err| {
                    if unreachable(&err) {
                        RpcError::Failed(format!(
                            "The host device is offline, so it can't let go of {}. Try again once it's online.",
                            built.name()
                        ))
                    } else {
                        err
                    }
                })?;
            reply_value(reply, methods::DISCONNECT_SANDBOX_PROVIDER)?;
            return Ok(());
        }
        // This device keeps it (or none does): forget it here.
        built.forget_key().await;
        self.workspace.clear_sandbox_host(provider);
        Ok(())
    }

    /// Become the provider's host device ([`Handover`]).
    async fn adopt_provider(
        &self,
        sandboxes: &Sandboxes,
        handover: Handover,
    ) -> Result<(), RpcError> {
        let provider = handover.provider.clone();
        let connected_on = handover.connected_on.clone().or_else(|| {
            self.workspace
                .sandbox_host(&provider)
                .and_then(|host| host.connected_on)
        });
        sandboxes
            .adopt(handover, self.auth.as_ref())
            .await
            .map_err(RpcError::Failed)?;
        self.workspace.set_sandbox_host(&SandboxHost {
            provider,
            host_device_id: self.this_device().to_owned(),
            connected_on,
        });
        Ok(())
    }

    /// `SetSandboxHost`: the current host device hands everything over to
    /// `deviceId`, then lets go. Asked of any device, it goes to that one.
    async fn move_host(
        &self,
        sandboxes: &Sandboxes,
        p: SetHostParams,
        params: Value,
    ) -> Result<String, RpcError> {
        let current = self
            .workspace
            .sandbox_host(&p.provider)
            .ok_or_else(|| RpcError::Failed(format!("{} isn't connected.", p.provider)))?;
        if current.host_device_id == p.device_id {
            return Ok(p.device_id);
        }
        if current.host_device_id != self.this_device() {
            if routed(&params) {
                return Err(RpcError::Failed(
                    "This device isn't the host device any more.".into(),
                ));
            }
            let reply = self
                .forward(&current.host_device_id, methods::SET_SANDBOX_HOST, route(params))
                .await
                .map_err(|err| {
                    if unreachable(&err) {
                        RpcError::Failed(format!(
                            "The host device is offline, so it can't hand {} over. Connect it again on the new host device.",
                            p.provider
                        ))
                    } else {
                        err
                    }
                })?;
            let reply = reply_value(reply, methods::SET_SANDBOX_HOST)?;
            return Ok(reply["hostDeviceId"]
                .as_str()
                .unwrap_or(&p.device_id)
                .to_owned());
        }
        let target = self
            .workspace
            .read_devices()
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .into_iter()
            .find(|d| d.id == p.device_id)
            .ok_or_else(|| RpcError::Failed("That device isn't one of yours.".into()))?;
        if !target.supports(zeron_proto::capabilities::SANDBOX_HOST_V1) {
            return Err(RpcError::Failed(format!(
                "{} runs an older Zeron that can't be a host device. Update it first.",
                target.name
            )));
        }
        let handover = sandboxes
            .handover(&p.provider, self.auth.as_ref(), current.connected_on.clone())
            .map_err(RpcError::Failed)?;
        let params = route(
            serde_json::to_value(&handover).map_err(|e| RpcError::Failed(e.to_string()))?,
        );
        let reply = self
            .forward(&target.id, methods::ADOPT_SANDBOX_PROVIDER, params)
            .await?;
        reply_value(reply, methods::ADOPT_SANDBOX_PROVIDER)?;
        // It has everything now; this device lets go.
        sandboxes.forget(&p.provider, self.auth.as_ref()).await;
        Ok(target.id)
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
    /// checkout from the start, and the steps run while this returns. On a
    /// device that doesn't keep the provider, it's the host device's to do
    /// (`raw`: the call as given, to send it there).
    #[allow(clippy::too_many_arguments)] // `createChat`'s fields, as given
    pub(super) async fn create_chat_on_place(
        &self,
        raw: Option<Value>,
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
        if let Some(raw) = raw.filter(|raw| !routed(raw))
            && !self.keeps(sandboxes, provider)
        {
            let target = raw
                .get("targetDeviceId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    self.workspace
                        .sandbox_host(provider)
                        .map(|host| host.host_device_id)
                })
                .filter(|target| target != self.this_device());
            if let Some(target) = target {
                return self.forward(&target, methods::MUTATE, route(raw)).await;
            }
        }
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

/// Save a script, making its folders: its owner's only, and runnable where
/// that means something.
fn write_script(path: &std::path::Path, content: &str) -> Result<(), String> {
    crate::runner::write_private_atomic(path, content.as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
