//! Targeting and shared Designs (#27): which Design a call is for, the
//! open and recent Designs, `design_state`, and one live Design changed by
//! several chats.
//!
//! The Design tab (#30, #31) tells the engine which Designs a chat's tabs
//! show with `Designs::set_tabs`; these tests call it in the tab's place.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use zeron_engine::designs::Designs;

use crate::seam::{CHAT, OTHER, Seam};

const SCREEN: &str = r##"Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF" });"##;

/// A call with neither a path nor a name.
async fn untargeted(seam: &Seam) -> crate::seam::Called {
    seam.call("design_execute", json!({ "snippet": SCREEN })).await
}

/// The ids of the nodes in the file at `path`, depth first.
fn ids_on_disk(path: &Path) -> Vec<String> {
    fn visit(nodes: &Value, ids: &mut Vec<String>) {
        for node in nodes.as_array().into_iter().flatten() {
            ids.push(node["id"].as_str().unwrap_or_default().to_owned());
            visit(&node["children"], ids);
        }
    }
    let file: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut ids = Vec::new();
    visit(&file["children"], &mut ids);
    ids
}

fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

#[tokio::test]
async fn an_explicit_path_targets_any_zdesign_on_disk() {
    let seam = Seam::new().await;
    let made = seam.create("Source", SCREEN).await;

    // A Design kept in a project, outside the managed folder.
    let project = tempfile::tempdir().unwrap();
    let mine = project.path().join("Mine.zdesign");
    std::fs::copy(made.path(), &mine).unwrap();
    let changed = seam
        .execute(&mine, &format!(r##"Insert("{}", {{ type: "rectangle", name: "Card", width: 10, height: 10 }});"##, made.id("Screen")))
        .await;
    assert_eq!(changed.json()["design"]["name"], "Mine");
    assert!(same_file(&changed.path(), &mine));
    assert_eq!(ids_on_disk(&mine), [made.id("Screen"), changed.id("Card")]);
    // The managed one is a Design of its own.
    assert_eq!(ids_on_disk(&made.path()), [made.id("Screen")]);

    // A file that isn't there says so, and makes nothing.
    let gone = project.path().join("Gone.zdesign");
    let missing = seam.execute(&gone, SCREEN).await;
    assert!(missing.is_error);
    assert!(missing.text.contains("Gone.zdesign") && missing.text.contains("no file"), "{}", missing.text);
    assert!(!gone.exists());
    seam.shutdown().await;
}

#[tokio::test]
async fn with_no_path_and_no_tab_a_call_lists_the_open_and_recent_designs() {
    let seam = Seam::new().await;
    let none = untargeted(&seam).await;
    assert!(none.is_error);
    assert!(none.text.contains("path") && none.text.contains("name"), "{}", none.text);
    assert!(none.text.contains("no Designs yet"), "{}", none.text);

    // Designs agents made with no tab open are recent, newest first.
    let alpha = seam.create("Alpha", SCREEN).await.path();
    let beta = seam.create("Beta", SCREEN).await.path();
    let listed = untargeted(&seam).await;
    assert!(listed.is_error);
    let (at_alpha, at_beta) = (
        listed.text.find(&*alpha.to_string_lossy()).expect(&listed.text),
        listed.text.find(&*beta.to_string_lossy()).expect(&listed.text),
    );
    assert!(at_beta < at_alpha, "{}", listed.text);
    assert!(!listed.text.contains("Open in Design tabs"), "{}", listed.text);

    // A Design open in another chat's tab is open, not this chat's front tab.
    seam.core.designs.set_tabs(OTHER, vec![alpha.clone()]);
    let listed = untargeted(&seam).await;
    assert!(listed.is_error);
    let open = listed.text.find("Open in Design tabs").expect(&listed.text);
    let recent = listed.text.find("Recent").expect(&listed.text);
    let at_alpha = listed.text.find(&*alpha.to_string_lossy()).unwrap();
    assert!(open < at_alpha && at_alpha < recent, "{}", listed.text);
    assert_eq!(listed.text.matches(&*alpha.to_string_lossy()).count(), 1, "{}", listed.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn recent_designs_include_ones_agents_made_with_no_tab_open() {
    let seam = Seam::new().await;
    let made = seam.create("Untabbed", SCREEN).await.path();
    let project = tempfile::tempdir().unwrap();
    let theirs = project.path().join("Theirs.zdesign");
    std::fs::copy(&made, &theirs).unwrap();
    seam.execute(&theirs, SCREEN).await.json();
    // A call that fails touches nothing.
    assert!(seam.create("Failed", "throw new Error('no')").await.is_error);

    let recent = seam.core.designs.recent();
    let names: Vec<_> = recent.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Theirs", "Untabbed"]);
    assert_eq!(recent[1].path, made);

    // Kept on this computer: an engine started later lists them too.
    let later = Designs::new(&seam.data_dir()).recent();
    let paths: Vec<PathBuf> = later.into_iter().map(|r| r.path).collect();
    assert_eq!(paths.len(), 2);
    assert!(same_file(&paths[0], &theirs) && paths[1] == made);

    // A Design whose file went away isn't offered.
    std::fs::remove_file(&theirs).unwrap();
    let names: Vec<_> = seam.core.designs.recent().into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["Untabbed"]);
    seam.shutdown().await;
}

#[tokio::test]
async fn a_call_with_no_path_targets_the_chats_front_design_tab() {
    let seam = Seam::new().await;
    let alpha = seam.create("Alpha", SCREEN).await.path();
    let beta = seam.create("Beta", SCREEN).await.path();

    seam.core.designs.set_tabs(CHAT, vec![beta.clone(), alpha.clone()]);
    let to_beta = untargeted(&seam).await;
    assert_eq!(to_beta.path(), beta);
    assert_eq!(ids_on_disk(&beta).len(), 2);

    // The tab brought to the front is the one changed next.
    seam.core.designs.set_tabs(CHAT, vec![alpha.clone(), beta.clone()]);
    assert_eq!(untargeted(&seam).await.path(), alpha);

    // A path or a name still wins over the tab.
    assert_eq!(seam.execute(&beta, SCREEN).await.path(), beta);
    assert_eq!(seam.create("Gamma", SCREEN).await.json()["design"]["name"], "Gamma");

    // Another chat's tabs aren't this chat's.
    seam.core.designs.set_tabs(CHAT, vec![]);
    seam.core.designs.set_tabs(OTHER, vec![beta.clone()]);
    assert!(untargeted(&seam).await.is_error);
    seam.shutdown().await;
}

#[tokio::test]
async fn design_state_lists_the_chats_open_designs_and_the_guide_reminder() {
    let seam = Seam::new().await;
    let tools = seam.tool_list().await;
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "design_state")
        .expect("design_state is listed");
    let description = tool["description"].as_str().unwrap();
    assert!(!description.is_empty() && !description.contains('\n'), "{description}");

    let empty = seam.call("design_state", json!({})).await.json();
    assert_eq!(empty["designs"], json!([]));
    assert!(empty["guide"].as_str().unwrap().contains("design_guide"), "{empty}");

    let alpha = seam.create("Alpha", SCREEN).await.path();
    let beta = seam.create("Beta", SCREEN).await.path();
    let gamma = seam.create("Gamma", SCREEN).await.path();
    seam.core.designs.set_tabs(CHAT, vec![beta.clone(), alpha.clone()]);
    seam.core.designs.set_tabs(OTHER, vec![gamma.clone()]);

    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(
        state["designs"],
        json!([
            { "name": "Beta", "path": beta, "front": true },
            { "name": "Alpha", "path": alpha, "front": false },
        ])
    );
    assert!(state["guide"].as_str().unwrap().contains("design_guide"), "{state}");
    // And the recent ones, for a Design named by path in an earlier call.
    let recent: Vec<_> = state["recent"].as_array().unwrap().iter().map(|r| r["name"].clone()).collect();
    assert_eq!(recent, [json!("Gamma"), json!("Beta"), json!("Alpha")]);

    let theirs = seam.call_other("design_state", json!({})).await.json();
    assert_eq!(theirs["designs"], json!([{ "name": "Gamma", "path": gamma, "front": true }]));
    seam.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_chats_changing_one_design_share_one_live_copy() {
    let seam = Seam::new().await;
    let made = seam.create("Shared", SCREEN).await;
    let (path, screen) = (made.path(), made.id("Screen"));
    let card = |name: &str| format!(r##"Insert("{screen}", {{ type: "rectangle", name: "{name}", width: 10, height: 10 }});"##);
    let shared = &seam;
    let from_other = |snippet: String| {
        let path = path.clone();
        async move {
            shared
                .call_other("design_execute", json!({ "path": path, "snippet": snippet }))
                .await
        }
    };

    // The other chat sees this chat's Screen at once, and this chat is told
    // its Design changed since its last call.
    let theirs = from_other(card("Theirs")).await;
    assert!(theirs.json().get("notice").is_none(), "{}", theirs.text);
    let mine = seam.execute(&path, &card("Mine")).await;
    let notice = mine.json()["notice"].as_str().unwrap_or_default().to_owned();
    assert!(notice.contains("changed since your last call"), "{}", mine.text);
    // Not when no one else changed it.
    let again = seam.execute(&path, &card("Again")).await;
    assert!(again.json().get("notice").is_none(), "{}", again.text);
    assert_eq!(
        ids_on_disk(&path),
        [screen.clone(), theirs.id("Theirs"), mine.id("Mine"), again.id("Again")]
    );

    // Calls from both chats at once all land, none lost.
    let (ours, others) = tokio::join!(
        async {
            let mut ids = Vec::new();
            for i in 0..12 {
                ids.push(seam.execute(&path, &card(&format!("Ours{i}"))).await.id(&format!("Ours{i}")));
            }
            ids
        },
        async {
            let mut ids = Vec::new();
            for i in 0..12 {
                ids.push(from_other(card(&format!("Others{i}"))).await.id(&format!("Others{i}")));
            }
            ids
        },
    );
    let on_disk = ids_on_disk(&path);
    assert_eq!(on_disk.len(), 4 + 24);
    for id in ours.iter().chain(&others) {
        assert!(on_disk.contains(id), "{id} was lost");
    }
    seam.shutdown().await;
}
