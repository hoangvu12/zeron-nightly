//! The edit API (#25): Copy, Update, Replace, Move and Delete change nodes
//! already on the Canvas, and FindEmptySpace finds room for new ones.

use serde_json::{Value, json};

use crate::seam::Seam;

/// A Screen with a title and a button holding a label.
const SCREEN: &str = r##"
const screen = Insert({ type: "frame", name: "Screen", x: 0, y: 0, width: 400, height: 300, fill: "#FFFFFF" });
Insert(screen, { type: "rectangle", name: "Title", x: 20, y: 20, width: 200, height: 30, fill: "#111111" });
const button = Insert(screen, { type: "frame", name: "Button", x: 20, y: 200, width: 120, height: 40, fill: "#4F46E5" });
Insert(button, { type: "rectangle", name: "Label", x: 10, y: 10, width: 100, height: 20, fill: "#FFFFFF" });
"##;

fn file(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn line(printed: &[String], at: usize) -> Value {
    serde_json::from_str(&printed[at]).unwrap_or_else(|_| panic!("{}", printed[at]))
}

/// Every id in `value`, at any depth.
fn ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    fn collect(value: &Value, ids: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                if let Some(Value::String(id)) = object.get("id") {
                    ids.push(id.clone());
                }
                object.values().for_each(|value| collect(value, ids));
            }
            Value::Array(items) => items.iter().for_each(|value| collect(value, ids)),
            _ => {}
        }
    }
    collect(value, &mut ids);
    ids
}

