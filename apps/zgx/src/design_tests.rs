//! Seam 2: the Design tab (ui/screens/panel/design.luau) mounted headless
//! with its real host (src/design.rs) over a stand-in engine, which holds
//! Designs and says each change as the engine's `WatchDesign` does. Tests
//! look at what the tab shows: the tiles' images where they lie on screen.
//! `cargo test --bin zeron-zgx design_tests` (`-- --nocapture` for the
//! timings).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use zeron_design::Design;
use zgx::input::InputEvent;
use zgx_script::{ScriptHost, testing::TestApp};

use crate::design::{CACHE_BYTES, Canvas, Heard, Seen, Source};

/// The fixture's Design tab, on `fixture.zdesign`.
const TAB: &str = "tab1";
const PATH: &str = "fixture.zdesign";
const SIZE: (f32, f32) = (960., 700.);

const WHITE: [u8; 4] = [255, 255, 255, 255];
const INDIGO: [u8; 4] = [79, 70, 229, 255];
const RED: [u8; 4] = [239, 68, 68, 255];
const GREEN: [u8; 4] = [34, 197, 94, 255];

fn design(json: serde_json::Value) -> Design {
    serde_json::from_value(json).expect("a Design")
}

/// A white 800 × 600 Screen at (100, 50) with an indigo hero and a badge
/// of `badge`'s colour, and maybe a second, grey Screen far to its right.
fn landing(badge: &str, second: bool) -> Design {
    let mut children = vec![serde_json::json!({
        "id": "screen", "type": "frame", "x": 100, "y": 50, "width": 800, "height": 600, "fill": "#FFFFFF",
        "children": [
            { "id": "hero", "type": "rectangle", "x": 40, "y": 40, "width": 200, "height": 100, "fill": "#4F46E5" },
            { "id": "badge", "type": "rectangle", "x": 600, "y": 400, "width": 160, "height": 160, "fill": badge },
        ],
    })];
    if second {
        children.push(serde_json::json!({
            "id": "other", "type": "frame", "x": 2100, "y": 50, "width": 400, "height": 300, "fill": "#888888",
        }));
    }
    design(serde_json::json!({ "version": "1.0", "children": children }))
}

/// Twelve 1440 × 1024 Screens in rows of four, each with a few cards: big
/// enough at 5x to need more tiles than the cache holds.
fn many_screens() -> Design {
    let screens: Vec<_> = (0..12)
        .map(|i| {
            let (x, y) = ((i % 4) as f32 * 1680., (i / 4) as f32 * 1264.);
            let cards: Vec<_> = (0..6)
                .map(|c| serde_json::json!({
                    "id": format!("card{i}_{c}"), "type": "rectangle",
                    "x": 80 + (c % 3) * 440, "y": 120 + (c / 3) * 440, "width": 400, "height": 380, "fill": "#4F46E5",
                }))
                .collect();
            serde_json::json!({
                "id": format!("screen{i}"), "type": "frame", "x": x, "y": y, "width": 1440, "height": 1024,
                "fill": "#FFFFFF", "children": cards,
            })
        })
        .collect();
    design(serde_json::json!({ "version": "1.0", "children": screens }))
}

/// The engine, stood in for: Designs by path, and who follows each.
#[derive(Default)]
struct StandIn {
    designs: RefCell<HashMap<String, (u64, Design)>>,
    following: RefCell<Vec<(String, Weak<RefCell<Heard>>)>>,
}

impl StandIn {
    /// A new version of the Design at `path`, said to whoever follows it.
    fn say(&self, path: &str, design: Design) {
        let version = {
            let mut designs = self.designs.borrow_mut();
            let entry = designs.entry(path.into()).or_insert((0, Design::default()));
            entry.0 += 1;
            entry.1 = design.clone();
            entry.0
        };
        let listeners: Vec<_> = self.following.borrow().iter().filter(|(p, _)| p == path).filter_map(|(_, h)| h.upgrade()).collect();
        for heard in listeners {
            (heard.borrow_mut())(Ok(Seen { version, design: design.clone() }));
        }
    }
}

struct Engine(Rc<StandIn>);

