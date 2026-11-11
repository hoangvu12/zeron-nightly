//! The Design tab's Canvas (ui/screens/panel/design.luau; docs/design,
//! ADR-0002): each tab's Design, followed from the engine that owns it
//! (`WatchDesign`), drawn by the design crate (Skia on the CPU, as the
//! engine draws agents' screenshots) on worker threads into 512 px tiles
//! and shown as host images. The script owns the view, its pan and zoom
//! moving the tiles' layers at once; this picks the tiles for it.
//!
//! ```text
//! shared  design.boards   { [tab] = Board }
//!         Board           { state = "opening" | "ready" | "failed", error?, dpr,
//!                           extent? = { x, y, w, h },   where the Design draws
//!                           layers = { Layer… } }   the previous zoom's first
//!         Layer           { level, tiles = { { id, src, x, y, size }… } }
//! invoke  design.open     { tab, path }           follow the Design at `path`
//! invoke  design.view     { tab, x, y, zoom, w, h, settled }
//! invoke  design.close    tab                      its watch and tiles let go
//! ```
//!
//! A layer's tiles are placed in points at `level` points a Canvas unit, so
//! the script scales it by `zoom / level` and moves it by the pan. Tiles are
//! drawn at the zoom (times the display's scale) at rest; while a zoom is
//! under way (`settled` false) the level changes only by twofold steps, and
//! the level before stays under the new one until the new one has every
//! tile in view. A change to the Design redraws only the tiles of the
//! top-level nodes it changed (a Screen lays itself out on its own), the
//! old ones shown until the new come. Every board's tiles together stay
//! under [`CACHE_BYTES`], those shown longest ago let go first.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, mpsc};

use serde_json::{Value as Json, json};
use zeron_design::Design;
use zeron_design::render::{self, Rect};
use zgx::image::ImageData;
use zgx_script::{ScriptHost, Shared, Value};

use crate::engine;

/// A tile's side, in device pixels.
pub const TILE: u32 = 512;
/// The most tile memory every Design tab of a window holds together (a
/// tile is 1 MiB): the benchmark's 256 MiB took the window to 450 MB.
pub const CACHE_BYTES: usize = 160 << 20;
const TILE_BYTES: usize = (TILE * TILE * 4) as usize;
/// How far the view zooms out and in.
pub const ZOOM: (f32, f32) = (0.25, 5.);

/// A Design as the engine last told it. (`WatchDesign` also names what
/// each call made or changed, for the tab's flashes: not read yet.)
pub struct Seen {
    pub version: u64,
    pub design: Design,
}

impl Seen {
    /// `WatchDesign`'s item.
    pub fn from_json(item: &Json) -> Result<Self, String> {
        let design = serde_json::from_value(item["design"].clone()).map_err(|e| format!("not a Design: {e}"))?;
        Ok(Self {
            version: item["version"].as_u64().unwrap_or(0),
            design,
        })
    }
}

/// What a board hears from, as the Design changes or can't be followed.
pub type Heard = Box<dyn FnMut(Result<Seen, String>)>;

/// Where boards follow their Designs: the engine, or a test's stand-in.
pub trait Source {
    /// Follows the Design at `path`, telling `heard` it now and after each
    /// change, until the handle it gives drops. `None` while it can't yet
    /// (no engine): [`Canvas::follow_again`] asks again.
    fn follow(&self, path: &str, heard: Heard) -> Option<Box<dyn Any>>;
}

/// The engine of this computer (ADR-0003), over its local connection.
struct EngineSource;

impl Source for EngineSource {
    fn follow(&self, path: &str, heard: Heard) -> Option<Box<dyn Any>> {
        let rpc = engine::get().current()?;
        let heard = Rc::new(RefCell::new(heard));
        let ended = heard.clone();
        let watch = rpc.watch_ending(
            "WatchDesign",
            json!({ "path": path }),
            move |item| (heard.borrow_mut())(Seen::from_json(&item)),
            move |why| {
                // (A connection that ended is followed again on the next.)
                if why != "closed" {
                    (ended.borrow_mut())(Err(why));
                }
            },
        );
        Some(Box::new(watch))
    }
}

