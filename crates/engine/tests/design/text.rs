//! Text (#21): text nodes set in Google Fonts (a fixture folder here, see
//! [`crate::seam::FONTS`]), measured by the layout pass as agents read it
//! back with `Get`, and drawn in screenshots.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use zeron_design::fonts::{FontFolder, FontSource, Fonts};

use crate::seam::{Called, FONTS, Image, Seam};

/// Visits the whole Canvas, printing each node as `Get` gives it.
const VISIT_ALL: &str = r#"
Get((node, info) => { Print(JSON.stringify({ name: node.name, ...info })); });"#;

const WORDS: &str = "The quick brown fox jumps over the lazy dog";

/// One node as the visitor saw it.
#[derive(Debug)]
struct Seen {
    bounds: [f64; 4],
    problems: Vec<String>,
}

impl Seen {
    fn width(&self) -> f64 {
        self.bounds[2]
    }

    fn height(&self) -> f64 {
        self.bounds[3]
    }
}

/// Each node the call's visit printed, by name.
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
                    problems: serde_json::from_value(seen["problems"].clone()).unwrap(),
                },
            )
        })
        .collect()
}

/// Runs `snippet` on a new Design, then visits it.
async fn laid_out(seam: &Seam, snippet: &str) -> (Called, HashMap<String, Seen>) {
    let called = seam.create("Text", &format!("{snippet}\n{VISIT_ALL}")).await;
    let nodes = seen(&called);
    (called, nodes)
}

fn near(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 0.5
}

fn warnings(called: &Called) -> Vec<String> {
    match called.json().get("warnings") {
        Some(warnings) => serde_json::from_value(warnings.clone()).unwrap(),
        None => Vec::new(),
    }
}