impl Source for Engine {
    fn follow(&self, path: &str, heard: Heard) -> Option<Box<dyn std::any::Any>> {
        let heard = Rc::new(RefCell::new(heard));
        self.0.following.borrow_mut().push((path.into(), Rc::downgrade(&heard)));
        let now = self.0.designs.borrow().get(path).cloned();
        match now {
            Some((version, design)) => (heard.borrow_mut())(Ok(Seen { version, design })),
            None => (heard.borrow_mut())(Err(format!("can't open {path}"))),
        }
        Some(Box::new(heard))
    }
}

struct Tab {
    app: TestApp,
    canvas: Rc<Canvas>,
    engine: Rc<StandIn>,
}

/// The fixture's Design tab on `design`, every tile in view drawn.
fn mount(design: Design) -> Tab {
    let engine = Rc::new(StandIn::default());
    engine.say(PATH, design);
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/design_fixture.luau");
    let mut made = None;
    let app = TestApp::new(|runtime| {
        let host = ScriptHost::new(runtime, path);
        made = Some(crate::design::register(&host, Box::new(Engine(engine.clone()))));
        host
    });
    let (canvas, pump) = made.expect("registered");
    app.executor.borrow_mut().spawn(pump);
    app.ui.scene.borrow_mut().resize(SIZE.0, SIZE.1);
    app.frames(2);
    assert!(app.host.run_command("design_fit"));
    let mut tab = Tab { app, canvas, engine };
    tab.settle();
    assert_eq!(tab.app.error(), None, "the fixture failed");
    tab
}

impl Tab {
    /// Until every tile in view is drawn at the zoom's level, as the
    /// Design is now: how long that took.
    fn settle(&mut self) -> Duration {
        let started = Instant::now();
        let canvas = self.canvas.clone();
        let sharp = self.app.wait_until(|_| canvas.sharp(TAB));
        assert!(sharp, "the tab never settled sharp:\n{}", self.app.tree());
        started.elapsed()
    }

    /// What the tab shows, `SIZE` at a pixel a point: each tile's image
    /// where its node shows on screen, back to front, over transparent.
    fn picture(&mut self) -> Picture {
        let (w, h) = (SIZE.0 as usize, SIZE.1 as usize);
        let mut px = vec![0u8; w * h * 4];
        for (id, image) in self.canvas.shown(TAB) {
            let node = self.app.target(&format!("#{id}")).unwrap_or_else(|e| panic!("{e}\n{}", self.app.tree()));
            let at = self.app.ui.scene.borrow().transformed_bounds(node);
            let pixels = image.pixels();
            let (iw, ih) = (image.width() as f32, image.height() as f32);
            let (x0, y0) = (at.x.max(0.).floor() as usize, at.y.max(0.).floor() as usize);
            let x1 = ((at.x + at.width).ceil() as usize).min(w);
            let y1 = ((at.y + at.height).ceil() as usize).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let u = (((x as f32 + 0.5 - at.x) / at.width) * iw).floor();
                    let v = (((y as f32 + 0.5 - at.y) / at.height) * ih).floor();
                    if !(0. ..iw).contains(&u) || !(0. ..ih).contains(&v) {
                        continue;
                    }
                    let from = ((v as usize) * iw as usize + u as usize) * 4;
                    let src = &pixels[from..from + 4];
                    let to = (y * w + x) * 4;
                    // (Premultiplied, over what's under it.)
                    let a = src[3] as u32;
                    for c in 0..4 {
                        px[to + c] = (src[c] as u32 + px[to + c] as u32 * (255 - a) / 255) as u8;
                    }
                }
            }
        }
        Picture { w, px }
    }

    /// Where Canvas point (`x`, `y`) shows in the tab.
    fn on_screen(&self, x: f32, y: f32) -> (usize, usize) {
        let (px, py, zoom) = self.canvas.view(TAB).expect("a view");
        ((px + x * zoom).floor() as usize, (py + y * zoom).floor() as usize)
    }

    fn pinch(&mut self, delta: f32) {
        let (x, y) = (SIZE.0 / 2., SIZE.1 / 2.);
        self.app.ui.dispatch(InputEvent::Pinch { x, y, delta });
        self.app.frames(1);
    }

    fn pan(&mut self, dx: f32, dy: f32) {
        let node = self.app.target(&format!("#design-canvas-{TAB}")).expect("the Canvas");
        zgx_script::driver::wheel(&mut self.app.ui, node, -dx, -dy, None);
        self.app.frames(1);
    }
}

