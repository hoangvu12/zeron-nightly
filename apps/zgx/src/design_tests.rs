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
use zeron_design::fonts::{FontFile, Fonts};
use zgx::input::InputEvent;
use zgx_script::{ScriptHost, testing::TestApp};

use crate::design::{CACHE_BYTES, DesignTabs, Listener, Source};

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
            // Laid out (#20): three 60 px chips in a row from (40, 300),
            // 10 apart inside 20 of padding, whatever their x and y.
            { "id": "chips", "type": "frame", "x": 40, "y": 300, "width": 260, "height": 100, "fill": "#F3F4F6",
              "layout": "horizontal", "gap": 10, "padding": 20, "children": [
                { "id": "chip1", "type": "rectangle", "x": 500, "y": 500, "width": 60, "height": 60, "fill": "#4F46E5" },
                { "id": "chip2", "type": "rectangle", "width": 60, "height": 60, "fill": "#EF4444" },
                { "id": "chip3", "type": "rectangle", "width": 60, "height": 60, "fill": "#4F46E5" },
            ] },
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

/// The engine, stood in for: Designs by path (with the font files they
/// use), and who follows each.
#[derive(Default)]
struct StandIn {
    designs: RefCell<HashMap<String, (Design, Vec<FontFile>)>>,
    following: RefCell<Vec<(String, Weak<RefCell<Listener>>)>>,
    /// What each chat's Design tabs were said to be, in order.
    tabs_said: RefCell<Vec<(String, Vec<String>)>>,
}

impl StandIn {
    /// A new version of the Design at `path`, said to whoever follows it.
    fn say(&self, path: &str, design: Design) {
        self.say_with_fonts(path, design, Vec::new());
    }

    /// A new version of the Design at `path` using the font files `fonts`.
    fn say_with_fonts(&self, path: &str, design: Design, fonts: Vec<FontFile>) {
        self.designs.borrow_mut().insert(path.into(), (design.clone(), fonts.clone()));
        let listeners: Vec<_> = self.following.borrow().iter().filter(|(p, _)| p == path).filter_map(|(_, l)| l.upgrade()).collect();
        for listener in listeners {
            (listener.borrow_mut())(Ok((design.clone(), Fonts::of_files(&fonts))));
        }
    }
}

struct Engine(Rc<StandIn>);

impl Source for Engine {
    fn follow(&self, path: &str, listener: Listener) -> Option<Box<dyn std::any::Any>> {
        let listener = Rc::new(RefCell::new(listener));
        self.0.following.borrow_mut().push((path.into(), Rc::downgrade(&listener)));
        let now = self.0.designs.borrow().get(path).cloned();
        let now = now.map(|(design, fonts)| (design, Fonts::of_files(&fonts)));
        (listener.borrow_mut())(now.ok_or_else(|| format!("can't open {path}")));
        Some(Box::new(listener))
    }

    fn set_tabs(&self, chat: &str, paths: &[String]) {
        self.0.tabs_said.borrow_mut().push((chat.into(), paths.to_vec()));
    }
}

struct Tab {
    app: TestApp,
    canvas: Rc<DesignTabs>,
    engine: Rc<StandIn>,
}

/// The fixture's Design tab on `design`, every tile in view drawn.
fn mount(design: Design) -> Tab {
    mount_with_fonts(design, Vec::new())
}

/// The fixture's Design tab on `design` using the font files `fonts`.
fn mount_with_fonts(design: Design, fonts: Vec<FontFile>) -> Tab {
    let engine = Rc::new(StandIn::default());
    engine.say_with_fonts(PATH, design, fonts);
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
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default()).expect("a screenshot");
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
    // The row's second chip, where the layout puts it (the Screen at
    // (100, 50), the row at (40, 300) in it, 20 in, past a chip and a gap).
    assert_eq!(shown.rgba(tab.on_screen(100. + 40. + 20. + 70. + 30., 50. + 300. + 20. + 30.)), RED);
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

#[test]
fn the_engine_hears_which_designs_a_chats_tabs_show() {
    let tab = mount(landing("#EF4444", false));
    let said = || tab_feed(&tab);
    assert_eq!(said(), [("design-fixture".to_string(), vec![PATH.to_string()])]);

    // A second Design tab with no Design yet, in front: the same Designs,
    // not said again.
    assert!(tab.app.host.run_command("design_empty"));
    tab.app.frames(4);
    assert_eq!(said().len(), 1, "{:?}", said());

    // A new engine (the connection made again) hears them again.
    tab.canvas.follow_again();
    assert_eq!(said().len(), 2);
    assert_eq!(said()[1], said()[0]);
}