#[tokio::test]
async fn copy_gives_the_copy_and_its_descendants_new_ids_and_takes_overrides() {
    let seam = Seam::new().await;
    let made = seam.create("Copies", SCREEN).await;
    let (screen, button, label) = (made.id("Screen"), made.id("Button"), made.id("Label"));
    let copied = seam
        .execute(
            &made.path(),
            &format!(
                r##"const copy = Copy("{button}", "{screen}", {{ name: "Cancel", y: 250, descendants: {{ "{label}": {{ fill: "#000000" }} }} }});
const onCanvas = Copy("{screen}");
Print(Get(copy, {{ depth: Infinity }}));
Print(Get(onCanvas, {{ depth: Infinity }}));"##
            ),
        )
        .await;
    let printed = copied.printed();
    let copy = line(&printed, 0);
    assert_eq!(copy["id"], copied.id("Cancel"));
    assert_eq!((copy["name"].as_str(), copy["y"].as_f64(), copy["x"].as_f64()), (Some("Cancel"), Some(250.), Some(20.)));
    assert_eq!(copy["children"][0]["name"], "Label");
    assert_eq!(copy["children"][0]["fill"], "#000000");
    assert_ne!(copy["children"][0]["id"], label);

    // The whole Screen, copied onto the Canvas: every id in it is new.
    let whole = line(&printed, 1);
    assert_eq!(whole["id"], copied.id("Screen"));
    let before = ids(&file(&made.path())["children"][0]);
    assert!(ids(&whole).iter().all(|id| !before.contains(id)), "{whole}");
    // Screen, Title, Button, Label, and Cancel with its Label.
    assert_eq!(ids(&whole).len(), 6);

    // The original is as it was.
    let original = &file(&made.path())["children"][0]["children"][1];
    assert_eq!(original["id"], button);
    assert_eq!(original["children"][0]["fill"], "#FFFFFF");

    // An override for a node that isn't in the copy is an error.
    let stray = seam
        .execute(&made.path(), &format!(r#"Copy("{button}", null, {{ descendants: {{ "{screen}": {{ x: 1 }} }} }});"#))
        .await;
    assert!(stray.is_error && stray.text.contains(&screen), "{}", stray.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn update_sets_properties_and_refuses_to_change_id_type_or_ref() {
    let seam = Seam::new().await;
    let made = seam.create("Updates", SCREEN).await;
    let (title, button) = (made.id("Title"), made.id("Button"));
    let updated = seam
        .execute(
            &made.path(),
            &format!(r##"Update("{title}", {{ width: 300, fill: "#FF0000", x: null }});"##),
        )
        .await;
    assert!(!updated.is_error, "{}", updated.text);
    let node = &file(&made.path())["children"][0]["children"][0];
    assert_eq!((node["width"].as_f64(), node["fill"].as_str()), (Some(300.), Some("#FF0000")));
    // null unsets a property; the rest are kept.
    assert!(node.get("x").is_none(), "{node}");
    assert_eq!((node["y"].as_f64(), node["height"].as_f64()), (Some(20.), Some(30.)));

    for (change, key) in [
        (r#"{ id: "other" }"#, "id"),
        (r#"{ type: "frame" }"#, "type"),
        (r#"{ ref: "abc" }"#, "ref"),
    ] {
        let refused = seam.execute(&made.path(), &format!(r#"Update("{title}", {change});"#)).await;
        assert!(refused.is_error && refused.text.contains(key), "{change}: {}", refused.text);
    }
    // Nor does it change children: Insert, Move, Delete and Replace do.
    let children = seam
        .execute(&made.path(), &format!(r#"Update("{button}", {{ children: [] }});"#))
        .await;
    assert!(children.is_error && children.text.contains("children"), "{}", children.text);
    let wrong = seam
        .execute(&made.path(), &format!(r#"Update("{title}", {{ width: "wide" }});"#))
        .await;
    assert!(wrong.is_error && wrong.text.contains("Update"), "{}", wrong.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn what_get_reads_updates_the_same_node() {
    let seam = Seam::new().await;
    let made = seam.create("Round trip", SCREEN).await;
    let button = made.id("Button");
    let before = file(&made.path());
    // As read, with its id, type and children: an Update that changes
    // nothing, then one that changes what was read.
    let updated = seam
        .execute(
            &made.path(),
            &format!(
                r##"const read = Get("{button}");
Update("{button}", read);
read.fill = "#00FF00"; read.width = 160;
Update("{button}", read);
Print(Get("{button}", {{ depth: Infinity }}));"##
            ),
        )
        .await;
    let button_read = line(&updated.printed(), 0);
    assert_eq!((button_read["fill"].as_str(), button_read["width"].as_f64()), (Some("#00FF00"), Some(160.)));
    let mut expected = before["children"][0]["children"][1].clone();
    expected["fill"] = json!("#00FF00");
    expected["width"] = json!(160.);
    assert_eq!(file(&made.path())["children"][0]["children"][1], expected);
    seam.shutdown().await;
}

#[tokio::test]
async fn an_updated_property_the_model_doesnt_know_is_written_once() {
    let seam = Seam::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Kept.zdesign");
    std::fs::write(
        &path,
        r##"{ "version": "1.0", "children": [
  { "id": "card1", "type": "rectangle", "width": 10, "height": 10, "glowz": 1 }
] }"##,
    )
    .unwrap();
    let updated = seam
        .execute(&path, r##"Update("card1", { glowz: 2, fill: "#FFFFFF" });"##)
        .await;
    // Warned of again, about that node and property.
    let warnings = updated.json()["warnings"].clone();
    assert_eq!(warnings.as_array().map(Vec::len), Some(1), "{warnings}");
    assert_eq!((warnings[0]["node"].as_str(), warnings[0]["property"].as_str()), (Some("card1"), Some("glowz")));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("\"glowz\"").count(), 1, "{text}");
    assert_eq!(file(&path)["children"][0]["glowz"], 2);

    // Unset, it's gone, and so is the warning an Insert in the same call
    // gave about it.
    let fixed = seam
        .execute(
            &path,
            r##"const typo = Insert({ type: "rectangle", name: "Typo", width: 5, height: 5, glowz: 1 });
Update(typo, { glowz: null });
Update("card1", { glowz: null });"##,
        )
        .await;
    assert!(fixed.json().get("warnings").is_none(), "{}", fixed.text);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("glowz"));
    seam.shutdown().await;
}

#[tokio::test]
async fn replace_puts_a_new_node_in_the_old_ones_place() {
    let seam = Seam::new().await;
    let made = seam.create("Replaced", SCREEN).await;
    let (title, button, label) = (made.id("Title"), made.id("Button"), made.id("Label"));
    let replaced = seam
        .execute(
            &made.path(),
            &format!(
                r##"Print(Replace("{title}", {{ type: "frame", name: "Heading", width: 200, height: 40,
  children: [{{ type: "rectangle", name: "Rule", width: 200, height: 2 }}] }}));"##
            ),
        )
        .await;
    let heading = replaced.id("Heading");
    assert_eq!(replaced.printed(), [format!("{heading}")]);
    assert!(replaced.json()["ids"]["Rule"].is_string());
    let screen = &file(&made.path())["children"][0];
    let children = screen["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!((children[0]["id"].as_str(), children[0]["name"].as_str()), (Some(heading.as_str()), Some("Heading")));
    assert_eq!(children[1]["id"], button);
    assert!(!ids(screen).contains(&title));

    // Replacing a node with one that won't do keeps the old one, even when
    // the snippet carries on past the error.
    let kept = seam
        .execute(
            &made.path(),
            &format!(r#"try {{ Replace("{label}", {{ type: "blob" }}); }} catch (e) {{ Print(String(e.message)); }}"#),
        )
        .await;
    assert!(kept.printed()[0].contains("blob"), "{:?}", kept.printed());
    assert_eq!(file(&made.path())["children"][0]["children"][1]["children"][0]["id"], label);
    seam.shutdown().await;
}

#[tokio::test]
async fn move_puts_a_node_under_a_new_parent_at_an_index() {
    let seam = Seam::new().await;
    let made = seam.create("Moves", SCREEN).await;
    let (screen, title, button, label) = (made.id("Screen"), made.id("Title"), made.id("Button"), made.id("Label"));
    let moved = seam
        .execute(
            &made.path(),
            &format!(
                r#"Move("{label}", "{screen}", 0);
Move("{title}", "{button}");
Move("{button}", null);"#
            ),
        )
        .await;
    assert!(!moved.is_error, "{}", moved.text);
    let canvas = &file(&made.path())["children"];
    assert_eq!(canvas[0]["children"].as_array().map(Vec::len), Some(1));
    assert_eq!(canvas[0]["children"][0]["id"], label);
    assert_eq!(canvas[1]["id"], button);
    assert_eq!(canvas[1]["children"][0]["id"], title);
    // Kept at its x and y, now from its new parent.
    assert_eq!(canvas[1]["children"][0]["x"].as_f64(), Some(20.));

    for (snippet, says) in [
        (format!(r#"Move("{button}", "{title}");"#), "inside itself"),
        (format!(r#"Move("{label}", "{screen}", 5);"#), "index"),
        (format!(r#"Move("{button}", "{label}");"#), "children"),
        (format!(r#"Move("nope", null);"#), "nope"),
    ] {
        let refused = seam.execute(&made.path(), &snippet).await;
        assert!(refused.is_error && refused.text.contains(says), "{snippet}: {}", refused.text);
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn delete_removes_nodes_and_their_descendants() {
    let seam = Seam::new().await;
    let made = seam.create("Deletes", SCREEN).await;
    let (title, button) = (made.id("Title"), made.id("Button"));
    let deleted = seam
        .execute(
            &made.path(),
            &format!(
                r#"Delete("{button}");
const gone = Insert({{ type: "frame", name: "Gone" }});
const kept = Insert({{ type: "frame", name: "Kept" }});
Delete([gone, "{title}"]);"#
            ),
        )
        .await;
    // Only what's still there is in the map of new ids.
    let ids_made: Vec<_> = deleted.json()["ids"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(ids_made, ["Kept"]);
    let canvas = &file(&made.path())["children"];
    assert_eq!(canvas.as_array().map(Vec::len), Some(2));
    assert!(canvas[0].get("children").is_none_or(|c| c == &json!([])), "{canvas}");

    let missing = seam.execute(&made.path(), &format!(r#"Delete("{button}");"#)).await;
    assert!(missing.is_error && missing.text.contains(&button), "{}", missing.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn find_empty_space_never_overlaps_top_level_nodes() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Space",
            r##"const a = Insert({ type: "frame", name: "A", x: 0, y: 0, width: 400, height: 300 });
Insert({ type: "frame", name: "B", x: 500, y: 0, width: 400, height: 800 });
Insert({ type: "frame", name: "C", x: 0, y: 400, width: 200, height: 200 });
Print(FindEmptySpace({ width: 400, height: 300 }));
Print(FindEmptySpace({ width: 400, height: 300, from: a, padding: 50 }));
Print(FindEmptySpace({ width: 100, height: 100, from: a, direction: "bottom", padding: 20 }));
Print(FindEmptySpace({ width: 100, height: 100, from: a, direction: "left" }));
Print(FindEmptySpace({ width: 100, height: 100, from: a, direction: "top", padding: 0 }));"##,
        )
        .await;
    let printed: Vec<Value> = (0..5).map(|at| line(&made.printed(), at)).collect();
    // Right of everything, 100 px apart by default.
    assert_eq!(printed[0], json!({ "x": 1000, "y": 0 }));
    // Right of A, past B, which is in the way.
    assert_eq!(printed[1], json!({ "x": 950, "y": 0 }));
    // Below A, past C.
    assert_eq!(printed[2], json!({ "x": 0, "y": 620 }));
    assert_eq!(printed[3], json!({ "x": -200, "y": 0 }));
    assert_eq!(printed[4], json!({ "x": 0, "y": -100 }));

    // An empty Canvas's space starts at its origin.
    let empty = seam.create("Empty", "Print(FindEmptySpace({ width: 10, height: 10 }));").await;
    assert_eq!(line(&empty.printed(), 0), json!({ "x": 0, "y": 0 }));

    // Screens put where it says never overlap, however many.
    let many = seam
        .execute(
            &made.path(),
            r#"for (let i = 0; i < 6; i++) {
  const at = FindEmptySpace({ width: 300, height: 200, direction: i % 2 ? "bottom" : "right" });
  Insert({ type: "frame", name: "S", x: at.x, y: at.y, width: 300, height: 200 });
}
Get((node, info) => { if (info.depth === 0) Print(info.bounds); return false; });"#,
        )
        .await;
    let rects: Vec<Value> = many.printed().iter().map(|line| serde_json::from_str(line).unwrap()).collect();
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let f = |v: &Value, k: &str| v[k].as_f64().unwrap();
            let apart = f(a, "x") + f(a, "width") <= f(b, "x")
                || f(b, "x") + f(b, "width") <= f(a, "x")
                || f(a, "y") + f(a, "height") <= f(b, "y")
                || f(b, "y") + f(b, "height") <= f(a, "y");
            assert!(apart, "{a} overlaps {b}");
        }
    }

    let bad = seam.execute(&made.path(), r#"FindEmptySpace({ width: 10, height: 10, direction: "up" });"#).await;
    assert!(bad.is_error && bad.text.contains("direction"), "{}", bad.text);
    seam.shutdown().await;
}
