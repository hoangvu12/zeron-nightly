//! Auto-layout (#20): Frames lay their children out in a row or column,
//! and agents read the laid-out bounds and clipping problems back with
//! `Get`'s visitor, as they would to check a Screen.

use std::collections::HashMap;

use serde_json::Value;

use crate::seam::{Called, Seam};

/// Visits the whole Canvas, printing each node as `Get` gives it.
const VISIT_ALL: &str = r#"
Get((node, info) => { Print(JSON.stringify({ name: node.name, ...info })); });"#;

/// One node as the visitor saw it.
#[derive(Debug)]
struct Seen {
    bounds: [f64; 4],
    depth: u64,
    index: u64,
    parent: Option<String>,
    problems: Vec<String>,
}

/// Runs `snippet` on a new Design, then visits it: each node by name.
async fn laid_out(seam: &Seam, snippet: &str) -> HashMap<String, Seen> {
    let called = seam.create("Layout", &format!("{snippet}\n{VISIT_ALL}")).await;
    seen(&called)
}

fn seen(called: &Called) -> HashMap<String, Seen> {
    called
        .printed()
        .iter()
        .map(|line| {
            let seen: Value = serde_json::from_str(line).unwrap_or_else(|_| panic!("{line}"));
            let b = &seen["bounds"];
            let n = |v: &Value| v.as_f64().unwrap_or_else(|| panic!("{line}"));
            (
                seen["name"].as_str().unwrap().to_owned(),
                Seen {
                    bounds: [n(&b["x"]), n(&b["y"]), n(&b["width"]), n(&b["height"])],
                    depth: seen["depth"].as_u64().unwrap(),
                    index: seen["index"].as_u64().unwrap(),
                    parent: seen["parent"].as_str().map(str::to_owned),
                    problems: serde_json::from_value(seen["problems"].clone()).unwrap(),
                },
            )
        })
        .collect()
}

impl Seen {
    /// Its bounds from `parent`'s top-left corner.
    fn within(&self, parent: &Seen) -> [f64; 4] {
        let [x, y, w, h] = self.bounds;
        [x - parent.bounds[0], y - parent.bounds[1], w, h]
    }
}

#[tokio::test]
async fn a_row_justifies_and_aligns_its_children_past_their_x_and_y() {
    let seam = Seam::new().await;
    // 300 × 100 rows with 20 px padding left and right and 10 top and
    // bottom: three 40 × 20 children and two 10 px gaps leave 120 px free.
    let nodes = laid_out(
        &seam,
        r##"
const rows = [["start", "start"], ["center", "center"], ["end", "end"], ["space_between", "start"], ["space_around", "start"]];
rows.forEach(([justify, align], i) => {
  const row = Insert({ type: "frame", name: justify, x: 0, y: i * 200, width: 300, height: 100,
    layout: "horizontal", gap: 10, padding: [10, 20], justifyContent: justify, alignItems: align });
  for (const n of [1, 2, 3])
    Insert(row, { type: "rectangle", name: `${justify} ${n}`, x: 999, y: 999, width: 40, height: 20, fill: "#000" });
});"##,
    )
    .await;
    let xs = |justify: &str| -> Vec<[f64; 2]> {
        (1..=3)
            .map(|n| {
                let [x, y, ..] = nodes[&format!("{justify} {n}")].within(&nodes[justify]);
                [x, y]
            })
            .collect()
    };
    assert_eq!(xs("start"), [[20., 10.], [70., 10.], [120., 10.]]);
    assert_eq!(xs("center"), [[80., 40.], [130., 40.], [180., 40.]]);
    assert_eq!(xs("end"), [[140., 70.], [190., 70.], [240., 70.]]);
    assert_eq!(xs("space_between"), [[20., 10.], [130., 10.], [240., 10.]]);
    assert_eq!(xs("space_around"), [[40., 10.], [130., 10.], [220., 10.]]);
    seam.shutdown().await;
}

