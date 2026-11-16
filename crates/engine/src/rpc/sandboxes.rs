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
use zeron_rpc::{RpcError, RpcReply, methods};

use super::{EngineRpc, parse_params, watch_stream};
use crate::sandboxes::{Handover, Host, Sandboxes, pick_host};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_SANDBOX_PROVIDERS
            | methods::SANDBOX_STEPS
            | methods::WATCH_SANDBOX_STEPS
            | methods::RUN_SANDBOX_STEPS_AGAIN
            | methods::SANDBOX_HOST
            | methods::CONNECT_SANDBOX_PROVIDER
            | methods::ADOPT_SANDBOX_PROVIDER
            | methods::SET_SANDBOX_HOST
            | methods::DELETE_SANDBOX
    )
}

/// Set on a call one device sends another's way: answer it there.
const ROUTED: &str = "routed";
/// A device whose last presence beat is this recent is online (beats come
/// every 15 s; the engine's own `PRESENCE_FRESH_MS`).
const ONLINE_WITHIN_SECS: i64 = 45;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatParams {
    chat_id: String,
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
    key: Value,
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
                | methods::DELETE_SANDBOX
        ) && !routed(&params)
        {
            let p: ChatParams = parse_params(params.clone())?;
            if sandboxes.provider_of(&p.chat_id).is_none()
                && let Some(keeper) = self.chat_keeper(&p.chat_id).await
            {
                return self.forward(&keeper, method, route(params)).await;
            }
        }
        match method {
            methods::LIST_SANDBOX_PROVIDERS => RpcReply::value(&sandboxes.providers()),
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
            methods::DELETE_SANDBOX => {
                let p: ChatParams = parse_params(params)?;
                sandboxes
                    .delete_sandbox(&self.sandbox_host(), &p.chat_id)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&json!({ "ok": true }))
            }
            methods::SANDBOX_HOST => {
                let p: ProviderParams = parse_params(params)?;
                RpcReply::value(&self.workspace.sandbox_host(&p.provider))
            }
            methods::CONNECT_SANDBOX_PROVIDER => {
                let p: ConnectParams = parse_params(params)?;
                let host = self.connect_provider(sandboxes, p).await?;
                RpcReply::value(&json!({ "ok": true, "hostDeviceId": host }))
            }
            methods::ADOPT_SANDBOX_PROVIDER => {
                let mut params = params;
                if let Some(object) = params.as_object_mut() {
                    object.remove(ROUTED);
                    object.remove("targetDeviceId");
                }
                let handover: Handover = parse_params(params)?;
                self.adopt_provider(sandboxes, handover)?;
                RpcReply::value(&json!({ "ok": true, "hostDeviceId": self.this_device() }))
            }
            methods::SET_SANDBOX_HOST => {
                let p: SetHostParams = parse_params(params.clone())?;
                let host = self.move_host(sandboxes, p, params).await?;
                RpcReply::value(&json!({ "ok": true, "hostDeviceId": host }))
            }
            _ => Err(RpcError::UnknownMethod(method.to_owned())),
        }
    }

    /// `ConnectSandboxProvider`: the key goes to the provider's host device,
    /// or else to the one picked, once; this device keeps it only when it's
    /// that one. Returns the host device.
    async fn connect_provider(
        &self,
        sandboxes: &Sandboxes,
        p: ConnectParams,
    ) -> Result<String, RpcError> {
        let me = self.this_device().to_owned();
        let current = self.workspace.sandbox_host(&p.provider);
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
        let handover = Handover {
            provider: p.provider.clone(),
            key: p.key,
            connected_on: Some(me.clone()),
            grants: Vec::new(),
            chats: Vec::new(),
        };
        if target != me {
            let params = route(
                serde_json::to_value(&handover).map_err(|e| RpcError::Failed(e.to_string()))?,
            );
            let sent = self
                .forward(&target, methods::ADOPT_SANDBOX_PROVIDER, params)
                .await
                .and_then(|reply| reply_value(reply, methods::ADOPT_SANDBOX_PROVIDER));
            match sent {
                Ok(_) => return Ok(target),
                // A device picked just now that can't be reached: this one
                // keeps it instead. Any other answer stands.
                Err(err) if current.is_none() && unreachable(&err) => {
                    tracing::warn!(device = %target, error = %err,
                        "sandboxes: the picked host device can't be reached; this one keeps the key");
                }
                Err(err) => return Err(err),
            }
        }
        self.adopt_provider(sandboxes, handover)?;
        Ok(me)
    }

    /// Become the provider's host device ([`Handover`]).
    fn adopt_provider(&self, sandboxes: &Sandboxes, handover: Handover) -> Result<(), RpcError> {
        let provider = handover.provider.clone();
        let connected_on = handover.connected_on.clone().or_else(|| {
            self.workspace
                .sandbox_host(&provider)
                .and_then(|host| host.connected_on)
        });
        sandboxes
            .adopt(handover, self.auth.as_ref())
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
        sandboxes.forget(&p.provider, self.auth.as_ref());
        Ok(target.id)
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
            && !sandboxes.keeps(provider)
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
