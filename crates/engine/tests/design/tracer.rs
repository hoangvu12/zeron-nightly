//! The tracer (#19): an agent makes a Design with `design_execute`, inserts
//! frames and rectangles, and sees them in screenshots.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::seam::Seam;

const INDIGO: [u8; 4] = [79, 70, 229, 255];
const RED: [u8; 4] = [239, 68, 68, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A white 800 × 600 Screen with an indigo hero and a red badge.
const LANDING: &str = r##"
const screen = Insert({ type: "frame", name: "Screen", x: 100, y: 50, width: 800, height: 600, fill: "#FFFFFF" });
Insert(screen, { type: "rectangle", name: "Hero", x: 40, y: 40, width: 200, height: 100, fill: "#4F46E5" });
Insert(screen, { type: "rectangle", name: "Badge", x: 600, y: 400, width: 160, height: 160, fill: "#EF4444" });
"##;

#[tokio::test]
async fn design_execute_is_offered_with_a_one_line_description() {
    let seam = Seam::new().await;
    let tools = seam.tool_list().await;
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "design_execute")
        .expect("design_execute is listed");
    let description = tool["description"].as_str().unwrap();
    assert!(!description.is_empty() && !description.contains('\n'), "{description}");
    // A retry sends an edit id and edits in place of the snippet.
    let properties = &tool["inputSchema"]["properties"];
    assert!(properties["snippet"].is_object() && properties["editId"].is_object() && properties["edits"].is_object());
    seam.shutdown().await;
}

#[tokio::test]
async fn a_new_name_makes_a_managed_design_with_the_nodes_it_inserts() {
    let seam = Seam::new().await;
    let made = seam.create("Landing", LANDING).await;
    let result = made.json();
    assert_eq!(result["design"]["name"], "Landing");
    let ids = result["ids"].as_object().unwrap();
    let mut names: Vec<_> = ids.keys().collect();
    names.sort();
    assert_eq!(names, ["Badge", "Hero", "Screen"]);

    // One folder per Design in the managed folder, with an images folder.
    let path = made.path();
    let folder = seam.designs_folder().join("Landing");
    assert_eq!(path, folder.join("Landing.zdesign"));
    assert!(folder.join("images").is_dir());

    // Pretty-printed JSON: the Screen on the Canvas, holding both
    // rectangles, each with the id the call gave it.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\n  \"children\": ["), "{text}");
    let file: Value = serde_json::from_str(&text).unwrap();
    let screen = &file["children"][0];
    assert_eq!(screen["type"], "frame");
    assert_eq!(screen["id"], made.id("Screen"));
    assert_eq!((screen["width"].as_f64(), screen["fill"].as_str()), (Some(800.), Some("#FFFFFF")));
    let children = screen["children"].as_array().unwrap();
    assert_eq!(children[0]["id"], made.id("Hero"));
    assert_eq!(children[0]["type"], "rectangle");
    assert_eq!((children[0]["x"].as_f64(), children[0]["y"].as_f64()), (Some(40.), Some(40.)));
    assert_eq!(children[1]["id"], made.id("Badge"));

    // Every id is new; a node with no name is named after its type.
    let more = seam
        .execute(
            &path,
            &format!(r##"Insert("{}", {{ type: "rectangle", width: 10, height: 10, fill: "#000" }});"##, made.id("Screen")),
        )
        .await;
    let id = more.id("Rectangle");
    assert!(![made.id("Screen"), made.id("Hero"), made.id("Badge")].contains(&id));
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["children"][0]["children"][2]["id"], id);

    // The same name again makes another Design beside it.
    let again = seam.create("Landing", LANDING).await;
    assert_eq!(again.json()["design"]["name"], "Landing 2");
    assert!(again.path().starts_with(seam.designs_folder().join("Landing 2")));
    seam.shutdown().await;
}