/// A Design's version as tiles draw it: the Design and where each of its
/// top-level nodes draws.
struct Scene {
    version: u64,
    design: Design,
    tops: Vec<Rect>,
}

impl Scene {
    fn new(seen: Seen) -> Self {
        let tops = seen.design.children.iter().map(render::bounds).collect();
        Self {
            version: seen.version,
            design: seen.design,
            tops,
        }
    }

    /// Where `newer` draws differently: each top-level node changed, made,
    /// gone or moved in the order, where it was and where it is. Everything
    /// when the Tokens or Themes changed.
    fn damage(&self, newer: &Scene) -> Vec<Rect> {
        if self.design.tokens != newer.design.tokens || self.design.themes != newer.design.themes {
            return vec![Rect { x: f32::MIN / 4., y: f32::MIN / 4., width: f32::MAX / 2., height: f32::MAX / 2. }];
        }
        let old: HashMap<&str, usize> = self.design.children.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let mut damage = Vec::new();
        let mut kept = HashSet::new();
        for (i, node) in newer.design.children.iter().enumerate() {
            match old.get(node.id.as_str()) {
                Some(&j) => {
                    kept.insert(j);
                    if self.design.children[j] != *node || i != j {
                        damage.extend([self.tops[j], newer.tops[i]]);
                    }
                }
                None => damage.push(newer.tops[i]),
            }
        }
        damage.extend((0..self.tops.len()).filter(|j| !kept.contains(j)).map(|j| self.tops[j]));
        damage
    }
}

/// A level (device pixels a Canvas unit, in thousandths) and a tile there.
type Key = (u32, i32, i32);

fn level_of(q: u32) -> f32 {
    q as f32 / 1000.
}

/// The Canvas a tile covers.
fn tile_rect((q, x, y): Key) -> Rect {
    let span = TILE as f32 / level_of(q);
    Rect { x: x as f32 * span, y: y as f32 * span, width: span, height: span }
}

/// Tiles at level `q` meeting `area`, grown by `ring` tiles each way.
fn keys(q: u32, area: Rect, ring: i32) -> Vec<Key> {
    let span = TILE as f32 / level_of(q);
    let x0 = (area.x / span).floor() as i32 - ring;
    let y0 = (area.y / span).floor() as i32 - ring;
    let x1 = ((area.x + area.width) / span).ceil() as i32 - 1 + ring;
    let y1 = ((area.y + area.height) / span).ceil() as i32 - 1 + ring;
    (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| (q, x, y))).collect()
}

struct Job {
    tab: String,
    key: Key,
    scene: Arc<Scene>,
    priority: f32,
}

struct Done {
    tab: String,
    key: Key,
    version: u64,
    /// None where nothing draws.
    image: Option<Arc<ImageData>>,
}

/// Draws one tile.
fn draw(scene: &Scene, key: Key) -> Option<Arc<ImageData>> {
    let area = tile_rect(key);
    let nodes: Vec<usize> = (0..scene.tops.len()).filter(|&i| scene.tops[i].meets(&area)).collect();
    if nodes.is_empty() {
        return None;
    }
    let pixels = render::region(&scene.design, &nodes, area, level_of(key.0), TILE, TILE)?;
    ImageData::new_premultiplied(TILE, TILE, pixels).ok().map(Arc::new)
}

type Queue = Arc<(Mutex<Vec<Job>>, Condvar)>;

/// The worker threads every board's tiles are drawn on, the most wanted
/// first; started with the first tile asked for.
struct Pool {
    queue: Queue,
    done: RefCell<Option<mpsc::Receiver<Done>>>,
    wake: zgx::task::LatestSender<()>,
}

