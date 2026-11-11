//! Fills, strokes and effects (#22): paints and their blend modes, strokes,
//! corner radii, shadows and blurs, rotation, flip, opacity and clipping,
//! seen in screenshots, and warnings for values the Design doesn't support.

use serde_json::Value;

use crate::seam::{Called, Image, Seam};

const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const INDIGO: [u8; 4] = [79, 70, 229, 255];

/// Runs `snippet` in a new Design; it puts a white Screen in `screen`,
/// which is then screenshot. Returns the call and its screenshot.
async fn shot(seam: &Seam, size: (u32, u32), snippet: &str) -> (Called, Image) {
    let code = format!(
        r##"const screen = Insert({{ type: "frame", name: "Screen", width: {}, height: {}, fill: "#FFFFFF" }});
const at = (props) => Insert(screen, props);
{snippet}
TakeScreenshot(screen);"##,
        size.0, size.1
    );
    let mut called = seam.create("Looks", &code).await;
    assert_eq!(called.images.len(), 1, "{}", called.text);
    let image = called.images.remove(0);
    assert_eq!((image.width, image.height), size);
    (called, image)
}

/// Asserts the pixel at (x, y) is within `tolerance` of `expected` in
/// each channel.
#[track_caller]
fn near(image: &Image, (x, y): (u32, u32), expected: [u8; 4], tolerance: u8) {
    let actual = image.rgba(x, y);
    let close = actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= tolerance);
    assert!(close, "at ({x}, {y}): {actual:?}, expected {expected:?} ± {tolerance}");
}

/// A grey's level (the red channel) at (x, y).
fn level(image: &Image, x: u32, y: u32) -> u8 {
    image.rgba(x, y)[0]
}

#[tokio::test]
async fn several_fills_stack_in_order_with_their_blend_modes() {
    let seam = Seam::new().await;
    let (_, image) = shot(
        &seam,
        (300, 100),
        r##"
at({ type: "rectangle", x: 0, width: 100, height: 100, fill: ["#FF0000", "#0000FF80"] });
at({ type: "rectangle", x: 100, width: 100, height: 100,
     fill: ["#FF0000", { type: "color", color: "#00FF00", blendMode: "screen" }] });
at({ type: "rectangle", x: 200, width: 100, height: 100,
     fill: ["#FFFFFF", { type: "color", color: "#00FF00", blendMode: "multiply" },
            { type: "color", color: "#FF0000", blendMode: "multiply", opacity: 0.5 },
            { type: "color", color: "#0000FF", enabled: false }] });
"##,
    )
    .await;
    // Half-transparent blue over red; green screened over red is yellow;
    // red multiplied over green at half strength darkens it by half, and
    // the disabled blue isn't drawn.
    near(&image, (50, 50), [127, 0, 128, 255], 2);
    near(&image, (150, 50), [255, 255, 0, 255], 1);
    near(&image, (250, 50), [0, 128, 0, 255], 2);
    seam.shutdown().await;
}

