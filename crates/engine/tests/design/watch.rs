//! Design tabs follow the engine's Design (#30): the window watches a
//! Design by its path (`WatchDesign`) and hears it whole, then again after
//! each call that changes it, with the nodes that call made or changed.

use std::time::Duration;

use serde_json::{Value, json};

use crate::seam::Seam;

const SCREEN: &str = r##"
Insert({ type: "frame", name: "Screen", x: 100, y: 50, width: 800, height: 600, fill: "#FFFFFF" });
"##;

/// The next thing the watch says, within a few seconds.
async fn next(seen: &mut tokio::sync::mpsc::Receiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the watch said nothing")
        .expect("the watch ended")
}

#[tokio::test]
async fn a_watched_design_is_heard_whole_then_after_each_change() {
    let seam = Seam::new().await;
    let made = seam.create("Followed", SCREEN).await;
    let screen = made.id("Screen");
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let mut seen = window
        .subscribe("WatchDesign", json!({ "path": made.path() }))
        .await
        .expect("the window may watch a Design");

    // The Design as it is now.
    let first = next(&mut seen).await;
    assert_eq!(first["design"]["children"][0]["id"], screen, "{first}");
    let version = first["version"].as_u64().expect("a version");

    // An agent's call: the Design again, a version on, naming what it made.
    let more = seam
        .execute(
            &made.path(),
            &format!(r##"Insert("{screen}", {{ type: "rectangle", name: "Hero", width: 200, height: 100, fill: "#4F46E5" }});"##),
        )
        .await;
    let hero = more.id("Hero");
    let after = next(&mut seen).await;
    assert!(after["version"].as_u64().unwrap() > version, "{after}");
    assert_eq!(after["design"]["children"][0]["children"][0]["id"], hero, "{after}");
    let changed: Vec<&str> = after["changed"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(changed.contains(&hero.as_str()), "{after}");

    // A failed call says nothing.
    let failed = seam.execute(&made.path(), "throw new Error('no')").await;
    assert!(failed.is_error);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), seen.recv()).await.is_err(),
        "a failed call was heard"
    );
    seam.shutdown().await;
}

#[tokio::test]
async fn watching_a_file_that_is_no_design_fails() {
    let seam = Seam::new().await;
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let missing = seam.designs_folder().join("Nowhere").join("Nowhere.zdesign");
    let refused = window.call("WatchDesign", json!({ "path": missing })).await;
    assert!(refused.is_err(), "{refused:?}");
    seam.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_watch_a_design() {
    assert!(!zeron_rpc::agent_allowed("WatchDesign"));
}

#[tokio::test]
async fn the_windows_design_tabs_are_what_a_chat_targets() {
    let seam = Seam::new().await;
    let made = seam.create("Tabbed", SCREEN).await;
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    window
        .call("SetDesignTabs", json!({ "chatId": crate::seam::CHAT, "paths": [made.path()] }))
        .await
        .expect("the window may say its Design tabs");

    // The agent's chat now has it in front: design_state says so, and a
    // call naming no Design changes it.
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(state["designs"][0]["name"], "Tabbed", "{state}");
    assert_eq!(state["designs"][0]["front"], true, "{state}");
    let more = seam
        .call("design_execute", json!({ "snippet": r##"Insert({ type: "rectangle", name: "Box", width: 10, height: 10, fill: "#000" });"## }))
        .await;
    assert!(same_file(&more.path(), &made.path()), "{}", more.text);

    // Its last Design tab closed: none in front.
    window
        .call("SetDesignTabs", json!({ "chatId": crate::seam::CHAT, "paths": [] }))
        .await
        .unwrap();
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(state["designs"], json!([]), "{state}");
    seam.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_say_a_chats_design_tabs() {
    assert!(!zeron_rpc::agent_allowed("SetDesignTabs"));
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

#[tokio::test]
async fn the_window_hears_that_an_agent_made_a_change() {
    let seam = Seam::new().await;
    let made = seam.create("Touched", SCREEN).await;
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let mut seen = window.subscribe("WatchDesign", json!({ "path": made.path() })).await.unwrap();
    let first = next(&mut seen).await;
    // (As it was opened: nobody's change.)
    assert_eq!(first["by"], Value::Null, "{first}");
    seam.execute(&made.path(), &format!(r##"Insert("{}", {{ type: "rectangle", name: "Dot", width: 4, height: 4 }});"##, made.id("Screen")))
        .await;
    let after = next(&mut seen).await;
    assert_eq!(after["by"], "agent", "{after}");
    seam.shutdown().await;
}

#[tokio::test]
async fn a_screen_is_marked_in_progress_while_an_agent_builds_it() {
    let seam = Seam::new().await;
    let made = seam
        .create("Working", r##"Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF", inProgress: true });"##)
        .await;
    let result = made.json();
    assert!(result.get("warnings").is_none(), "inProgress is known: {result}");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!(file["children"][0]["inProgress"], true);
    // Not written when it's off.
    let done = seam
        .execute(&made.path(), r##"Insert({ type: "frame", name: "Done", width: 10, height: 10, inProgress: false });"##)
        .await;
    let file: Value = serde_json::from_str(&std::fs::read_to_string(done.path()).unwrap()).unwrap();
    assert!(file["children"][1].get("inProgress").is_none(), "{file}");
    seam.shutdown().await;
}
