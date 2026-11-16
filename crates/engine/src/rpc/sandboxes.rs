//! Places chats run on (docs/design/cloud-byo.md): `createChat` with a
//! `place`, and the Sandbox* methods the device keeping the provider answers
//! ([`crate::sandboxes`]).

use serde::Deserialize;
use zeron_proto::ChatConfig;
use zeron_rpc::{RpcError, RpcReply, methods};

use super::{EngineRpc, parse_params, watch_stream};
use crate::sandboxes::{Credential, Host, Sandboxes};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_SANDBOX_PROVIDERS
            | methods::CONNECT_SANDBOX_PROVIDER
            | methods::SANDBOX_STEPS
            | methods::WATCH_SANDBOX_STEPS
            | methods::RUN_SANDBOX_STEPS_AGAIN
            | methods::CHECK_SANDBOX
            | methods::START_SANDBOX_FRESH
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatParams {
    chat_id: String,
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