#[tokio::test]
async fn screenshots_show_each_node_at_1x_capped_at_400_px() {
    let seam = Seam::new().await;
    let made = seam.create("Shots", LANDING).await;
    let shot = seam
        .execute(
            &made.path(),
            &format!(r#"TakeScreenshot(["{}", "{}"]);"#, made.id("Screen"), made.id("Badge")),
        )
        .await;
    assert_eq!(shot.images.len(), 2, "{}", shot.text);
    let shots = shot.json()["screenshots"].clone();
    assert_eq!(shots[0]["id"], made.id("Screen"));
    assert!(shots[0].get("data").is_none(), "the PNG goes as an image, not text");

    // The 800 × 600 Screen at half size, drawn from its own corner: white,
    // the hero at (40, 40) and the badge at (600, 400), halved.
    let screen = &shot.images[0];
    assert_eq!(screen.mime_type, "image/png");
    assert_eq!((screen.width, screen.height), (400, 300));
    assert_eq!(screen.rgba(5, 5), WHITE);
    assert_eq!(screen.rgba(21, 21), INDIGO);
    assert_eq!(screen.rgba(70, 45), INDIGO);
    assert_eq!(screen.rgba(119, 69), INDIGO);
    assert_eq!(screen.rgba(122, 45), WHITE);
    assert_eq!(screen.rgba(70, 72), WHITE);
    assert_eq!(screen.rgba(340, 240), RED);
    assert_eq!(screen.rgba(298, 240), WHITE);
    assert_eq!(screen.rgba(200, 150), WHITE);

    // The 160 px badge, small enough for 1x.
    let badge = &shot.images[1];
    assert_eq!((badge.width, badge.height), (160, 160));
    assert_eq!(badge.rgba(0, 0), RED);
    assert_eq!(badge.rgba(159, 159), RED);
    seam.shutdown().await;
}

#[tokio::test]
async fn a_snippet_that_throws_changes_nothing() {
    let seam = Seam::new().await;
    let made = seam.create("Kept", LANDING).await;
    let before = std::fs::read_to_string(made.path()).unwrap();
    let failed = seam
        .execute(
            &made.path(),
            r##"Insert({ type: "rectangle", width: 5, height: 5, fill: "#000" });
throw new Error("not this way");"##,
        )
        .await;
    assert!(failed.is_error);
    assert!(failed.text.contains("not this way"), "{}", failed.text);
    assert_eq!(std::fs::read_to_string(made.path()).unwrap(), before);

    // Nor does an edit the Design refuses, or a failed new Design.
    let refused = seam
        .execute(&made.path(), r##"Insert("nope", { type: "rectangle", width: 5, height: 5 });"##)
        .await;
    assert!(refused.is_error && refused.text.contains("nope"), "{}", refused.text);
    let unknown = seam
        .execute(&made.path(), r##"Insert({ type: "blob", width: 5, height: 5 });"##)
        .await;
    assert!(unknown.is_error && unknown.text.contains("blob"), "{}", unknown.text);
    let none = seam.create("Never", "throw new Error('no')").await;
    assert!(none.is_error);
    assert!(!seam.designs_folder().join("Never").exists());
    assert_eq!(std::fs::read_to_string(made.path()).unwrap(), before);
    seam.shutdown().await;
}

#[tokio::test]
async fn runaway_snippets_fail_without_hanging_the_engine() {
    let seam = Seam::new().await;
    for (runaway, says) in [
        ("while (true) {}", "limit"),
        ("const all = []; for (;;) all.push(new Array(1e6).fill(1));", "memory"),
        ("function deeper(n) { return deeper(n + 1) + 1; } deeper(0);", "stack"),
    ] {
        let started = Instant::now();
        let failed = seam.create("Runaway", runaway).await;
        assert!(failed.is_error, "{runaway}");
        assert!(failed.text.contains(says), "{runaway}: {}", failed.text);
        assert!(started.elapsed() < Duration::from_secs(15), "{runaway} took {:?}", started.elapsed());
    }
    assert!(!seam.designs_folder().join("Runaway").exists());

    // The engine still answers.
    let made = seam.create("After", LANDING).await;
    assert!(!made.is_error, "{}", made.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn a_screenshot_that_cant_be_drawn_says_why_and_keeps_the_edit() {
    let seam = Seam::new().await;
    let made = seam
        .create("Blank", r#"TakeScreenshot([Insert({ type: "frame", name: "Empty" })]);"#)
        .await;
    assert!(made.images.is_empty());
    let shot = &made.json()["screenshots"][0];
    assert_eq!(shot["id"], made.id("Empty"));
    assert!(shot["error"].as_str().unwrap().contains("no area"), "{shot}");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!(file["children"][0]["id"], made.id("Empty"));
    seam.shutdown().await;
}

#[tokio::test]
async fn nodes_nest_at_most_48_deep() {
    const NEST: &str = r#"let parent = null;
for (let i = 0; i < DEPTH; i++) parent = Insert(parent, { type: "frame", width: 10, height: 10 });"#;
    let seam = Seam::new().await;
    let deep = seam.create("Deep", &NEST.replace("DEPTH", "49")).await;
    assert!(deep.is_error && deep.text.contains("48"), "{}", deep.text);
    assert!(!seam.designs_folder().join("Deep").exists());

    // As deep as it goes, and the file still reads back.
    let made = seam.create("Deep", &NEST.replace("DEPTH", "48")).await;
    let again = seam.execute(&made.path(), r#"Insert({ type: "frame", width: 1, height: 1 });"#).await;
    assert!(!again.is_error, "{}", again.text);
    seam.shutdown().await;
}
