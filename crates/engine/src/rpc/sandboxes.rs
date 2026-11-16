//! Places chats run on (docs/design/cloud-byo.md): `createChat` with a
//! `place`, and the Sandbox* methods the device keeping the provider answers
//! ([`crate::sandboxes`]).

use serde::Deserialize;
use zeron_proto::ChatConfig;
use zeron_rpc::{RpcError, RpcReply, RpcService, methods};

use super::{EngineRpc, parse_params, watch_stream};
use crate::sandboxes::{Host, Sandboxes};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_SANDBOX_PROVIDERS
            | methods::SANDBOX_STEPS
            | methods::WATCH_SANDBOX_STEPS
            | methods::RUN_SANDBOX_STEPS_AGAIN
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatParams {
    chat_id: String,
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
