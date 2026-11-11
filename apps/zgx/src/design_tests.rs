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
use zeron_design::assets::Resolved;
use zeron_design::fonts::{FontFile, Fonts};
use zgx::input::InputEvent;
use zgx_script::{ScriptHost, testing::TestApp};

use crate::design::{CACHE_BYTES, DesignTabs, Heard, Listener, Source, heard_of};

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
    designs: RefCell<HashMap<String, Design>>,
    /// The font files its Designs use, as the engine would say them.
    fonts: RefCell<Vec<FontFile>>,
    /// The file each image is in, by url, as the engine says them.
    images: RefCell<HashMap<String, std::path::PathBuf>>,
    /// The files of the other assets its Designs use, as the engine would
    /// say them.
    assets: RefCell<Vec<Resolved>>,
    following: RefCell<Vec<(String, Weak<RefCell<Listener>>)>>,
    /// What each chat's Design tabs were said to be, in order.
    tabs_said: RefCell<Vec<(String, Vec<String>)>>,
}

impl StandIn {
    /// A new version of the Design at `path`, said to whoever follows it.
    /// A new version the person made (in a Design tab, as #33's edits will
    /// be told): it doesn't flash.
    fn say(&self, path: &str, design: Design) {
        self.tell(path, design, &[], "person");
    }

    /// A new version an agent's call made, changing `changed`.
    fn say_by_agent(&self, path: &str, design: Design, changed: &[&str]) {
        self.tell(path, design, changed, "agent");
    }

    /// As the engine's `WatchDesign` says it, read as the window reads it.
    fn tell(&self, path: &str, design: Design, changed: &[&str], by: &str) {
        self.designs.borrow_mut().insert(path.into(), design.clone());
        let item = serde_json::json!({
            "design": design,
            "changes": [{ "changed": changed, "by": by }],
            "fonts": *self.fonts.borrow(),
            "images": *self.images.borrow(),
        });
        let listeners: Vec<_> = self.following.borrow().iter().filter(|(p, _)| p == path).filter_map(|(_, l)| l.upgrade()).collect();
        for listener in listeners {
            (listener.borrow_mut())(heard_of(&item, &mut Fonts::default()));
        }
    }
}

struct Engine(Rc<StandIn>);

impl Source for Engine {
    fn follow(&self, path: &str, listener: Listener) -> Option<Box<dyn std::any::Any>> {
        let listener = Rc::new(RefCell::new(listener));
        self.0.following.borrow_mut().push((path.into(), Rc::downgrade(&listener)));
        let now = self.0.designs.borrow().get(path).cloned();
        let fonts = Fonts::of_files(&self.0.fonts.borrow());
        let images = self.0.images.borrow().clone();
        (listener.borrow_mut())(now.map(|design| Heard { design, flash: Vec::new(), fonts, images, assets: self.0.assets.borrow().clone() }).ok_or_else(|| format!("can't open {path}")));
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
    mount_on(design, Vec::new(), Vec::new())
}

/// The fixture's Design tab on `design` using the font files `fonts`.
fn mount_with_fonts(design: Design, fonts: Vec<FontFile>) -> Tab {
    mount_on(design, fonts, Vec::new())
}

/// As [`mount`], the engine naming the files of the Design's assets.
fn mount_with(design: Design, assets: Vec<Resolved>) -> Tab {
    mount_on(design, Vec::new(), assets)
}

/// The fixture's Design tab on `design`, the engine naming the files of
/// its fonts and other assets.
fn mount_on(design: Design, fonts: Vec<FontFile>, assets: Vec<Resolved>) -> Tab {
    let engine = Rc::new(StandIn::default());
    *engine.fonts.borrow_mut() = fonts;
    *engine.assets.borrow_mut() = assets;
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
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &zeron_design::render::NoImages, &zeron_design::assets::NoAssets).expect("a screenshot");
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

/// What the tab shows at 1x, by Canvas pixel: a tile is 512 Canvas pixels,
/// and each pixel is read from the tile holding it
/// ("design-tile-<tab>-<level>_<x>_<y>").
fn tiled_at_1x(tab: &Tab) -> impl Fn(i32, i32) -> [u8; 4] + use<> {
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
    move |x, y| {
        let image = &tiles[&(x.div_euclid(512), y.div_euclid(512))];
        let at = ((y.rem_euclid(512) * 512 + x.rem_euclid(512)) * 4) as usize;
        image.pixels()[at..at + 4].try_into().unwrap()
    }
}

#[test]
fn shadows_blurs_and_turns_across_tile_edges_draw_without_seams() {
    let design = effects_across_tiles();
    let tab = mount(design.clone());
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);