#[tokio::test]
async fn linear_radial_and_angular_gradients_lie_over_the_node() {
    let seam = Seam::new().await;
    let (_, image) = shot(
        &seam,
        (400, 200),
        r##"
const bw = ["#000000", "#FFFFFF"];
const gradient = (props) => ({ type: "gradient", colors: bw, ...props });
at({ type: "rectangle", x: 0, width: 100, height: 100, fill: gradient({}) });
at({ type: "rectangle", x: 100, width: 100, height: 100, fill: gradient({ rotation: 90 }) });
at({ type: "rectangle", x: 200, width: 100, height: 100, fill: gradient({ gradientType: "radial" }) });
at({ type: "rectangle", x: 300, width: 100, height: 100, fill: gradient({ gradientType: "angular" }) });
at({ type: "rectangle", x: 0, y: 100, width: 100, height: 100, fill: gradient({ rotation: 180, colors: [
  { color: "#FF0000", position: 0 }, { color: "#FF0000", position: 0.5 },
  { color: "#0000FF", position: 0.5 }, { color: "#0000FF", position: 1 }] }) });
at({ type: "rectangle", x: 100, y: 100, width: 200, height: 100,
     fill: gradient({ gradientType: "radial", center: { x: 0.25, y: 0.5 }, size: { width: 0.5, height: 1 } }) });
"##,
    )
    .await;
    // Linear, upwards by default: black at the bottom, white at the top.
    assert!(level(&image, 50, 97) < 15 && level(&image, 50, 2) > 240);
    assert!(level(&image, 50, 50).abs_diff(128) < 10, "{}", level(&image, 50, 50));
    // Turned 90° counter-clockwise, it runs leftwards.
    assert!(level(&image, 197, 50) < 15 && level(&image, 102, 50) > 240);
    // Radial: black at the center, white at its edge and past it.
    assert!(level(&image, 250, 50) < 10);
    assert!(level(&image, 275, 50).abs_diff(128) < 12, "{}", level(&image, 275, 50));
    near(&image, (201, 1), WHITE, 2);
    // Angular: from the top, clockwise.
    assert!(level(&image, 395, 50).abs_diff(64) < 12, "{}", level(&image, 395, 50));
    assert!(level(&image, 350, 95).abs_diff(128) < 12, "{}", level(&image, 350, 95));
    assert!(level(&image, 305, 50).abs_diff(191) < 12, "{}", level(&image, 305, 50));
    // Stops at their positions, turned to run downwards: red over blue.
    near(&image, (50, 120), RED, 1);
    near(&image, (50, 180), BLUE, 1);
    // A radial gradient with its own center and size: black at a quarter
    // across, white well before the middle.
    assert!(level(&image, 150, 150) < 10);
    near(&image, (205, 150), WHITE, 2);
    seam.shutdown().await;
}

#[tokio::test]
async fn strokes_align_inside_centered_or_outside_with_a_width_per_side() {
    let seam = Seam::new().await;
    let (_, image) = shot(
        &seam,
        (400, 200),
        r##"
const line = (props) => ({ fill: "#000000", thickness: 10, ...props });
at({ type: "rectangle", x: 20, y: 40, width: 60, height: 60, stroke: line({ align: "inside" }) });
at({ type: "rectangle", x: 120, y: 40, width: 60, height: 60, stroke: line({ align: "center" }) });
at({ type: "rectangle", x: 220, y: 40, width: 60, height: 60, stroke: line({ align: "outside" }) });
at({ type: "rectangle", x: 320, y: 40, width: 60, height: 60, fill: "#4F46E5",
     stroke: line({ thickness: { bottom: 6 } }) });
"##,
    )
    .await;
    near(&image, (25, 70), BLACK, 1);
    near(&image, (15, 70), WHITE, 1);
    near(&image, (35, 70), WHITE, 1);
    near(&image, (117, 70), BLACK, 1);
    near(&image, (123, 70), BLACK, 1);
    near(&image, (112, 70), WHITE, 1);
    near(&image, (127, 70), WHITE, 1);
    near(&image, (215, 70), BLACK, 1);
    near(&image, (207, 70), WHITE, 1);
    near(&image, (222, 70), WHITE, 1);
    // Only the bottom side, inside.
    near(&image, (350, 97), BLACK, 1);
    near(&image, (350, 90), INDIGO, 1);
    near(&image, (322, 70), INDIGO, 1);
    near(&image, (350, 42), INDIGO, 1);
    seam.shutdown().await;
}

