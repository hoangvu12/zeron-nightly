//! Ellipses (with arcs and rings), polygons and paths (#23), drawn at
//! several sizes and probed in their screenshots.

use serde_json::Value;

use crate::seam::{Called, Image, Seam};

const RED: [u8; 4] = [239, 68, 68, 255];

/// Inserts `nodes` on the Canvas (each a node object in JS), screenshots
/// each and returns the call with its images in the same order.
async fn drawn(seam: &Seam, nodes: &[&str]) -> Called {
    let snippet = format!(
        "TakeScreenshot([{}].map((node) => Insert(node)));",
        nodes.join(", ")
    );
    let called = seam.create("Shapes", &snippet).await;
    assert_eq!(called.images.len(), nodes.len(), "{}", called.text);
    called
}

fn clear(image: &Image, x: u32, y: u32) -> bool {
    image.rgba(x, y)[3] == 0
}

#[tokio::test]
async fn ellipses_fill_their_box_at_any_size() {
    let seam = Seam::new().await;
    let made = drawn(
        &seam,
        &[
            r##"{ type: "ellipse", width: 200, height: 100, fill: "#EF4444" }"##,
            r##"{ type: "ellipse", width: 40, height: 40, fill: "#EF4444" }"##,
        ],
    )
    .await;
    let wide = &made.images[0];
    assert_eq!((wide.width, wide.height), (200, 100));
    for (x, y) in [(100, 50), (5, 50), (195, 50), (100, 4), (100, 96)] {
        assert_eq!(wide.rgba(x, y), RED, "({x}, {y})");
    }
    for (x, y) in [(3, 3), (190, 10), (10, 90), (196, 97)] {
        assert!(clear(wide, x, y), "({x}, {y}): {:?}", wide.rgba(x, y));
    }

    let small = &made.images[1];
    assert_eq!((small.width, small.height), (40, 40));
    assert_eq!(small.rgba(20, 20), RED);
    assert_eq!(small.rgba(37, 20), RED);
    assert!(clear(small, 2, 2) && clear(small, 37, 37));
    seam.shutdown().await;
}

#[tokio::test]
async fn arcs_and_rings_cut_the_ellipse() {
    let seam = Seam::new().await;
    let made = drawn(
        &seam,
        &[
            // A quarter, from 3 o'clock clockwise to 6.
            r##"{ type: "ellipse", name: "Quarter", width: 200, height: 200, fill: "#EF4444", sweepAngle: 90 }"##,
            r##"{ type: "ellipse", name: "Ring", width: 200, height: 200, fill: "#EF4444", innerRadius: 0.5 }"##,
            // The bottom half of a ring, at another size.
            r##"{ type: "ellipse", width: 300, height: 150, fill: "#EF4444", startAngle: 0, sweepAngle: 180, innerRadius: 0.5 }"##,
        ],
    )
    .await;
    let quarter = &made.images[0];
    assert_eq!(quarter.rgba(150, 150), RED);
    assert_eq!(quarter.rgba(104, 104), RED);
    for (x, y) in [(50, 50), (150, 50), (50, 150)] {
        assert!(clear(quarter, x, y), "({x}, {y}): {:?}", quarter.rgba(x, y));
    }

    let ring = &made.images[1];
    for (x, y) in [(100, 20), (100, 180), (20, 100), (180, 100), (100, 4)] {
        assert_eq!(ring.rgba(x, y), RED, "({x}, {y})");
    }
    for (x, y) in [(100, 100), (100, 60), (60, 100), (3, 3)] {
        assert!(clear(ring, x, y), "({x}, {y}): {:?}", ring.rgba(x, y));
    }

    let half = &made.images[2];
    assert_eq!((half.width, half.height), (300, 150));
    assert_eq!(half.rgba(150, 131), RED);
    assert_eq!(half.rgba(262, 85), RED);
    for (x, y) in [(150, 18), (150, 100), (38, 65), (150, 75)] {
        assert!(clear(half, x, y), "({x}, {y}): {:?}", half.rgba(x, y));
    }

    // The angles and hole are kept in the file as written.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    let quarter = &file["children"][0];
    assert_eq!((quarter["type"].as_str(), quarter["sweepAngle"].as_f64()), (Some("ellipse"), Some(90.)));
    assert_eq!(file["children"][1]["innerRadius"].as_f64(), Some(0.5));
    assert!(file["children"][1].get("sweepAngle").is_none(), "a whole ellipse writes no sweep");
    seam.shutdown().await;
}

