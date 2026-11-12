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
use zeron_design::fonts::Fonts;
use zgx::input::InputEvent;
use zgx_script::{ScriptHost, Value, testing::TestApp};

use crate::design::{FileState, CACHE_BYTES, DesignTabs, Heard, Listener, Source, heard_of};

/// The fixture's Design tab, on `fixture.zdesign`.
const TAB: &str = "tab1";
const PATH: &str = "fixture.zdesign";
/// The window: the layers list's 220 points, a Canvas 960 wide (under the
/// toolbar), the properties panel's 220.
const SIZE: (f32, f32) = (1400., 700.);

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

/// The engine, stood in for: Designs by path, and who follows each.
#[derive(Default)]
struct StandIn {
    designs: RefCell<HashMap<String, Design>>,
    /// The files of the assets its Designs use (icons, fonts, images), as
    /// the engine would say them.
    assets: RefCell<Vec<Resolved>>,
    following: RefCell<Vec<(String, Weak<RefCell<Listener>>)>>,
    /// What each chat's Design tabs were said to be, in order.
    tabs_said: RefCell<Vec<(String, Vec<String>)>>,
    /// What each Design tab's selection was said to be: chat, Design, ids.
    selected_said: RefCell<Vec<(String, String, Vec<String>)>>,
    /// Each Design's file state (#28): `{unsaved, recovered, stale}`.
    saving: RefCell<HashMap<String, serde_json::Value>>,
    /// The Designs touched recently, newest first, as `RecentDesigns` says.
    recent: RefCell<Vec<serde_json::Value>>,
    /// The engine calls the window made (`SaveDesign`…), in order.
    asked: RefCell<Vec<(String, serde_json::Value)>>,
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

    /// The Design at `path` with its file state as the engine says it
    /// (saved, changed since, recovered after a crash, its file changed on
    /// disk since): it again, unchanged.
    fn say_saving(&self, path: &str, unsaved: bool, recovered: bool, stale: bool) {
        self.saving.borrow_mut().insert(path.into(), serde_json::json!({ "unsaved": unsaved, "recovered": recovered, "stale": stale }));
        let design = self.designs.borrow()[path].clone();
        self.tell(path, design, &[], "person");
    }

    /// As the engine's `WatchDesign` says it, read as the window reads it.
    fn tell(&self, path: &str, design: Design, changed: &[&str], by: &str) {
        self.designs.borrow_mut().insert(path.into(), design.clone());
        let mut item = serde_json::json!({
            "design": design,
            "changes": [{ "changed": changed, "by": by }],
            "assets": *self.assets.borrow(),
            "path": path,
        });
        if let Some(saving) = self.saving.borrow().get(path) {
            for key in ["unsaved", "recovered", "stale"] {
                item[key] = saving[key].clone();
            }
        }
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
        let assets = self.0.assets.borrow().clone();
        let fonts = Fonts::of_files(&assets.iter().filter_map(Resolved::font_file).collect::<Vec<_>>());
        let saving = FileState { path: Some(path.into()), ..Default::default() };
        (listener.borrow_mut())(now.map(|design| Heard { design, flash: Vec::new(), fonts, assets, saving }).ok_or_else(|| format!("can't open {path}")));
        Some(Box::new(listener))
    }

    fn select(&self, chat: &str, path: &str, ids: &[String]) {
        self.0.selected_said.borrow_mut().push((chat.into(), path.into(), ids.to_vec()));
    }

    fn set_tabs(&self, chat: &str, paths: &[String]) {
        self.0.tabs_said.borrow_mut().push((chat.into(), paths.to_vec()));
    }

    /// As the engine answers (#27, #28, #31's calls), its Designs changed
    /// so and told to whoever follows them.
    fn ask(&self, method: &str, params: serde_json::Value) -> crate::design::Asked {
        let me = &self.0;
        me.asked.borrow_mut().push((method.into(), params.clone()));
        let path = params["path"].as_str().unwrap_or_default().to_owned();
        let answer = match method {
            "NewDesign" => {
                // (As the engine names one: its folder's name, numbered on.)
                let n = me.asked.borrow().iter().filter(|(m, _)| m == "NewDesign").count();
                let name = if n == 1 { "Untitled design".to_string() } else { format!("Untitled design {n}") };
                let made = format!("C:/designs/{name}/{name}.zdesign");
                me.designs.borrow_mut().insert(made.clone(), Design::default());
                me.recent.borrow_mut().insert(0, serde_json::json!({ "name": name, "path": made }));
                Ok(serde_json::json!({ "name": name, "path": made }))
            }
            "RecentDesigns" => Ok(serde_json::json!({ "recent": *me.recent.borrow() })),
            // The person's edits, applied as the engine applies them (the
            // design crate's own), and told to the tab as theirs.
            "EditDesign" => {
                let design = me.designs.borrow().get(&path).cloned().unwrap_or_default();
                let mut draft = zeron_design::edit::Draft::new(design);
                let applied: Result<(), String> = params["edits"].as_array().into_iter().flatten().try_for_each(|edit| {
                    let done = if let Some(id) = edit["update"].as_str() {
                        draft.update(id, edit["props"].clone())
                    } else {
                        let id = edit["move"].as_str().unwrap_or_default();
                        draft.move_to(id, edit["parent"].as_str(), edit["index"].as_u64().map(|i| i as usize))
                    };
                    done.map_err(|e| e.to_string())
                });
                applied.map(|()| {
                    me.say(&path, draft.finish().design);
                    serde_json::json!({ "path": path })
                })
            }
            "UndoDesign" | "RedoDesign" => Ok(serde_json::json!({ "path": path, "done": true })),
            "DesignSaving" => {
                let saving = me.saving.borrow().get(&path).cloned().unwrap_or_default();
                Ok(serde_json::json!({ "path": path, "managed": path.starts_with("C:/designs/"), "unsaved": saving["unsaved"] }))
            }
            "SaveDesign" | "DiscardDesign" => {
                me.say_saving(&path, false, false, false);
                Ok(serde_json::json!({ "path": path, "unsaved": false }))
            }
            "SaveDesignAs" => {
                let to = params["to"].as_str().unwrap_or_default().to_owned();
                let design = me.designs.borrow_mut().remove(&path).unwrap_or_default();
                // (Its followers follow it to its new file, as the engine's.)
                for (followed, _) in me.following.borrow_mut().iter_mut().filter(|(p, _)| *p == path) {
                    followed.clone_from(&to);
                }
                me.say(&to, design);
                Ok(serde_json::json!({ "path": to, "unsaved": false }))
            }
            other => Err(format!("unknown method: {other}")),
        };
        Box::pin(async move { answer })
    }
}

