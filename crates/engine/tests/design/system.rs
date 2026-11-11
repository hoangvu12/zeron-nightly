//! Design systems (#26): Components and their Instances, Tokens and the
//! Themes they vary by.

use serde_json::{Value, json};

use crate::seam::Seam;

const INDIGO: [u8; 4] = [79, 70, 229, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const RED: [u8; 4] = [239, 68, 68, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const DARK: [u8; 4] = [17, 17, 17, 255];

/// A Button Component (indigo, with a white Label) and a black Screen
/// holding two Instances of it: as it is at x 0, and at x 150 red with a
/// green Label.
const BUTTONS: &str = r##"
button = Insert({ type: "frame", name: "Button", reusable: true, x: 0, y: 200, width: 100, height: 40, fill: "#4F46E5",
  children: [{ type: "rectangle", name: "Label", x: 10, y: 10, width: 80, height: 20, fill: "#FFFFFF" }] });
label = Get(button).children[0].id;
screen = Insert({ type: "frame", name: "Screen", x: 0, y: 0, width: 300, height: 100, fill: "#000000" });
plain = Copy(button, screen, { x: 0, y: 0 });
red = Copy(button, screen, { x: 150, y: 0, fill: "#EF4444", descendants: { Label: { fill: "#00FF00" } } });
"##;

fn line(printed: &[String], at: usize) -> Value {
    serde_json::from_str(&printed[at]).unwrap_or_else(|_| panic!("{}", printed[at]))
}

#[tokio::test]
async fn instances_draw_their_component_with_overrides_and_follow_it() {
    let seam = Seam::new().await;
    let made = seam.create("Buttons", &format!("{BUTTONS}\nTakeScreenshot([screen]);")).await;
    let shot = &made.images[0];
    assert_eq!((shot.rgba(5, 5), shot.rgba(50, 20)), (INDIGO, WHITE));
    assert_eq!((shot.rgba(155, 5), shot.rgba(200, 20)), (RED, GREEN));
    assert_eq!(shot.rgba(120, 20), BLACK);

    // On file, an Instance is its Component's id and its overrides.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    let red = &file["children"][1]["children"][1];
    assert_eq!((red["type"].as_str(), red["ref"].as_str()), (Some("instance"), Some(made.id("Button").as_str())));
    assert_eq!(red["fill"], "#EF4444");
    assert_eq!(red["descendants"], json!({ "Label": { "fill": "#00FF00" } }));
    assert!(red.get("children").is_none(), "{red}");

    // A change to the Component reaches every Instance, under their overrides.
    let changed = seam
        .execute(
            &made.path(),
            r##"Update(button, { fill: "#111111", width: 120 }); Update(label, { height: 10 });
TakeScreenshot([screen]);"##,
        )
        .await;
    let shot = &changed.images[0];
    assert_eq!((shot.rgba(5, 5), shot.rgba(50, 15), shot.rgba(50, 25)), (DARK, WHITE, DARK));
    assert_eq!(shot.rgba(115, 5), DARK);
    assert_eq!((shot.rgba(155, 5), shot.rgba(200, 15), shot.rgba(200, 25)), (RED, GREEN, RED));
    seam.shutdown().await;
}

#[tokio::test]
async fn slash_paths_reach_into_instances() {
    let seam = Seam::new().await;
    let made = seam.create("Paths", BUTTONS).await;
    let path = made.path();
    let changed = seam
        .execute(
            &path,
            r##"Print(Get(`${plain}/${label}`));
Update(`${plain}/${label}`, { fill: "#0000FF" });
Print(Replace(`${red}/${label}`, { type: "frame", name: "Dot", width: 10, height: 10, fill: "#FFFFFF" }));
TakeScreenshot([screen, `${plain}/${label}`]);"##,
        )
        .await;
    let label = made.id("Label");
    let printed = changed.printed();
    let read = line(&printed, 0);
    let plain_id = read["id"].as_str().unwrap().split('/').next().unwrap().to_owned();
    assert_eq!(read["id"], format!("{plain_id}/{label}"));
    assert_eq!((read["name"].as_str(), read["fill"].as_str()), (Some("Label"), Some("#FFFFFF")));
    // The new node takes the path of the one it replaced.
    assert!(printed[1].ends_with(&format!("/{label}")), "{printed:?}");
    assert_eq!(changed.id("Dot"), printed[1]);
    let shot = &changed.images[0];
    assert_eq!((shot.rgba(5, 5), shot.rgba(50, 20)), (INDIGO, BLUE));
    // The Dot at its own x and y in the red Button, the Label gone.
    assert_eq!((shot.rgba(155, 5), shot.rgba(165, 15), shot.rgba(200, 20)), (WHITE, RED, RED));
    // A node inside an Instance screenshots on its own.
    assert_eq!((changed.images[1].width, changed.images[1].rgba(40, 10)), (80, BLUE));

    // Deleting one is refused, with the way to hide it.
    let deleted = seam.execute(&path, "Delete(`${plain}/${label}`);").await;
    assert!(deleted.is_error && deleted.text.contains("enabled: false"), "{}", deleted.text);
    let hidden = seam
        .execute(&path, "Update(`${plain}/${label}`, { enabled: false }); TakeScreenshot([screen]);")
        .await;
    assert_eq!(hidden.images[0].rgba(50, 20), INDIGO);
    // Nor can a node move in or out of one.
    let moved = seam.execute(&path, "Move(`${plain}/${label}`, screen);").await;
    assert!(moved.is_error && moved.text.contains("inside an Instance"), "{}", moved.text);
    let into = seam.execute(&path, r#"Insert(`${plain}/${label}`, { type: "rectangle" });"#).await;
    assert!(into.is_error && into.text.contains("inside an Instance"), "{}", into.text);
    let missing = seam.execute(&path, "Update(`${plain}/nope`, { fill: \"#000000\" });").await;
    assert!(missing.is_error && missing.text.contains("nope"), "{}", missing.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn nested_instances_take_overrides_by_path() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Nested",
            &format!(
                r##"{BUTTONS}
card = Insert({{ type: "frame", name: "Card", reusable: true, x: 400, y: 200, width: 120, height: 60, fill: "#FFFFFF" }});
inner = Copy(button, card, {{ x: 10, y: 10 }});
one = Copy(card, screen, {{ x: 0, y: 0 }});
two = Copy(card, screen, {{ x: 150, y: 0, descendants: {{ [`${{inner}}/${{label}}`]: {{ fill: "#00FF00" }} }} }});
Update(`${{one}}/${{inner}}/${{label}}`, {{ fill: "#0000FF" }});
Print(Get(one, {{ expandInstances: true, depth: Infinity }}));
TakeScreenshot([screen]);"##
            ),
        )
        .await;
    let shot = &made.images[0];
    // Each Card's Button's Label, 10 px into the Button, 10 into the Card.
    assert_eq!((shot.rgba(5, 5), shot.rgba(15, 15), shot.rgba(60, 30)), (WHITE, INDIGO, BLUE));
    assert_eq!(shot.rgba(210, 30), GREEN);
    let expanded = line(&made.printed(), 0);
    assert_eq!(expanded["type"], "frame");
    let button = &expanded["children"][0];
    assert_eq!(button["type"], "frame");
    assert_eq!(button["children"][0]["fill"], "#0000FF");
    let id = button["children"][0]["id"].as_str().unwrap();
    assert_eq!(id.matches('/').count(), 2, "{id}");
    seam.shutdown().await;
}

#[tokio::test]
async fn overrides_by_id_win_and_component_changes_that_break_instances_warn() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Precedence",
            &format!(
                r##"{BUTTONS}
Update(`${{plain}}/${{label}}`, {{ fill: "#0000FF" }});
Update(plain, {{ descendants: {{ Label: {{ fill: "#00FF00" }}, [label]: {{ fill: "#0000FF" }} }} }});
TakeScreenshot([screen]);"##
            ),
        )
        .await;
    // By id over by name, whichever came first.
    assert_eq!(made.images[0].rgba(50, 20), BLUE);

    // Deleting the Label from the Component leaves the red Instance's
    // override with nothing to override: said, though only the Component changed.
    let broke = seam.execute(&made.path(), "Delete(label);").await;
    let warnings = broke.json()["warnings"].to_string();
    assert!(warnings.contains("Label") && warnings.contains("to override"), "{warnings}");
    seam.shutdown().await;
}

#[tokio::test]
async fn overrides_by_unique_name_and_replacements_through_copy() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Named",
            &format!(
                r##"{BUTTONS}
Insert(button, {{ type: "rectangle", name: "Dot", x: 90, y: 0, width: 5, height: 5, fill: "#FFFFFF" }});
Insert(button, {{ type: "rectangle", name: "Dot", x: 95, y: 0, width: 5, height: 5, fill: "#FFFFFF" }});
lone = Insert({{ type: "frame", name: "Lone", x: 0, y: 400, width: 300, height: 100, fill: "#000000" }});
byName = Copy(button, lone, {{ x: 0, y: 0, descendants: {{ Label: {{ fill: "#0000FF" }} }} }});
swapped = Copy(button, lone, {{ x: 150, y: 0, descendants: {{ [label]: {{ type: "rectangle", name: "Swap", x: 0, y: 0, width: 20, height: 20, fill: "#00FF00" }} }} }});
Print(Get(`${{swapped}}/${{label}}`).name);
TakeScreenshot([lone]);"##
            ),
        )
        .await;
    let shot = &made.images[0];
    // The one node named Label, overridden by its name.
    assert_eq!((shot.rgba(5, 5), shot.rgba(50, 20)), (INDIGO, BLUE));
    // The Label replaced outright, the new node in its place by path.
    assert_eq!((shot.rgba(160, 10), shot.rgba(200, 25)), (GREEN, INDIGO));
    assert_eq!(made.printed(), ["Swap"]);

    // A name two nodes have overrides neither, and says so.
    let ambiguous = seam
        .execute(&made.path(), r##"Copy(button, lone, { x: 0, y: 50, descendants: { Dot: { fill: "#FF0000" } } });"##)
        .await;
    let warnings = ambiguous.json()["warnings"].to_string();
    assert!(warnings.contains("2 nodes") && warnings.contains("Dot"), "{warnings}");
    seam.shutdown().await;
}