#[tokio::test]
async fn polygons_are_regular_and_stretched_to_their_box() {
    let seam = Seam::new().await;
    let made = drawn(
        &seam,
        &[
            r##"{ type: "polygon", width: 200, height: 100, fill: "#EF4444", polygonCount: 3 }"##,
            r##"{ type: "polygon", width: 120, height: 120, fill: "#EF4444", polygonCount: 6 }"##,
        ],
    )
    .await;
    // A triangle, its top corner at the top's middle and its base the box's.
    let triangle = &made.images[0];
    for (x, y) in [(100, 10), (100, 90), (10, 96), (189, 96)] {
        assert_eq!(triangle.rgba(x, y), RED, "({x}, {y})");
    }
    for (x, y) in [(10, 10), (190, 10), (40, 50), (160, 50)] {
        assert!(clear(triangle, x, y), "({x}, {y}): {:?}", triangle.rgba(x, y));
    }

    // A hexagon with corners at the top and bottom, its sides upright.
    let hexagon = &made.images[1];
    for (x, y) in [(60, 60), (60, 3), (60, 116), (3, 60), (116, 60)] {
        assert_eq!(hexagon.rgba(x, y), RED, "({x}, {y})");
    }
    for (x, y) in [(3, 3), (116, 3), (3, 116), (116, 116)] {
        assert!(clear(hexagon, x, y), "({x}, {y}): {:?}", hexagon.rgba(x, y));
    }

    let refused = seam
        .create("Line", r#"Insert({ type: "polygon", width: 10, height: 10, polygonCount: 2 });"#)
        .await;
    assert!(refused.is_error && refused.text.contains("polygonCount"), "{}", refused.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn paths_stretch_their_view_box_onto_their_box() {
    let seam = Seam::new().await;
    let made = drawn(
        &seam,
        &[
            // The top-left half of a 10 × 10 view box, on a 200 × 100 box.
            r##"{ type: "path", width: 200, height: 100, fill: "#EF4444", geometry: "M0 0 L10 0 L0 10 Z", viewBox: [0, 0, 10, 10] }"##,
            // The same, the view box away from the origin.
            r##"{ type: "path", width: 200, height: 100, fill: "#EF4444", geometry: "M10 10 L20 10 L10 20 Z", viewBox: [10, 10, 10, 10] }"##,
            // Without a view box, the geometry's own bounds are stretched.
            r##"{ type: "path", width: 200, height: 100, fill: "#EF4444", geometry: "M10 10 L20 10 L10 20 Z" }"##,
            // A view box larger than the geometry leaves its margin empty.
            r##"{ type: "path", width: 100, height: 100, fill: "#EF4444", geometry: "M5 5 H15 V15 H5 Z", viewBox: [0, 0, 20, 20] }"##,
        ],
    )
    .await;
    for triangle in &made.images[..3] {
        assert_eq!((triangle.width, triangle.height), (200, 100));
        for (x, y) in [(20, 10), (150, 20), (5, 90), (180, 5)] {
            assert_eq!(triangle.rgba(x, y), RED, "({x}, {y})");
        }
        for (x, y) in [(180, 90), (100, 60), (60, 85)] {
            assert!(clear(triangle, x, y), "({x}, {y}): {:?}", triangle.rgba(x, y));
        }
    }
    let framed = &made.images[3];
    assert_eq!(framed.rgba(50, 50), RED);
    assert_eq!(framed.rgba(27, 27), RED);
    assert!(clear(framed, 22, 50) && clear(framed, 50, 78) && clear(framed, 10, 10));
    seam.shutdown().await;
}

#[tokio::test]
async fn paths_honour_their_fill_rule() {
    // A square with a square inside it, both drawn the same way round.
    const SQUARES: &str = "M0 0 H10 V10 H0 Z M3 3 H7 V7 H3 Z";
    let seam = Seam::new().await;
    let made = drawn(
        &seam,
        &[
            &format!(r##"{{ type: "path", width: 100, height: 100, fill: "#EF4444", geometry: "{SQUARES}" }}"##),
            &format!(r##"{{ type: "path", width: 100, height: 100, fill: "#EF4444", geometry: "{SQUARES}", fillRule: "evenodd" }}"##),
        ],
    )
    .await;
    let (nonzero, evenodd) = (&made.images[0], &made.images[1]);
    assert_eq!(nonzero.rgba(50, 50), RED);
    assert!(clear(evenodd, 50, 50), "{:?}", evenodd.rgba(50, 50));
    assert_eq!((nonzero.rgba(15, 15), evenodd.rgba(15, 15)), (RED, RED));

    let file: Value = serde_json::from_str(&std::fs::read_to_string(made.path()).unwrap()).unwrap();
    assert_eq!(file["children"][1]["fillRule"], "evenodd");
    assert_eq!(file["children"][1]["geometry"], SQUARES);
    assert!(file["children"][0].get("fillRule").is_none());
    seam.shutdown().await;
}

#[tokio::test]
async fn shapes_take_strokes_and_effects() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Looks",
            r##"const board = Insert({ type: "frame", width: 240, height: 120 });
Insert(board, { type: "ellipse", width: 100, height: 100,
  stroke: { fill: "#000000", thickness: 10, align: "inside" } });
Insert(board, { type: "ellipse", x: 120, width: 100, height: 100, fill: "#EF4444",
  effect: { type: "shadow", color: "#000000", offset: { x: 10, y: 10 }, blur: 0 } });
TakeScreenshot(board);"##,
        )
        .await;
    let image = &made.images[0];
    // A ring of stroke inside the first ellipse's edge, nothing within it.
    assert_eq!(image.rgba(50, 4), [0, 0, 0, 255]);
    assert_eq!(image.rgba(4, 50), [0, 0, 0, 255]);
    assert!(clear(image, 50, 50) && clear(image, 50, 16) && clear(image, 3, 3));
    // The second ellipse, and its shadow following its shape, not its box.
    assert_eq!(image.rgba(170, 50), RED);
    assert_eq!(image.rgba(225, 60), [0, 0, 0, 255]);
    assert!(clear(image, 228, 18), "{:?}", image.rgba(228, 18));
    seam.shutdown().await;
}

#[tokio::test]
async fn a_path_that_isnt_path_data_is_refused() {
    let seam = Seam::new().await;
    for (node, says) in [
        (r#"{ type: "path", width: 10, height: 10, geometry: "a squiggle" }"#, "a squiggle"),
        (r#"{ type: "path", width: 10, height: 10 }"#, "path data"),
        (r#"{ type: "path", width: 10, height: 10, geometry: "M0 0 L1 1", viewBox: [0, 0, 0, 5] }"#, "viewBox"),
    ] {
        let refused = seam.create("Squiggle", &format!("Insert({node});")).await;
        assert!(refused.is_error && refused.text.contains(says), "{node}: {}", refused.text);
    }
    assert!(!seam.designs_folder().join("Squiggle").exists());
    seam.shutdown().await;
}