#[tokio::test]
async fn stroke_joins_caps_and_dashes_and_corner_radii_draw_as_given() {
    let seam = Seam::new().await;
    let (_, image) = shot(
        &seam,
        (400, 200),
        r##"
const line = (props) => ({ fill: "#000000", thickness: 10, align: "center", ...props });
at({ type: "rectangle", x: 20, y: 40, width: 60, height: 60, stroke: line({ join: "miter" }) });
at({ type: "rectangle", x: 120, y: 40, width: 60, height: 60, stroke: line({ join: "round" }) });
at({ type: "rectangle", x: 220, y: 40, width: 60, height: 60, stroke: line({ join: "bevel" }) });
at({ type: "rectangle", x: 20, y: 140, width: 60, height: 40, stroke: line({ thickness: 4, dashPattern: [6, 6] }) });
at({ type: "rectangle", x: 120, y: 140, width: 60, height: 40,
     stroke: line({ thickness: 4, dashPattern: [6, 6], cap: "square" }) });
at({ type: "rectangle", x: 220, y: 120, width: 60, height: 60, fill: "#000000", cornerRadius: [0, 30, 0, 0] });
at({ type: "rectangle", x: 320, y: 120, width: 60, height: 60, fill: "#000000", cornerRadius: 30 });
"##,
    )
    .await;
    // At the outer corner: a miter fills it, a round join and a bevel cut it.
    near(&image, (16, 36), BLACK, 1);
    near(&image, (115, 35), WHITE, 1);
    near(&image, (120, 36), BLACK, 1);
    near(&image, (215, 35), WHITE, 1);
    near(&image, (220, 36), BLACK, 1);
    // Dashes along the top edge: half of it with butt ends, more with
    // square caps reaching past each dash's end.
    let dark = |x0: u32| (x0 + 4..x0 + 56).filter(|&x| level(&image, x, 140) < 128).count();
    assert!((20..=32).contains(&dark(20)), "{} dark of 52", dark(20));
    assert!(dark(120) >= 40, "{} dark of 52", dark(120));
    // A radius on the top right corner only; on all four.
    near(&image, (221, 121), BLACK, 1);
    near(&image, (278, 122), WHITE, 1);
    near(&image, (278, 178), BLACK, 1);
    near(&image, (322, 122), WHITE, 1);
    near(&image, (378, 178), WHITE, 1);
    near(&image, (350, 150), BLACK, 1);
    seam.shutdown().await;
}