struct Tab {
    app: TestApp,
    canvas: Rc<DesignTabs>,
    engine: Rc<StandIn>,
    /// Where the Canvas starts in the window (past the layers list).
    origin: (f32, f32),
}

/// The fixture's Design tab on `design`, every tile in view drawn.
fn mount(design: Design) -> Tab {
    mount_with(design, Vec::new())
}

/// As [`mount`], the engine naming the files of the Design's assets.
fn mount_with(design: Design, assets: Vec<Resolved>) -> Tab {
    let engine = Rc::new(StandIn::default());
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
    let mut app = app;
    let at = on_screen_of(&mut app, &format!("#design-canvas-{TAB}"));
    let mut tab = Tab { app, canvas, engine, origin: (at.x, at.y) };
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
        ((self.origin.0 + px + x * zoom).floor() as usize, (self.origin.1 + py + y * zoom).floor() as usize)
    }

    /// A click at Canvas point (`x`, `y`), with these keys held.
    fn click_canvas(&mut self, x: f32, y: f32, held: zgx::input::Modifiers) {
        let (sx, sy) = self.on_screen(x, y);
        zgx::input::set_held_modifiers(&self.app.ui.runtime, held);
        self.app.frames(1);
        zgx_script::driver::click_at(&mut self.app.ui, zgx::scene::Rect { x: sx as f32, y: sy as f32, width: 1., height: 1. });
        self.app.frames(2);
        zgx::input::set_held_modifiers(&self.app.ui.runtime, zgx::input::Modifiers::default());
        self.app.frames(2);
    }

    /// What the layers list shows selected (its rows' `selected` read this
    /// tab's selection, `design_selections`).
    fn selected_in_list(&self) -> Vec<String> {
        let selections = self
            .app
            .host
            .states()
            .into_iter()
            .find(|(n, _)| n.ends_with("::design_selections"))
            .map(|(_, v)| v)
            .expect("the tabs' selections");
        match selections.field(TAB) {
            Some(Value::List(ids)) => ids.iter().map(crate::auth::text).collect(),
            _ => Vec::new(),
        }
    }

    /// The last selection said to the engine.
    fn said_selected(&self) -> Vec<String> {
        let said = self.engine.selected_said.borrow();
        let (chat, path, ids) = said.last().cloned().expect("a selection said");
        assert_eq!((chat.as_str(), path.as_str()), ("design-fixture", PATH));
        ids
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
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &zeron_design::assets::NoAssets).expect("a screenshot");
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
    let whole = zeron_design::render::region(&design, &laid_out, &typesetter, &[0], area, 1., 0., (860, 600), &zeron_design::assets::NoAssets).expect("drawn");
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
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &files).expect("a screenshot");
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
    let serif = Resolved {
        key: zeron_design::assets::AssetKey::font("Instrument Serif", 400),
        path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fonts/InstrumentSerif-Regular.ttf"),
    };
    let tab = mount_with(design.clone(), vec![serif.clone()]);
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
        zeron_design::render::region(&design, &laid_out, &typesetter, &[0], area, 1., 0., (860, 600), &zeron_design::assets::NoAssets).expect("drawn")
    };
    let one = whole(&Fonts::of_files(&[serif.font_file().expect("a font")]));
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
    // The engine names each image's file, as an asset; one it hasn't (a
    // download that failed) draws as a placeholder: the window never
    // downloads.
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
    let assets = vec![Resolved { key: zeron_design::assets::AssetKey::image("images/stripes.png"), path: file }];
    let mut tab = mount_with(design.clone(), assets.clone());
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
    let files = zeron_design::assets::Files::new(assets);
    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &files).expect("a screenshot");
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

#[test]
fn instances_and_themed_tokens_draw_in_tiles_as_in_the_engines_screenshot() {
    // A Component (a card of `$surface` holding an `$accent` dot), and a
    // Screen with two Instances of it: one in the Design's light Theme,
    // one set dark.
    let design = design(serde_json::json!({
        "version": "1.0",
        "themes": { "mode": ["light", "dark"] },
        "tokens": {
            "surface": { "type": "color", "value": [
                { "value": "#FFFFFF", "theme": { "mode": "light" } },
                { "value": "#000000", "theme": { "mode": "dark" } },
            ] },
            "accent": { "type": "color", "value": "#4F46E5" },
        },
        "children": [
            { "id": "screen", "type": "frame", "width": 500, "height": 400, "fill": "#F3F4F6", "children": [
                { "id": "one", "type": "instance", "ref": "card", "x": 40, "y": 40 },
                { "id": "two", "type": "instance", "ref": "card", "x": 260, "y": 40, "theme": { "mode": "dark" } },
            ] },
            { "id": "card", "type": "frame", "reusable": true, "x": 600, "width": 200, "height": 100, "fill": "$surface",
              "children": [{ "id": "dot", "type": "rectangle", "x": 20, "y": 20, "width": 60, "height": 60, "fill": "$accent" }] },
        ],
    }));
    let mut tab = mount(design.clone());
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);
    let shown = tab.picture();

    let shot = zeron_design::render::screenshot(&design, "screen", 10_000., &Fonts::default(), &zeron_design::assets::NoAssets).expect("a screenshot");
    let decoder = png::Decoder::new(std::io::Cursor::new(shot.png));
    let mut reader = decoder.read_info().expect("a PNG");
    let mut expected = vec![0; reader.output_buffer_size().expect("its size")];
    reader.next_frame(&mut expected).expect("its pixels");
    let (w, h) = (shot.width as usize, shot.height as usize);
    let (left, top) = tab.on_screen(0., 0.);
    let off = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let want = &expected[(y * w + x) * 4..][..4];
            want.iter().zip(shown.rgba((left + x, top + y))).any(|(a, b)| a.abs_diff(b) > 2)
        })
        .count();
    assert_eq!(off, 0, "pixels differ from the screenshot");
    // The light Instance's card white, the dark one's black, both dots the
    // accent.
    assert_eq!(shown.rgba(tab.on_screen(50., 50.)), WHITE);
    assert_eq!(shown.rgba(tab.on_screen(270., 50.)), [0, 0, 0, 255]);
    assert_eq!(shown.rgba(tab.on_screen(90., 90.)), INDIGO);
    assert_eq!(shown.rgba(tab.on_screen(310., 90.)), INDIGO);
}