fn tab_feed(tab: &Tab) -> Vec<(String, Vec<String>)> {
    tab.engine.tabs_said.borrow().clone()
}

/// An 860 × 600 Screen at the Canvas's origin (so at 1x its tiles' edges
/// are at 512): across the edges, a card with a drop shadow, glass with a
/// background blur over stripes, a blurred square and a turned one.
fn effects_across_tiles() -> Design {
    let mut children = Vec::new();
    for i in 0..40 {
        children.push(serde_json::json!({
            "id": format!("stripe{i}"), "type": "rectangle", "x": 380 + 8 * i, "y": 280, "width": 4, "height": 160,
            "fill": if i % 2 == 0 { "#EF4444" } else { "#4F46E5" },
        }));
    }
    children.extend([
        serde_json::json!({ "id": "card", "type": "rectangle", "x": 450, "y": 80, "width": 120, "height": 120, "fill": "#FFFFFF",
            "effect": { "type": "shadow", "color": "#00000080", "offset": { "x": 12, "y": 12 }, "blur": 24, "spread": 4 } }),
        serde_json::json!({ "id": "glass", "type": "rectangle", "x": 440, "y": 300, "width": 150, "height": 120,
            "effect": { "type": "background_blur", "radius": 16 } }),
        serde_json::json!({ "id": "soft", "type": "rectangle", "x": 200, "y": 470, "width": 80, "height": 80, "fill": "#22C55E",
            "effect": { "type": "blur", "radius": 12 } }),
        serde_json::json!({ "id": "turned", "type": "rectangle", "x": 480, "y": 440, "width": 100, "height": 100, "fill": "#4F46E5",
            "rotation": 30 }),
    ]);
    design(serde_json::json!({ "version": "1.0", "children": [
        { "id": "screen", "type": "frame", "width": 860, "height": 600, "fill": "#F3F4F6", "children": children },
    ] }))
}

#[test]
fn shadows_blurs_and_turns_across_tile_edges_draw_without_seams() {
    let design = effects_across_tiles();
    let tab = mount(design.clone());
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);

    // At 1x a tile is 512 Canvas pixels: each Canvas pixel read from the
    // tile holding it ("design-tile-<tab>-<level>_<x>_<y>").
    let tiles: HashMap<(i32, i32), std::sync::Arc<zgx::image::ImageData>> = tab
        .canvas
        .shown(TAB)
        .into_iter()
        .map(|(id, image)| {
            let key = id.strip_prefix(&format!("design-tile-{TAB}-")).expect("a tile's id");
            let at: Vec<i32> = key.split('_').skip(1).map(|n| n.parse().unwrap()).collect();
            ((at[0], at[1]), image)
        })
        .collect();
    let tiled = |x: i32, y: i32| -> [u8; 4] {
        let image = &tiles[&(x.div_euclid(512), y.div_euclid(512))];
        let at = ((y.rem_euclid(512) * 512 + x.rem_euclid(512)) * 4) as usize;
        image.pixels()[at..at + 4].try_into().unwrap()
    };

    // The Screen drawn in one piece, no tiles.
    let typesetter = Fonts::default().typesetter(&design);
    let laid_out = zeron_design::layout::LaidOut::of(&design, &typesetter);
    let area = zeron_design::render::Rect { x: 0., y: 0., width: 860., height: 600. };
    let whole = zeron_design::render::region(&design, &laid_out, &typesetter, &[0], area, 1., 0., (860, 600)).expect("drawn");
    let one = |x: i32, y: i32| -> [u8; 4] {
        let at = ((y * 860 + x) * 4) as usize;
        whole[at..at + 4].try_into().unwrap()
    };

    // Along each tile edge in the Screen, a few pixels either side: the
    // shadow, the glass and the blurred square as drawn whole. (A turned
    // edge's anti-aliasing moves a few levels with where on the surface it's
    // drawn, tiles or not: within the turned square's box, a seam would show
    // as more than that.)
    let mut off = Vec::new();
    let near = [-4, -2, -1, 0, 1, 3];
    let edges = (0..600).flat_map(|y| near.map(|d| (512 + d, y))).chain((0..860).flat_map(|x| near.map(|d| (x, 512 + d))));
    for (x, y) in edges {
        let turned = (475..575).contains(&x) && (385..545).contains(&y);
        let (want, got) = (one(x, y), tiled(x, y));
        if want.iter().zip(got).any(|(a, b)| a.abs_diff(b) > if turned { 24 } else { 2 }) {
            off.push(((x, y), want, got));
        }
    }
    assert!(off.is_empty(), "{} pixels along the tile edges differ from the whole, as {:?}", off.len(), &off[..off.len().min(6)]);
    // (They were drawn: the shadow's shade right of the edge, the glass
    // softening the stripes, the turned square.)
    assert_ne!(tiled(580, 150), tiled(300, 150));
    assert_ne!(tiled(514, 360), one(514, 280));
}