#[tokio::test]
async fn shadows_and_blurs_render_and_background_blur_softens_whats_behind() {
    let seam = Seam::new().await;
    let (_, image) = shot(
        &seam,
        (400, 300),
        r##"
const shadow = (props) => ({ type: "shadow", color: "#000000", offset: { x: 0, y: 0 }, blur: 0, ...props });
at({ type: "rectangle", x: 40, y: 40, width: 60, height: 60, fill: "#4F46E5",
     effect: shadow({ offset: { x: 10, y: 10 } }) });
at({ type: "rectangle", x: 140, y: 40, width: 60, height: 60, fill: "#FFFFFF", effect: shadow({ blur: 20 }) });
at({ type: "rectangle", x: 240, y: 40, width: 60, height: 60, fill: "#FFFFFF",
     effect: shadow({ shadowType: "inner", spread: 10 }) });
at({ type: "rectangle", x: 40, y: 160, width: 60, height: 60, fill: "#000000", effect: { type: "blur", radius: 10 } });
for (let i = 0; i < 20; i++) at({ type: "rectangle", x: 200 + 8 * i, y: 140, width: 4, height: 120, fill: "#000000" });
at({ type: "rectangle", name: "Glass", x: 240, y: 160, width: 80, height: 80,
     effect: { type: "background_blur", radius: 12 } });
"##,
    )
    .await;
    // A hard drop shadow down and to the right, behind the node.
    near(&image, (105, 105), BLACK, 1);
    near(&image, (70, 70), INDIGO, 1);
    near(&image, (35, 35), WHITE, 1);
    near(&image, (105, 45), WHITE, 1);
    // A soft one fading out around the node.
    let soft = level(&image, 205, 70);
    assert!((20..235).contains(&soft), "{soft}");
    assert!(level(&image, 215, 70) > soft);
    near(&image, (170, 70), WHITE, 1);
    // An inner shadow spread 10 px in.
    near(&image, (244, 70), BLACK, 1);
    near(&image, (270, 70), WHITE, 1);
    // A blurred node: soft at its edge, still dark within.
    let edge = level(&image, 40, 190);
    assert!((40..215).contains(&edge), "{edge}");
    assert!(level(&image, 70, 190) < 40);
    assert!(level(&image, 32, 190) > edge);
    // Stripes behind the glass blur to grey; beside it they stay sharp.
    near(&image, (202, 150), BLACK, 1);
    near(&image, (206, 150), WHITE, 1);
    for x in [270, 274, 278, 282] {
        let grey = level(&image, x, 200);
        assert!((60..200).contains(&grey), "at {x}: {grey}");
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn rotation_flip_and_opacity_take_children_along_and_clip_hides_overflow() {
    let seam = Seam::new().await;
    let (_, image) = shot(
        &seam,
        (300, 300),
        r##"
const turned = at({ type: "frame", x: 100, y: 100, width: 60, height: 20, fill: "#FF0000", rotation: 90 });
Insert(turned, { type: "rectangle", width: 20, height: 20, fill: "#0000FF" });
const flipped = at({ type: "frame", x: 20, y: 200, width: 80, height: 40, fill: "#00FF00", flipX: true });
Insert(flipped, { type: "rectangle", width: 20, height: 40, fill: "#FF0000" });
const faded = at({ type: "frame", x: 200, y: 20, width: 60, height: 60, fill: "#000000", opacity: 0.5 });
Insert(faded, { type: "rectangle", x: 30, width: 30, height: 30, fill: "#000000" });
const open = at({ type: "frame", x: 120, y: 140, width: 40, height: 40, fill: "#00FF00" });
Insert(open, { type: "rectangle", x: 30, y: 30, width: 40, height: 40, fill: "#FF0000" });
const clipped = at({ type: "frame", x: 200, y: 200, width: 60, height: 60, cornerRadius: 20, clip: true });
Insert(clipped, { type: "rectangle", x: -10, y: -10, width: 50, height: 50, fill: "#FF0000" });
Insert(clipped, { type: "rectangle", x: 40, y: 40, width: 60, height: 60, fill: "#0000FF" });
"##,
    )
    .await;
    // Turned 90° counter-clockwise about its top-left corner: it stands up
    // from (100, 100), its child at its foot.
    near(&image, (110, 90), BLUE, 1);
    near(&image, (110, 60), RED, 1);
    near(&image, (130, 60), WHITE, 1);
    near(&image, (140, 110), WHITE, 1);
    // Mirrored, the child sits at the right.
    near(&image, (95, 220), RED, 1);
    near(&image, (25, 220), GREEN, 1);
    // Half opacity for the frame and its child as one: the child over the
    // frame is no darker than the frame alone.
    near(&image, (210, 50), [127, 127, 127, 255], 2);
    near(&image, (250, 30), [127, 127, 127, 255], 2);
    // Without clipping a child shows past its frame; with it, it's cut at
    // the frame's rounded outline.
    near(&image, (180, 200), RED, 1);
    near(&image, (215, 215), RED, 1);
    near(&image, (250, 250), BLUE, 1);
    near(&image, (270, 270), WHITE, 1);
    near(&image, (201, 201), WHITE, 1);
    seam.shutdown().await;
}

#[tokio::test]
async fn looks_are_saved_in_the_files_shape() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Saved",
            r##"Insert({ type: "frame", name: "Card", width: 100, height: 100, rotation: 15, flipY: true,
  opacity: 0.8, cornerRadius: [4, 4, 0, 0],
  fill: [{ type: "gradient", gradientType: "radial", colors: ["#000000", "#FFFFFF"] },
         { type: "color", color: "#FF000080", blendMode: "overlay" }],
  stroke: { fill: "#000000", thickness: { top: 1, right: 2, bottom: 3, left: 4 }, align: "outside", join: "round", cap: "square", dashPattern: [2, 1] },
  effect: [{ type: "shadow", shadowType: "inner", offset: { x: 1, y: 2 }, blur: 3, spread: 4, color: "#00000040" },
           { type: "background_blur", radius: 8 }] });"##,
        )
        .await;
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    let card = &file["children"][0];
    assert_eq!(card["rotation"], 15.);
    assert_eq!(card["flipY"], true);
    assert!((card["opacity"].as_f64().unwrap() - 0.8).abs() < 1e-6);
    assert_eq!(card["cornerRadius"], serde_json::json!([4., 4., 0., 0.]));
    assert_eq!(card["fill"][0]["gradientType"], "radial");
    assert_eq!(card["fill"][0]["colors"][1]["position"], 1.);
    assert_eq!(card["fill"][1]["blendMode"], "overlay");
    assert_eq!(card["stroke"]["thickness"]["left"], 4.);
    assert_eq!((card["stroke"]["align"].as_str(), card["stroke"]["cap"].as_str()), (Some("outside"), Some("square")));
    assert_eq!(card["stroke"]["dashPattern"], serde_json::json!([2., 1.]));
    assert_eq!(card["effect"][0]["shadowType"], "inner");
    assert_eq!(card["effect"][0]["spread"], 4.);
    assert_eq!(card["effect"][1]["type"], "background_blur");

    // And read back as written: the next call keeps it.
    let again = seam.execute(&made.path(), r#"Insert({ type: "frame", width: 1, height: 1 });"#).await;
    assert!(!again.is_error, "{}", again.text);
    let reread: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!(&reread["children"][0], card);
    seam.shutdown().await;
}