#[tokio::test]
async fn text_content_takes_string_tokens_and_keeps_text_that_only_looks_like_one() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Words",
            r##"SetTokens({ title: { type: "string", value: "Hello" } });
const named = Insert({ type: "text", name: "Named", content: "$title", fontSize: 16, fill: "#000000" });
const price = Insert({ type: "text", name: "Price", content: "$5", fontSize: 16, fill: "#000000" });
Print(Get(named, { resolveTokens: true }).content, Get(price, { resolveTokens: true }).content, Get(named).content);"##,
        )
        .await;
    assert_eq!(made.printed(), ["Hello $5 $title"]);
    // "$5" is text, not a missing Token.
    let warnings = made.json().get("warnings").map(Value::to_string).unwrap_or_default();
    assert!(!warnings.contains("$5"), "{warnings}");
    // Anywhere else, a missing Token is still said.
    let fill = seam
        .execute(&made.path(), r##"Insert({ type: "text", name: "Odd", content: "x", fontSize: 16, fill: "$5" });"##)
        .await;
    assert!(fill.json()["warnings"].to_string().contains("$5"), "{}", fill.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn a_components_theme_reaches_inside_its_instances() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Themed",
            r##"SetTokens({ surface: { type: "color", value: [{ value: "#FFFFFF", theme: { scheme: "light" } }, { value: "#000000", theme: { scheme: "dark" } }] } });
const card = Insert({ type: "frame", name: "Card", reusable: true, theme: { scheme: "dark" }, x: 0, y: 200, width: 40, height: 40,
  children: [{ type: "rectangle", name: "Inside", width: 40, height: 40, fill: "$surface" }] });
const screen = Insert({ type: "frame", name: "Screen", width: 100, height: 40, fill: "#4F46E5" });
Copy(card, screen, { x: 0, y: 0 });
Copy(card, screen, { x: 50, y: 0, theme: { scheme: "light" } });
TakeScreenshot([screen]);"##,
        )
        .await;
    let shot = &made.images[0];
    assert_eq!((shot.rgba(20, 20), shot.rgba(70, 20)), (BLACK, WHITE));
    seam.shutdown().await;
}