struct Picture {
    w: usize,
    px: Vec<u8>,
}

impl Picture {
    fn rgba(&self, (x, y): (usize, usize)) -> [u8; 4] {
        let at = (y * self.w + x) * 4;
        self.px[at..at + 4].try_into().unwrap()
    }
}

#[test]
fn the_tab_draws_its_design_as_the_engines_screenshot_does() {
    let mut tab = mount(landing("#EF4444", false));
    // The whole Design in view at 1x, centred.
    let (_, _, zoom) = tab.canvas.view(TAB).unwrap();
    assert_eq!(zoom, 1.);
    let shown = tab.picture();

    // The engine's screenshot of the Screen (crates/engine designs.rs),
    // against where the Screen shows in the tab.
    let design = landing("#EF4444", false);
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000.).expect("a screenshot");
    let decoder = png::Decoder::new(std::io::Cursor::new(shot.png));
    let mut reader = decoder.read_info().expect("a PNG");
    let mut expected = vec![0; reader.output_buffer_size().expect("its size")];
    reader.next_frame(&mut expected).expect("its pixels");
    assert_eq!((shot.width, shot.height), (800, 600));
    let (left, top) = tab.on_screen(100., 50.);
    let mut off = 0;
    for y in 0..600 {
        for x in 0..800 {
            let want = &expected[(y * 800 + x) * 4..][..4];
            let got = shown.rgba((left + x, top + y));
            if want.iter().zip(got).any(|(a, b)| a.abs_diff(b) > 2) {
                off += 1;
            }
        }
    }
    // (A tolerance for edges a tile's seam might shade apart; none so far.)
    assert!(off <= 800 * 600 / 1000, "{off} pixels differ from the screenshot");
    assert_eq!(shown.rgba(tab.on_screen(150., 100.)), INDIGO);
    assert_eq!(shown.rgba(tab.on_screen(780., 530.)), RED);
    assert_eq!(shown.rgba(tab.on_screen(500., 300.)), WHITE);
    assert_eq!(shown.rgba(tab.on_screen(50., 300.)), [0; 4], "the Canvas shows through");
}

#[test]
fn engine_changes_show_in_the_open_tab_redrawing_only_what_changed() {
    let mut tab = mount(landing("#EF4444", true));
    assert_eq!(tab.picture().rgba(tab.on_screen(780., 530.)), RED);
    let before: HashMap<String, _> = tab.canvas.shown(TAB).into_iter().collect();
    let other = tab.on_screen(2300., 200.);
    assert_eq!(tab.picture().rgba(other)[..3], [0x88; 3]);

    // An agent turns the badge green: the tab shows it, as it is.
    let started = Instant::now();
    tab.engine.say(PATH, landing("#22C55E", true));
    let took = tab.settle();
    let after = tab.picture();
    assert_eq!(after.rgba(tab.on_screen(780., 530.)), GREEN);
    assert_eq!(after.rgba(tab.on_screen(150., 100.)), INDIGO);
    eprintln!("design: a change shown in {:?} (settled in {took:?})", started.elapsed());

    // The other Screen's tiles weren't drawn again.
    let now: HashMap<String, _> = tab.canvas.shown(TAB).into_iter().collect();
    let (redrawn, kept): (Vec<_>, Vec<_>) = now.iter().partition(|(id, image)| before.get(*id).is_none_or(|was| !std::sync::Arc::ptr_eq(was, image)));
    assert!(!redrawn.is_empty() && !kept.is_empty(), "redrawn {}, kept {}", redrawn.len(), kept.len());
    assert_eq!(after.rgba(other)[..3], [0x88; 3]);
    let other_tile = |shown: &HashMap<String, std::sync::Arc<zgx::image::ImageData>>| {
        shown.iter().filter(|(_, image)| image.pixels().chunks(4).any(|p| p[..3] == [0x88; 3])).map(|(id, _)| id.clone()).collect::<Vec<_>>()
    };
    for id in other_tile(&now) {
        assert!(std::sync::Arc::ptr_eq(&before[&id], &now[&id]), "{id} was drawn again");
    }

    // A Screen it adds, and one it takes away.
    let mut more = landing("#22C55E", false);
    more.children.push(
        serde_json::from_value(serde_json::json!({
            "id": "new", "type": "frame", "x": 100, "y": 700, "width": 300, "height": 100, "fill": "#EF4444",
        }))
        .expect("a node"),
    );
    tab.engine.say(PATH, more);
    tab.settle();
    let shown = tab.picture();
    assert_eq!(shown.rgba(tab.on_screen(200., 750.)), RED);
    assert_eq!(shown.rgba(other), [0; 4], "the Screen taken away still shows");
}