    let tiled = tiled_at_1x(&tab);

    // The Screen drawn in one piece, no tiles.
    let typesetter = Fonts::default().typesetter(&design);
    let laid_out = zeron_design::layout::LaidOut::of(&design, &typesetter);
    let area = zeron_design::render::Rect { x: 0., y: 0., width: 860., height: 600. };
    let whole = zeron_design::render::region(&design, &laid_out, &typesetter, &[0], area, 1., 0., (860, 600), &zeron_design::render::NoImages, &zeron_design::assets::NoAssets).expect("drawn");
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

/// An 860 × 600 Screen at the Canvas's origin with an icon (a square, the
/// engine's cached file of `mdi:square`) and a path, each across the 1x
/// tiles' edges at 512.
fn icon_and_path_across_tiles(dir: &std::path::Path) -> (Design, Vec<Resolved>) {
    let file = dir.join("square.svg");
    // As Iconify serves it: sized in em, drawn in its text colour.
    std::fs::write(
        &file,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24"><path fill="currentColor" d="M4 4h16v16H4z"/></svg>"#,
    )
    .unwrap();
    let assets = vec![Resolved { key: zeron_design::assets::AssetKey::icon("mdi:square"), path: file }];
    let design = design(serde_json::json!({ "version": "1.0", "children": [
        { "id": "screen", "type": "frame", "width": 860, "height": 600, "fill": "#FFFFFF", "children": [
            { "id": "icon", "type": "icon", "icon": "mdi:square", "x": 464, "y": 464, "width": 96, "height": 96, "fill": "#EF4444" },
            { "id": "star", "type": "path", "x": 380, "y": 60, "width": 240, "height": 200, "fill": "#4F46E5",
              "geometry": "M50 0 L61 35 L98 35 L68 57 L79 91 L50 70 L21 91 L32 57 L2 35 L39 35 Z", "viewBox": [0, 0, 100, 100] },
        ] },
    ] }));
    (design, assets)
}

#[test]
fn icons_and_paths_across_tile_edges_draw_as_the_engines_screenshot_does() {
    let dir = std::env::temp_dir().join(format!("zeron-design-tiles-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (design, assets) = icon_and_path_across_tiles(&dir);
    let tab = mount_with(design.clone(), assets.clone());
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);
    let tiled = tiled_at_1x(&tab);

    // The engine's screenshot of the Screen (at the Canvas's origin), its
    // icon drawn from the same file.
    let files = zeron_design::assets::Files::new(assets);
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &zeron_design::render::NoImages, &files).expect("a screenshot");
    assert_eq!((shot.width, shot.height), (860, 600));
    let mut reader = png::Decoder::new(std::io::Cursor::new(shot.png)).read_info().expect("a PNG");
    let mut whole = vec![0; reader.output_buffer_size().expect("its size")];
    reader.next_frame(&mut whole).expect("its pixels");
    let one = |x: i32, y: i32| -> [u8; 4] {
        let at = ((y * 860 + x) * 4) as usize;
        whole[at..at + 4].try_into().unwrap()
    };

    // Every pixel of the icon and the star, and round them, as in the
    // screenshot. (A slanted edge's anti-aliasing moves a few levels with
    // where on the surface it's drawn, tiles or not: a seam would show as
    // far more.)
    let mut off = Vec::new();
    for (x, y) in (370..630).flat_map(|x| (50..270).map(move |y| (x, y))).chain((455..570).flat_map(|x| (455..570).map(move |y| (x, y)))) {
        let (want, got) = (one(x, y), tiled(x, y));
        if want.iter().zip(got).any(|(a, b)| a.abs_diff(b) > 8) {
            off.push(((x, y), want, got));
        }
    }
    assert!(off.is_empty(), "{} pixels differ from the screenshot, as {:?}", off.len(), &off[..off.len().min(6)]);
    // (They were drawn: the icon's square either side of both edges, white
    // round it; the star's arms either side of x 512.)
    for (x, y) in [(500, 500), (520, 500), (500, 520), (520, 520)] {
        assert_eq!(tiled(x, y), RED, "({x}, {y})");
    }
    assert_eq!(tiled(470, 470), WHITE);
    assert_eq!((tiled(508, 120), tiled(516, 120)), (INDIGO, INDIGO));
    let _ = std::fs::remove_dir_all(dir);
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
        zeron_design::render::region(&design, &laid_out, &typesetter, &[0], area, 1., 0., (860, 600), &zeron_design::render::NoImages, &zeron_design::assets::NoAssets).expect("drawn")
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

/// Where node `target` shows in the tab, through its transforms.
fn on_screen_of(app: &mut TestApp, target: &str) -> zgx::scene::Rect {
    let node = app.target(target).unwrap_or_else(|e| panic!("{e}
{}", app.tree()));
    app.ui.scene.borrow().transformed_bounds(node)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1.5
}

#[test]
fn an_agents_change_flashes_what_it_changed_and_no_one_elses_does() {
    let mut tab = mount(landing("#EF4444", false));
    assert!(tab.app.find(".design-flash").is_none(), "nothing flashes as it opens");

    // An agent turns the badge green: it flashes over the badge, then goes.
    tab.engine.say_by_agent(PATH, landing("#22C55E", false), &["badge"]);
    tab.app.frames(2);
    let flash = on_screen_of(&mut tab.app, &format!("#design-flash-{TAB}-badge"));
    let (left, top) = tab.on_screen(700., 450.);
    let (_, _, zoom) = tab.canvas.view(TAB).unwrap();
    assert!(near(flash.x, left as f32) && near(flash.y, top as f32), "{flash:?} against ({left}, {top})");
    assert!(near(flash.width, 160. * zoom) && near(flash.height, 160. * zoom), "{flash:?}");
    assert!(tab.app.find(&format!("#design-flash-{TAB}-hero")).is_none(), "only what changed flashes");
    let gone = tab.app.wait_until(|app| app.find(".design-flash").is_none());
    assert!(gone, "the flash stays:
{}", tab.app.tree());

    // Two calls one after the other, each changing its node: both flash.
    tab.engine.say_by_agent(PATH, landing("#EF4444", false), &["badge"]);
    let mut moved = landing("#EF4444", false);
    moved.children[0].name = "Landing".into();
    tab.engine.say_by_agent(PATH, moved, &["hero"]);
    tab.app.frames(2);
    assert!(tab.app.find(&format!("#design-flash-{TAB}-badge")).is_some(), "{}", tab.app.tree());
    assert!(tab.app.find(&format!("#design-flash-{TAB}-hero")).is_some(), "{}", tab.app.tree());
    assert!(tab.app.wait_until(|app| app.find(".design-flash").is_none()));

    // A change that isn't an agent's (the person's own) doesn't flash.
    tab.engine.say(PATH, landing("#EF4444", false));
    tab.app.frames(2);
    assert!(tab.app.find(".design-flash").is_none(), "{}", tab.app.tree());
}

#[test]
fn in_progress_screens_look_in_progress_until_the_mark_is_cleared() {
    let mut working = landing("#EF4444", true);
    working.children[0].in_progress = true;
    let mut tab = mount(working);
    let mark = on_screen_of(&mut tab.app, &format!("#design-working-{TAB}-screen"));
    let (left, top) = tab.on_screen(100., 50.);
    let (_, _, zoom) = tab.canvas.view(TAB).unwrap();
    assert!(near(mark.x, left as f32) && near(mark.y, top as f32), "{mark:?} against ({left}, {top})");
    assert!(near(mark.width, 800. * zoom), "{mark:?}");
    assert!(tab.app.find(&format!("#design-working-{TAB}-screen >> \"In progress\"")).is_some(), "{}", tab.app.tree());
    assert!(tab.app.find(&format!("#design-working-{TAB}-other")).is_none(), "the finished Screen looks as ever");

    // The agent clears the mark: the Screen looks finished.
    tab.engine.say_by_agent(PATH, landing("#EF4444", true), &["screen"]);
    tab.app.frames(4);
    assert!(tab.app.wait_until(|app| app.find(".design-working").is_none()), "{}", tab.app.tree());
}

/// A 40 × 20 PNG of four upright stripes: red, green, blue and black.
fn stripes() -> Vec<u8> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, 40, 20);
    encoder.set_color(png::ColorType::Rgba);
    let mut writer = encoder.write_header().expect("a PNG header");
    let colours = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [0, 0, 0, 255]];
    let pixels: Vec<u8> = (0..20).flat_map(|_| (0..40).flat_map(|x: usize| colours[x / 10])).collect();
    writer.write_image_data(&pixels).expect("its pixels");
    writer.finish().expect("a PNG");
    png
}

#[test]
fn image_fills_draw_from_the_files_the_engine_names_across_tile_edges() {
    // The engine names each image's file; one it hasn't (a download that
    // failed) draws as a placeholder, as the window never downloads.
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("stripes.png");
    std::fs::write(&file, stripes()).expect("the image written");
    let design = design(serde_json::json!({ "version": "1.0", "children": [
        { "id": "screen", "type": "frame", "width": 860, "height": 600, "fill": "#F3F4F6", "children": [
            { "id": "photo", "type": "rectangle", "x": 400, "y": 300, "width": 240, "height": 260,
              "fill": { "type": "image", "url": "images/stripes.png", "mode": "stretch" } },
            { "id": "gone", "type": "rectangle", "x": 100, "y": 100, "width": 100, "height": 100,
              "fill": { "type": "image", "url": "https://example.com/gone.png" } },
        ] },
    ] }));
    let mut tab = mount(design.clone());
    tab.engine.images.borrow_mut().insert("images/stripes.png".into(), file.clone());
    tab.engine.say(PATH, design.clone());
    tab.settle();
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);
    let shown = tab.picture();
    // Stretched, a stripe 60 wide each, across the tiles' edges at 512.
    for (x, colour) in [(430., [255, 0, 0, 255]), (490., [0, 255, 0, 255]), (550., [0, 0, 255, 255]), (610., [0, 0, 0, 255])] {
        for y in [320., 540.] {
            let got = shown.rgba(tab.on_screen(x, y));
            assert!(got.iter().zip(colour).all(|(a, b)| a.abs_diff(b) <= 2), "at ({x}, {y}): {got:?}, not {colour:?}");
        }
    }
    let placeholder = shown.rgba(tab.on_screen(150., 120.));
    assert!(placeholder.iter().zip([228, 228, 231, 255]).all(|(a, b)| a.abs_diff(b) <= 2), "{placeholder:?}");

    // As the engine's screenshot draws it, with the same files.
    let images = zeron_design::render::ImageFiles::new([("images/stripes.png".to_string(), file)].into());
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &images, &zeron_design::assets::NoAssets).expect("a screenshot");
    let decoder = png::Decoder::new(std::io::Cursor::new(shot.png));
    let mut reader = decoder.read_info().expect("a PNG");
    let mut expected = vec![0; reader.output_buffer_size().expect("its size")];
    reader.next_frame(&mut expected).expect("its pixels");
    let (left, top) = tab.on_screen(0., 0.);
    let mut off = 0;
    for y in 0..600 {
        for x in 0..860 {
            let want = &expected[(y * 860 + x) * 4..][..4];
            if want.iter().zip(shown.rgba((left + x, top + y))).any(|(a, b)| a.abs_diff(b) > 2) {
                off += 1;
            }
        }
    }
    assert!(off <= 860 * 600 / 1000, "{off} pixels differ from the screenshot");
}
