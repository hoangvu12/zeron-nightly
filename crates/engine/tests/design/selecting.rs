//! Selection (#32): the person's selection in a chat's Design tabs reaches
//! its agent through `design_state`, per Design, by the ids and names of
//! what's selected. The tab's call (`SetDesignSelection`), made here in its
//! place.

use serde_json::{Value, json};

use crate::seam::{CHAT, Seam};

const SCREEN: &str = r##"const screen = Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF" });
Insert(screen, { type: "rectangle", name: "Hero", width: 100, height: 50, fill: "#4F46E5" });
Insert(screen, { type: "text", name: "Title", content: "Hello" });"##;

/// The selection `design_state` reports for the Design named `name`.
fn selection(state: &Value, name: &str) -> Value {
    state["designs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == name)
        .map(|d| d["selection"].clone())
        .unwrap_or_else(|| panic!("{name} isn't open: {state}"))
}

#[tokio::test]
async fn design_state_reports_the_persons_selection_in_each_open_design() {
    let seam = Seam::new().await;
    let one = seam.create("One", SCREEN).await;
    let two = seam.create("Two", SCREEN).await;
    seam.window("SetDesignTabs", json!({ "chatId": CHAT, "paths": [one.path(), two.path()] })).await.unwrap();
    // Nothing selected yet.
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(selection(&state, "One"), json!([]), "{state}");

    // The person selects two nodes in One, one in Two.
    seam.window("SetDesignSelection", json!({ "chatId": CHAT, "path": one.path(), "ids": [one.id("Hero"), one.id("Title")] }))
        .await
        .expect("the window may say a selection");
    seam.window("SetDesignSelection", json!({ "chatId": CHAT, "path": two.path(), "ids": [two.id("Screen")] })).await.unwrap();
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(
        selection(&state, "One"),
        json!([
            { "id": one.id("Hero"), "name": "Hero", "type": "rectangle" },
            { "id": one.id("Title"), "name": "Title", "type": "text" },
        ]),
        "{state}"
    );
    assert_eq!(selection(&state, "Two"), json!([{ "id": two.id("Screen"), "name": "Screen", "type": "frame" }]));

    // A node deleted since isn't reported; nor is a Design's selection once
    // its tab closes.
    seam.execute(&one.path(), &format!(r#"Delete("{}");"#, one.id("Hero"))).await;
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(selection(&state, "One"), json!([{ "id": one.id("Title"), "name": "Title", "type": "text" }]));
    seam.window("SetDesignTabs", json!({ "chatId": CHAT, "paths": [one.path()] })).await.unwrap();
    seam.window("SetDesignTabs", json!({ "chatId": CHAT, "paths": [one.path(), two.path()] })).await.unwrap();
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(selection(&state, "Two"), json!([]), "{state}");
    seam.shutdown().await;
}

#[tokio::test]
async fn a_node_inside_an_instance_is_reported_by_its_path() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Kit",
            r##"const card = Insert({ type: "frame", name: "Card", reusable: true, width: 100, height: 60, fill: "#FFFFFF",
  children: [{ type: "text", name: "Label", content: "Hi" }] });
const screen = Insert({ type: "frame", name: "Screen", x: 200, width: 400, height: 300 });
Copy(card, screen, { x: 20, y: 20 });"##,
        )
        .await;
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    let instance = file["children"][1]["children"][0]["id"].as_str().unwrap().to_owned();
    let label = format!("{instance}/{}", made.id("Label"));
    seam.window("SetDesignTabs", json!({ "chatId": CHAT, "paths": [made.path()] })).await.unwrap();
    seam.window("SetDesignSelection", json!({ "chatId": CHAT, "path": made.path(), "ids": [label] })).await.unwrap();
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(selection(&state, "Kit"), json!([{ "id": label, "name": "Label", "type": "text" }]), "{state}");
    seam.shutdown().await;
}

#[tokio::test]
async fn another_chats_selection_isnt_this_ones() {
    let seam = Seam::new().await;
    let one = seam.create("One", SCREEN).await;
    seam.window("SetDesignTabs", json!({ "chatId": CHAT, "paths": [one.path()] })).await.unwrap();
    seam.window("SetDesignSelection", json!({ "chatId": "elsewhere", "path": one.path(), "ids": [one.id("Hero")] })).await.unwrap();
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(selection(&state, "One"), json!([]), "{state}");
    seam.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_say_a_selection() {
    assert!(!zeron_rpc::agent_allowed("SetDesignSelection"));
}