#[test]
fn text_draws_in_tiles_as_it_does_whole_in_the_fonts_the_engine_names() {
    // Text across the tile edges at x and y 512, in a font the engine says
    // the Design uses (the zgx app's own Instrument Serif here) and the
    // bundled Geist.
    let words = "The quick brown fox jumps over the lazy dog";
    let design = design(serde_json::json!({ "version": "1.0", "children": [
        { "id": "screen", "type": "frame", "width": 860, "height": 600, "fill": "#FFFFFF", "children": [
            { "id": "serif", "type": "text", "x": 380, "y": 60, "content": "Hamburgefonstiv",
              "fontFamily": "Instrument Serif", "fontSize": 56, "fill": "#111827" },
            { "id": "bold", "type": "text", "x": 300, "y": 488, "content": "Heavy, spaced, underlined",
              "fontWeight": 700, "fontSize": 36, "letterSpacing": 3, "underline": true, "fill": "#4F46E5" },
            { "id": "body", "type": "text", "x": 420, "y": 380, "width": 220, "content": words,
              "fontSize": 18, "lineHeight": 1.6, "textAlign": "center", "fill": "#EF4444" },
        ] },
    ] }));
    let serif = FontFile {
        family: "Instrument Serif".into(),
        path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fonts/InstrumentSerif-Regular.ttf"),
    };
    let tab = mount_with_fonts(design.clone(), vec![serif.clone()]);
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);
    let tiles: HashMap<(i32, i32), std::sync::Arc<zgx::image::ImageData>> = tab
        .canvas
        .shown(TAB)
        .into_iter()
        .map(|(id, image)| {
            let key = id.strip_prefix(&format!("design-tile-{TAB}-")).expect("a tile's id");
            let at: Vec<i32> = key.split('_').skip(1).map(|n| n.parse().unwrap()).collect();
            ((at[0], at[1]), image)
        })
        .collect();
    let tiled = |x: i32, y: i32| -> [u8; 4] {
        let image = &tiles[&(x.div_euclid(512), y.div_euclid(512))];
        let at = ((y.rem_euclid(512) * 512 + x.rem_euclid(512)) * 4) as usize;
        image.pixels()[at..at + 4].try_into().unwrap()
    };

    // The Screen drawn whole, as the engine's screenshots draw it.
    let whole = |fonts: &Fonts| {
        let typesetter = fonts.typesetter(&design);
        let laid_out = zeron_design::layout::LaidOut::of(&design, &typesetter);
        let area = zeron_design::render::Rect { x: 0., y: 0., width: 860., height: 600. };
        zeron_design::render::region(&design, &laid_out, &typesetter, &[0], area, 1., 0., (860, 600)).expect("drawn")
    };
    let one = whole(&Fonts::of_files(&[serif]));
    // As the engine draws it, fetching the font itself (from the same
    // files, standing in for Google Fonts).
    let fixture = zeron_design::fonts::FontFolder::new(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fonts"));
    assert!(whole(&Fonts::new(None, Some(Box::new(fixture)))) == one, "the window draws text as the engine does");
    let mut off = Vec::new();
    for y in 0..600 {
        for x in 0..860 {
            let at = ((y * 860 + x) * 4) as usize;
            let want: [u8; 4] = one[at..at + 4].try_into().unwrap();
            let got = tiled(x, y);
            if want.iter().zip(got).any(|(a, b)| a.abs_diff(b) > 2) {
                off.push(((x, y), want, got));
            }
        }
    }
    assert!(off.is_empty(), "{} text pixels differ from the whole, as {:?}", off.len(), &off[..off.len().min(6)]);

    // The serif line was set in Instrument Serif: drawn without it (in
    // Geist, as a family the window wasn't given), it differs.
    let geist = whole(&Fonts::default());
    let differ = (60..130).flat_map(|y| (380..860).map(move |x| (x, y))).filter(|&(x, y)| {
        let at = ((y * 860 + x) * 4) as usize;
        one[at..at + 4] != geist[at..at + 4]
    });
    assert!(differ.count() > 500);
    // (Ink on both sides of each edge.)
    let inked = |x0: i32, x1: i32, y0: i32, y1: i32| {
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).any(|(x, y)| tiled(x, y)[0] < 200)
    };
    assert!(inked(440, 512, 60, 130) && inked(512, 600, 60, 130));
    assert!(inked(300, 700, 488, 512) && inked(300, 700, 512, 540));
}