#[test]
fn panning_shows_no_blank_tiles_and_zooming_settles_sharp_within_the_cache() {
    let started = Instant::now();
    let mut tab = mount(many_screens());
    eprintln!("design: cold open to every tile in view, {:?}", started.elapsed());

    // Zoomed in about twofold.
    let (_, _, fit) = tab.canvas.view(TAB).unwrap();
    for _ in 0..7 {
        tab.pinch(0.1);
    }
    let took = tab.settle();
    let (_, _, zoom) = tab.canvas.view(TAB).unwrap();
    assert!(zoom > fit * 1.9, "{fit} → {zoom}");
    eprintln!("design: zoom {fit:.2}x → {zoom:.2}x sharp in {took:?}");

    // Panning a step at a time (as a trackpad does, each step drawn), the
    // ring of tiles round the view keeps it covered.
    let mut most = 0;
    for step in 0..40 {
        tab.pan(if step < 20 { -60. } else { 0. }, if step < 20 { 0. } else { -45. });
        assert_eq!(tab.canvas.blank(TAB), 0, "a blank tile on step {step}");
        tab.settle();
        most = most.max(tab.canvas.cached_bytes());
    }

    // While zooming in, the level before shows (scaled) until the new one
    // is drawn; at rest it's the zoom's own.
    let mut pinches = 0;
    while tab.canvas.view(TAB).unwrap().2 < 5. && pinches < 40 {
        tab.pinch(0.15);
        pinches += 1;
        assert_eq!(tab.canvas.blank(TAB), 0, "a blank tile while zooming");
    }
    let took = tab.settle();
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 5.);
    eprintln!("design: zoom on to 5x sharp in {took:?}");

    // Across the Canvas at 5x, along a row, down and back along the next: far more tiles
    // than the cache holds, kept under its cap.
    let mut walk = Vec::new();
    for row in 0..4 {
        // (From the Canvas's lower right, where the zoom left the view.)
        let along = if row % 2 == 0 { 900. } else { -900. };
        walk.extend(std::iter::repeat_n((along, 0.), 25));
        walk.extend(std::iter::repeat_n((0., 600.), 3));
    }
    for (dx, dy) in walk {
        tab.pan(dx, dy);
        tab.settle();
        most = most.max(tab.canvas.cached_bytes());
        assert!(tab.canvas.cached_bytes() <= CACHE_BYTES, "{} MiB held", tab.canvas.cached_bytes() >> 20);
    }
    eprintln!("design: the tile cache at most {} MiB (cap {} MiB)", most >> 20, CACHE_BYTES >> 20);
    assert!(most > CACHE_BYTES / 2, "the walk never filled the cache: {} MiB", most >> 20);
}

#[test]
fn a_design_that_cannot_be_followed_says_so() {
    let engine = Rc::new(StandIn::default());
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/design_fixture.luau");
    let mut made = None;
    let mut app = TestApp::new(|runtime| {
        let host = ScriptHost::new(runtime, path);
        made = Some(crate::design::register(&host, Box::new(Engine(engine.clone()))));
        host
    });
    app.executor.borrow_mut().spawn(made.unwrap().1);
    app.ui.scene.borrow_mut().resize(SIZE.0, SIZE.1);
    app.frames(4);
    assert!(app.find("~\"The Design didn't open\"").is_some(), "{}", app.tree());
    assert!(app.find("~\"can't open fixture.zdesign\"").is_some(), "{}", app.tree());
}