#[tokio::test]
async fn a_column_takes_four_paddings_and_fill_container_divides_the_space_left() {
    let seam = Seam::new().await;
    let nodes = laid_out(
        &seam,
        r##"
const column = Insert({ type: "frame", name: "Column", x: 50, y: 60, width: 200, height: 400,
  layout: "vertical", gap: 20, padding: [10, 20, 30, 40] });
Insert(column, { type: "rectangle", name: "Fixed", width: 100, height: 100 });
Insert(column, { type: "rectangle", name: "Fill 1", width: "fill_container", height: "fill_container" });
Insert(column, { type: "rectangle", name: "Fill 2", width: 30, height: "fill_container" });"##,
    )
    .await;
    assert_eq!(nodes["Column"].bounds, [50., 60., 200., 400.]);
    // 400 - 10 - 30 padding, - 100 fixed, - 2 gaps of 20: 220 for two fills.
    let column = &nodes["Column"];
    assert_eq!(nodes["Fixed"].within(column), [40., 10., 100., 100.]);
    assert_eq!(nodes["Fill 1"].within(column), [40., 130., 140., 110.]);
    assert_eq!(nodes["Fill 2"].within(column), [40., 260., 30., 110.]);
    assert!(column.problems.is_empty(), "{:?}", column.problems);
    seam.shutdown().await;
}

#[tokio::test]
async fn fit_content_sizes_a_frame_to_its_children() {
    let seam = Seam::new().await;
    let nodes = laid_out(
        &seam,
        r##"
const card = Insert({ type: "frame", name: "Card", layout: "vertical", gap: 6, padding: 8 });
const row = Insert(card, { type: "frame", name: "Row", layout: "horizontal", gap: 4,
  width: "fit_content", height: "fit_content" });
Insert(row, { type: "rectangle", name: "A", width: 30, height: 10 });
Insert(row, { type: "rectangle", name: "B", width: 50, height: 20 });
Insert(card, { type: "rectangle", name: "Rule", width: "fill_container", height: 2 });
const loose = Insert({ type: "frame", name: "Loose", x: 500 });
Insert(loose, { type: "rectangle", name: "Far", x: 70, y: 30, width: 10, height: 10 });"##,
    )
    .await;
    // A frame's size is fit content unless given: the row is 30 + 4 + 50
    // wide, and the card 8 + that + 8, with the rule filling its width.
    assert_eq!(nodes["Row"].within(&nodes["Card"]), [8., 8., 84., 20.]);
    assert_eq!(nodes["Card"].bounds, [0., 0., 100., 8. + 20. + 6. + 2. + 8.]);
    assert_eq!(nodes["Rule"].within(&nodes["Card"]), [8., 34., 84., 2.]);
    // Without a layout, fit content reaches its farthest child.
    assert_eq!(nodes["Loose"].bounds, [500., 0., 80., 40.]);
    seam.shutdown().await;
}

#[tokio::test]
async fn a_fit_content_frame_of_only_fill_children_keeps_a_size_and_is_a_problem() {
    let seam = Seam::new().await;
    let nodes = laid_out(
        &seam,
        r##"
const menu = Insert({ type: "frame", name: "Menu", layout: "vertical", gap: 4, padding: 10 });
Insert(menu, { type: "rectangle", name: "Item 1", width: "fill_container", height: 20 });
Insert(menu, { type: "rectangle", name: "Item 2", width: "fill_container", height: 20 });"##,
    )
    .await;
    let menu = &nodes["Menu"];
    let [_, _, width, height] = menu.bounds;
    assert!(width > 20., "{menu:?}");
    assert_eq!(height, 10. + 20. + 4. + 20. + 10.);
    assert_eq!(nodes["Item 1"].within(menu), [10., 10., width - 20., 20.]);
    assert!(
        menu.problems.iter().any(|p| p.contains("fill")),
        "{:?}",
        menu.problems
    );
    seam.shutdown().await;
}

