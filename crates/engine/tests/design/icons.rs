//! Icon nodes (#23): any Iconify icon by set and name, fetched on first use
//! (from the seam's fixture icons, not Iconify) and cached on the computer.

use serde_json::Value;

use crate::seam::Seam;

const RED: [u8; 4] = [239, 68, 68, 255];

/// The square fixture: `mdi:square`, filling 4 to 20 of a 24 view box.
#[tokio::test]
async fn an_icon_draws_at_its_size_in_its_colour() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Icons",
            r##"TakeScreenshot([
  Insert({ type: "icon", icon: "mdi:square", width: 48, height: 48, fill: "#EF4444" }),
  Insert({ type: "icon", icon: "mdi:square", width: 200, height: 200, fill: "#EF4444" }),
  Insert({ type: "icon", icon: "mdi:square", width: 48, height: 48, fill: "#EF444480" }),
  Insert({ type: "icon", icon: "mdi:square", width: 48, height: 48 }),
]);"##,
        )
        .await;
    assert_eq!(made.images.len(), 4, "{}", made.text);
    let small = &made.images[0];
    assert_eq!((small.width, small.height), (48, 48));
    for (x, y) in [(24, 24), (9, 9), (38, 38)] {
        assert_eq!(small.rgba(x, y), RED, "({x}, {y})");
    }
    assert_eq!((small.rgba(5, 24)[3], small.rgba(42, 42)[3]), (0, 0));

    let large = &made.images[1];
    assert_eq!((large.width, large.height), (200, 200));
    assert_eq!((large.rgba(100, 100), large.rgba(36, 36), large.rgba(163, 163)), (RED, RED, RED));
    assert_eq!((large.rgba(30, 100)[3], large.rgba(100, 170)[3]), (0, 0));

    // A translucent colour, and black without a fill.
    let [r, g, b, a] = made.images[2].rgba(24, 24);
    assert!((r, g, b) == (239, 68, 68) && (120..=136).contains(&a), "{:?}", [r, g, b, a]);
    assert_eq!(made.images[3].rgba(24, 24), [0, 0, 0, 255]);

    // The file names the icon; it isn't embedded.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!((file["children"][0]["type"].as_str(), file["children"][0]["icon"].as_str()), (Some("icon"), Some("mdi:square")));
    seam.shutdown().await;
}

#[tokio::test]
async fn icons_turn_fade_and_clip_as_any_node_does() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Turned",
            r##"const board = Insert({ type: "frame", width: 220, height: 100 });
// Cut off at the frame's bottom, halfway down the icon.
const half = Insert(board, { type: "frame", width: 48, height: 24, clip: true });
Insert(half, { type: "icon", icon: "mdi:square", width: 48, height: 48, fill: "#EF4444" });
Insert(board, { type: "icon", icon: "mdi:square", x: 60, width: 48, height: 48, fill: "#EF4444", opacity: 0.5 });
// A level line, turned upright about its top-left corner.
Insert(board, { type: "icon", icon: "lucide:minus", x: 150, y: 60, width: 48, height: 48, fill: "#EF4444", rotation: 90 });
TakeScreenshot(board);"##,
        )
        .await;
    let image = &made.images[0];
    assert_eq!(image.rgba(24, 20), RED);
    assert_eq!(image.rgba(24, 30)[3], 0);
    let [r, g, b, a] = image.rgba(84, 24);
    assert!((r, g, b) == (239, 68, 68) && (120..=136).contains(&a), "{:?}", [r, g, b, a]);
    assert_eq!(image.rgba(174, 36), RED);
    assert_eq!((image.rgba(160, 36)[3], image.rgba(174, 84)[3]), (0, 0));
    seam.shutdown().await;
}