impl Pool {
    fn start(&self) {
        if self.done.borrow().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        *self.done.borrow_mut() = Some(rx);
        // (Half the cores at most: the window and the agents' engine work too.)
        let count = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 6);
        for _ in 0..count {
            let (queue, tx, wake) = (self.queue.clone(), tx.clone(), self.wake.clone());
            std::thread::spawn(move || {
                loop {
                    let job = {
                        let (jobs, ready) = &*queue;
                        let mut jobs = jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        while jobs.is_empty() {
                            jobs = ready.wait(jobs).unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                        let best = (0..jobs.len())
                            .min_by(|&a, &b| jobs[a].priority.total_cmp(&jobs[b].priority))
                            .unwrap_or(0);
                        jobs.swap_remove(best)
                    };
                    let image = draw(&job.scene, job.key);
                    let done = Done { tab: job.tab, key: job.key, version: job.scene.version, image };
                    if tx.send(done).is_err() || !wake.send(()) {
                        return;
                    }
                }
            });
        }
    }

    /// `tab`'s jobs not started yet, out of the queue.
    fn take(&self, tab: &str) -> Vec<Key> {
        let mut queue = self.queue.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let taken = queue.iter().filter(|job| job.tab == tab).map(|job| job.key).collect();
        queue.retain(|job| job.tab != tab);
        taken
    }

    fn ask(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        self.start();
        let (queue, ready) = &*self.queue;
        queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(jobs);
        ready.notify_all();
    }
}

#[derive(Default)]
struct Tile {
    image: Option<Arc<ImageData>>,
    /// The version drawn, once one is.
    drawn: Option<u64>,
    /// The version it must show.
    wanted: u64,
    /// The newest version being drawn.
    coming: Option<u64>,
    /// When it last showed (the canvas's clock).
    used: u64,
}

impl Tile {
    fn needed(&self) -> bool {
        self.drawn.is_none_or(|v| v < self.wanted) && self.coming.is_none_or(|v| v < self.wanted)
    }
}

/// What the script shows: the pan (where the Canvas's origin is, in
/// points), the zoom, the tab's size, and whether a zoom is under way.
#[derive(Clone, Copy)]
struct View {
    x: f32,
    y: f32,
    zoom: f32,
    w: f32,
    h: f32,
    settled: bool,
}

impl View {
    /// The Canvas in view.
    fn area(&self) -> Rect {
        Rect { x: -self.x / self.zoom, y: -self.y / self.zoom, width: self.w / self.zoom, height: self.h / self.zoom }
    }
}

/// One tab's Design and its tiles.
struct Board {
    path: String,
    following: Option<Box<dyn Any>>,
    scene: Option<Arc<Scene>>,
    failed: Option<String>,
    view: Option<View>,
    front: Option<u32>,
    back: Option<u32>,
    tiles: HashMap<Key, Tile>,
}

/// A window's Design tabs.
pub struct Canvas {
    host: ScriptHost,
    boards_shared: Shared,
    source: Box<dyn Source>,
    boards: RefCell<HashMap<String, Board>>,
    pool: Pool,
    /// The display's scale.
    dpr: Cell<f32>,
    clock: Cell<u64>,
}

/// The Design tabs of `cx`'s window, following this computer's engine.
pub fn install(cx: &mut zgx_desktop::WindowContext, host: &ScriptHost) {
    let (canvas, pump) = register(host, Box::new(EngineSource));
    cx.tasks.spawn(pump);
    let weak = Rc::downgrade(&canvas);
    engine::get().on_connect(move |_| {
        if let Some(canvas) = weak.upgrade() {
            canvas.follow_again();
        }
    });
    // The display's scale, once the window knows its display.
    let info = cx.window_info.clone();
    let weak = Rc::downgrade(&canvas);
    let _ = cx.ui.runtime.effect(move || {
        let displays = info.displays.get();
        let at = info.current_display.get().unwrap_or(0);
        if let (Some(display), Some(canvas)) = (displays.get(at), weak.upgrade()) {
            canvas.set_dpr(display.scale_factor as f32);
        }
    });
}

