//! Settings › Sandboxes (spec #37, ticket #48): the engine's sandbox
//! methods, asked for the page (ui/screens/settings/sandboxes.luau).
//!
//! - `sandbox_settings.ask {method, params?, device?}`: what the page reads
//!   (the providers, a provider's host device, the step scripts and one's
//!   text).
//! - `sandbox_settings.change {method, params?, device?}`: what it changes
//!   (connect, disconnect, the host device, a script saved or deleted).
//!   Refused while a UI template is only a preview.
//!
//! `device` sends the call to that device (`targetDeviceId`): step scripts
//! are files on the host device, and only the host device says who a
//! provider is connected as. A refusal is the engine's own sentence when it
//! has one ("The host device is offline, …"), else an outcome word
//! (`offline`, `outdated`, `failed`).

use serde_json::{Value as Json, json};
use zgx_desktop::WindowContext;
use zgx_script::{ScriptHost, Value};

use crate::auth::text;
use crate::engine;
use crate::plugins::{json_value, value_json};

/// What the page reads.
const ASK: &[&str] = &["ListSandboxProviders", "SandboxHost", "ListStepScripts", "ReadStepScript"];
/// What it changes.
const CHANGE: &[&str] = &[
    "ConnectSandboxProvider",
    "DisconnectSandboxProvider",
    "SetSandboxHost",
    "WriteStepScript",
    "DeleteStepScript",
];

pub fn install(_cx: &mut WindowContext, host: &ScriptHost) {
    host.async_action("sandbox_settings.ask", |args| {
        let call = request(&args, ASK);
        async move { send(call?).await }
    });
    host.async_action("sandbox_settings.change", |args| {
        let call = request(&args, CHANGE);
        async move {
            crate::plugins::preview::refuse("sandbox_settings.change")?;
            send(call?).await
        }
    });
}

/// One call to the engine: its method and its params, `targetDeviceId` set
/// when it's for another device.
#[derive(Debug, PartialEq)]
struct Call {
    method: String,
    params: Json,
}

fn request(args: &Value, allowed: &[&str]) -> Result<Call, String> {
    let method = args.field("method").map(text).unwrap_or_default();
    let params = args.field("params").map(value_json).unwrap_or(json!({}));
    let device = args.field("device").map(text).filter(|d| !d.is_empty());
    call(&method, params, device.as_deref(), allowed)
}

fn call(method: &str, params: Json, device: Option<&str>, allowed: &[&str]) -> Result<Call, String> {
    if !allowed.contains(&method) {
        return Err("refused".into());
    }
    let mut params = if params.is_object() { params } else { json!({}) };
    if let Some(device) = device {
        params["targetDeviceId"] = Json::from(device);
    }
    Ok(Call { method: method.to_owned(), params })
}

async fn send(call: Call) -> Result<Value, String> {
    engine::get()
        .call(&call.method, call.params)
        .await
        .map(|answer| json_value(&answer))
        .map_err(|error| {
            eprintln!("zeron-zgx: sandbox settings: {}: {error}", call.method);
            words(&error)
        })
}

/// Why a call didn't go, as the page says it: the engine's own sentence
/// when it's one a person can read, else an outcome word.
fn words(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("unknown method") {
        // A device running an older Zeron.
        return "outdated".into();
    }
    if engine::outcome(error) == "offline" && !lower.contains("host device") {
        return "offline".into();
    }
    let mut said = error.trim();
    for prefix in ["forwarded:", "failed:", "Failed:"] {
        while let Some(rest) = said.strip_prefix(prefix) {
            said = rest.trim();
        }
    }
    let readable = !said.is_empty() && said.len() <= 240 && !said.contains('{');
    if readable { said.to_owned() } else { "failed".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_reads_and_changes_only_the_sandbox_methods() {
        let read = call("ListStepScripts", json!({}), Some("box"), ASK).unwrap();
        assert_eq!(read.params, json!({ "targetDeviceId": "box" }));
        let here = call("SandboxHost", json!({ "provider": "docker" }), None, ASK).unwrap();
        assert_eq!(here.params, json!({ "provider": "docker" }));
        // A change isn't a read, and nothing else is either.
        for (method, allowed) in [
            ("WriteStepScript", ASK),
            ("ListStepScripts", CHANGE),
            ("AccessToken", ASK),
            ("Mutate", CHANGE),
            ("AdoptSandboxProvider", CHANGE),
        ] {
            assert_eq!(call(method, json!({}), None, allowed), Err("refused".into()), "{method}");
        }
        // Params that aren't a record are none.
        assert_eq!(call("ListSandboxProviders", json!([1]), None, ASK).unwrap().params, json!({}));
    }

    #[test]
    fn a_refusal_says_the_engines_sentence_or_a_word() {
        assert_eq!(
            words("failed: The host device is offline, so it can't hand docker over. Connect it again on the new host device."),
            "The host device is offline, so it can't hand docker over. Connect it again on the new host device."
        );
        assert_eq!(words("offline"), "offline");
        assert_eq!(words("unknown method: DisconnectSandboxProvider"), "outdated");
        assert_eq!(words("failed: {\"weird\":1}"), "failed");
        assert_eq!(words(""), "failed");
    }
}
