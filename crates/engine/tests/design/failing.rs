//! The failure model (#25): a failed call changes nothing and gives an edit
//! id, the agent fixes its snippet with find-and-replace edits against that
//! id, and each successful call is one undo step.

use serde_json::{Value, json};

use crate::seam::Seam;

const SCREEN: &str = r##"
screen = Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF" });
Insert(screen, { type: "rectangle", name: "Title", x: 20, y: 20, width: 200, height: 30, fill: "#111111" });
"##;

/// Fails on line 3, after an edit and an assignment to a global.
const BROKEN: &str = r##"Insert(screen, { type: "rectangle", name: "Badge", width: 10, height: 10 });
counter = 5;
Insert("missing", { type: "rectangle", name: "Dot", width: 4, height: 4 });
Print(counter);"##;

#[tokio::test]
async fn a_failed_call_changes_nothing_and_gives_an_edit_id_and_its_line() {
    let seam = Seam::new().await;
    let made = seam.create("Failing", &format!("{SCREEN}\ncounter = 1;")).await;
    let before = std::fs::read_to_string(made.path()).unwrap();
    let failed = seam.execute(&made.path(), BROKEN).await;
    let failure = failed.failure();
    assert!(failure["error"].as_str().unwrap().contains("missing"), "{failure}");
    assert_eq!(failure["line"], 3);
    assert!(failure["code"].as_str().unwrap().starts_with("Insert(\"missing\""), "{failure}");
    assert!(failure["editId"].as_str().is_some_and(|id| !id.is_empty()), "{failure}");

    // The Design and the chat's globals are as they were.
    assert_eq!(std::fs::read_to_string(made.path()).unwrap(), before);
    let after = seam.execute(&made.path(), "Print(counter, Get().length, Get(screen).children.length);").await;
    assert_eq!(after.printed(), ["1 1 1"]);

    // A syntax error has its line too, and a thrown value that isn't an
    // Error still fails the call.
    let syntax = seam.execute(&made.path(), "Print(1);\nPrint(2;\n").await.failure();
    assert_eq!(syntax["line"], 2, "{syntax}");
    let thrown = seam.execute(&made.path(), r#"throw "plain";"#).await.failure();
    assert!(thrown["error"].as_str().unwrap().contains("plain"), "{thrown}");
    seam.shutdown().await;
}

#[tokio::test]
async fn retrying_with_edits_patches_the_stored_snippet_and_runs_it() {
    let seam = Seam::new().await;
    let made = seam.create("Retry", SCREEN).await;
    let edit_id = seam.execute(&made.path(), BROKEN).await.failure()["editId"].clone();

    // Each try patches the snippet as the last one left it; a failing try
    // keeps the same edit id.
    let again = seam
        .retry(&edit_id, json!([{ "find": "\"missing\"", "replace": "\"nope\"" }]))
        .await
        .failure();
    assert_eq!(again["editId"], edit_id);
    assert!(again["error"].as_str().unwrap().contains("nope"), "{again}");
    assert_eq!(again["code"].as_str().map(|code| code.contains("\"nope\"")), Some(true), "{again}");

    // An edit that doesn't match once changes nothing and says which.
    let unmatched = seam
        .retry(&edit_id, json!([{ "find": "\"nope\"", "replace": "screen" }, { "find": "absent", "replace": "" }]))
        .await
        .failure();
    assert!(unmatched["error"].as_str().unwrap().contains("absent"), "{unmatched}");
    let twice = seam.retry(&edit_id, json!([{ "find": "Insert", "replace": "Insert" }])).await.failure();
    assert!(twice["error"].as_str().unwrap().contains("2 times"), "{twice}");

    let fixed = seam.retry(&edit_id, json!([{ "find": "\"nope\"", "replace": "screen" }])).await;
    assert_eq!(fixed.printed(), ["5"]);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    let names: Vec<_> = file["children"][0]["children"].as_array().unwrap().iter().map(|n| n["name"].clone()).collect();
    assert_eq!(names, ["Title", "Badge", "Dot"]);

    // Done with once it succeeds; and another chat can't use it.
    let used = seam.retry(&edit_id, json!([])).await;
    assert!(used.is_error && used.text.contains("edit id"), "{}", used.text);
    let other_id = seam.execute(&made.path(), "throw new Error('x');").await.failure()["editId"].clone();
    let stranger = seam
        .call_from("other chat", "design_execute", json!({ "editId": other_id, "edits": [] }))
        .await;
    assert!(stranger.is_error && stranger.text.contains("edit id"), "{}", stranger.text);
    // A retry sends edits, not the snippet again.
    let both = seam
        .call("design_execute", json!({ "editId": other_id, "edits": [], "snippet": "1" }))
        .await;
    assert!(both.is_error && both.text.contains("snippet"), "{}", both.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn retrying_a_failed_new_design_makes_it() {
    let seam = Seam::new().await;
    let failed = seam.create("Later", "Insert({ type: \"frame\", name: \"A\", width: 10, height: 10 }); oops();").await;
    let edit_id = failed.failure()["editId"].clone();
    assert!(!seam.designs_folder().join("Later").exists());
    let made = seam.retry(&edit_id, json!([{ "find": " oops();", "replace": "" }])).await;
    assert_eq!(made.json()["design"]["name"], "Later");
    assert!(made.json()["ids"]["A"].is_string());
    seam.shutdown().await;
}

#[tokio::test]
async fn undoing_one_call_reverts_exactly_that_calls_changes() {
    let seam = Seam::new().await;
    let made = seam.create("Undo", SCREEN).await;
    let path = made.path();
    let title = made.id("Title");
    let read = || std::fs::read_to_string(&path).unwrap();
    let first = read();

    seam.execute(&path, &format!(r##"Update("{title}", {{ fill: "#FF0000" }}); Insert({{ type: "frame", name: "Second", x: 600, width: 50, height: 50 }});"##))
        .await
        .json();
    let second = read();
    seam.call_from(
        "other chat",
        "design_execute",
        json!({ "path": path, "snippet": format!(r#"Delete("{title}"); Insert({{ type: "frame", name: "Third", x: 900, width: 50, height: 50 }});"#) }),
    )
    .await
    .json();
    // A call that changes nothing is no step.
    seam.execute(&path, "Print(1);").await.json();

    // Undone a call at a time, latest first, whichever chat made it.
    assert!(seam.core.designs.undo(&path).unwrap());
    assert_eq!(read(), second);
    assert!(seam.core.designs.undo(&path).unwrap());
    assert_eq!(read(), first);
    // Redone, then a new call drops what's left to redo.
    assert!(seam.core.designs.redo(&path).unwrap());
    assert_eq!(read(), second);
    seam.execute(&path, r#"Insert({ type: "frame", name: "Fourth", width: 5, height: 5 });"#).await.json();
    assert!(!seam.core.designs.redo(&path).unwrap());
    // Back to the first call, and no further: making the Design is a step.
    assert!(seam.core.designs.undo(&path).unwrap());
    assert_eq!(read(), second);
    assert!(seam.core.designs.undo(&path).unwrap());
    assert_eq!(read(), first);
    assert!(seam.core.designs.undo(&path).unwrap());
    assert!(!seam.core.designs.undo(&path).unwrap());

    // Undo is read by the next call: the snippet sees the Design undone.
    let after = seam.execute(&path, "Print(Get().length);").await;
    assert_eq!(after.printed(), ["0"]);
    seam.shutdown().await;
}