#[tokio::test]
async fn absolute_children_sit_at_their_x_and_y_outside_the_flow() {
    let seam = Seam::new().await;
    let nodes = laid_out(
        &seam,
        r##"
const bar = Insert({ type: "frame", name: "Bar", x: 10, y: 10, width: 200, height: 50, layout: "horizontal", padding: 5 });
Insert(bar, { type: "rectangle", name: "Badge", absolute: true, x: 180, y: -6, width: 12, height: 12 });
Insert(bar, { type: "rectangle", name: "Icon", width: 40, height: 40 });"##,
    )
    .await;
    let bar = &nodes["Bar"];
    assert_eq!(nodes["Badge"].within(bar), [180., -6., 12., 12.]);
    assert_eq!(nodes["Icon"].within(bar), [5., 5., 40., 40.]);
    seam.shutdown().await;
}

#[tokio::test]
async fn the_visitor_sees_clipped_overflowing_and_centred_children() {
    let seam = Seam::new().await;
    let nodes = laid_out(
        &seam,
        r##"
const clip = Insert({ type: "frame", name: "Clip", width: 100, height: 100, clip: true });
Insert(clip, { type: "rectangle", name: "Inside", x: 10, y: 10, width: 50, height: 50 });
Insert(clip, { type: "rectangle", name: "Edge", x: 80, y: 0, width: 50, height: 50 });
Insert(clip, { type: "rectangle", name: "Gone", x: 200, y: 0, width: 50, height: 50 });
const open = Insert(clip, { type: "frame", name: "Open", x: 50, y: 50, width: 20, height: 20 });
Insert(open, { type: "rectangle", name: "Deep", x: 40, y: 0, width: 20, height: 20 });

const row = Insert({ type: "frame", name: "Row", x: 200, width: 100, height: 40, clip: true,
  layout: "horizontal", gap: 10 });
for (const n of [1, 2, 3]) Insert(row, { type: "rectangle", name: `Cell ${n}`, width: 50, height: 40 });

const loose = Insert({ type: "frame", name: "Loose", x: 400, width: 100, height: 40,
  layout: "horizontal", gap: 10 });
for (const n of [1, 2, 3]) Insert(loose, { type: "rectangle", name: `Spill ${n}`, width: 50, height: 40 });

const centre = Insert({ type: "frame", name: "Centre", x: 600, width: 100, height: 100, clip: true,
  layout: "horizontal", justifyContent: "center", alignItems: "center" });
Insert(centre, { type: "rectangle", name: "Wide", width: 140, height: 40 });
const fits = Insert({ type: "frame", name: "Fits", x: 800, width: 100, height: 100, clip: true,
  layout: "vertical", justifyContent: "center", alignItems: "center" });
Insert(fits, { type: "rectangle", name: "Small", width: 60, height: 40 });"##,
    )
    .await;
    let problems = |name: &str| nodes[name].problems.clone();
    let none: Vec<String> = Vec::new();
    assert_eq!(problems("Inside"), none);
    assert_eq!(problems("Edge"), ["partially clipped"]);
    assert_eq!(problems("Gone"), ["fully clipped"]);
    assert_eq!(problems("Open"), none);
    // Its own parent doesn't clip, but the frame above it does.
    assert_eq!(problems("Deep"), ["partially clipped"]);

    // A row too narrow for its cells: the overflow is cut off where it clips.
    assert_eq!(problems("Cell 1"), none);
    assert_eq!(problems("Cell 2"), ["partially clipped"]);
    assert_eq!(problems("Cell 3"), ["fully clipped"]);
    assert_eq!(nodes["Cell 3"].within(&nodes["Row"]), [120., 0., 50., 40.]);
    // And shows past a frame that doesn't clip.
    assert_eq!(problems("Spill 3"), none);

    // Centred, a child wider than its frame overflows both sides evenly.
    assert_eq!(nodes["Wide"].within(&nodes["Centre"]), [-20., 30., 140., 40.]);
    assert_eq!(problems("Wide"), ["partially clipped"]);
    assert_eq!(nodes["Small"].within(&nodes["Fits"]), [20., 30., 60., 40.]);
    assert_eq!(problems("Small"), none);
    seam.shutdown().await;
}