#[tokio::test]
async fn auto_text_is_one_line_fixed_width_wraps_and_a_fixed_box_overflows() {
    let seam = Seam::new().await;
    let (_, nodes) = laid_out(
        &seam,
        &format!(
            r##"
const text = {{ type: "text", content: "{WORDS}", fontSize: 20, lineHeight: 1.5, fill: "#000000" }};
const screen = Insert({{ type: "frame", name: "Screen", width: 800, height: 400, fill: "#FFFFFF" }});
Insert(screen, {{ ...text, name: "Auto" }});
Insert(screen, {{ ...text, name: "Wide", y: 50, width: 120 }});
Insert(screen, {{ ...text, name: "Set wide", y: 50, x: 200, width: 120, textGrowth: "fixed-width" }});
Insert(screen, {{ ...text, name: "Box", y: 50, x: 400, width: 120, height: 30 }});
Insert(screen, {{ ...text, name: "Set auto", y: 300, width: 120, textGrowth: "auto" }});"##
        ),
    )
    .await;

    // One 30 px line (20 px at 1.5), as wide as its words.
    let auto = &nodes["Auto"];
    assert!(near(auto.height(), 30.), "{auto:?}");
    assert!(auto.width() > 300. && auto.width() < 600., "{auto:?}");
    assert_eq!(nodes["Set auto"].bounds[2..], auto.bounds[2..]);

    // Wrapped at 120 px, growing a line at a time.
    let wide = &nodes["Wide"];
    assert_eq!(wide.width(), 120.);
    let lines = wide.height() / 30.;
    assert!(lines >= 3. && near(lines, lines.round()), "{wide:?}");
    assert_eq!(nodes["Set wide"].bounds[2..], wide.bounds[2..]);

    // Wrapped in a fixed box its lines overflow, which the agent hears of.
    let fixed = &nodes["Box"];
    assert_eq!(fixed.bounds, [400., 50., 120., 30.]);
    assert_eq!(fixed.problems, ["text taller than its height, overflowing it"]);
    for name in ["Auto", "Wide", "Set wide", "Set auto"] {
        assert!(nodes[name].problems.is_empty(), "{name}: {:?}", nodes[name].problems);
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn a_fill_container_paragraph_wraps_to_its_frames_width_and_the_frame_fits_it() {
    let seam = Seam::new().await;
    let (_, nodes) = laid_out(
        &seam,
        &format!(
            r##"
const card = Insert({{ type: "frame", name: "Card", width: 240, layout: "vertical", padding: 20, gap: 10, fill: "#FFFFFF" }});
Insert(card, {{ type: "text", name: "Title", content: "Pricing", fontSize: 24, lineHeight: 1.25, fill: "#000000" }});
Insert(card, {{ type: "text", name: "Body", width: "fill_container", content: "{WORDS}. {WORDS}.", fontSize: 16, lineHeight: 1.5, fill: "#000000" }});

// A fill-container card in a fixed column: the width reaches the text
// through it, and the card's height fits the wrapped text.
const column = Insert({{ type: "frame", name: "Column", x: 400, width: 300, layout: "vertical" }});
const inner = Insert(column, {{ type: "frame", name: "Inner", width: "fill_container", layout: "vertical" }});
Insert(inner, {{ type: "text", name: "Nested", width: "fill_container", content: "{WORDS}. {WORDS}.", fontSize: 16, lineHeight: 1.5, fill: "#000000" }});

// Two fill-container paragraphs in a 400 px row share it.
const row = Insert({{ type: "frame", name: "Row", y: 400, width: 400, layout: "horizontal", gap: 20 }});
Insert(row, {{ type: "text", name: "Left", width: "fill_container", content: "{WORDS}", fontSize: 16, lineHeight: 1.5, fill: "#000000" }});
Insert(row, {{ type: "text", name: "Right", width: "fill_container", content: "{WORDS}", fontSize: 16, lineHeight: 1.5, fill: "#000000" }});"##
        ),
    )
    .await;

    let (card, title, body) = (&nodes["Card"], &nodes["Title"], &nodes["Body"]);
    assert!(near(title.height(), 30.), "{title:?}");
    // The card's inside: 240 less 20 px each side.
    assert_eq!(body.width(), 200.);
    let lines = body.height() / 24.;
    assert!(lines >= 3. && near(lines, lines.round()), "{body:?}");
    assert!(near(body.bounds[1], 20. + title.height() + 10.), "{body:?}");
    assert!(near(card.height(), 20. + title.height() + 10. + body.height() + 20.), "{card:?}");
    assert!(card.problems.is_empty() && body.problems.is_empty(), "{card:?} {body:?}");

    let (inner, nested) = (&nodes["Inner"], &nodes["Nested"]);
    assert_eq!((inner.width(), nested.width()), (300., 300.));
    assert!(nested.height() >= 48. && near(inner.height(), nested.height()), "{inner:?} {nested:?}");
    assert!(near(nodes["Column"].height(), nested.height()));
    // Wrapped at 300 px, it takes no more lines than at 200.
    assert!(nested.height() <= body.height(), "{nested:?} {body:?}");

    let (left, right) = (&nodes["Left"], &nodes["Right"]);
    assert_eq!((left.width(), right.width()), (190., 190.));
    assert_eq!(right.bounds[0], 210.);
    assert!(left.height() >= 48. && near(nodes["Row"].height(), left.height()), "{left:?}");
    seam.shutdown().await;
}

#[tokio::test]
async fn text_without_a_fill_is_warned_invisible() {
    let seam = Seam::new().await;
    let made = seam
        .create(
            "Ghost",
            r##"
const screen = Insert({ type: "frame", name: "Screen", width: 200, height: 100,
  children: [{ type: "text", name: "Ghost", content: "Boo" }] });
Insert(screen, { type: "text", name: "Seen", content: "Hi", fill: "#000000" });"##,
        )
        .await;
    let warnings = warnings(&made);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("\"Ghost\" (text) has no fill") && warnings[0].contains("invisible"), "{warnings:?}");
    seam.shutdown().await;
}

/// The fixture folder, noting each family fetched.
struct Counting {
    folder: FontFolder,
    fetched: Arc<Mutex<Vec<String>>>,
}

impl FontSource for Counting {
    fn fetch(&self, family: &str, weight: u16) -> Result<Option<Vec<u8>>, String> {
        self.fetched.lock().unwrap().push(format!("{family} {weight}"));
        self.folder.fetch(family, weight)
    }
}

/// The seam's fonts cached in `cache`, fetched from the fixture through a
/// count of what's fetched.
fn counted(seam: &Seam, cache: &PathBuf) -> Arc<Mutex<Vec<String>>> {
    let fetched = Arc::new(Mutex::new(Vec::new()));
    let source = Counting {
        folder: FontFolder::new(FONTS),
        fetched: fetched.clone(),
    };
    seam.core.designs.use_fonts(Fonts::new(Some(cache.clone()), Some(Box::new(source))));
    fetched
}

#[tokio::test]
async fn a_family_not_cached_is_fetched_once_cached_and_used() {
    let seam = Seam::new().await;
    let cache = seam.data_dir().join("fonts under test");
    let fetched = counted(&seam, &cache);

    let line = |name: &str, family: &str| {
        format!(r##"Insert({{ type: "text", name: "{name}", content: "Hamburgefonstiv", fontFamily: "{family}", fontSize: 40, fill: "#000000" }});"##)
    };
    let made = seam
        .create(
            "Fonts",
            &format!("{}\n{}\n{VISIT_ALL}", line("Serif", "Instrument Serif"), line("Sans", "Geist")),
        )
        .await;
    let nodes = seen(&made);
    // Instrument Serif sets it narrower than Geist: it's the font used.
    let serif = nodes["Serif"].width();
    assert!(serif > 100. && serif < nodes["Sans"].width() - 20., "{nodes:?}");
    assert_eq!(warnings(&made), Vec::<String>::new());
    // Fetched once (Geist is bundled), into the cache.
    assert_eq!(*fetched.lock().unwrap(), ["Instrument Serif 400"]);
    assert!(cache.join("Instrument Serif").join("400.ttf").is_file());

    // Used again without a fetch, and screenshots draw it.
    let again = seam
        .execute(
            &made.path(),
            &format!("{}\nTakeScreenshot(\"{}\");\n{VISIT_ALL}", line("Serif 2", "Instrument Serif"), made.id("Serif")),
        )
        .await;
    assert_eq!(seen(&again)["Serif 2"].width(), serif);
    assert_eq!(again.images.len(), 1, "{}", again.text);
    assert_eq!(fetched.lock().unwrap().len(), 1);

    // A new run of the engine reads it from the cache, fetching nothing.
    let fresh = counted(&seam, &cache);
    let after = seam.execute(&made.path(), VISIT_ALL).await;
    assert_eq!(seen(&after)["Serif"].width(), serif);
    assert!(fresh.lock().unwrap().is_empty(), "{:?}", fresh.lock().unwrap());
    seam.shutdown().await;
}

#[tokio::test]
async fn a_family_that_cant_be_had_draws_in_geist_with_a_warning() {
    let seam = Seam::new().await;
    let fetched = counted(&seam, &seam.data_dir().join("fonts under test"));
    let made = seam
        .create(
            "Missing",
            &format!(
                r##"
Insert({{ type: "text", name: "Missing", content: "Hamburgefonstiv", fontFamily: "No Such Family", fontSize: 40, fill: "#000000" }});
Insert({{ type: "text", name: "Geist", content: "Hamburgefonstiv", fontSize: 40, fill: "#000000" }});
{VISIT_ALL}"##
            ),
        )
        .await;
    let nodes = seen(&made);
    assert_eq!(nodes["Missing"].bounds[2..], nodes["Geist"].bounds[2..]);
    let warnings = warnings(&made);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("no font family \"No Such Family\"") && warnings[0].contains("Geist"), "{warnings:?}");
    // Asked of the source once, not again for each layout.
    assert_eq!(*fetched.lock().unwrap(), ["No Such Family 400"]);
    seam.shutdown().await;
}

#[tokio::test]
async fn font_files_beside_a_design_are_used() {
    let seam = Seam::new().await;
    // Nothing to fetch from: only the file beside the Design has it.
    seam.core.designs.use_fonts(Fonts::new(None, None));
    let made = seam.create("Beside", r#"Insert({ type: "frame", name: "Screen", width: 10, height: 10 });"#).await;
    let fonts = made.path().parent().unwrap().join("fonts");
    std::fs::create_dir_all(&fonts).unwrap();
    std::fs::copy(
        PathBuf::from(FONTS).join("InstrumentSerif-Regular.ttf"),
        fonts.join("InstrumentSerif-Regular.ttf"),
    )
    .unwrap();
    let used = seam
        .execute(
            &made.path(),
            &format!(
                r##"
Insert({{ type: "text", name: "Serif", content: "Hamburgefonstiv", fontFamily: "Instrument Serif", fontSize: 40, fill: "#000000" }});
Insert({{ type: "text", name: "Sans", content: "Hamburgefonstiv", fontSize: 40, fill: "#000000" }});
{VISIT_ALL}"##
            ),
        )
        .await;
    let nodes = seen(&used);
    assert!(nodes["Serif"].width() < nodes["Sans"].width() - 20., "{nodes:?}");
    assert_eq!(warnings(&used), Vec::<String>::new());
    seam.shutdown().await;
}

/// Pixels darker than mid-grey, on a white background.
fn inked(image: &Image, x: u32, y: u32) -> bool {
    let [r, g, b, _] = image.rgba(x, y);
    (u32::from(r) + u32::from(g) + u32::from(b)) / 3 < 128
}

/// The inked pixels' extent: left, top, right, bottom (inclusive).
fn ink(image: &Image) -> [u32; 4] {
    let mut extent = [u32::MAX, u32::MAX, 0, 0];
    for y in 0..image.height {
        for x in 0..image.width {
            if inked(image, x, y) {
                extent = [extent[0].min(x), extent[1].min(y), extent[2].max(x), extent[3].max(y)];
            }
        }
    }
    assert!(extent[0] != u32::MAX, "nothing drawn");
    extent
}

fn ink_count(image: &Image) -> usize {
    (0..image.height)
        .flat_map(|y| (0..image.width).map(move |x| (x, y)))
        .filter(|&(x, y)| inked(image, x, y))
        .count()
}

/// The rows with ink in them, as runs of `[first, last]`.
fn ink_bands(image: &Image) -> Vec<[u32; 2]> {
    let mut bands: Vec<[u32; 2]> = Vec::new();
    for y in 0..image.height {
        if (0..image.width).any(|x| inked(image, x, y)) {
            match bands.last_mut() {
                Some(band) if band[1] + 1 == y => band[1] = y,
                _ => bands.push([y, y]),
            }
        }
    }
    bands
}

/// The longest run of inked pixels along any row.
fn longest_run(image: &Image) -> u32 {
    (0..image.height)
        .map(|y| {
            let (mut longest, mut run) = (0, 0);
            for x in 0..image.width {
                run = if inked(image, x, y) { run + 1 } else { 0 };
                longest = longest.max(run);
            }
            longest
        })
        .max()
        .unwrap_or(0)
}

#[tokio::test]
async fn screenshots_show_the_font_weight_letter_spacing_line_height_and_decorations() {
    let seam = Seam::new().await;
    // Each sample in a white frame of its own, screenshotted at 1x.
    let made = seam
        .create(
            "Samples",
            r##"
const samples = {
  Regular: { content: "Heavy ink", fontSize: 32 },
  Bold: { content: "Heavy ink", fontSize: 32, fontWeight: 700 },
  Serif: { content: "Heavy ink", fontSize: 32, fontFamily: "Instrument Serif" },
  Tight: { content: "IIII", fontSize: 32 },
  Spaced: { content: "IIII", fontSize: 32, letterSpacing: 20 },
  Lines: { content: "I\nI", fontSize: 20, lineHeight: 3 },
  Plain: { content: "Mama", fontSize: 32 },
  Underlined: { content: "Mama", fontSize: 32, underline: true },
  Struck: { content: "Mama", fontSize: 32, strikethrough: true },
  Centred: { content: "Mid", fontSize: 32, width: 300, height: 120, textAlign: "center", textAlignVertical: "middle" },
  Bottom: { content: "Mid", fontSize: 32, width: 300, height: 120, textAlign: "right", textAlignVertical: "bottom" },
  Red: { content: "Red", fontSize: 32, fill: "#DC2626" },
};
const ids = [];
Object.entries(samples).forEach(([name, text], i) => {
  const frame = Insert({ type: "frame", name: `${name} frame`, x: 0, y: i * 200, width: 300, height: 120, fill: "#FFFFFF" });
  Insert(frame, { type: "text", name, fill: "#000000", ...text });
  ids.push(frame);
});
TakeScreenshot(ids);"##,
        )
        .await;
    let names = [
        "Regular", "Bold", "Serif", "Tight", "Spaced", "Lines", "Plain", "Underlined", "Struck", "Centred", "Bottom", "Red",
    ];
    assert_eq!(made.images.len(), names.len(), "{}", made.text);
    let shots: HashMap<&str, &Image> = names.into_iter().zip(&made.images).collect();
    for shot in shots.values() {
        assert_eq!((shot.width, shot.height), (300, 120));
    }

    // Weight: bold lays down much more ink than regular.
    let (regular, bold) = (ink_count(shots["Regular"]), ink_count(shots["Bold"]));
    assert!(bold * 4 > regular * 5, "bold {bold}, regular {regular}");

    // Family: Instrument Serif's line is narrower than Geist's.
    let (serif, sans) = (ink(shots["Serif"]), ink(shots["Regular"]));
    assert!(serif[2] + 15 < sans[2], "serif {serif:?}, sans {sans:?}");

    // Letter spacing: 20 px after each of four letters pushes the last
    // one 60 px further.
    let (tight, spaced) = (ink(shots["Tight"]), ink(shots["Spaced"]));
    let pushed = i64::from(spaced[2]) - i64::from(tight[2]);
    assert!((58..=62).contains(&pushed), "pushed {pushed} px");

    // Line height: two lines 60 px apart (20 px at 3).
    let bands = ink_bands(shots["Lines"]);
    assert_eq!(bands.len(), 2, "{bands:?}");
    let apart = i64::from(bands[1][0]) - i64::from(bands[0][0]);
    assert!((59..=61).contains(&apart), "{bands:?}");
    // The first line's room is split above and below it: its letter
    // starts well below the top.
    assert!(bands[0][0] > 20, "{bands:?}");

    // Decorations: a line across the whole word, below it for an
    // underline and through it for a strikethrough.
    let (plain, under, struck) = (ink(shots["Plain"]), ink(shots["Underlined"]), ink(shots["Struck"]));
    let word = plain[2] - plain[0];
    assert!(longest_run(shots["Plain"]) < word / 2);
    assert!(longest_run(shots["Underlined"]) > word * 9 / 10 && under[3] > plain[3], "{under:?} {plain:?}");
    assert!(longest_run(shots["Struck"]) > word * 9 / 10 && struck[3] == plain[3], "{struck:?} {plain:?}");

    // Alignment: centred both ways, and right and bottom.
    let centred = ink(shots["Centred"]);
    let (left, right) = (centred[0], 299 - centred[2]);
    assert!(left.abs_diff(right) <= 2, "{centred:?}");
    let (top, bottom) = (centred[1], 119 - centred[3]);
    assert!(top.abs_diff(bottom) <= 12, "{centred:?}");
    let low = ink(shots["Bottom"]);
    assert!(low[2] >= 294 && low[3] >= 100, "{low:?}");

    // Fill: the text's colour.
    let red = shots["Red"];
    let reddest = (0..120)
        .flat_map(|y| (0..300).map(move |x| (x, y)))
        .map(|(x, y)| red.rgba(x, y))
        .find(|&[r, g, b, _]| r > 200 && g < 60 && b < 60);
    assert!(reddest.is_some(), "no red pixel");
    seam.shutdown().await;
}