#[tokio::test]
async fn icons_are_fetched_once_and_cached_on_the_computer() {
    let seam = Seam::new().await;
    let made = seam
        .create("Cached", r#"Insert({ type: "icon", icon: "mdi:square", width: 24, height: 24 });"#)
        .await;
    assert!(!made.is_error, "{}", made.text);
    let again = seam
        .execute(
            &made.path(),
            r#"TakeScreenshot(Insert({ type: "icon", icon: "mdi:square", width: 24, height: 24 }));"#,
        )
        .await;
    assert_eq!(again.images.len(), 1, "{}", again.text);
    assert_eq!(*seam.iconify.fetched.lock().unwrap(), ["mdi:square"]);
    let cached = seam.core.designs.icons().dir().join("mdi").join("square.svg");
    assert!(std::fs::read_to_string(cached).unwrap().contains("viewBox=\"0 0 24 24\""));
    seam.shutdown().await;
}

#[tokio::test]
async fn an_unknown_icon_fails_the_call_and_says_why() {
    let seam = Seam::new().await;
    let made = seam
        .create("Known", r#"Insert({ type: "icon", icon: "mdi:square", width: 24, height: 24 });"#)
        .await;
    let before = std::fs::read_to_string(made.path()).unwrap();
    for (icon, says) in [
        ("lucide:no-such-icon", "no icon \"lucide:no-such-icon\""),
        ("house", "set and name"),
        ("../mdi:square", "set and name"),
        ("", "set and name"),
    ] {
        let failed = seam
            .execute(
                &made.path(),
                &format!(
                    r#"Insert({{ type: "frame", width: 10, height: 10, children: [{{ type: "icon", icon: "{icon}", width: 24, height: 24 }}] }});"#
                ),
            )
            .await;
        assert!(failed.is_error && failed.text.contains(says), "{icon}: {}", failed.text);
    }
    assert_eq!(std::fs::read_to_string(made.path()).unwrap(), before);
    seam.shutdown().await;
}

#[tokio::test]
async fn weight_thickens_icons_where_the_set_has_weights() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Weights",
            r##"TakeScreenshot([
  Insert({ type: "icon", icon: "lucide:minus", width: 48, height: 48, fill: "#EF4444" }),
  Insert({ type: "icon", icon: "lucide:minus", width: 48, height: 48, fill: "#EF4444", weight: 800 }),
  Insert({ type: "icon", icon: "ph:circle", width: 256, height: 256, fill: "#EF4444" }),
  Insert({ type: "icon", icon: "ph:circle", width: 256, height: 256, fill: "#EF4444", weight: 700 }),
]);"##,
        )
        .await;
    assert_eq!(made.images.len(), 4, "{}", made.text);

    // Lucide draws with strokes: a 2-unit line, 4 px at 48, twice that at 800.
    let (regular, heavy) = (&made.images[0], &made.images[1]);
    assert_eq!((regular.rgba(24, 24), heavy.rgba(24, 24)), (RED, RED));
    assert_eq!(regular.rgba(24, 21)[3], 0);
    assert_eq!(heavy.rgba(24, 21), RED);

    // Phosphor's weights are icons of their own: bold is `ph:circle-bold`.
    let (regular, bold) = (&made.images[2], &made.images[3]);
    assert!(seam.iconify.fetched.lock().unwrap().contains(&"ph:circle-bold".to_owned()));
    assert_eq!((regular.rgba(128, 30), bold.rgba(128, 30)), (RED, RED));
    assert_eq!(regular.rgba(128, 22)[3], 0);
    assert_eq!(bold.rgba(128, 22), RED);
    assert!(made.json().get("warnings").is_none(), "{}", made.text);

    // A set without weights draws the icon and says the weight does nothing.
    let unweighted = seam
        .execute(
            &made.path(),
            r#"Insert({ type: "icon", name: "Box", icon: "mdi:square", width: 24, height: 24, weight: 700 });"#,
        )
        .await;
    let warnings = unweighted.json()["warnings"].to_string();
    assert!(warnings.contains("Box") && warnings.contains("no weights"), "{}", unweighted.text);
    seam.shutdown().await;
}