/// The Design tabs' calls on `host`, following `source`: the canvas, and
/// the task that takes finished tiles to it (for the window's tasks).
pub fn register(host: &ScriptHost, source: Box<dyn Source>) -> (Rc<Canvas>, impl std::future::Future<Output = ()> + use<>) {
    let (wake, mut woken) = zgx::task::latest::<()>();
    let canvas = Rc::new(Canvas {
        host: host.clone(),
        boards_shared: host.shared("design.boards", Value::record(Vec::<(&str, Value)>::new())),
        source,
        boards: RefCell::default(),
        pool: Pool { queue: Arc::default(), done: RefCell::default(), wake },
        dpr: Cell::new(1.),
        clock: Cell::new(0),
    });
    let text = |v: &Value, k: &str| v.field(k).map(crate::auth::text).unwrap_or_default();
    let num = |v: &Value, k: &str| v.field(k).and_then(Value::as_f64).unwrap_or(0.) as f32;
    let me = Rc::downgrade(&canvas);
    host.action("design.open", move |arg| {
        if let Some(canvas) = me.upgrade() {
            canvas.open(&text(&arg, "tab"), &text(&arg, "path"));
        }
    });
    let me = Rc::downgrade(&canvas);
    host.action("design.view", move |arg| {
        let Some(canvas) = me.upgrade() else { return };
        let view = View {
            x: num(&arg, "x"),
            y: num(&arg, "y"),
            zoom: num(&arg, "zoom").clamp(ZOOM.0, ZOOM.1),
            w: num(&arg, "w"),
            h: num(&arg, "h"),
            settled: arg.field("settled").is_none_or(Value::truthy),
        };
        let tab = text(&arg, "tab");
        if let Some(board) = canvas.boards.borrow_mut().get_mut(&tab) {
            board.view = Some(view);
        }
        canvas.update(&tab);
    });
    let me = Rc::downgrade(&canvas);
    host.action("design.close", move |arg| {
        if let Some(canvas) = me.upgrade() {
            canvas.close(&crate::auth::text(&arg));
        }
    });
    let me = Rc::downgrade(&canvas);
    let pump = async move {
        while woken.recv().await.is_some() {
            let Some(canvas) = me.upgrade() else { return };
            canvas.take_done();
        }
    };
    (canvas, pump)
}

impl Canvas {
    fn open(self: &Rc<Self>, tab: &str, path: &str) {
        if tab.is_empty() || path.is_empty() {
            return;
        }
        if self.boards.borrow().get(tab).is_some_and(|b| b.path == path) {
            return;
        }
        self.close(tab);
        self.boards.borrow_mut().insert(
            tab.to_owned(),
            Board {
                path: path.to_owned(),
                following: None,
                scene: None,
                failed: None,
                view: None,
                front: None,
                back: None,
                tiles: HashMap::new(),
            },
        );
        self.follow(tab);
        self.publish();
    }

    /// Starts following `tab`'s Design, if it isn't yet.
    fn follow(self: &Rc<Self>, tab: &str) {
        let Some(path) = self.boards.borrow().get(tab).filter(|b| b.following.is_none()).map(|b| b.path.clone()) else {
            return;
        };
        let (me, at) = (Rc::downgrade(self), tab.to_owned());
        let following = self.source.follow(
            &path,
            Box::new(move |seen| {
                if let Some(canvas) = me.upgrade() {
                    canvas.heard(&at, seen);
                }
            }),
        );
        if let Some(board) = self.boards.borrow_mut().get_mut(tab) {
            board.following = following;
        }
    }

    /// Every board's Design followed again (a new connection to the engine).
    pub fn follow_again(self: &Rc<Self>) {
        let tabs: Vec<String> = self.boards.borrow().keys().cloned().collect();
        for tab in tabs {
            if let Some(board) = self.boards.borrow_mut().get_mut(&tab) {
                board.following = None;
            }
            self.follow(&tab);
        }
    }