#[tokio::test]
async fn themed_tokens_resolve_per_theme_and_theme_overrides_reach_a_subtree() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Tokens",
            r##"SetTokens({
  surface: { type: "color", value: [{ value: "#FFFFFF", theme: { mode: "light" } }, { value: "#000000", theme: { mode: "dark" } }] },
  accent: { type: "color", value: "#4F46E5" },
  side: { type: "number", value: 50 },
  title: { type: "string", value: "Hello" },
});
Print(GetTokens().themes);
light = Insert({ type: "frame", name: "Light", x: 0, y: 0, width: 100, height: 100, fill: "$surface",
  children: [{ type: "rectangle", name: "Box", x: 0, y: 0, width: "$side", height: "$side", fill: "$accent" }] });
dark = Insert({ type: "frame", name: "Dark", theme: { mode: "dark" }, x: 200, y: 0, width: 100, height: 100, fill: "$surface",
  children: [{ type: "rectangle", name: "Inner", x: 50, y: 50, width: 50, height: 50, fill: "$surface", theme: { mode: "light" } }] });
Print(Get(light).fill, Get(dark, { resolveTokens: true }).fill);
Get(light, (node, info) => { if (info.depth === 1) Print(info.bounds); });
TakeScreenshot([light, dark]);"##,
        )
        .await;
    let printed = made.printed();
    assert_eq!(line(&printed, 0), json!({ "mode": ["light", "dark"] }));
    assert_eq!(printed[1], "$surface #000000");
    assert_eq!(line(&printed, 2), json!({ "x": 0, "y": 0, "width": 50, "height": 50 }));
    let (light, dark) = (&made.images[0], &made.images[1]);
    assert_eq!((light.rgba(10, 10), light.rgba(75, 75)), (INDIGO, WHITE));
    // Dark over the Design's light, and light again inside it.
    assert_eq!((dark.rgba(10, 10), dark.rgba(75, 75)), (BLACK, WHITE));

    // On file, as written: the reference, not its value.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!(file["children"][0]["fill"], "$surface");
    assert_eq!(file["children"][0]["children"][0]["width"], "$side");
    assert_eq!(file["tokens"]["side"], json!({ "type": "number", "value": 50 }));

    // A Token's change reaches every node that refers to it.
    let changed = seam
        .execute(
            &made.path(),
            r##"SetTokens({ accent: { type: "color", value: "#EF4444" } }); Print(Object.keys(GetTokens().tokens).length);
TakeScreenshot([light]);"##,
        )
        .await;
    assert_eq!(changed.printed(), ["4"]);
    assert_eq!(changed.images[0].rgba(10, 10), RED);
    let replaced = seam
        .execute(&made.path(), r##"SetTokens({ accent: { type: "color", value: "#00FF00" } }, { replace: true });
Print(Object.keys(GetTokens().tokens));"##)
        .await;
    assert_eq!(replaced.printed(), [r#"["accent"]"#]);
    seam.shutdown().await;
}

#[tokio::test]
async fn bad_token_names_and_bare_values_are_refused() {
    let seam = Seam::new().await;
    let made = seam.create("Bad", "1;").await;
    for (snippet, says) in [
        (r##"SetTokens({ "$primary": { type: "color", value: "#FFFFFF" } });"##, "refer to it as \"$primary\""),
        (r##"SetTokens({ primary: "#FFFFFF" });"##, "a type and a value"),
        (r##"SetTokens({ primary: { type: "colour", value: "#FFFFFF" } });"##, "color, number or string"),
        (r##"SetTokens({ primary: { type: "color", value: "blue" } });"##, "colour"),
        (r##"SetTokens({ gap: { type: "number", value: "8" } });"##, "number"),
    ] {
        let refused = seam.execute(&made.path(), snippet).await;
        let error = refused.failure()["error"].as_str().unwrap().to_owned();
        assert!(error.contains(says), "{snippet}: {error}");
    }
    let tokens = seam.execute(&made.path(), "Print(GetTokens());").await;
    assert_eq!(tokens.printed(), [r#"{"themes":{},"tokens":{}}"#]);

    // A reference to a Token the Design lacks draws nothing, with a warning.
    let missing = seam
        .execute(&made.path(), r##"Insert({ type: "rectangle", name: "Odd", width: 5, height: 5, fill: "$nope" });"##)
        .await;
    let warnings = missing.json()["warnings"].to_string();
    assert!(warnings.contains("$nope") && warnings.contains(&missing.id("Odd")), "{warnings}");
    seam.shutdown().await;
}