#[tokio::test]
async fn screenshots_draw_children_where_the_layout_puts_them() {
    const RED: [u8; 4] = [239, 68, 68, 255];
    const BLUE: [u8; 4] = [59, 130, 246, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    let seam = Seam::new().await;
    let shot = seam
        .create(
            "Painted",
            r##"
const bar = Insert({ type: "frame", width: 200, height: 50, fill: "#FFFFFF", layout: "horizontal", gap: 20, padding: 10 });
Insert(bar, { type: "rectangle", x: 150, width: "fill_container", height: "fill_container", fill: "#EF4444" });
Insert(bar, { type: "rectangle", width: "fill_container", height: 10, fill: "#3B82F6" });
TakeScreenshot(bar);"##,
        )
        .await;
    let image = &shot.images[0];
    assert_eq!((image.width, image.height), (200, 50));
    // Two fills of 80 between 10 px padding and a 20 px gap.
    assert_eq!(image.rgba(5, 25), WHITE);
    assert_eq!(image.rgba(12, 12), RED);
    assert_eq!(image.rgba(88, 38), RED);
    assert_eq!(image.rgba(100, 25), WHITE);
    assert_eq!(image.rgba(112, 12), BLUE);
    assert_eq!(image.rgba(188, 18), BLUE);
    assert_eq!(image.rgba(150, 30), WHITE);
    seam.shutdown().await;
}

#[tokio::test]
async fn the_visitor_gives_depth_index_and_parent_and_can_skip_a_subtree() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Tree",
            r##"
const screen = Insert({ type: "frame", name: "Screen", width: 100, height: 100 });
const nav = Insert(screen, { type: "frame", name: "Nav", width: 100, height: 20 });
Insert(nav, { type: "rectangle", name: "Logo", width: 10, height: 10 });
Insert(screen, { type: "rectangle", name: "Body", width: 10, height: 10 });
Get(screen, (node, info) => { Print(JSON.stringify({ name: node.name, ...info })); return node.name !== "Nav"; });"##,
        )
        .await;
    let nodes = seen(&made);
    let mut names: Vec<_> = nodes.keys().cloned().collect();
    names.sort();
    assert_eq!(names, ["Body", "Nav", "Screen"], "Logo is skipped with Nav's subtree");
    let screen = made.id("Screen");
    assert_eq!((nodes["Screen"].depth, nodes["Screen"].parent.as_deref()), (0, None));
    assert_eq!((nodes["Nav"].depth, nodes["Nav"].index), (1, 0));
    assert_eq!((nodes["Body"].depth, nodes["Body"].index), (1, 1));
    assert_eq!(nodes["Body"].parent, Some(screen));
    seam.shutdown().await;
}

#[tokio::test]
async fn edge_cases_overflow_around_the_middle_and_clips_that_dont_meet_hide_all() {
    let seam = Seam::new().await;
    let nodes = laid_out(
        &seam,
        r##"
const around = Insert({ type: "frame", name: "Around", width: 100, height: 20, layout: "horizontal", justifyContent: "space_around" });
Insert(around, { type: "rectangle", name: "Big", width: 160, height: 20 });

const outer = Insert({ type: "frame", name: "Outer", x: 300, width: 100, height: 100, clip: true });
const inner = Insert(outer, { type: "frame", name: "Inner", x: 150, width: 100, height: 100, clip: true });
Insert(inner, { type: "rectangle", name: "Hidden", x: -100, width: 120, height: 20 });

const loose = Insert({ type: "frame", name: "Plain", x: 600, width: 100, height: 80 });
Insert(loose, { type: "rectangle", name: "Rest", x: 30, y: 20, width: "fill_container", height: "fill_container" });"##,
    )
    .await;
    // Too wide to space out: centred, as CSS does.
    assert_eq!(nodes["Big"].within(&nodes["Around"]), [-30., 0., 160., 20.]);
    // Inside two clips that don't overlap, nothing shows.
    assert_eq!(nodes["Hidden"].problems, ["fully clipped"]);
    // Without a layout, fill container takes the rest from its x and y.
    assert_eq!(nodes["Rest"].within(&nodes["Plain"]), [30., 20., 70., 60.]);
    seam.shutdown().await;
}