    fn set_dpr(&self, dpr: f32) {
        if dpr > 0. && dpr != self.dpr.get() {
            self.dpr.set(dpr);
            let tabs: Vec<String> = self.boards.borrow().keys().cloned().collect();
            for tab in tabs {
                self.update(&tab);
            }
        }
    }

    fn close(&self, tab: &str) {
        let Some(board) = self.boards.borrow_mut().remove(tab) else { return };
        self.pool.take(tab);
        for key in board.tiles.keys() {
            self.host.image(&image_name(tab, *key), None);
        }
        self.publish();
    }

    /// A new version of `tab`'s Design (or why it can't be followed).
    fn heard(&self, tab: &str, seen: Result<Seen, String>) {
        {
            let mut boards = self.boards.borrow_mut();
            let Some(board) = boards.get_mut(tab) else { return };
            match seen {
                Err(why) => board.failed = Some(why),
                Ok(seen) => {
                    board.failed = None;
                    let scene = Arc::new(Scene::new(seen));
                    if let Some(old) = &board.scene {
                        let damage = old.damage(&scene);
                        let mut gone = Vec::new();
                        for (key, tile) in &mut board.tiles {
                            if !damage.iter().any(|d| d.meets(&tile_rect(*key))) {
                                continue;
                            }
                            if Some(key.0) == board.front {
                                // (Shown as it was until the new one comes.)
                                tile.wanted = scene.version;
                            } else {
                                gone.push(*key);
                            }
                        }
                        for key in gone {
                            board.tiles.remove(&key);
                            self.host.image(&image_name(tab, key), None);
                        }
                    }
                    board.scene = Some(scene);
                }
            }
        }
        self.update(tab);
    }

    /// Finished tiles, shown, and what they change.
    fn take_done(&self) {
        let mut touched = HashSet::new();
        while let Some(done) = self.pool.done.borrow().as_ref().and_then(|rx| rx.try_recv().ok()) {
            let mut boards = self.boards.borrow_mut();
            let Some(tile) = boards.get_mut(&done.tab).and_then(|b| b.tiles.get_mut(&done.key)) else {
                continue;
            };
            if tile.coming.is_some_and(|v| v <= done.version) {
                tile.coming = None;
            }
            if tile.drawn.is_some_and(|v| v >= done.version) {
                continue;
            }
            tile.drawn = Some(done.version);
            tile.image = done.image;
            self.host.image(&image_name(&done.tab, done.key), tile.image.clone());
            touched.insert(done.tab);
        }
        for tab in touched {
            self.update(&tab);
        }
    }