#[test]
fn an_image_and_a_font_set_only_in_an_instances_overrides_draw_in_tiles() {
    // A Component of a plain swatch and a line of Geist text; its Instance
    // overrides the swatch with an image and the text with a font the
    // engine names, neither anywhere else in the Design.
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("stripes.png");
    std::fs::write(&file, stripes()).expect("the image written");
    let design = design(serde_json::json!({ "version": "1.0", "children": [
        { "id": "screen", "type": "frame", "width": 600, "height": 300, "fill": "#FFFFFF", "children": [
            { "id": "one", "type": "instance", "ref": "card", "x": 40, "y": 40, "descendants": {
                "swatch": { "fill": { "type": "image", "url": "images/stripes.png", "mode": "stretch" } },
                "words": { "fontFamily": "Instrument Serif" },
            } },
        ] },
        { "id": "card", "type": "frame", "reusable": true, "y": 340, "width": 500, "height": 200, "fill": "#F3F4F6", "children": [
            { "id": "swatch", "type": "rectangle", "x": 20, "y": 20, "width": 160, "height": 160, "fill": "#9CA3AF" },
            { "id": "words", "type": "text", "x": 200, "y": 60, "content": "Hamburgefonstiv", "fontSize": 40, "fill": "#111827" },
        ] },
    ] }));
    let serif = Resolved {
        key: zeron_design::assets::AssetKey::font("Instrument Serif", 400),
        path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fonts/InstrumentSerif-Regular.ttf"),
    };
    let image = Resolved { key: zeron_design::assets::AssetKey::image("images/stripes.png"), path: file };
    let mut tab = mount_with(design.clone(), vec![serif.clone(), image.clone()]);
    assert_eq!(tab.canvas.view(TAB).unwrap().2, 1.);
    let shown = tab.picture();

    let images = zeron_design::assets::Files::new([image]);
    let shot = |fonts: &Fonts| {
        let shot = zeron_design::render::screenshot(&design, "screen", 10_000., fonts, &images)
            .expect("a screenshot");
        let decoder = png::Decoder::new(std::io::Cursor::new(shot.png));
        let mut reader = decoder.read_info().expect("a PNG");
        let mut pixels = vec![0; reader.output_buffer_size().expect("its size")];
        reader.next_frame(&mut pixels).expect("its pixels");
        pixels
    };
    let expected = shot(&Fonts::of_files(&[serif.font_file().expect("a font")]));
    // The override's font is drawn, not the Component's Geist.
    assert!(shot(&Fonts::default()) != expected, "the serif changes the text");
    let (left, top) = tab.on_screen(0., 0.);
    let off = (0..300usize)
        .flat_map(|y| (0..600usize).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let want = &expected[(y * 600 + x) * 4..][..4];
            want.iter().zip(shown.rgba((left + x, top + y))).any(|(a, b)| a.abs_diff(b) > 2)
        })
        .count();
    assert!(off <= 600 * 300 / 1000, "{off} pixels differ from the screenshot");
    // The image's first and last stripes, stretched over the swatch.
    assert_eq!(shown.rgba(tab.on_screen(70., 140.)), [255, 0, 0, 255]);
    assert_eq!(shown.rgba(tab.on_screen(210., 140.)), [0, 0, 0, 255]);
}

// ---------------------------------------------------------------------------
// Opening and saving (#31): the real panel (ui/panel_fixture.luau, its other
// hosts stood in for) with the Design tab's real host over the stand-in.

struct Panel {
    app: TestApp,
    engine: Rc<StandIn>,
}

fn panel() -> Panel {
    let engine = Rc::new(StandIn::default());
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/panel_fixture.luau");
    let mut made = None;
    let app = TestApp::with_desktop(|runtime| {
        let host = ScriptHost::new(runtime, path);
        made = Some(crate::design::register(&host, Box::new(Engine(engine.clone()))));
        host
    });
    let (_tabs, pump) = made.expect("registered");
    app.executor.borrow_mut().spawn(pump);
    app.ui.scene.borrow_mut().resize(1200., 800.);
    app.frames(10);
    assert_eq!(app.error(), None, "the fixture failed");
    let mut panel = Panel { app, engine };
    assert!(panel.app.wait_until(|app| app.find("#panel-open-design").is_some()), "the panel's home:\n{}", panel.app.tree());
    panel
}

impl Panel {
    fn asked(&self, method: &str) -> Vec<serde_json::Value> {
        self.engine.asked.borrow().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
    }

    /// A Design tab on the Design at `path`, opened from Recent designs.
    fn open_recent(&mut self, name: &str, path: &str, design: Design) {
        self.engine.say(path, design);
        self.engine.recent.borrow_mut().insert(0, serde_json::json!({ "name": name, "path": path }));
        self.app.click("#panel-open-design");
        assert!(self.app.wait_until(|app| app.find(&format!("~\"{name}\"")).is_some()), "{}", self.app.tree());
        self.app.click(&format!("role:button \"{name}\""));
        assert!(self.app.wait_until(|app| app.find(".design-canvas").is_some()), "{}", self.app.tree());
    }
}

