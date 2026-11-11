//! Hand edits (#33): the person's moves, text and property edits in a
//! Design tab reach the engine as changes like an agent's, told to every
//! tab as the person's (they don't flash), and one undo stack takes back
//! the latest change, whoever made it. The tab's calls, made here in its
//! place.

use std::time::Duration;

use serde_json::{Value, json};

use crate::seam::Seam;

const SCREEN: &str = r##"const screen = Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF" });
const row = Insert(screen, { type: "frame", name: "Row", x: 20, y: 20, width: 300, height: 60, layout: "horizontal", gap: 10 });
Insert(row, { type: "rectangle", name: "A", width: 40, height: 40, fill: "#EF4444" });
Insert(row, { type: "rectangle", name: "B", width: 40, height: 40, fill: "#4F46E5" });
Insert(screen, { type: "text", name: "Title", x: 20, y: 200, content: "Hello" });"##;

fn on_disk(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The first node named `name` in the file, depth first.
fn named<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    if node["name"] == name {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| named(child, name))
}

#[tokio::test]
async fn the_persons_edits_are_changes_told_as_theirs_and_an_agent_hears_of_them() {
    let seam = Seam::new().await;
    let made = seam.create("Hand", SCREEN).await;
    let path = made.path();
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let mut seen = window.subscribe("WatchDesign", json!({ "path": path })).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), seen.recv()).await;

    // A move, a text edit and a property edit, one change.
    window
        .call(
            "EditDesign",
            json!({ "path": path, "description": "Move and retitle", "edits": [
                { "update": made.id("Title"), "props": { "x": 60, "y": 220, "content": "Hi there" } },
                { "update": made.id("Screen"), "props": { "fill": "#F3F4F6" } },
                { "move": made.id("B"), "parent": made.id("Row"), "index": 0 },
            ] }),
        )
        .await
        .expect("the window may edit a Design");
    let file = on_disk(&path);
    let title = named(&file["children"][0], "Title").unwrap();
    assert_eq!((title["x"].as_f64(), title["y"].as_f64(), title["content"].as_str()), (Some(60.), Some(220.), Some("Hi there")));
    assert_eq!(file["children"][0]["fill"], "#F3F4F6");
    let row = named(&file["children"][0], "Row").unwrap();
    assert_eq!(row["children"][0]["name"], "B", "{row}");

    // Every tab hears it, as the person's (so it doesn't flash).
    let told = tokio::time::timeout(Duration::from_secs(5), seen.recv()).await.unwrap().unwrap();
    assert_eq!(told["changes"][0]["by"], "person", "{told}");

    // The agent's next call is told the Design changed since its last.
    let after = seam.execute(&path, "Print(1);").await;
    assert!(after.json()["notice"].as_str().unwrap_or_default().contains("changed since"), "{}", after.text);

    // An edit the Design refuses changes nothing.
    let refused = window.call("EditDesign", json!({ "path": path, "edits": [{ "update": "nope", "props": { "x": 1 } }] })).await;
    assert!(refused.is_err(), "{refused:?}");
    seam.shutdown().await;
}

#[tokio::test]
async fn undo_takes_back_the_latest_change_whoever_made_it_and_redo_does_it_again() {
    let seam = Seam::new().await;
    let made = seam.create("Steps", SCREEN).await;
    let path = made.path();
    // The agent recolours A; the person moves the title.
    seam.execute(&path, &format!(r##"Update("{}", {{ fill: "#22C55E" }});"##, made.id("A"))).await;
    seam.window("EditDesign", json!({ "path": path, "edits": [{ "update": made.id("Title"), "props": { "y": 250 } }] })).await.unwrap();

    // Undo: the person's move first, then the agent's colour.
    let undone = seam.window("UndoDesign", json!({ "path": path })).await.unwrap();
    assert_eq!(undone["done"], true, "{undone}");
    assert_eq!(named(&on_disk(&path)["children"][0], "Title").unwrap()["y"].as_f64(), Some(200.));
    seam.window("UndoDesign", json!({ "path": path })).await.unwrap();
    assert_eq!(named(&on_disk(&path)["children"][0], "A").unwrap()["fill"], "#EF4444");
    // Redo: the agent's colour again.
    let redone = seam.window("RedoDesign", json!({ "path": path })).await.unwrap();
    assert_eq!(redone["done"], true);
    assert_eq!(named(&on_disk(&path)["children"][0], "A").unwrap()["fill"], "#22C55E");
    seam.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_edit_by_hand_or_step_through_the_history() {
    for method in ["EditDesign", "UndoDesign", "RedoDesign"] {
        assert!(!zeron_rpc::agent_allowed(method), "{method}");
    }
}