    /// Chooses `tab`'s levels and tiles for its view, publishes what shows,
    /// asks for what's missing and keeps the cache under its cap.
    fn update(&self, tab: &str) {
        let clock = self.clock.get() + 1;
        self.clock.set(clock);
        let dpr = self.dpr.get();
        let mut jobs = Vec::new();
        {
            let mut boards = self.boards.borrow_mut();
            let Some(board) = boards.get_mut(tab) else { return };
            let (Some(view), Some(scene)) = (board.view, board.scene.clone()) else {
                drop(boards);
                self.publish();
                return;
            };
            if view.w <= 0. || view.h <= 0. {
                return;
            }
            // What it asked for and wasn't started on is asked for again
            // below, if still wanted.
            for key in self.pool.take(tab) {
                if let Some(tile) = board.tiles.get_mut(&key) {
                    tile.coming = None;
                }
            }
            let shown = |board: &Board, key: Key| board.tiles.get(&key).is_some_and(|t| t.drawn.is_some());
            // Levels: the zoom's own at rest; while it zooms, the last one
            // until the zoom is twofold away.
            let want = (view.zoom * dpr * 1000.).round().max(1.) as u32;
            let switch = match board.front {
                None => true,
                Some(q) if q == want => false,
                Some(q) => view.settled || !(0.5..=2.).contains(&(level_of(want) / level_of(q))),
            };
            let area = view.area();
            if switch {
                if let Some(front) = board.front {
                    let whole = keys(front, area, 0).into_iter().all(|k| shown(board, k));
                    if whole || board.back.is_none() {
                        board.back = Some(front);
                    }
                }
                board.front = Some(want);
            }
            let front = board.front.unwrap_or(want);
            let visible = keys(front, area, 0);
            if visible.iter().all(|&k| shown(board, k)) {
                board.back = None;
            }
            // What's missing: in view first, nearest the middle first, then
            // the ring of tiles round it.
            let (cx, cy) = (area.x + area.width / 2., area.y + area.height / 2.);
            let in_view: HashSet<Key> = visible.iter().copied().collect();
            for key in keys(front, area, 1) {
                let tile = board.tiles.entry(key).or_default();
                tile.used = clock;
                if !tile.needed() {
                    continue;
                }
                tile.wanted = tile.wanted.max(scene.version);
                tile.coming = Some(scene.version);
                let r = tile_rect(key);
                let far = ((r.x + r.width / 2. - cx).powi(2) + (r.y + r.height / 2. - cy).powi(2)).sqrt() / r.width;
                let priority = far + if in_view.contains(&key) { 0. } else { 1000. };
                jobs.push(Job { tab: tab.to_owned(), key, scene: scene.clone(), priority });
            }
            if let Some(back) = board.back {
                for key in keys(back, area, 0) {
                    if let Some(tile) = board.tiles.get_mut(&key) {
                        tile.used = clock;
                    }
                }
            }
        }
        self.pool.ask(jobs);
        self.evict(clock);
        self.publish();
    }

    /// Lets go of the tiles shown longest ago while every board's together
    /// are over [`CACHE_BYTES`]; never one shown now.
    fn evict(&self, now: u64) {
        let mut boards = self.boards.borrow_mut();
        let mut held: Vec<(u64, String, Key)> = boards
            .iter()
            .flat_map(|(tab, b)| b.tiles.iter().filter(|(_, t)| t.image.is_some()).map(move |(k, t)| (t.used, tab.clone(), *k)))
            .collect();
        let most = CACHE_BYTES / TILE_BYTES;
        if held.len() <= most {
            return;
        }
        held.sort();
        let excess = held.len() - most;
        for (used, tab, key) in held.into_iter().take(excess) {
            if used == now {
                break;
            }
            if let Some(board) = boards.get_mut(&tab) {
                board.tiles.remove(&key);
                self.host.image(&image_name(&tab, key), None);
            }
        }
    }

    /// The tile memory held now, in bytes.
    #[cfg(test)]
    pub fn cached_bytes(&self) -> usize {
        self.boards.borrow().values().map(|b| b.tiles.values().filter(|t| t.image.is_some()).count()).sum::<usize>() * TILE_BYTES
    }

    /// The tiles `tab` shows, back to front: each node's id and its image.
    #[cfg(test)]
    pub fn shown(&self, tab: &str) -> Vec<(String, Arc<ImageData>)> {
        let boards = self.boards.borrow();
        let Some(board) = boards.get(tab) else { return Vec::new() };
        let Some(area) = board.view.map(|v| v.area()) else { return Vec::new() };
        [board.back, board.front]
            .into_iter()
            .flatten()
            .flat_map(|q| keys(q, area, 0))
            .filter_map(|key| board.tiles.get(&key)?.image.clone().map(|image| (tile_id(tab, key), image)))
            .collect()
    }

    /// Tiles in `tab`'s view where its Design draws that show nothing: no
    /// tile of the zoom's level drawn yet, nor the level's before it.
    #[cfg(test)]
    pub fn blank(&self, tab: &str) -> usize {
        let boards = self.boards.borrow();
        let Some(board) = boards.get(tab) else { return 0 };
        let (Some(view), Some(scene), Some(front)) = (board.view, &board.scene, board.front) else { return 0 };
        let drawn = |key: &Key| board.tiles.get(key).is_some_and(|t| t.drawn.is_some());
        keys(front, view.area(), 0)
            .into_iter()
            .filter(|key| scene.tops.iter().any(|top| top.meets(&tile_rect(*key))))
            .filter(|key| {
                !drawn(key)
                    && board.back.is_none_or(|back| !keys(back, tile_rect(*key), 0).iter().all(drawn))
            })
            .count()
    }

