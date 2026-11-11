//! Opening Designs from the Design tab (#31): New design makes a managed
//! Design on disk at once, and Recent designs lists the Designs touched on
//! this computer, by an agent with no tab open too. The tab's calls, made
//! here in its place.

use serde_json::{Value, json};

use crate::seam::Seam;

const SCREEN: &str = r##"Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF" });"##;

#[tokio::test]
async fn new_design_makes_a_managed_design_on_disk_at_once() {
    let seam = Seam::new().await;
    let made = seam.window("NewDesign", json!({})).await.expect("the window makes a Design");
    let path = std::path::PathBuf::from(made["path"].as_str().unwrap());
    assert_eq!(made["name"], "Untitled design", "{made}");
    assert!(path.starts_with(seam.designs_folder()), "{made}");
    assert!(path.is_file() && path.parent().unwrap().join("images").is_dir(), "{made}");
    // Empty, saving itself, and first among the recent.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["children"], json!([]), "{file}");
    let saving = seam.window("DesignSaving", json!({ "path": path })).await.unwrap();
    assert_eq!(saving["managed"], true, "{saving}");
    let recent = seam.window("RecentDesigns", json!({})).await.unwrap();
    assert_eq!(recent["recent"][0]["path"], made["path"], "{recent}");

    // Another, beside it; and one named.
    let again = seam.window("NewDesign", json!({})).await.unwrap();
    assert_eq!(again["name"], "Untitled design 2", "{again}");
    let named = seam.window("NewDesign", json!({ "name": "Checkout" })).await.unwrap();
    assert_eq!(named["name"], "Checkout", "{named}");
    seam.shutdown().await;
}

#[tokio::test]
async fn recent_designs_lists_what_agents_made_with_no_tab_open() {
    let seam = Seam::new().await;
    let one = seam.create("Landing", SCREEN).await.path();
    let two = seam.create("Pricing", SCREEN).await.path();
    let recent = seam.window("RecentDesigns", json!({})).await.unwrap();
    let names: Vec<&str> = recent["recent"].as_array().unwrap().iter().filter_map(|r| r["name"].as_str()).collect();
    assert_eq!(names, ["Pricing", "Landing"], "newest first: {recent}");
    assert_eq!(recent["recent"][1]["path"], json!(one));
    assert!(recent["recent"][0]["touchedAt"].as_u64().is_some());
    // A Design whose file is gone isn't listed.
    std::fs::remove_file(&two).unwrap();
    let recent = seam.window("RecentDesigns", json!({})).await.unwrap();
    assert_eq!(recent["recent"].as_array().unwrap().len(), 1, "{recent}");
    seam.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_make_or_list_designs_through_the_windows_calls() {
    assert!(!zeron_rpc::agent_allowed("NewDesign"));
    assert!(!zeron_rpc::agent_allowed("RecentDesigns"));
}