#[tokio::test]
async fn unsupported_values_come_back_as_warnings_and_the_rest_draws() {
    let seam = Seam::new().await;
    let (called, image) = shot(
        &seam,
        (100, 100),
        r##"
at({ type: "rectangle", name: "Odd", width: 100, height: 100, opacity: 2,
     fill: [{ type: "color", color: "#FF0000", blendMode: "plasma" }, { type: "mesh_gradient" },
            { type: "gradient", gradientType: "diamond", colors: ["#000000", "#FFFFFF"] }],
     stroke: { fill: "#0000FF", thickness: 10, align: "middle" },
     effect: [{ type: "glow" }, { type: "blur", radius: -3 }] });
at({ type: "rectangle", name: "Odder", width: 1, height: 1, cornerRadius: -5,
     fill: { type: "mesh_gradient" }, effect: { type: "glow" } });
"##,
    )
    .await;
    let warnings: Vec<String> = called.json()["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .map(|w| w.as_str().unwrap().to_owned())
        .collect();
    for said in ["plasma", "mesh_gradient", "diamond", "middle", "glow", "radius", "opacity"] {
        assert!(
            warnings.iter().any(|w| w.contains(said) && w.contains("\"Odd\"")),
            "no warning about {said}: {warnings:?}"
        );
    }
    // Each said once, as unsupported values of properties it knows.
    assert_eq!(warnings.iter().filter(|w| w.contains("plasma")).count(), 1, "{warnings:?}");
    for said in ["corner radius", "mesh_gradient", "glow"] {
        assert!(warnings.iter().any(|w| w.contains(said) && w.contains("\"Odder\"")), "{warnings:?}");
    }
    assert!(!warnings.iter().any(|w| w.contains("doesn't know")), "{warnings:?}");
    // Red at full opacity, with the default (inside) blue stroke.
    near(&image, (50, 50), RED, 1);
    near(&image, (5, 50), BLUE, 1);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(called.path()).unwrap()).unwrap();
    let odd = &file["children"][0]["children"][0];
    assert_eq!(odd["fill"], "#FF0000");
    assert!(odd.get("opacity").is_none(), "{odd}");

    // A call with nothing unsupported has no warnings.
    let clean = seam
        .execute(&called.path(), r##"Insert({ type: "rectangle", width: 5, height: 5, fill: "#000" });"##)
        .await;
    assert!(clean.json().get("warnings").is_none(), "{}", clean.text);
    seam.shutdown().await;
}