    /// Whether `tab` shows every tile in view at its zoom's own level, each
    /// of the Design as it is now.
    #[cfg(test)]
    pub fn sharp(&self, tab: &str) -> bool {
        let boards = self.boards.borrow();
        let Some(board) = boards.get(tab) else { return false };
        let Some(view) = board.view else { return false };
        let want = (view.zoom * self.dpr.get() * 1000.).round().max(1.) as u32;
        // (A tile where nothing draws is drawn too, as nothing.)
        board.front == Some(want)
            && board.back.is_none()
            && keys(want, view.area(), 0)
                .iter()
                .all(|key| board.tiles.get(key).is_some_and(|t| t.drawn.is_some_and(|v| v >= t.wanted)))
    }

    /// `tab`'s view: its pan and zoom.
    #[cfg(test)]
    pub fn view(&self, tab: &str) -> Option<(f32, f32, f32)> {
        self.boards.borrow().get(tab)?.view.map(|v| (v.x, v.y, v.zoom))
    }

    fn publish(&self) {
        self.boards_shared.set(self.boards_value());
    }

    fn boards_value(&self) -> Value {
        let dpr = self.dpr.get();
        let boards = self.boards.borrow();
        Value::record(boards.iter().map(|(tab, board)| {
            let state = if board.failed.is_some() {
                "failed"
            } else if board.scene.is_some() {
                "ready"
            } else {
                "opening"
            };
            let layer = |q: u32| {
                let area = board.view.map(|v| v.area());
                let span = TILE as f64 / dpr as f64;
                let tiles: Vec<Value> = area
                    .map(|area| keys(q, area, 0))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|key| board.tiles.get(key).is_some_and(|t| t.image.is_some()))
                    .map(|key| {
                        Value::record([
                            ("id", Value::from(tile_id(tab, key).as_str())),
                            ("src", Value::from(format!("host:{}", image_name(tab, key)).as_str())),
                            ("x", Value::Num(key.1 as f64 * span)),
                            ("y", Value::Num(key.2 as f64 * span)),
                            ("size", Value::Num(span)),
                        ])
                    })
                    .collect();
                Value::record([
                    ("level", Value::Num((level_of(q) / dpr) as f64)),
                    ("tiles", Value::List(tiles.into())),
                ])
            };
            let layers: Vec<Value> = [board.back.map(layer), board.front.map(layer)].into_iter().flatten().collect();
            let mut fields = vec![
                ("state", Value::from(state)),
                ("dpr", Value::Num(dpr as f64)),
                ("layers", Value::List(layers.into())),
            ];
            if let Some(why) = &board.failed {
                fields.push(("error", Value::from(why.as_str())));
            }
            // Where the whole Design draws, for a view of all of it.
            if let Some(extent) = board.scene.as_ref().and_then(|s| s.tops.iter().copied().reduce(|a, b| a.union(&b))) {
                fields.push((
                    "extent",
                    Value::record([
                        ("x", Value::Num(extent.x as f64)),
                        ("y", Value::Num(extent.y as f64)),
                        ("w", Value::Num(extent.width as f64)),
                        ("h", Value::Num(extent.height as f64)),
                    ]),
                ));
            }
            (tab.as_str(), Value::record(fields))
        }))
    }
}

/// A tile's host image.
fn image_name(tab: &str, (q, x, y): Key) -> String {
    format!("design/{tab}/{q}/{x}/{y}")
}

/// A tile's node.
fn tile_id(tab: &str, (q, x, y): Key) -> String {
    format!("design-tile-{tab}-{q}_{x}_{y}")
}
