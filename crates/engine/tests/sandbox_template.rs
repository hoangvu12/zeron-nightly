//! Save as template (spec #37, docs/design/cloud-byo.md "What's in a
//! sandbox"), at the engine's RPCs with a fake provider whose sandboxes run
//! a second real engine, against a scripted edge:
//!
//! - the chat's sandbox is saved through the provider under the name given,
//!   and the template is then among the provider's templates (the picker's);
//! - Zeron's identity (its device id and data) is wiped from what's saved,
//!   and no sign-in is in it: the lent token and its key are nowhere on the
//!   saved disk, and a sandbox made from the template takes a device id of
//!   its own;
//! - the chat goes on afterwards on the same device;
//! - a place that can't wipe or save is refused, saving nothing.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/places.rs"]
mod places;
#[path = "support/scripted_edge.rs"]
mod scripted_edge;

use std::path::Path;
use std::time::Duration;

use fake_provider::FakeProvider;
use places::{
    ORG, SPACE, add_project, create_on_place, eventually, host_device, origin, project, ready, send,
};
use scripted_edge::ScriptedEdge;
use serde_json::{Value, json};
use zeron_doc::{MessageRole, MessageStatus};
use zeron_rpc::{RpcClient, memory_client, methods};

/// The chat's messages that ran on `sandbox`'s engine: user messages with an
/// id in `ids` and finished replies, counted.
async fn replies(provider: &FakeProvider, sandbox: &str, chat: &str, message: &str) {
    eventually(
        &format!("{message} ran on {sandbox}"),
        Duration::from_secs(60),
        async || {
            let entries = provider.with_engine(sandbox, |core| {
                core.doc_host
                    .open(chat)
                    .ok()
                    .and_then(|h| h.doc().read_entries().ok())
                    .unwrap_or_default()
            })?;
            let at = entries
                .iter()
                .position(|e| e.role == MessageRole::User && e.id == message)?;
            entries[at..]
                .iter()
                .any(|e| {
                    e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
                })
                .then_some(())
        },
    )
    .await;
}

