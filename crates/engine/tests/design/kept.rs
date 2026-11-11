//! What the model doesn't know is kept: unknown properties and node types
//! survive a save, and Insert warns about the properties it doesn't know.

use serde_json::{Value, json};

use crate::seam::Seam;

/// A Design with a key, a property and a node type this Zeron doesn't know.
fn newer_design() -> Value {
    json!({
        "version": "1.0",
        "generator": { "by": "a newer Zeron" },
        "children": [{
            "id": "scr01",
            "name": "Screen",
            "type": "frame",
            "width": 200,
            "height": 100,
            "fill": "#FFFFFF",
            "cornerRadius": 12,
            "children": [
                {
                    "id": "blob1", "name": "Blob", "type": "blob", "x": "50%", "points": [1, 2, 3],
                    "children": [{ "id": "inner", "type": "frame" }]
                }
            ]
        }]
    })
}

#[tokio::test]
async fn unknown_properties_and_node_types_survive_a_save() {
    let seam = Seam::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Newer.zdesign");
    let original = newer_design();
    std::fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

    let changed = seam
        .execute(
            &path,
            r##"Insert("scr01", { type: "rectangle", name: "Bar", x: 10, y: 10, width: 50, height: 20, fill: "#4F46E5" });
TakeScreenshot(["scr01"]);"##,
        )
        .await;
    let result = changed.json();
    assert!(result.get("warnings").is_none(), "{result}");
    let shot = &changed.images[0];
    assert_eq!(shot.rgba(30, 20), [79, 70, 229, 255]);
    assert_eq!(shot.rgba(150, 50), [255, 255, 255, 255]);

    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["generator"], original["generator"]);
    let screen = &file["children"][0];
    assert_eq!(screen["cornerRadius"], 12);
    assert_eq!(screen["children"][0], original["children"][0]["children"][0]);
    assert_eq!(screen["children"][1]["id"], changed.id("Bar"));

    // A node of an unknown type draws nothing and takes no edits.
    let blob_shot = seam.execute(&path, r#"TakeScreenshot(["blob1"]);"#).await;
    assert!(blob_shot.images.is_empty());
    assert!(blob_shot.json()["screenshots"][0]["error"].is_string(), "{}", blob_shot.text);
    let into_blob = seam
        .execute(&path, r#"Insert("blob1", { type: "rectangle", width: 1, height: 1 });"#)
        .await;
    assert!(into_blob.is_error && into_blob.text.contains("blob"), "{}", into_blob.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn insert_warns_about_properties_it_doesnt_know() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Typo",
            r##"Insert({ type: "rectangle", name: "Card", x: 0, width: 10, height: 10, fills: "#FF0000" });"##,
        )
        .await;
    let warnings = made.json()["warnings"].clone();
    assert_eq!(warnings.as_array().map(Vec::len), Some(1), "{warnings}");
    // About that node and property, and saying so.
    let warning = &warnings[0];
    assert_eq!((&warning["node"], &warning["property"]), (&json!(made.id("Card")), &json!("fills")));
    let message = warning["message"].as_str().unwrap();
    assert!(message.contains("\"fills\"") && message.contains("Card"), "{message}");

    // Kept as written, though it does nothing.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!(file["children"][0]["fills"], "#FF0000");

    // Not an unknown type, though: Insert refuses that.
    let unknown = seam.execute(&made.path(), r#"Insert({ type: "blob" });"#).await;
    let error = unknown.failure()["error"].as_str().unwrap().to_owned();
    assert!(error.contains("\"blob\""), "{error}");
    seam.shutdown().await;
}
