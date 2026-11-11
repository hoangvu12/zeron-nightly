//! Reading back (#20): `Get` reads nodes as the file holds them, `Print`
//! adds lines to the result, and globals carry over to the chat's next call.

use serde_json::{Value, json};

use crate::seam::Seam;

/// A card with every layout property, a fill child and an absolute one.
const CARD: &str = r##"
const card = Insert({ type: "frame", name: "Card", x: 40, y: 30, width: 320, height: "fit_content", fill: "#FFFFFF",
  clip: true, layout: "vertical", gap: 12, padding: [16, 24], justifyContent: "space_between", alignItems: "center" });
const head = Insert(card, { type: "frame", name: "Head", width: "fill_container", layout: "horizontal", padding: [1, 2, 3, 4] });
Insert(head, { type: "rectangle", name: "Dot", width: 8, height: 8, fill: "#EF4444" });
Insert(card, { type: "rectangle", name: "Pin", absolute: true, x: 300, y: -4, width: 8, height: 8 });
"##;

/// `value` without its ids, at any depth.
fn without_ids(mut value: Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.remove("id");
                object.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut value);
    value
}

fn line(printed: &[String], at: usize) -> Value {
    serde_json::from_str(&printed[at]).unwrap_or_else(|_| panic!("{}", printed[at]))
}

#[tokio::test]
async fn get_reads_a_node_to_a_depth() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Read",
            &format!(
                "{CARD}
Print(Get(card, {{ depth: 0 }}));
Print(Get(card));
Print(Get(card, {{ depth: Infinity }}));
Print(Get());"
            ),
        )
        .await;
    let printed = made.printed();

    // The node as the file holds it, without children at depth 0.
    let alone = line(&printed, 0);
    assert_eq!(alone["id"], made.id("Card"));
    assert_eq!(
        without_ids(alone),
        json!({
            "type": "frame", "name": "Card", "x": 40, "y": 30, "width": 320, "height": "fit_content",
            "fill": "#FFFFFF", "clip": true, "layout": "vertical", "gap": 12, "padding": [16, 24],
            "justifyContent": "space_between", "alignItems": "center",
        })
    );
    // One level of children by default: Head's own children aren't read.
    let one = line(&printed, 1);
    assert_eq!(one["children"][0]["name"], "Head");
    assert_eq!(one["children"][0]["padding"], json!([1, 2, 3, 4]));
    assert!(one["children"][0].get("children").is_none(), "{one}");
    assert_eq!(one["children"][1]["absolute"], true);
    let all = line(&printed, 2);
    assert_eq!(all["children"][0]["children"][0]["name"], "Dot");
    // Without an id, the Canvas's nodes.
    assert_eq!(line(&printed, 3)[0]["name"], "Card");

    let missing = seam.execute(&made.path(), r#"Get("nope");"#).await;
    assert!(missing.is_error && missing.text.contains("nope"), "{}", missing.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn what_get_reads_inserts_as_the_same_nodes() {
    let seam = Seam::new().await;
    let made = seam.create("Copy", CARD).await;
    let card = made.id("Card");
    let copied = seam
        .execute(
            &made.path(),
            &format!(
                r#"const read = Get("{card}", {{ depth: Infinity }});
const copy = Insert(read);
Print(read);
Print(Get(copy, {{ depth: Infinity }}));"#
            ),
        )
        .await;
    let printed = copied.printed();
    let (original, copy) = (line(&printed, 0), line(&printed, 1));
    assert_ne!(original["id"], copy["id"]);
    assert_eq!(without_ids(original), without_ids(copy));
    seam.shutdown().await;
}

#[tokio::test]
async fn print_adds_lines_to_the_result() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Printed",
            r#"Print("plain", 2, { a: [1, "b"] }, null);
Print();
Print("two\nlines");"#,
        )
        .await;
    assert_eq!(made.printed(), ["plain 2 {\"a\":[1,\"b\"]} null", "", "two\nlines"]);
    let quiet = seam.execute(&made.path(), "1 + 1;").await;
    assert!(quiet.json().get("printed").is_none(), "{}", quiet.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn globals_carry_over_to_the_same_chats_next_call_only() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Globals",
            r##"screen = Insert({ type: "frame", name: "Screen", width: 10, height: 10 });
counter = 1; nested = { list: [1, 2], name: "n" };
let local = 1; var declared = 2;
helper = () => 1;"##,
        )
        .await;
    let next = seam
        .execute(
            &made.path(),
            r#"counter += 1;
Print(screen, counter, nested, typeof local, typeof declared, typeof helper);"#,
        )
        .await;
    assert_eq!(
        next.printed(),
        [format!(
            r#"{} 2 {{"list":[1,2],"name":"n"}} undefined undefined undefined"#,
            made.id("Screen")
        )]
    );

    // A failed call's assignments are dropped with its edits.
    let failed = seam
        .execute(&made.path(), r#"counter = 100; throw new Error("no");"#)
        .await;
    assert!(failed.is_error);
    // They follow the chat to another Design.
    let other = seam
        .create("Elsewhere", "Print(counter);")
        .await;
    assert_eq!(other.printed(), ["2"]);

    // Another chat starts with none.
    let stranger = seam
        .call_from(
            "other chat",
            "design_execute",
            json!({ "path": made.path(), "snippet": "Print(typeof counter, typeof screen);" }),
        )
        .await;
    assert_eq!(stranger.printed(), ["undefined undefined"]);
    seam.shutdown().await;
}

#[tokio::test]
async fn globals_too_big_to_carry_fail_the_call() {
    let seam = Seam::new().await;
    let big = seam.create("Big", r#"big = "x".repeat(2 * 1024 * 1024);"#).await;
    assert!(big.is_error && big.text.contains("let or const"), "{}", big.text);
    let fine = seam.create("Fine", r#"const big = "x".repeat(2 * 1024 * 1024); small = big.length;"#).await;
    assert!(!fine.is_error, "{}", fine.text);
    seam.shutdown().await;
}