async fn save(rpc: &RpcClient, chat: &str, name: &str) -> Result<Value, String> {
    rpc.call(
        methods::SAVE_SANDBOX_TEMPLATE,
        json!({ "chatId": chat, "name": name }),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saved_template_leaves_zerons_identity_and_sign_in_behind() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let provider = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(provider.clone());
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    let created = create_on_place(&rpc, "chat-a", "fake").await;
    let device_a = created["deviceId"].as_str().unwrap().to_owned();
    let cwd = created["cwd"].as_str().unwrap().to_owned();
    host.core()
        .workspace
        .rename_chat("chat-a", "Pre-titled")
        .unwrap();
    send(&rpc, "chat-a", "chat-a-1", &cwd, "set it up").await;
    ready(&rpc, "chat-a").await;
    replies(&provider, "chat-a", "chat-a", "chat-a-1").await;

    // What the sandbox has besides Zeron's own data: the release Zeron
    // installed, and the user's work in the checkout.
    let data = provider.data_dir("chat-a").expect("its engine's data");
    assert!(data.join("device-id").exists());
    let release = data.join("app").join("1.2.3");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(release.join("zeron-engine"), "the release").unwrap();
    std::fs::write(Path::new(&cwd).join("NOTES.md"), "work\n").unwrap();
    let (lent_key, lent_token) = provider.lent("chat-a").expect("a lent sign-in");

    let saved = save(&rpc, "chat-a", " reportly-dev ").await.expect("saved");
    let template = saved["template"].clone();
    assert_eq!(template["name"], "reportly-dev", "{saved:#}");
    let id = template["id"].as_str().expect("its id").to_owned();

    // Zeron stopped and wiped itself, the provider saved it, and Zeron
    // started again, in that order.
    let calls = provider.calls();
    let at = |what: &str| {
        calls
            .iter()
            .rposition(|c| c == what)
            .unwrap_or_else(|| panic!("{what}: {calls:#?}"))
    };
    let forgot = at("run chat-a: zeron-engine forget-device");
    let snapshot = at("save chat-a: reportly-dev");
    let restarted = at("run chat-a: zeron-engine watchdog");
    assert!(forgot < snapshot && snapshot < restarted, "{calls:#?}");

    // What was saved: no device id, no data, no sign-in; the release kept.
    let files = provider.template_files(&id);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        names.contains(&"zeron-data/app/1.2.3/zeron-engine"),
        "{names:#?}"
    );
    for (name, _) in &files {
        let inside = name.trim_start_matches("zeron-data/");
        for gone in [
            "device-id",
            "session.json",
            "orgs",
            "profiles",
            "local-profile.json",
            "ipc-token",
            "engine.lock",
            "plugin-signing-key",
            "logs",
            "watchdog",
        ] {
            assert!(!inside.starts_with(gone), "{name} was saved");
        }
    }
    for (name, contents) in &files {
        let text = String::from_utf8_lossy(contents);
        assert!(
            !text.contains(&lent_token) && !text.contains(&lent_key),
            "the lent sign-in is in {name}"
        );
    }
    assert!(names.contains(&"home/reportly/NOTES.md"), "{names:#?}");

    // The template is in the picker.
    let listed = rpc
        .call(
            methods::LIST_SANDBOX_TEMPLATES,
            json!({ "provider": "fake" }),
        )
        .await
        .expect("ListSandboxTemplates");
    assert!(
        listed
            .as_array()
            .unwrap()
            .contains(&json!({ "id": id, "name": "reportly-dev" })),
        "{listed:#}"
    );

    // The chat goes on, on the same device.
    assert_eq!(
        std::fs::read_to_string(data.join("device-id"))
            .ok()
            .as_deref(),
        Some(device_a.as_str())
    );
    send(&rpc, "chat-a", "chat-a-2", &cwd, "and now").await;
    replies(&provider, "chat-a", "chat-a", "chat-a-2").await;
    assert_eq!(
        rpc.call(methods::SANDBOX_STEPS, json!({ "chatId": "chat-a" }))
            .await
            .unwrap()["state"],
        "ready"
    );

    // A chat on the template gets a sandbox with a device id of its own.
    let created = rpc
        .call(
            methods::MUTATE,
            json!({
                "op": "createChat", "chatId": "chat-b", "spaceId": SPACE,
                "place": { "provider": "fake", "template": id },
            }),
        )
        .await
        .expect("createChat on the template");
    let device_b = created["deviceId"].as_str().unwrap().to_owned();
    assert_ne!(device_b, device_a);
    ready(&rpc, "chat-b").await;
    let data_b = provider.data_dir("chat-b").unwrap();
    assert!(
        data_b.join("app/1.2.3/zeron-engine").exists(),
        "from the template"
    );
    assert_eq!(
        std::fs::read_to_string(data_b.join("device-id"))
            .ok()
            .as_deref(),
        Some(device_b.as_str())
    );
    let dials = edge.host_dials(&device_b);
    assert!(
        !dials.is_empty() && dials.iter().all(|d| d.org.as_deref() == Some(ORG)),
        "{dials:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_cant_be_saved_safely_is_refused() {
    let edge = ScriptedEdge::with_registry().await;
    let dirs = tempfile::tempdir().unwrap();
    let origin_url = dirs.path().join("origin.git");
    origin(&origin_url);
    let host = host_device(&dirs.path().join("host"), &edge).await;
    let rpc = memory_client(host.core().rpc_service());
    let path = project(dirs.path(), &origin_url);
    add_project(&rpc, &host, &path).await;

    // A chat not on a place.
    let err = save(&rpc, "nowhere", "x").await.unwrap_err();
    assert!(err.contains("sandbox"), "{err}");

    // An SSH box can't save templates.
    let ssh = FakeProvider::run_only(&dirs.path().join("ssh"));
    host.core().sandboxes.register(ssh.clone());
    let created = create_on_place(&rpc, "chat-ssh", "fake-ssh").await;
    host.core()
        .workspace
        .rename_chat("chat-ssh", "Pre-titled")
        .unwrap();
    send(
        &rpc,
        "chat-ssh",
        "m1",
        created["cwd"].as_str().unwrap(),
        "hi",
    )
    .await;
    ready(&rpc, "chat-ssh").await;
    let err = save(&rpc, "chat-ssh", "box").await.unwrap_err();
    assert!(err.contains("can't save templates"), "{err}");
    assert!(
        !ssh.calls().iter().any(|c| c.contains("forget-device")),
        "{:#?}",
        ssh.calls()
    );

    // A blank name.
    let cloud = FakeProvider::cloud(&dirs.path().join("cloud"));
    host.core().sandboxes.register(cloud.clone());
    let created = create_on_place(&rpc, "chat-c", "fake").await;
    host.core()
        .workspace
        .rename_chat("chat-c", "Pre-titled")
        .unwrap();
    send(&rpc, "chat-c", "m2", created["cwd"].as_str().unwrap(), "hi").await;
    ready(&rpc, "chat-c").await;
    let err = save(&rpc, "chat-c", "  ").await.unwrap_err();
    assert!(err.contains("name"), "{err}");
    let err = save(&rpc, "chat-c", "a/b").await.unwrap_err();
    assert!(err.contains("name"), "{err}");
    assert!(cloud.saved().is_empty());
    assert!(
        !cloud.calls().iter().any(|c| c.contains("forget-device")),
        "{:#?}",
        cloud.calls()
    );
}