#[test]
fn the_design_row_opens_a_tab_that_offers_new_recent_and_open_and_nothing_opens_on_its_own() {
    let mut p = panel();
    // Agents made Designs (no tab open): nothing opened, and Recent lists them.
    p.engine.say("C:/designs/Landing/Landing.zdesign", landing("#EF4444", false));
    p.engine.recent.borrow_mut().push(serde_json::json!({ "name": "Landing", "path": "C:/designs/Landing/Landing.zdesign" }));
    p.app.frames(10);
    assert!(p.app.find(".design-canvas").is_none() && p.app.find("~\"Start a Design\"").is_none(), "nothing opens on its own");

    // The Design row: a Design tab with New, Open and Recent.
    p.app.click("#panel-open-design");
    assert!(p.app.wait_until(|app| app.find("~\"Start a Design\"").is_some()), "{:?}
{}", p.app.error(), p.app.tree());
    assert!(p.app.find("role:button \"New design\"").is_some() && p.app.find("role:button \"Open design\"").is_some());
    assert!(p.app.wait_until(|app| app.find("role:button \"Landing\"").is_some()), "{}", p.app.tree());

    // New design: made in the managed folder, shown in this tab.
    p.app.click("role:button \"New design\"");
    assert!(p.app.wait_until(|app| app.find(".design-canvas").is_some()), "{}", p.app.tree());
    assert_eq!(p.asked("NewDesign").len(), 1);
    assert!(p.app.wait_until(|app| app.find("role:tab \"Untitled design\"").is_some()), "titled by its name:\n{}", p.app.tree());

    // Open design: a .zdesign picked anywhere, in another tab.
    p.engine.say("C:/work/app/checkout.zdesign", landing("#22C55E", false));
    // (A Design tab's + opens another Design tab.)
    p.app.click("role:button \"New Tab\"");
    assert!(p.app.wait_until(|app| app.find("role:button \"Open design\"").is_some()), "{}", p.app.tree());
    p.app.desktop.as_ref().unwrap().pick(["C:/work/app/checkout.zdesign"]);
    p.app.click("role:button \"Open design\"");
    assert!(p.app.wait_until(|app| app.find("~\"checkout\"").is_some() && app.find(".design-canvas").is_some()), "{}", p.app.tree());
    let filters = format!("{:?}", p.app.desktop.as_ref().unwrap().asked());
    assert!(filters.contains("zdesign"), "{filters}");

    // Recent designs: one an agent made with no tab open.
    p.app.click("role:button \"New Tab\"");
    assert!(p.app.wait_until(|app| app.find("role:button \"Landing\"").is_some()), "{}", p.app.tree());
    p.app.click("role:button \"Landing\"");
    assert!(p.app.wait_until(|app| app.find("~\"Landing\"").is_some() && app.find(".design-canvas").is_some()), "{}", p.app.tree());
}

#[test]
fn the_tab_follows_the_engines_saving_ctrl_s_saves_and_closing_asks_save_or_discard() {
    let mut p = panel();
    p.open_recent("Mine", "C:/work/mine.zdesign", landing("#EF4444", false));
    let unsaved = "role:img \"Unsaved changes\"";
    assert!(p.app.find(unsaved).is_none());

    // Changed and not saved (a Design of the person's own): the marker.
    p.engine.say_saving("C:/work/mine.zdesign", true, false, false);
    assert!(p.app.wait_until(|app| app.find(unsaved).is_some()), "{}", p.app.tree());
    // ⌘S (Ctrl+S) saves it: the engine writes it, and the marker goes.
    // (Its shortcut's, bound as the window binds the commands' keys:
    // src/shortcut_tests.rs presses them.)
    let save = p.app.host.commands().get().into_iter().find(|c| c.id == "panel.design.save").expect("the Save Design command");
    assert_eq!(save.key.as_deref(), Some("cmd+s"));
    assert!(p.app.host.run_command("panel.design.save"));
    assert!(p.app.wait_until(|app| app.find(unsaved).is_none()), "{}", p.app.tree());
    assert_eq!(p.asked("SaveDesign"), [serde_json::json!({ "path": "C:/work/mine.zdesign" })]);

    // Recovered after a crash, its file changed on disk since: it says so;
    // Discard puts the Design back as its file holds it.
    p.engine.say_saving("C:/work/mine.zdesign", true, true, true);
    assert!(p.app.wait_until(|app| app.find("~\"Recovered changes\"").is_some()), "{}", p.app.tree());
    assert!(p.app.find("~\"the file has changed on disk since\"").is_some(), "{}", p.app.tree());
    p.app.click("role:button \"Discard\"");
    assert!(p.app.wait_until(|app| app.find("~\"Recovered changes\"").is_none()), "{}", p.app.tree());
    assert_eq!(p.asked("DiscardDesign").len(), 1);

    // Recovered again: Keep puts the notice away, and it stays away when
    // the tab comes back.
    p.engine.say_saving("C:/work/mine.zdesign", true, true, false);
    assert!(p.app.wait_until(|app| app.find("~\"Recovered changes\"").is_some()), "{}", p.app.tree());
    p.app.click("role:button \"Keep\"");
    assert!(p.app.wait_until(|app| app.find("~\"Recovered changes\"").is_none()));
    // (The + of a Design tab opens another Design tab; ⌘S has nothing to
    // save in it yet.)
    p.app.click("role:button \"New Tab\"");
    assert!(p.app.wait_until(|app| app.find("~\"Start a Design\"").is_some()), "{}", p.app.tree());
    let saves = p.asked("SaveDesign").len();
    p.app.host.run_command("panel.design.save");
    p.app.frames(5);
    assert_eq!(p.asked("SaveDesign").len(), saves);
    p.app.click("role:tab \"mine\"");
    assert!(p.app.wait_until(|app| app.find(".design-canvas").is_some()), "{}", p.app.tree());
    p.app.frames(5);
    assert!(p.app.find("~\"Recovered changes\"").is_none(), "kept stays kept:
{}", p.app.tree());
    // (Off the tab, whose × stands in its dot under the pointer.)
    p.app.hover("#outside");

    // Closing with unsaved changes asks Save or Discard; Save writes it,
    // then the tab closes.
    p.engine.say_saving("C:/work/mine.zdesign", true, false, false);
    assert!(p.app.wait_until(|app| app.find(unsaved).is_some()));
    assert!(p.app.host.run_command("test.close_active_tab"));
    assert!(p.app.wait_until(|app| app.find("~\"Save changes to this Design?\"").is_some()), "{}", p.app.tree());
    // (Once it's in: a dialog arriving takes no click.)
    p.app.frames(20);
    p.app.click("#panel-design-close-question >> role:button \"Save\"");
    assert!(p.app.wait_until(|app| app.find(".design-canvas").is_none()), "{}", p.app.tree());
    assert_eq!(p.asked("SaveDesign").len(), 2);
}

#[test]
fn closing_asks_only_for_a_design_of_your_own_and_discard_closes_too() {
    let mut p = panel();
    let unsaved = "role:img \"Unsaved changes\"";
    // One of Zeron's own, a moment before it saves itself: closed at once.
    p.open_recent("Landing", "C:/designs/Landing/Landing.zdesign", landing("#EF4444", false));
    p.engine.say_saving("C:/designs/Landing/Landing.zdesign", true, false, false);
    assert!(p.app.wait_until(|app| app.find(unsaved).is_some()));
    assert!(p.app.host.run_command("test.close_active_tab"));
    assert!(p.app.wait_until(|app| app.find(".design-canvas").is_none()), "{}", p.app.tree());
    assert!(p.app.find("~\"Save changes to this Design?\"").is_none());

    // One of your own: asked; Discard puts it back as its file holds it,
    // then closes.
    p.open_recent("Mine", "C:/work/mine.zdesign", landing("#EF4444", false));
    p.engine.say_saving("C:/work/mine.zdesign", true, false, false);
    assert!(p.app.wait_until(|app| app.find(unsaved).is_some()));
    assert!(p.app.host.run_command("test.close_active_tab"));
    assert!(p.app.wait_until(|app| app.find("~\"Save changes to this Design?\"").is_some()), "{}", p.app.tree());
    p.app.frames(20);
    p.app.click("#panel-design-close-question >> role:button \"Discard\"");
    assert!(p.app.wait_until(|app| app.find(".design-canvas").is_none()), "{}", p.app.tree());
    assert_eq!(p.asked("DiscardDesign"), [serde_json::json!({ "path": "C:/work/mine.zdesign" })]);
    assert!(p.asked("SaveDesign").is_empty());
}

// ---------------------------------------------------------------------------
// Selection and layers (#32).

#[test]
fn a_click_on_the_canvas_selects_and_the_layers_list_agrees_and_both_reach_the_engine() {
    let mut tab = mount(landing("#EF4444", false));
    let none = zgx::input::Modifiers::default();
    let shift = zgx::input::Modifiers { shift: true, ..Default::default() };
    let deep = zgx::input::Modifiers { control: true, ..Default::default() };
    // The layers list: the Design's tree, a row a node.
    for id in ["screen", "hero", "badge", "chips", "chip2"] {
        assert!(tab.app.find(&format!("#design-layer-{TAB}-{id}")).is_some(), "{id}:\n{}", tab.app.tree());
    }

    // A click on the badge: selected, outlined where it is, and the engine
    // told (for the chat's agent).
    tab.click_canvas(780., 530., none);
    assert_eq!(tab.said_selected(), ["badge"]);
    let outline = on_screen_of(&mut tab.app, &format!("#design-selected-{TAB}-badge"));
    let (left, top) = tab.on_screen(700., 450.);
    assert!(near(outline.x, left as f32) && near(outline.y, top as f32), "{outline:?}");
    // (The layers list's rows show selected from the same selection.)
    assert_eq!(tab.selected_in_list(), ["badge"]);

    // Shift adds the hero; Shift again takes it away.
    tab.click_canvas(150., 100., shift);
    assert_eq!(tab.said_selected(), ["badge", "hero"]);
    assert!(tab.app.find(&format!("#design-selected-{TAB}-hero")).is_some());
    tab.click_canvas(150., 100., shift);
    assert_eq!(tab.said_selected(), ["badge"]);

    // Within a Screen a click picks its child; ⌘/Ctrl the innermost; the
    // Screen between its children; nothing on the bare Canvas.
    let chip2 = (100. + 40. + 20. + 70. + 30., 50. + 300. + 20. + 30.);
    tab.click_canvas(chip2.0, chip2.1, none);
    assert_eq!(tab.said_selected(), ["chips"]);
    tab.click_canvas(chip2.0, chip2.1, deep);
    assert_eq!(tab.said_selected(), ["chip2"]);
    tab.click_canvas(500., 300., none);
    assert_eq!(tab.said_selected(), ["screen"]);
    tab.click_canvas(50., 300., none);
    assert_eq!(tab.said_selected(), Vec::<String>::new());
    assert!(tab.app.find(".design-selected").is_none());

    // The layers list selects too, and the Canvas outlines it.
    tab.app.click(&format!("#design-layer-{TAB}-hero"));
    tab.app.frames(2);
    assert_eq!(tab.said_selected(), ["hero"]);
    assert_eq!(tab.selected_in_list(), ["hero"]);
    assert!(tab.app.find(&format!("#design-selected-{TAB}-hero")).is_some(), "{}", tab.app.tree());
    // Shift in the list adds, as on the Canvas.
    zgx::input::set_held_modifiers(&tab.app.ui.runtime, shift);
    tab.app.click(&format!("#design-layer-{TAB}-badge"));
    zgx::input::set_held_modifiers(&tab.app.ui.runtime, none);
    tab.app.frames(2);
    assert_eq!(tab.said_selected(), ["hero", "badge"]);
    // A node deleted since leaves the selection.
    let mut gone = landing("#EF4444", false);
    let zeron_design::model::Kind::Frame(screen) = &mut gone.children[0].kind else { panic!("a frame") };
    screen.children.retain(|n| n.id != "badge");
    tab.engine.say(PATH, gone);
    tab.app.frames(4);
    assert_eq!(tab.said_selected(), ["hero"]);
    // (A drag pans, and selects nothing.)
    let said = tab.engine.selected_said.borrow().len();
    let (sx, sy) = tab.on_screen(780., 530.);
    zgx_script::driver::drag_between(&mut tab.app.ui, (sx as f32, sy as f32), (sx as f32 + 80., sy as f32 + 40.));
    tab.app.frames(2);
    assert_eq!(tab.engine.selected_said.borrow().len(), said);
}

#[test]
fn a_click_hits_a_turned_node_where_it_is_drawn_and_a_lone_node_on_the_canvas_whole() {
    // A square turned 45° in a Screen, and a group-like rectangle alone on
    // the Canvas, holding nothing.
    let design = design(serde_json::json!({ "version": "1.0", "children": [
        { "id": "screen", "type": "frame", "width": 600, "height": 400, "fill": "#FFFFFF", "children": [
            { "id": "turned", "type": "rectangle", "x": 300, "y": 100, "width": 100, "height": 100, "fill": "#4F46E5", "rotation": 45 },
        ] },
        { "id": "lone", "type": "rectangle", "x": 700, "width": 100, "height": 100, "fill": "#EF4444" },
    ] }));
    let mut tab = mount(design);
    let none = zgx::input::Modifiers::default();
    // Turned about its top-left corner (300, 100), it's drawn up and to the
    // right of it, where its upright box never reaches.
    tab.click_canvas(370., 60., none);
    assert_eq!(tab.said_selected(), ["turned"]);
    // Inside its upright box but off the turned square: the Screen.
    tab.click_canvas(310., 190., none);
    assert_eq!(tab.said_selected(), ["screen"]);
    tab.click_canvas(750., 50., none);
    assert_eq!(tab.said_selected(), ["lone"]);
}

// ---------------------------------------------------------------------------
// Hand edits (#33): each sent to the engine as a change.

impl Tab {
    fn edits(&self) -> Vec<serde_json::Value> {
        self.engine.asked.borrow().iter().filter(|(m, _)| m == "EditDesign").map(|(_, p)| p["edits"].clone()).collect()
    }

    /// A drag from Canvas point `from` by (`dx`, `dy`) Canvas units.
    fn drag_canvas(&mut self, from: (f32, f32), (dx, dy): (f32, f32)) {
        let (sx, sy) = self.on_screen(from.0, from.1);
        let (_, _, zoom) = self.canvas.view(TAB).unwrap();
        zgx_script::driver::drag_between(&mut self.app.ui, (sx as f32, sy as f32), (sx as f32 + dx * zoom, sy as f32 + dy * zoom));
        self.app.frames(3);
    }
}

#[test]
fn moving_a_selected_node_sends_its_new_place_or_its_new_order_in_an_auto_layout() {
    let mut tab = mount(landing("#EF4444", false));
    let none = zgx::input::Modifiers::default();
    // The badge, in a Screen that lays nothing out: its new x and y.
    tab.click_canvas(780., 530., none);
    tab.drag_canvas((780., 530.), (40., -30.));
    assert_eq!(tab.edits().last().unwrap(), &serde_json::json!([{ "update": "badge", "props": { "x": 640, "y": 370 } }]));
    // Shown where it went (the engine told the tab, as the person's).
    tab.settle();
    assert_eq!(tab.picture().rgba(tab.on_screen(100. + 640. + 10., 50. + 370. + 10.)), RED);
    // The row's first chip, in an auto layout: dropped past the second,
    // it's second in the order.
    let deep = zgx::input::Modifiers { control: true, ..Default::default() };
    let chip1 = (100. + 40. + 20. + 30., 50. + 300. + 20. + 30.);
    tab.click_canvas(chip1.0, chip1.1, deep);
    assert_eq!(tab.said_selected(), ["chip1"]);
    tab.drag_canvas(chip1, (75., 0.));
    assert_eq!(tab.edits().last().unwrap(), &serde_json::json!([{ "move": "chip1", "parent": "chips", "index": 1 }]));
}

#[test]
fn text_edits_in_place_with_the_caret_on_the_drawn_glyphs_and_each_change_is_sent() {
    let mut design = landing("#EF4444", false);
    let zeron_design::model::Kind::Frame(screen) = &mut design.children[0].kind else { panic!("a frame") };
    screen.children.push(
        serde_json::from_value(serde_json::json!({
            "id": "title", "type": "text", "x": 300, "y": 40, "content": "Hello", "fontSize": 40, "fill": "#111827",
        }))
        .unwrap(),
    );
    let written = design.clone();
    let mut tab = mount(design);
    let none = zgx::input::Modifiers::default();
    // Selected, then pressed again at once: editing, the caret at its end.
    // (In the Screen at (100, 50), the text at (300, 40): (400, 90) on the
    // Canvas.)
    tab.click_canvas(420., 110., none);
    assert_eq!(tab.said_selected(), ["title"]);
    tab.click_canvas(420., 110., none);
    let caret = on_screen_of(&mut tab.app, &format!("#design-caret-{TAB}"));
    // Where the drawn word ends: its last column of ink.
    let shown = tab.picture();
    let (_, top) = tab.on_screen(0., 110.);
    let (left, _) = tab.on_screen(400., 110.);
    let (right, _) = tab.on_screen(700., 110.);
    let ink_right = (left..right).rev().find(|&x| {
        let [r, _, _, a] = shown.rgba((x, top));
        a > 200 && r < 100
    });
    let ink_right = ink_right.expect("the word drawn");
    // (Just past it: the caret's at the last glyph's advance, which the
    // "o"'s side bearing puts a few pixels beyond its ink.)
    let past = caret.x - ink_right as f32;
    assert!((-1. ..=7.).contains(&past), "the caret at {} against the ink's end at {ink_right}", caret.x);

    // Text that comes in as text (what a keyboard layout, Unikey's
    // Telex, a SendInput of Unicode give) is sent as the text's new words.
    let words = |tab: &Tab| tab.edits().last().unwrap()[0]["props"]["content"].clone();
    tab.app.ui.dispatch(InputEvent::Text("!".into()));
    tab.app.frames(2);
    assert_eq!(tab.edits().last().unwrap(), &serde_json::json!([{ "update": "title", "props": { "content": "Hello!" } }]));
    // Backspace, and the caret moved back one place.
    tab.app.key(zgx::input::Key::Backspace);
    tab.app.key(zgx::input::Key::ArrowLeft);
    tab.app.key(zgx::input::Key::Backspace);
    tab.app.frames(2);
    assert_eq!(words(&tab), "Helo");
    // An input method's: nothing sent while it composes, its words as it
    // commits them, where the caret was.
    let sent = tab.edits().len();
    tab.app.ui.dispatch(InputEvent::ImePreedit { text: "ê".into(), cursor: Some((2, 2)) });
    tab.app.frames(2);
    assert_eq!(tab.edits().len(), sent);
    tab.app.ui.dispatch(InputEvent::ImeCommit("ế".into()));
    tab.app.frames(2);
    assert_eq!(words(&tab), "Helếo");
    // The caret drawn after it: past the "ế", before the "o".
    let caret = on_screen_of(&mut tab.app, &format!("#design-caret-{TAB}"));
    // (Where the design crate's own layout of the words puts place 4.)
    let mut typed = written.clone();
    let zeron_design::model::Kind::Frame(screen) = &mut typed.children[0].kind else { panic!("a frame") };
    let title = screen.children.iter_mut().find(|n| n.id == "title").unwrap();
    let zeron_design::model::Kind::Text(text) = &mut title.kind else { panic!("a text") };
    text.content = "Helếo".into();
    let text = text.clone();
    let place_4 = Fonts::default().typesetter(&typed).carets(&text, (1000., 100.)).caret(4).x;
    let (x, _, zoom) = tab.canvas.view(TAB).unwrap();
    let expected = tab.origin.0 + x + (400. + place_4) * zoom;
    assert!((caret.x + 1. - expected).abs() <= 1.5, "the caret at {} against place 4 at {expected}", caret.x);
    // Escape ends it; edited again, a press on the text places the caret
    // there, and typing goes on.
    tab.app.key(zgx::input::Key::Escape);
    tab.app.frames(2);
    assert!(tab.app.find(&format!("#design-caret-{TAB}")).is_none());
    tab.click_canvas(420., 110., none);
    assert!(tab.app.find(&format!("#design-caret-{TAB}")).is_some());
    tab.click_canvas(401., 110., none);
    tab.app.ui.dispatch(InputEvent::Text("Đ".into()));
    tab.app.frames(2);
    assert_eq!(words(&tab), "ĐHelếo");

    // Select all, copy, paste and cut, as in any field (by the field's
    // editor as the window's Edit menu and its keys do them: headless, the
    // keys have no clipboard of their own).
    let field = tab.app.target(&format!("#design-text-field-{TAB}")).unwrap();
    let editor = tab.app.ui.editor_within(field).expect("an editor");
    tab.app.keys("cmd+a");
    assert_eq!(editor.selection(), (0, "ĐHelếo".len()));
    assert_eq!(editor.copy(), "ĐHelếo");
    tab.app.ui.paste_into(&editor, Some("Xin chào"));
    tab.app.frames(2);
    assert_eq!(words(&tab), "Xin chào");
    tab.app.keys("cmd+a");
    assert_eq!(editor.cut(), "Xin chào");
    tab.app.frames(2);
    assert_eq!(words(&tab), "");
    // (Each edit's changes after its first join it: one step to undo each.)
    let joins: Vec<_> = tab.engine.asked.borrow().iter().filter(|(m, _)| m == "EditDesign").map(|(_, p)| p["join"].clone()).collect();
    assert_eq!(joins, [false, true, true, true, false, true, true]);
    // Left for another field, the edit ends.
    tab.app.click("role:textbox \"Fill\"");
    tab.app.frames(2);
    assert!(tab.app.find(&format!("#design-caret-{TAB}")).is_none());
}

#[test]
fn the_properties_panel_sends_each_property_as_a_change_and_undo_and_redo_reach_the_engine() {
    let mut tab = mount(landing("#EF4444", false));
    tab.click_canvas(150., 100., zgx::input::Modifiers::default());
    assert!(tab.app.wait_until(|app| app.find(&format!("#design-props-{TAB}")).is_some()), "{}", tab.app.tree());
    tab.app.fill("role:textbox \"Fill\"", "#22C55E");
    tab.app.key(zgx::input::Key::Enter);
    tab.app.frames(2);
    assert_eq!(tab.edits().last().unwrap(), &serde_json::json!([{ "update": "hero", "props": { "fill": "#22C55E" } }]));
    tab.app.fill("role:textbox \"Width\"", "240");
    tab.app.key(zgx::input::Key::Enter);
    tab.app.frames(2);
    assert_eq!(tab.edits().last().unwrap(), &serde_json::json!([{ "update": "hero", "props": { "width": 240 } }]));
    // A value set from outside (assistive technology's, UI Automation's
    // ValuePattern), no Enter: sent once it stops changing.
    let fill = tab.app.target("role:textbox \"Fill\"").unwrap();
    assert!(tab.app.ui.set_accessible_value(fill, "#3B82F6"));
    let sent = serde_json::json!([{ "update": "hero", "props": { "fill": "#3B82F6" } }]);
    let engine = tab.engine.clone();
    let last_edit = move || engine.asked.borrow().iter().rev().find(|(m, _)| m == "EditDesign").map(|(_, p)| p["edits"].clone());
    assert!(tab.app.wait_until(|_| last_edit() == Some(sent.clone())), "{:?}", tab.edits());

    // ⌘Z and ⌘⇧Z: the Design's one history (their shortcuts bound as the
    // window binds commands' keys: src/shortcut_tests.rs presses them).
    let keys: Vec<_> = tab.app.host.commands().get().into_iter().filter(|c| c.id.starts_with("panel.design.")).map(|c| (c.id, c.key)).collect();
    assert!(keys.contains(&("panel.design.undo".into(), Some("cmd+z".into()))), "{keys:?}");
    assert!(keys.contains(&("panel.design.redo".into(), Some("cmd+shift+z".into()))), "{keys:?}");
    tab.app.host.run_command("panel.design.undo");
    tab.app.host.run_command("panel.design.redo");
    tab.app.frames(2);
    let asked: Vec<String> = tab.engine.asked.borrow().iter().map(|(m, _)| m.clone()).filter(|m| m.ends_with("oDesign")).collect();
    assert_eq!(asked, ["UndoDesign", "RedoDesign"]);
}

#[test]
fn switching_theme_draws_the_design_in_that_themes_token_values() {
    let design = design(serde_json::json!({
        "version": "1.0",
        "themes": { "mode": ["light", "dark"] },
        "tokens": { "surface": { "type": "color", "value": [
            { "value": "#FFFFFF", "theme": { "mode": "light" } },
            { "value": "#000000", "theme": { "mode": "dark" } },
        ] } },
        "children": [{ "id": "screen", "type": "frame", "width": 600, "height": 400, "fill": "$surface" }],
    }));
    let mut tab = mount(design);
    assert_eq!(tab.picture().rgba(tab.on_screen(300., 200.)), WHITE);
    tab.app.click(&format!("#design-theme-{TAB}-mode >> \"dark\""));
    tab.app.frames(4);
    tab.settle();
    assert_eq!(tab.picture().rgba(tab.on_screen(300., 200.)), [0, 0, 0, 255]);
    // (The tab's alone: nothing was asked of the engine.)
    assert!(tab.edits().is_empty());
}

// ---------------------------------------------------------------------------
// In-place text and the focus, as a person's mouse and keys give it (#33c):
// presses through zgx's own input (down, up, down, up, as a double click
// comes, nothing run between), so the focus moves, and fields hear their
// focus and blur, as zgx moves them in a window.

impl Tab {
    /// What has the keyboard focus: its node.
    fn focused(&self) -> Option<zgx::scene::NodeId> {
        self.app.ui.input.focused()
    }

    /// The node `target` names.
    fn node(&mut self, target: &str) -> zgx::scene::NodeId {
        self.app.target(target).unwrap_or_else(|e| panic!("{e}\n{}", self.app.tree()))
    }

    /// A double click at Canvas point (`x`, `y`): two presses and releases
    /// with nothing between, then what they started.
    fn double_click_canvas(&mut self, x: f32, y: f32) {
        let (sx, sy) = self.on_screen(x, y);
        let at = zgx::scene::Rect { x: sx as f32, y: sy as f32, width: 0., height: 0. };
        zgx_script::driver::double_click_at(&mut self.app.ui, at);
        self.app.frames(3);
    }

    /// The Fill field's text, as assistive technology reads it.
    fn fill_shown(&mut self) -> String {
        let fill = self.node("role:textbox \"Fill\"");
        self.app.ui.editor_within(fill).expect("an editor").value().get()
    }
}

fn with_title(content: &str) -> Design {
    let mut design = landing("#EF4444", false);
    let zeron_design::model::Kind::Frame(screen) = &mut design.children[0].kind else { panic!("a frame") };
    screen.children.push(
        serde_json::from_value(serde_json::json!({
            "id": "title", "type": "text", "x": 300, "y": 40, "content": content, "fontSize": 40, "fill": "#111827",
        }))
        .unwrap(),
    );
    design
}

#[test]
fn a_double_click_edits_the_text_and_typing_past_the_screen_stays_in_it_as_one_step() {
    let mut tab = mount(with_title("Hi"));
    // From nothing selected and no field focused, the title double-clicked:
    // selected by the first click, edited from the second, its caret at the
    // end.
    tab.double_click_canvas(410., 110.);
    assert_eq!(tab.said_selected(), ["title"]);
    let field = tab.node(&format!("#design-text-field-{TAB}"));
    assert_eq!(tab.focused(), Some(field), "the hidden field has the keys");
    let fill = tab.fill_shown();

    // Typed a character at a time (SendInput's Unicode), on past the
    // Screen's right edge (800 wide from 100: the title from 400).
    let typed = " Xin chào Đà Nẵng, Việt Nam ơi";
    for c in typed.chars() {
        tab.app.ui.dispatch(InputEvent::Text(c.to_string()));
        tab.app.frames(1);
    }
    tab.settle();
    let edits = tab.edits();
    assert_eq!(edits.last().unwrap()[0]["props"]["content"], format!("Hi{typed}"));
    assert_eq!(tab.focused(), Some(tab.node(&format!("#design-text-field-{TAB}"))), "still the field's");
    assert!(tab.app.find(&format!("#design-caret-{TAB}")).is_some(), "still editing");
    // (One burst, one undo step: every change after the first joins it.)
    let joins: Vec<_> = tab.engine.asked.borrow().iter().filter(|(m, _)| m == "EditDesign").map(|(_, p)| p["join"].clone()).collect();
    assert_eq!(joins[0], false);
    assert!(joins[1..].iter().all(|j| j == true), "{joins:?}");
    assert_eq!(tab.fill_shown(), fill, "nothing typed into Fill");

    // Escape: the edit over, the Canvas has the focus (not the next field).
    tab.app.key(zgx::input::Key::Escape);
    tab.app.frames(2);
    assert!(tab.app.find(&format!("#design-caret-{TAB}")).is_none());
    assert_eq!(tab.focused(), Some(tab.node(&format!("#design-canvas-{TAB}"))));
}

#[test]
fn select_all_and_paste_go_into_the_text_edited_not_a_property_field() {
    let mut tab = mount(with_title("Hello"));
    // The title clicked, then double-clicked (the first press of the double
    // click, so soon after the click, edits it; the second puts the caret
    // where it lands).
    tab.click_canvas(420., 110., zgx::input::Modifiers::default());
    tab.double_click_canvas(420., 110.);
    // Ctrl+A (`cmd` is Control off macOS), then Ctrl+V as the window
    // does it: into the focused editor.
    tab.app.keys("cmd+a");
    let focused = tab.focused().expect("a focus");
    assert_eq!(focused, tab.node(&format!("#design-text-field-{TAB}")));
    let editor = tab.app.ui.editor_within(focused).expect("an editor");
    assert_eq!(editor.selection(), (0, "Hello".len()));
    tab.app.ui.paste_into(&editor, Some("Xin chào Đà Nẵng"));
    tab.app.frames(2);
    assert_eq!(tab.edits().last().unwrap()[0], serde_json::json!({ "update": "title", "props": { "content": "Xin chào Đà Nẵng" } }));
    assert!(tab.edits().iter().all(|e| e[0]["props"].get("fill").is_none()), "{:?}", tab.edits());
}

#[test]
fn the_canvas_takes_the_focus_from_a_property_field_and_escape_takes_back_whats_typed() {
    let mut tab = mount(with_title("Hello"));
    tab.click_canvas(420., 110., zgx::input::Modifiers::default());
    let fill = tab.node("role:textbox \"Fill\"");
    // A value set from outside, then the field focused and junk typed.
    assert!(tab.app.ui.set_accessible_value(fill, "#0F172A"));
    let sent = serde_json::json!([{ "update": "title", "props": { "fill": "#0F172A" } }]);
    let engine = tab.engine.clone();
    let last_edit = move || engine.asked.borrow().iter().rev().find(|(m, _)| m == "EditDesign").map(|(_, p)| p["edits"].clone());
    assert!(tab.app.wait_until(|_| last_edit() == Some(sent.clone())), "{:?}", tab.edits());
    tab.settle();
    tab.app.click("role:textbox \"Fill\"");
    tab.app.ui.dispatch(InputEvent::Text("zzz".into()));
    tab.app.frames(2);
    assert!(tab.fill_shown().ends_with("zzz"));
    // Escape: the Design's value again, nothing sent.
    let sent_before = tab.edits().len();
    tab.app.key(zgx::input::Key::Escape);
    tab.app.frames(2);
    assert_eq!(tab.fill_shown(), "#0F172A");
    assert_eq!(tab.edits().len(), sent_before);
    assert_eq!(tab.focused(), Some(fill), "Escape keeps the field");
    // A value the engine refuses: told, and the Design's shown again.
    tab.app.ui.dispatch(InputEvent::Text("nope".into()));
    tab.app.key(zgx::input::Key::Enter);
    tab.app.frames(3);
    assert_eq!(tab.fill_shown(), "#0F172A");
    let refused = serde_json::json!([{ "update": "title", "props": { "fill": "#0F172Anope" } }]);
    assert_eq!(tab.edits().iter().filter(|e| **e == refused).count(), 1, "sent once: {:?}", tab.edits());

    // A click on the empty Canvas takes the focus from it.
    tab.click_canvas(930., 60., zgx::input::Modifiers::default());
    assert_eq!(tab.focused(), Some(tab.node(&format!("#design-canvas-{TAB}"))));
    // And a double click on the title, from there, edits it.
    tab.double_click_canvas(420., 110.);
    assert_eq!(tab.focused(), Some(tab.node(&format!("#design-text-field-{TAB}"))));
    tab.app.ui.dispatch(InputEvent::Text("!".into()));
    tab.app.frames(2);
    assert_eq!(tab.edits().last().unwrap()[0]["props"]["content"], "Hello!");
}

#[test]
fn a_slow_double_click_with_frames_between_its_presses_edits_the_text_and_keeps_the_keys() {
    let mut tab = mount(with_title("Hi"));
    // Each press and release a frame apart, as a window delivers them.
    let (sx, sy) = tab.on_screen(410., 110.);
    let (x, y) = (sx as f32, sy as f32);
    let button = zgx::input::PointerButton::Primary;
    for _ in 0..2 {
        tab.app.ui.dispatch(InputEvent::PointerDown { x, y, button });
        tab.app.frames(1);
        tab.app.ui.dispatch(InputEvent::PointerUp { x, y, button });
        tab.app.frames(1);
    }
    tab.app.frames(2);
    assert_eq!(tab.focused(), Some(tab.node(&format!("#design-text-field-{TAB}"))));
    tab.app.ui.dispatch(InputEvent::Text("!".into()));
    tab.app.frames(2);
    assert_eq!(tab.edits().last().unwrap()[0]["props"]["content"], "Hi!");
    // A press elsewhere on the Canvas ends it, the Canvas taking the keys.
    tab.click_canvas(930., 60., zgx::input::Modifiers::default());
    assert!(tab.app.find(&format!("#design-caret-{TAB}")).is_none());
    assert_eq!(tab.focused(), Some(tab.node(&format!("#design-canvas-{TAB}"))));
}
