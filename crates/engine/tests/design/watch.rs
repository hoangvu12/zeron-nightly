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
