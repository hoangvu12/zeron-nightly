//! The Design tab's Canvas (ui/screens/panel/design.luau; docs/design,
//! ADR-0002): each tab's Design, followed from the engine that owns it
//! (`WatchDesign`), drawn by the design crate (Skia on the CPU, as the
//! engine draws agents' screenshots) on worker threads into 512 px tiles
//! and shown as host images. The script owns the view, its pan and zoom
//! moving the tiles' layers at once; this picks the tiles for it.
//!
//! ```text
//! shared  design.tab.<tab>  { state = "opening" | "ready" | "failed", error?,
//!                             extent? = { x, y, w, h },   where the Design draws
//!                             flashes = { { id, version, x, y, w, h }… },  what an agent just changed
//!                             working = { { id, name, x, y, w, h }… },  Screens in progress
//!                             path, unsaved, recovered, stale,   its file, and whether it's current
//!                             selected = { { id, x, y, w, h }… },  the selection
//!                             props?,   the one node selected, as written (its properties)
//!                             themes, theme }   the Design's Theme axes, and the tab's choice
//! shared  design.tree.<tab>   { { id, name, type, depth }… }   its layers list, as the Design changes
//!                             layers = { Layer… } }   the previous zoom's first
//!         Layer             { level, tiles = { { id, src, x, y, size }… } }
//! invoke  design.open       { tab, path }           follow the Design at `path`
//! invoke  design.view       { tab, x, y, zoom, w, h, settled }
//! invoke  design.close      tab                      its watch and tiles let go
//! invoke  design.tabs       { chat, paths }          a chat's Design tabs, front first
//! function design.hit       { tab, x, y, deep? } → id?   what a click at a Canvas point selects
//! function design.moved     { tab, id, dx, dy } → edit?  what dragging a node so changes
//! function design.caret     { tab, id, content, at, from, to } → { caret, range, box }, or
//!                           { tab, id, content, x, y } → place   a text's caret, as drawn
//! invoke  design.theme      { tab, theme }           the Theme the tab shows (axis → value)
//! call    design.edit       { tab, edits, description? }   the person's edits, one change
//! call    design.undo / design.redo { tab }      the latest change taken back, done again
//! invoke  design.select     { tab, chat, ids }       the person's selection, told to the engine
//! invoke  design.flashed    { tab, version }         that flash is over
//! call    design.new        { name? } → { name, path }   a new managed Design
//! call    design.recent     → { recent = { { name, path, touchedAt }… } }
//! call    design.saving     { tab } → saving          { path, managed, unsaved, recovered, stale }
//! call    design.save       { tab } → saving          its file written now (⌘S)
//! call    design.save_as    { tab, to } → saving      written to `to`, the tab's from then
//! call    design.discard    { tab } → saving          back as its file holds it
//! ```
//!
//! A layer's tiles are placed in points at `level` points a Canvas unit, so
//! the script scales it by `zoom / level` and moves it by the pan. Tiles are
//! drawn at the zoom (times the display's scale) at rest; while a zoom is
//! under way (`settled` false) the level changes only by twofold steps, and
//! the level before stays under the new one until the new one has every
//! tile in view. A change to the Design redraws only the tiles of the
//! top-level nodes it changed (a Screen lays itself out on its own), the
//! old ones shown until the new come. Every tab's tiles together stay under
//! [`CACHE_BYTES`], those shown longest ago let go first, never one in view.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use serde_json::{Value as Json, json};
use zeron_design::Design;
use zeron_design::model::Theme;
use zeron_design::assets::{Files, Resolved};
use zeron_design::fonts::{FontFile, Fonts};
use zeron_design::layout::LaidOut;
use zeron_design::render::{self, Rect};
use zgx::image::ImageData;
use zgx_script::{ScriptHost, Shared, Value};

use crate::engine;

/// A tile's side, in device pixels.
pub const TILE: u32 = 512;
/// The most tile memory every Design tab of a window holds together (a
/// tile is 1 MiB on the CPU; zgx's GPU copies come on top): the benchmark's
/// 256 MiB took the window to 450 MB.
pub const CACHE_BYTES: usize = 160 << 20;
const TILE_BYTES: usize = (TILE * TILE * 4) as usize;
/// How far the view zooms out and in (the script's `ZOOM_MIN`, `ZOOM_MAX`).
pub const ZOOM: (f32, f32) = (0.25, 5.);

/// What a tab hears as it follows a Design: the Design whole after each
/// change, or why it can't be followed.
pub type Listener = Box<dyn FnMut(Result<Heard, String>)>;

/// A Design as it is now, and the nodes agents' changes to it made or
/// changed since the tab last heard (none for anyone else's), which flash.
/// And the files of the assets it uses (its icons, fonts and images) in
/// the engine's cache, or beside the Design: tiles draw them from those
/// files only, and never fetch.
pub struct Heard {
    pub design: Design,
    pub flash: Vec<String>,
    /// What its text draws in: the font files among its assets.
    pub fonts: Fonts,
    pub assets: Vec<Resolved>,
    /// Its file and whether that's current (#28).
    pub saving: FileState,
}

/// A Design's file and whether it's current, as the engine says (#28).
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct FileState {
    /// Its file now (another after Save as).
    pub path: Option<String>,
    /// It has changes its file doesn't.
    pub unsaved: bool,
    /// Its unsaved changes were found in its backup as it opened...
    pub recovered: bool,
    /// ...and its file changed on disk since they were made.
    pub stale: bool,
}

/// `WatchDesign`'s item as a tab hears it: what an agent changed flashes;
/// the person's own changes don't. `fonts` are those last heard, read
/// again only when the files change (font files that can't be read draw as
/// the engine said the families it can't get do: in the bundled one).
pub fn heard_of(item: &Json, fonts: &mut Fonts) -> Result<Heard, String> {
    #[derive(serde::Deserialize)]
    struct Change {
        #[serde(default)]
        changed: Vec<String>,
        by: Changer,
    }
    #[derive(serde::Deserialize, PartialEq)]
    #[serde(rename_all = "lowercase")]
    enum Changer {
        Agent,
        #[serde(other)]
        Other,
    }
    let design = serde_json::from_value(item["design"].clone()).map_err(|e| format!("not a Design: {e}"))?;
    let changes: Vec<Change> = serde_json::from_value(item["changes"].clone()).unwrap_or_default();
    let mut flash: Vec<String> = Vec::new();
    for id in changes.into_iter().filter(|c| c.by == Changer::Agent).flat_map(|c| c.changed) {
        if !flash.contains(&id) {
            flash.push(id);
        }
    }
    // (An asset not named draws nothing, or an image's placeholder, as the
    // engine does with one it couldn't fetch.)
    let assets: Vec<Resolved> = serde_json::from_value(item["assets"].clone()).unwrap_or_default();
    let files: Vec<FontFile> = assets.iter().filter_map(Resolved::font_file).collect();
    if !fonts.is_of(&files) {
        *fonts = Fonts::of_files(&files);
    }
    let saving = <FileState as serde::Deserialize>::deserialize(item).unwrap_or_default();
    Ok(Heard { design, flash, fonts: fonts.clone(), assets, saving })
}

/// Where tabs follow their Designs: the engine, or a test's stand-in.
pub trait Source {
    /// Follows the Design at `path`, telling `listener` it now and after
    /// each change, until the handle it gives drops. `None` while it can't
    /// yet (no engine): [`DesignTabs::follow_again`] asks again.
    fn follow(&self, path: &str, listener: Listener) -> Option<Box<dyn Any>>;

    /// Says which Designs `chat`'s Design tabs show, the front one first
    /// (what its agent's calls naming no Design go by).
    fn set_tabs(&self, chat: &str, paths: &[String]);

    /// Says what the person has selected in `chat`'s Design tab on the
    /// Design at `path` (`design_state` reports it to the chat's agent).
    fn select(&self, chat: &str, path: &str, ids: &[String]);

    /// One of the engine's Design calls (`NewDesign`, `RecentDesigns`,
    /// `SaveDesign`, `SaveDesignAs`, `DiscardDesign`): its answer, or why
    /// not.
    fn ask(&self, method: &str, params: Json) -> Asked;
}

/// An engine call's answer, to come.
pub type Asked = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Json, String>>>>;

/// The engine of this computer (ADR-0003), over its local connection.
#[derive(Default)]
struct EngineSource {
    /// Selections to tell it, in order (each waits for the one before, so
    /// the engine keeps the last).
    selections: Rc<RefCell<std::collections::VecDeque<Json>>>,
    telling: Rc<Cell<bool>>,
}

impl Source for EngineSource {
    fn select(&self, chat: &str, path: &str, ids: &[String]) {
        self.selections.borrow_mut().push_back(json!({ "chatId": chat, "path": path, "ids": ids }));
        if self.telling.replace(true) {
            return;
        }
        let (queue, telling) = (self.selections.clone(), self.telling.clone());
        engine::get().spawn(async move {
            loop {
                let next = queue.borrow_mut().pop_front();
                let Some(params) = next else { break };
                if let Err(e) = engine::get().call("SetDesignSelection", params).await {
                    eprintln!("zeron-zgx: design: the engine didn't take the selection: {e}");
                }
            }
            telling.set(false);
        });
    }

    fn ask(&self, method: &str, params: Json) -> Asked {
        let method = method.to_owned();
        Box::pin(async move { engine::get().call_soon(&method, params).await })
    }

    fn set_tabs(&self, chat: &str, paths: &[String]) {
        let params = json!({ "chatId": chat, "paths": paths });
        engine::get().spawn(async move {
            if let Err(e) = engine::get().call("SetDesignTabs", params).await {
                eprintln!("zeron-zgx: design: the engine didn't take the Design tabs: {e}");
            }
        });
    }

    fn follow(&self, path: &str, listener: Listener) -> Option<Box<dyn Any>> {
        let rpc = engine::get().current()?;
        let listener = Rc::new(RefCell::new(listener));
        let ended = listener.clone();
        let mut fonts = Fonts::default();
        let watch = rpc.watch_ending(
            "WatchDesign",
            json!({ "path": path }),
            move |item: Json| (listener.borrow_mut())(heard_of(&item, &mut fonts)),
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

/// A Design as tiles draw it: its version in this window (one more each
/// time it's heard), the Design laid out in its fonts, its other assets,
/// and where each of its top-level nodes draws.
struct Scene {
    version: u64,
    /// As written: what the layers list shows.
    written: Design,
    /// As drawn (Instances expanded, Tokens resolved).
    design: Design,
    fonts: Fonts,
    laid_out: LaidOut,
    assets: Files,
    tops: Vec<Rect>,
    /// How much more a tile draws round itself, for its blurs' sake.
    margin: f32,
    /// Its assets as heard, and the Theme it's drawn in: what drawing it
    /// in another Theme starts from.
    heard: Vec<Resolved>,
    theme: Theme,
}

impl Scene {
    fn new(version: u64, design: Design, fonts: Fonts, assets: Vec<Resolved>, theme: Theme) -> Self {
        // As drawn: Instances expanded, Tokens resolved, in the tab's Theme.
        let written = design;
        let design = zeron_design::resolve::drawn_in(&written, &theme);
        let laid_out = LaidOut::of(&design, &fonts.typesetter(&design));
        let tops = design.children.iter().map(|node| render::bounds(node, &laid_out)).collect();
        let margin = render::blur_reach(&design, &laid_out);
        Self { version, written, design, fonts, laid_out, assets: Files::new(assets.clone()), tops, margin, heard: assets, theme }
    }

    /// Where `newer` draws differently: each top-level node changed, made,
    /// gone or moved in the order, where it was and where it is. Everything
    /// when what the whole Design shares changed (its Tokens, Themes, and
    /// what the model keeps without knowing).
    fn damage(&self, newer: &Scene) -> Vec<Rect> {
        let (old, new) = (&self.design, &newer.design);
        // (An asset newly had, an image say, draws in place of what stood in
        // for it.)
        let assets = self.assets.paths() != newer.assets.paths();
        if assets || old.tokens != new.tokens || old.themes != new.themes || old.extra != new.extra {
            return vec![Rect { x: f32::MIN / 4., y: f32::MIN / 4., width: f32::MAX / 2., height: f32::MAX / 2. }];
        }
        let was: HashMap<&str, usize> = old.children.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let mut damage = Vec::new();
        let mut kept = HashSet::new();
        for (i, node) in new.children.iter().enumerate() {
            match was.get(node.id.as_str()) {
                Some(&j) => {
                    kept.insert(j);
                    if old.children[j] != *node || i != j {
                        damage.extend([self.tops[j], newer.tops[i]]);
                    }
                }
                None => damage.push(newer.tops[i]),
            }
        }
        damage.extend((0..self.tops.len()).filter(|j| !kept.contains(j)).map(|j| self.tops[j]));
        damage
    }

    /// Whether anything draws in `area`.
    fn draws_in(&self, area: &Rect) -> bool {
        self.tops.iter().any(|top| top.meets(area))
    }
}

/// The layers list (#32): every node of `design`, parents before their
/// children, as `{ id, name, type, depth }` (depth 0 on the Canvas).
fn layers_of(design: &Design) -> Vec<Value> {
    fn walk(nodes: &[zeron_design::model::Node], depth: usize, out: &mut Vec<Value>) {
        for node in nodes {
            out.push(Value::record([
                ("id", Value::from(node.id.as_str())),
                ("name", Value::from(if node.name.is_empty() { node.kind.name() } else { node.name.as_str() })),
                ("type", Value::from(node.kind.name())),
                ("depth", Value::Num(depth as f64)),
            ]));
            walk(node.kind.get().children(), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(&design.children, 0, &mut out);
    out
}

/// The node a click at Canvas point (`x`, `y`) selects in `scene`: within
/// a Screen (a top-level Frame), its child there (the Screen itself
/// between its children), as design tools pick; `deep`, the innermost node
/// there. The topmost of overlapping ones, each as it's turned; none on the
/// bare Canvas. (Inside an Instance, what's drawn has the Instance's id
/// first, slash-separated: a plain click picks the Instance.)
fn hit(scene: &Scene, x: f32, y: f32, deep: bool) -> Option<String> {
    use zeron_design::model::Node;
    // The point in `node`'s own (unturned) space: turned back about its
    // top-left corner, as `render::paint_node` turns it.
    fn unturned(node: &Node, at: Rect, (x, y): (f32, f32)) -> (f32, f32) {
        if node.rotation == 0. {
            return (x, y);
        }
        let (sin, cos) = node.rotation.to_radians().sin_cos();
        let (dx, dy) = (x - at.x, y - at.y);
        (at.x + dx * cos - dy * sin, at.y + dx * sin + dy * cos)
    }
    // The nodes under the point, outermost first: drawn last is on top, so
    // children are tried last to first.
    fn path<'a>(scene: &Scene, nodes: &'a [Node], point: (f32, f32), out: &mut Vec<&'a Node>) {
        for node in nodes.iter().rev() {
            let Some(at) = scene.laid_out.get(&node.id).map(|p| p.bounds) else { continue };
            let (x, y) = unturned(node, at, point);
            if x >= at.x && x <= at.x + at.width && y >= at.y && y <= at.y + at.height {
                out.push(node);
                path(scene, node.kind.get().children(), (x, y), out);
                return;
            }
        }
    }
    let mut under = Vec::new();
    path(scene, &scene.design.children, (x, y), &mut under);
    let screen = under.first().is_some_and(|top| top.kind.name() == "frame");
    let at = if deep { under.last() } else if screen { under.get(1).or(under.first()) } else { under.first() };
    at.map(|node| if deep { node.id.clone() } else { node.id.split('/').next().unwrap_or(&node.id).to_string() })
}

/// What dragging node `id` by (`dx`, `dy`) Canvas units changes in
/// `scene`: in a parent that lays its children out (and not placed apart
/// from it), its place among its siblings, by where its middle lands;
/// else its x and y. None for a node inside an Instance (its Component's
/// to change) or one not there.
fn moved(scene: &Scene, id: &str, dx: f32, dy: f32) -> Option<Json> {
    use zeron_design::model::{Direction, Node};
    fn parent_of<'a>(nodes: &'a [Node], id: &str) -> Option<Option<&'a Node>> {
        for node in nodes {
            if node.id == id {
                return Some(None);
            }
            if let Some(found) = parent_of(node.kind.get().children(), id) {
                return Some(found.or(Some(node)));
            }
        }
        None
    }
    if id.contains('/') {
        return None;
    }
    let node = scene.written.find(id)?;
    let parent = parent_of(&scene.written.children, id)?;
    let direction = parent.and_then(|p| p.kind.get().layout()).map(|l| l.layout);
    match (parent, direction) {
        (Some(parent), Some(direction @ (Direction::Horizontal | Direction::Vertical))) if !node.absolute => {
            let middle = |b: Rect| if direction == Direction::Horizontal { b.x + b.width / 2. } else { b.y + b.height / 2. };
            let at = middle(scene.laid_out.get(id)?.bounds) + if direction == Direction::Horizontal { dx } else { dy };
            let index = parent
                .kind
                .get()
                .children()
                .iter()
                // (Those placed apart from the layout aren't in its order.)
                .filter(|sibling| sibling.id != id && !sibling.absolute)
                .filter(|sibling| scene.laid_out.get(&sibling.id).is_some_and(|p| middle(p.bounds) < at))
                .count();
            Some(json!({ "move": id, "parent": parent.id, "index": index }))
        }
        _ => Some(json!({ "update": id, "props": { "x": node.x + dx, "y": node.y + dy } })),
    }
}

/// Text node `id` of `scene` laid out as drawn with `content` in its
/// place, and where its box is on the Canvas.
fn carets_of(scene: &Scene, id: &str, content: &str) -> Option<(zeron_design::skia::Carets, Rect)> {
    let node = scene.design.find(id)?;
    let zeron_design::model::Kind::Text(text) = &node.kind else { return None };
    let at = scene.laid_out.get(id)?.bounds;
    let mut text = text.clone();
    text.content = content.to_owned();
    let typesetter = scene.fonts.typesetter(&scene.design);
    // (A text as wide as its words grows as they're typed: laid out as wide
    // as they are now, not as they were drawn last, so none wraps.)
    let size = match text.growth() {
        zeron_design::model::TextGrowth::Auto => {
            let [w, h] = typesetter.measure(&text, None);
            (w, h)
        }
        _ => (at.width, at.height),
    };
    Some((typesetter.carets(&text, size), at))
}

fn rect_value(r: Rect) -> Value {
    Value::record([
        ("x", Value::Num(r.x as f64)),
        ("y", Value::Num(r.y as f64)),
        ("w", Value::Num(r.width as f64)),
        ("h", Value::Num(r.height as f64)),
    ])
}

/// How finely tiles are drawn: device pixels a Canvas unit, in
/// thousandths (so a level is a key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Level(u32);

impl Level {
    /// The level a zoom shows sharp at on a display of scale `dpr`.
    fn of(zoom: f32, dpr: f32) -> Self {
        Self((zoom * dpr * 1000.).round().max(1.) as u32)
    }

    fn scale(self) -> f32 {
        self.0 as f32 / 1000.
    }

    /// The Canvas a tile's side covers.
    fn span(self) -> f32 {
        TILE as f32 / self.scale()
    }
}

/// One tile: its level and where it is in that level's grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct TileKey {
    level: Level,
    x: i32,
    y: i32,
}

impl TileKey {
    /// The Canvas it covers.
    fn rect(self) -> Rect {
        let span = self.level.span();
        Rect { x: self.x as f32 * span, y: self.y as f32 * span, width: span, height: span }
    }

    /// Its host image.
    fn image_name(self, tab: &str) -> String {
        format!("design/{tab}/{}/{}/{}", self.level.0, self.x, self.y)
    }

    /// Its node.
    fn node_id(self, tab: &str) -> String {
        format!("design-tile-{tab}-{}_{}_{}", self.level.0, self.x, self.y)
    }
}

/// Tiles at `level` meeting `area`, grown by `ring` tiles each way.
fn keys(level: Level, area: Rect, ring: i32) -> Vec<TileKey> {
    let span = level.span();
    let x0 = (area.x / span).floor() as i32 - ring;
    let y0 = (area.y / span).floor() as i32 - ring;
    let x1 = ((area.x + area.width) / span).ceil() as i32 - 1 + ring;
    let y1 = ((area.y + area.height) / span).ceil() as i32 - 1 + ring;
    (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| TileKey { level, x, y })).collect()
}

struct Job {
    tab: String,
    key: TileKey,
    scene: Arc<Scene>,
    priority: f32,
}

/// A finished tile.
struct Drawn {
    tab: String,
    key: TileKey,
    version: u64,
    /// None where nothing draws.
    image: Option<Arc<ImageData>>,
}

/// Draws one tile.
fn draw(scene: &Scene, key: TileKey) -> Option<Arc<ImageData>> {
    let area = key.rect();
    // (What blurs at its edges read from round it is drawn too.)
    let m = scene.margin;
    let around = Rect { x: area.x - m, y: area.y - m, width: area.width + 2. * m, height: area.height + 2. * m };
    if !scene.draws_in(&area) {
        return None;
    }
    let nodes: Vec<usize> = (0..scene.tops.len()).filter(|&i| scene.tops[i].meets(&around)).collect();
    // (Made per tile: Skia's text layout isn't shared across threads.)
    let typesetter = scene.fonts.typesetter(&scene.design);
    let pixels = render::region(&scene.design, &scene.laid_out, &typesetter, &nodes, area, key.level.scale(), m, (TILE, TILE), &scene.assets)?;
    ImageData::new_premultiplied(TILE, TILE, pixels).ok().map(Arc::new)
}

/// The jobs waiting, and whether the window that asked is gone.
#[derive(Default)]
struct Queue {
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
    closed: AtomicBool,
}

impl Queue {
    fn jobs(&self) -> std::sync::MutexGuard<'_, Vec<Job>> {
        self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The most wanted job, waiting for one; `None` once closed.
    fn next(&self) -> Option<Job> {
        let mut jobs = self.jobs();
        loop {
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            if !jobs.is_empty() {
                let best = (0..jobs.len()).min_by(|&a, &b| jobs[a].priority.total_cmp(&jobs[b].priority)).unwrap_or(0);
                return Some(jobs.swap_remove(best));
            }
            jobs = self.ready.wait(jobs).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// The worker threads every tab's tiles are drawn on, the most wanted
/// first; started with the first tile asked for, ended with the window.
struct Pool {
    queue: Arc<Queue>,
    done: RefCell<Option<mpsc::Receiver<Drawn>>>,
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
            // (Drawing recurses once a nesting level: the engine's Design
            // stack, `zeron_engine::designs::DESIGN_STACK`, so the deepest
            // Design draws here as it does there.)
            let _ = std::thread::Builder::new().name("zeron-design-tiles".into()).stack_size(8 << 20).spawn(move || {
                while let Some(job) = queue.next() {
                    let image = draw(&job.scene, job.key);
                    let drawn = Drawn { tab: job.tab, key: job.key, version: job.scene.version, image };
                    if tx.send(drawn).is_err() || !wake.send(()) {
                        return;
                    }
                }
            });
        }
    }

    /// `tab`'s jobs not started yet, out of the queue.
    fn take(&self, tab: &str) -> Vec<TileKey> {
        let mut jobs = self.queue.jobs();
        let taken = jobs.iter().filter(|job| job.tab == tab).map(|job| job.key).collect();
        jobs.retain(|job| job.tab != tab);
        taken
    }

    fn ask(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        self.start();
        self.queue.jobs().extend(jobs);
        self.queue.ready.notify_all();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.queue.closed.store(true, Ordering::Release);
        self.queue.ready.notify_all();
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
    /// When it last showed (the tabs' clock).
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

/// One tab's Canvas: its Design and its tiles.
struct TabCanvas {
    path: String,
    following: Option<Box<dyn Any>>,
    /// What the script reads (`design.tab.<tab>`).
    shared: Shared,
    scene: Option<Arc<Scene>>,
    failed: Option<String>,
    view: Option<View>,
    /// The Canvas in view when the levels were last chosen.
    area: Option<Rect>,
    front: Option<Level>,
    back: Option<Level>,
    tiles: HashMap<TileKey, Tile>,
    /// What flashes now, a flash for each version agents changed.
    flashes: Vec<Flash>,
    /// The person's selection, in the order selected, and the chat it's
    /// told for (again if the file changes: Save as).
    selection: Vec<String>,
    selection_chat: String,
    /// The layers list (`design.tree.<tab>`), set as the Design changes,
    /// not with every view.
    tree: Shared,
    /// The Theme the tab shows its Design in (#33's switcher; agents'
    /// screenshots keep the Design's first values): an axis each, those
    /// not chosen at their first value.
    theme: Theme,
    saving: FileState,
}

/// The nodes agents changed in one version of a Design, lit for a moment.
struct Flash {
    /// The version (so the script tells one flash from the next).
    version: u64,
    ids: Vec<String>,
    /// When it was heard: one not shown by [`FLASH_STALE`] (its tab in the
    /// back) never is.
    at: std::time::Instant,
}

/// How long a flash may wait to be shown.
const FLASH_STALE: std::time::Duration = std::time::Duration::from_secs(2);

impl TabCanvas {
    fn drawn(&self, key: &TileKey) -> bool {
        self.tiles.get(key).is_some_and(|t| t.drawn.is_some())
    }

    /// The tiles kept whatever the cache: those its layers show now, and
    /// the ring round the view being drawn.
    fn in_use(&self) -> HashSet<TileKey> {
        let Some(area) = self.area else { return HashSet::new() };
        let front = self.front.into_iter().flat_map(|level| keys(level, area, 1));
        let back = self.back.into_iter().flat_map(|level| keys(level, area, 0));
        front.chain(back).collect()
    }

    /// What the script reads of it.
    fn value(&self, tab: &str, dpr: f32) -> Value {
        let state = match (&self.failed, &self.scene) {
            (Some(_), _) => "failed",
            (None, Some(_)) => "ready",
            (None, None) => "opening",
        };
        let span = (TILE as f32 / dpr) as f64;
        let layer = |level: Level| {
            let tiles: Vec<Value> = self
                .area
                .map(|area| keys(level, area, 0))
                .unwrap_or_default()
                .into_iter()
                .filter(|key| self.tiles.get(key).is_some_and(|t| t.image.is_some()))
                .map(|key| {
                    Value::record([
                        ("id", Value::from(key.node_id(tab).as_str())),
                        ("src", Value::from(format!("host:{}", key.image_name(tab)).as_str())),
                        ("x", Value::Num(key.x as f64 * span)),
                        ("y", Value::Num(key.y as f64 * span)),
                        ("size", Value::Num(span)),
                    ])
                })
                .collect();
            Value::record([("level", Value::Num((level.scale() / dpr) as f64)), ("tiles", Value::List(tiles.into()))])
        };
        let layers: Vec<Value> = [self.back.map(layer), self.front.map(layer)].into_iter().flatten().collect();
        let mut fields = vec![("state", Value::from(state)), ("layers", Value::List(layers.into()))];
        // Its file, and whether that's current (#28).
        fields.extend([
            ("path", Value::from(self.path.as_str())),
            ("unsaved", Value::Bool(self.saving.unsaved)),
            ("recovered", Value::Bool(self.saving.recovered)),
            ("stale", Value::Bool(self.saving.stale)),
        ]);
        // The live touches (#34), on the Canvas: what an agent just changed,
        // and the Screens it marked in progress.
        if let Some(scene) = &self.scene {
            let placed = |id: &str, extra: Vec<(&str, Value)>| {
                let placed = scene.laid_out.get(id)?;
                // (Nothing of it shows: nothing to light.)
                if placed.problems.contains(&zeron_design::layout::Problem::FullyClipped) {
                    return None;
                }
                let bounds = placed.bounds;
                let mut fields = vec![
                    ("id", Value::from(id)),
                    ("x", Value::Num(bounds.x as f64)),
                    ("y", Value::Num(bounds.y as f64)),
                    ("w", Value::Num(bounds.width as f64)),
                    ("h", Value::Num(bounds.height as f64)),
                ];
                fields.extend(extra);
                Some(Value::record(fields))
            };
            let flashes: Vec<Value> = self
                .flashes
                .iter()
                .filter(|f| f.at.elapsed() < FLASH_STALE)
                .flat_map(|f| f.ids.iter().filter_map(move |id| placed(id, vec![("version", Value::Num(f.version as f64))])))
                .collect();
            let working: Vec<Value> = scene
                .design
                .children
                .iter()
                .filter(|node| node.in_progress)
                .filter_map(|node| placed(&node.id, vec![("name", Value::from(node.name.as_str()))]))
                .collect();
            fields.push(("flashes", Value::List(flashes.into())));
            fields.push(("working", Value::List(working.into())));
            // The selection (#32), where each node is.
            let selected: Vec<Value> = self.selection.iter().filter_map(|id| placed(id, Vec::new())).collect();
            fields.push(("selected", Value::List(selected.into())));
            // The properties panel's: the one node selected, as written.
            if let [one] = self.selection.as_slice()
                && let Some(node) = scene.written.find(one)
                && let Ok(props) = serde_json::to_value(node)
            {
                fields.push(("props", crate::plugins::json_value(&props)));
            }
            fields.push(("themes", crate::plugins::json_value(&Json::Object(scene.written.themes.clone()))));
            fields.push(("theme", crate::plugins::json_value(&Json::Object(self.theme.clone()))));

        }
        if let Some(why) = &self.failed {
            fields.push(("error", Value::from(why.as_str())));
        }
        // Where the whole Design draws, for a view of all of it.
        if let Some(extent) = self.scene.as_ref().and_then(|s| s.tops.iter().copied().reduce(|a, b| a.union(&b))) {
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
        Value::record(fields)
    }
}

/// A window's Design tabs.
pub struct DesignTabs {
    host: ScriptHost,
    source: Box<dyn Source>,
    tabs: RefCell<HashMap<String, TabCanvas>>,
    /// Each chat's Design tabs as last said (said again to a new engine).
    chats: RefCell<HashMap<String, Vec<String>>>,
    pool: Pool,
    /// The display's scale.
    dpr: Cell<f32>,
    clock: Cell<u64>,
    /// The versions of Designs heard (one count for every tab).
    versions: Cell<u64>,
}

/// The Design tabs of `cx`'s window, following this computer's engine.
pub fn install(cx: &mut zgx_desktop::WindowContext, host: &ScriptHost) {
    let (tabs, pump) = register(host, Box::new(EngineSource::default()));
    cx.tasks.spawn(pump);
    let weak = Rc::downgrade(&tabs);
    engine::get().on_connect(move |_| {
        if let Some(tabs) = weak.upgrade() {
            tabs.follow_again();
        }
    });
    // The display's scale, once the window knows its display.
    let info = cx.window_info.clone();
    let weak = Rc::downgrade(&tabs);
    let _ = cx.ui.runtime.effect(move || {
        let displays = info.displays.get();
        let at = info.current_display.get().unwrap_or(0);
        if let (Some(display), Some(tabs)) = (displays.get(at), weak.upgrade()) {
            tabs.set_dpr(display.scale_factor as f32);
        }
    });
}

/// The Design tabs' calls on `host`, following `source`: the tabs, and the
/// task that takes finished tiles to them, which keeps them (spawn it on
/// the window's tasks).
pub fn register(host: &ScriptHost, source: Box<dyn Source>) -> (Rc<DesignTabs>, impl std::future::Future<Output = ()> + use<>) {
    let (wake, mut woken) = zgx::task::latest::<()>();
    let tabs = Rc::new(DesignTabs {
        host: host.clone(),
        source,
        tabs: RefCell::default(),
        chats: RefCell::default(),
        pool: Pool { queue: Arc::default(), done: RefCell::default(), wake },
        dpr: Cell::new(1.),
        clock: Cell::new(0),
        versions: Cell::new(0),
    });
    let text = |v: &Value, k: &str| v.field(k).map(crate::auth::text).unwrap_or_default();
    let num = |v: &Value, k: &str| v.field(k).and_then(Value::as_f64).unwrap_or(0.) as f32;
    let me = Rc::downgrade(&tabs);
    host.action("design.open", move |arg| {
        if let Some(tabs) = me.upgrade() {
            tabs.open(&text(&arg, "tab"), &text(&arg, "path"));
        }
    });
    let me = Rc::downgrade(&tabs);
    host.action("design.view", move |arg| {
        let Some(tabs) = me.upgrade() else { return };
        let view = View {
            x: num(&arg, "x"),
            y: num(&arg, "y"),
            zoom: num(&arg, "zoom").clamp(ZOOM.0, ZOOM.1),
            w: num(&arg, "w"),
            h: num(&arg, "h"),
            settled: arg.field("settled").is_none_or(Value::truthy),
        };
        let tab = text(&arg, "tab");
        if let Some(canvas) = tabs.tabs.borrow_mut().get_mut(&tab) {
            canvas.view = Some(view);
        }
        tabs.update(&tab);
    });
    let me = Rc::downgrade(&tabs);
    host.action("design.flashed", move |arg| {
        let Some(tabs) = me.upgrade() else { return };
        let tab = text(&arg, "tab");
        let version = arg.field("version").and_then(Value::as_f64).unwrap_or(0.) as u64;
        // (Over, so a reload doesn't flash it again; a newer flash stays.)
        if let Some(canvas) = tabs.tabs.borrow_mut().get_mut(&tab) {
            canvas.flashes.retain(|f| f.version != version);
        }
        tabs.publish(&tab);
    });
    let me = Rc::downgrade(&tabs);
    // The engine's Design calls the tab makes (#31): New design and Recent
    // designs, and a tab's Design's file (by its tab: its file may have
    // changed since the script last heard).
    let calls = [
        ("design.new", "NewDesign", false),
        ("design.recent", "RecentDesigns", false),
        ("design.saving", "DesignSaving", true),
        ("design.save", "SaveDesign", true),
        ("design.save_as", "SaveDesignAs", true),
        ("design.discard", "DiscardDesign", true),
        // The person's edits by hand (#33), and stepping the Design's one
        // history, whoever's change is latest.
        ("design.edit", "EditDesign", true),
        ("design.undo", "UndoDesign", true),
        ("design.redo", "RedoDesign", true),
    ];
    for (action, method, of_tab) in calls {
        let me = Rc::downgrade(&tabs);
        host.async_action(action, move |arg| {
            let me = me.clone();
            async move {
                let asked = me.upgrade().ok_or("the window closed")?.ask(method, &arg, of_tab)?;
                Ok(crate::plugins::json_value(&asked.await?))
            }
        });
    }
    let me = Rc::downgrade(&tabs);
    host.action("design.close", move |arg| {
        if let Some(tabs) = me.upgrade() {
            tabs.close(&crate::auth::text(&arg));
        }
    });
    // Hand edits (#33): what a drag of a node by (`dx`, `dy`) Canvas units
    // changes, for the script to send (`design.edit`).
    let me = Rc::downgrade(&tabs);
    host.function("design.moved", move |arg| {
        let Some(tabs) = me.upgrade() else { return Value::Nil };
        let scene = tabs.tabs.borrow().get(&text(&arg, "tab")).and_then(|c| c.scene.clone());
        scene
            .and_then(|scene| moved(&scene, &text(&arg, "id"), num(&arg, "dx"), num(&arg, "dy")))
            .map_or(Value::Nil, |edit| crate::plugins::json_value(&edit))
    });
    // Editing text in place: the caret, a range and the place at a point,
    // in Canvas units, laid out as the text is drawn with `content` (what's
    // being typed, before the engine has it).
    let me = Rc::downgrade(&tabs);
    host.function("design.caret", move |arg| {
        let Some(tabs) = me.upgrade() else { return Value::Nil };
        let scene = tabs.tabs.borrow().get(&text(&arg, "tab")).and_then(|c| c.scene.clone());
        let Some(scene) = scene else { return Value::Nil };
        let Some((carets, at)) = carets_of(&scene, &text(&arg, "id"), &text(&arg, "content")) else { return Value::Nil };
        let shift = |r: Rect| rect_value(Rect { x: r.x + at.x, y: r.y + at.y, ..r });
        let place = |k: &str| arg.field(k).and_then(Value::as_f64).unwrap_or(0.).max(0.) as usize;
        match arg.field("x").and_then(Value::as_f64) {
            // A point: the place nearest it.
            Some(x) => Value::Num(carets.place_at(x as f32 - at.x, num(&arg, "y") - at.y) as f64),
            None => Value::record([
                ("caret", shift(carets.caret(place("at")))),
                ("range", Value::List(carets.range(place("from"), place("to")).into_iter().map(shift).collect::<Vec<_>>().into())),
                ("box", rect_value(at)),
            ]),
        }
    });
    let me = Rc::downgrade(&tabs);
    host.action("design.theme", move |arg| {
        let Some(tabs) = me.upgrade() else { return };
        let tab = text(&arg, "tab");
        let theme = match crate::plugins::value_json(arg.field("theme").unwrap_or(&Value::Nil)) {
            Json::Object(theme) => theme,
            _ => Theme::new(),
        };
        tabs.show_in(&tab, theme);
    });
    // Selecting (#32): what a click on the Canvas hits, and the tab's
    // selection, told to the engine for the chat's agent.
    let me = Rc::downgrade(&tabs);
    host.function("design.hit", move |arg| {
        let Some(tabs) = me.upgrade() else { return Value::Nil };
        let tab = text(&arg, "tab");
        let deep = arg.field("deep").is_some_and(Value::truthy);
        let scene = tabs.tabs.borrow().get(&tab).and_then(|c| c.scene.clone());
        scene
            .and_then(|scene| hit(&scene, num(&arg, "x"), num(&arg, "y"), deep))
            .map_or(Value::Nil, |id| Value::from(id.as_str()))
    });
    let me = Rc::downgrade(&tabs);
    host.action("design.select", move |arg| {
        let Some(tabs) = me.upgrade() else { return };
        let (tab, chat) = (text(&arg, "tab"), text(&arg, "chat"));
        let ids: Vec<String> = match arg.field("ids") {
            Some(Value::List(ids)) => ids.iter().map(crate::auth::text).collect(),
            _ => Vec::new(),
        };
        let path = {
            let mut all = tabs.tabs.borrow_mut();
            let Some(canvas) = all.get_mut(&tab) else { return };
            if canvas.selection == ids && canvas.selection_chat == chat {
                return;
            }
            canvas.selection.clone_from(&ids);
            canvas.selection_chat.clone_from(&chat);
            canvas.path.clone()
        };
        if !chat.is_empty() {
            tabs.source.select(&chat, &path, &ids);
        }
        tabs.publish(&tab);
    });
    let me = Rc::downgrade(&tabs);
    host.action("design.tabs", move |arg| {
        let Some(tabs) = me.upgrade() else { return };
        let chat = text(&arg, "chat");
        let paths: Vec<String> = match arg.field("paths") {
            Some(Value::List(paths)) => paths.iter().map(crate::auth::text).collect(),
            _ => Vec::new(),
        };
        if chat.is_empty() || tabs.chats.borrow().get(&chat) == Some(&paths) {
            return;
        }
        tabs.source.set_tabs(&chat, &paths);
        tabs.chats.borrow_mut().insert(chat, paths);
    });
    // (The task owns the tabs: its calls hold them only weakly, so they
    // live as long as the window's tasks, and go with them.)
    let held = tabs.clone();
    let pump = async move {
        while woken.recv().await.is_some() {
            held.take_drawn();
        }
    };
    (tabs, pump)
}

impl DesignTabs {
    /// The engine's `method` with the script's `arg`, or (`of_tab`) with the
    /// file of the tab it names (and its `to`, for Save as).
    fn ask(&self, method: &str, arg: &Value, of_tab: bool) -> Result<Asked, String> {
        if !of_tab {
            return Ok(self.source.ask(method, crate::plugins::value_json(arg)));
        }
        let tab = arg.field("tab").map(crate::auth::text).unwrap_or_default();
        let path = self.tabs.borrow().get(&tab).map(|c| c.path.clone()).ok_or("no Design in that tab")?;
        // (What else the script gives: Save as's `to`, an edit's `edits`.)
        let mut params = crate::plugins::value_json(arg);
        if !params.is_object() {
            params = json!({});
        }
        params.as_object_mut().map(|p| p.remove("tab"));
        params["path"] = json!(path);
        Ok(self.source.ask(method, params))
    }

    fn open(self: &Rc<Self>, tab: &str, path: &str) {
        if tab.is_empty() || path.is_empty() {
            return;
        }
        if self.tabs.borrow().get(tab).is_some_and(|c| c.path == path) {
            return;
        }
        self.close(tab);
        let canvas = TabCanvas {
            path: path.to_owned(),
            following: None,
            shared: self.host.shared(&format!("design.tab.{tab}"), Value::Nil),
            scene: None,
            failed: None,
            view: None,
            area: None,
            front: None,
            back: None,
            tiles: HashMap::new(),
            flashes: Vec::new(),
            selection: Vec::new(),
            selection_chat: String::new(),
            tree: self.host.shared(&format!("design.tree.{tab}"), Value::Nil),
            theme: Theme::new(),
            saving: FileState::default(),
        };
        self.tabs.borrow_mut().insert(tab.to_owned(), canvas);
        self.follow(tab);
        self.publish(tab);
    }

    /// Starts following `tab`'s Design, if it isn't yet.
    fn follow(self: &Rc<Self>, tab: &str) {
        let Some(path) = self.tabs.borrow().get(tab).filter(|c| c.following.is_none()).map(|c| c.path.clone()) else {
            return;
        };
        let (me, at) = (Rc::downgrade(self), tab.to_owned());
        let following = self.source.follow(
            &path,
            Box::new(move |heard| {
                if let Some(tabs) = me.upgrade() {
                    tabs.heard(&at, heard);
                }
            }),
        );
        if let Some(canvas) = self.tabs.borrow_mut().get_mut(tab) {
            canvas.following = following;
        }
    }

    /// Every tab's Design followed again, and every chat's Design tabs said
    /// again (a new connection to the engine, which may be a new engine).
    pub fn follow_again(self: &Rc<Self>) {
        for (chat, paths) in self.chats.borrow().iter() {
            self.source.set_tabs(chat, paths);
        }
        let tabs: Vec<String> = self.tabs.borrow().keys().cloned().collect();
        for tab in tabs {
            // (Let go once the tabs aren't borrowed: an ending may say so.)
            let old = self.tabs.borrow_mut().get_mut(&tab).and_then(|c| c.following.take());
            drop(old);
            self.follow(&tab);
        }
    }

    fn set_dpr(&self, dpr: f32) {
        if dpr > 0. && dpr != self.dpr.get() {
            self.dpr.set(dpr);
            let tabs: Vec<String> = self.tabs.borrow().keys().cloned().collect();
            for tab in tabs {
                self.update(&tab);
            }
        }
    }

    fn close(&self, tab: &str) {
        let Some(canvas) = self.tabs.borrow_mut().remove(tab) else { return };
        self.pool.take(tab);
        for key in canvas.tiles.keys() {
            self.host.image(&key.image_name(tab), None);
        }
        canvas.shared.set(Value::Nil);
        canvas.tree.set(Value::Nil);
    }

    /// The Design of `tab` as it is now (or why it can't be followed).
    fn heard(&self, tab: &str, heard: Result<Heard, String>) {
        // The layers list, set once the tabs aren't borrowed (what follows
        // it may call back: a selection pruned to what's still there).
        let mut tree = None;
        {
            let mut tabs = self.tabs.borrow_mut();
            let Some(canvas) = tabs.get_mut(tab) else { return };
            match heard {
                Err(why) => canvas.failed = Some(why),
                Ok(Heard { design, flash, fonts, assets, saving }) => {
                    canvas.failed = None;
                    // (Saved as another file: still this tab's, followed on,
                    // its selection told for that file.)
                    if let Some(path) = saving.path.as_ref().filter(|p| **p != canvas.path) {
                        canvas.path.clone_from(path);
                        if !canvas.selection.is_empty() && !canvas.selection_chat.is_empty() {
                            self.source.select(&canvas.selection_chat, path, &canvas.selection);
                        }
                    }
                    // The layers list: the Design's tree as written (an
                    // Instance one row, not what it expands to).
                    tree = Some((canvas.tree.clone(), Value::List(layers_of(&design).into())));
                    canvas.saving = saving;
                    self.versions.set(self.versions.get() + 1);
                    let scene = Arc::new(Scene::new(self.versions.get(), design, fonts, assets, canvas.theme.clone()));
                    canvas.flashes.retain(|f| f.at.elapsed() < FLASH_STALE);
                    if !flash.is_empty() {
                        canvas.flashes.push(Flash { version: scene.version, ids: flash, at: std::time::Instant::now() });
                    }
                    if let Some(old) = &canvas.scene {
                        let damage = old.damage(&scene);
                        let mut gone = Vec::new();
                        for (key, tile) in &mut canvas.tiles {
                            if !damage.iter().any(|d| d.meets(&key.rect())) {
                                continue;
                            }
                            if Some(key.level) == canvas.front {
                                // (Shown as it was until the new one comes.)
                                tile.wanted = scene.version;
                            } else if Some(key.level) != canvas.back {
                                // (The level before, shown while the zoom's
                                // fills in, goes once it has.)
                                gone.push(*key);
                            }
                        }
                        for key in gone {
                            canvas.tiles.remove(&key);
                            self.host.image(&key.image_name(tab), None);
                        }
                    }
                    canvas.scene = Some(scene);
                }
            }
        }
        if let Some((shared, list)) = tree {
            shared.set(list);
        }
        self.update(tab);
    }

    /// `tab`'s Design shown in `theme` (the switcher's): drawn again from
    /// what it last heard, every tile.
    fn show_in(&self, tab: &str, theme: Theme) {
        {
            let mut tabs = self.tabs.borrow_mut();
            let Some(canvas) = tabs.get_mut(tab) else { return };
            if canvas.theme == theme {
                return;
            }
            canvas.theme = theme.clone();
            let Some(old) = canvas.scene.clone() else { return };
            self.versions.set(self.versions.get() + 1);
            let scene = Arc::new(Scene::new(self.versions.get(), old.written.clone(), old.fonts.clone(), old.heard.clone(), theme));
            let front = canvas.front;
            let mut gone = Vec::new();
            for (key, tile) in &mut canvas.tiles {
                if Some(key.level) == front {
                    tile.wanted = scene.version;
                } else {
                    gone.push(*key);
                }
            }
            for key in gone {
                canvas.tiles.remove(&key);
                self.host.image(&key.image_name(tab), None);
            }
            canvas.scene = Some(scene);
        }
        self.update(tab);
    }

    /// Finished tiles, shown, and what they change.
    fn take_drawn(&self) {
        let mut touched = HashSet::new();
        while let Some(drawn) = self.pool.done.borrow().as_ref().and_then(|rx| rx.try_recv().ok()) {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tile) = tabs.get_mut(&drawn.tab).and_then(|c| c.tiles.get_mut(&drawn.key)) else {
                continue;
            };
            if tile.coming.is_some_and(|v| v <= drawn.version) {
                tile.coming = None;
            }
            if tile.drawn.is_some_and(|v| v >= drawn.version) {
                continue;
            }
            tile.drawn = Some(drawn.version);
            tile.image = drawn.image;
            self.host.image(&drawn.key.image_name(&drawn.tab), tile.image.clone());
            touched.insert(drawn.tab);
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
            let mut tabs = self.tabs.borrow_mut();
            let Some(canvas) = tabs.get_mut(tab) else { return };
            let (Some(view), Some(scene)) = (canvas.view, canvas.scene.clone()) else {
                drop(tabs);
                self.publish(tab);
                return;
            };
            if view.w <= 0. || view.h <= 0. {
                return;
            }
            // What it asked for and wasn't started on is asked for again
            // below, if still wanted.
            for key in self.pool.take(tab) {
                if let Some(tile) = canvas.tiles.get_mut(&key) {
                    tile.coming = None;
                }
            }
            // Levels: the zoom's own at rest; while it zooms, the last one
            // until the zoom is twofold away.
            let want = Level::of(view.zoom, dpr);
            let switch = match canvas.front {
                None => true,
                Some(level) if level == want => false,
                Some(level) => view.settled || !(0.5..=2.).contains(&(want.scale() / level.scale())),
            };
            let area = view.area();
            if switch {
                if let Some(front) = canvas.front {
                    // The level going stays under the new one if it showed
                    // whole (else the one under it already does).
                    let was = canvas.area.unwrap_or(area);
                    let whole = keys(front, was, 0).iter().all(|k| canvas.drawn(k));
                    if whole || canvas.back.is_none() {
                        canvas.back = Some(front);
                    }
                }
                canvas.front = Some(want);
            }
            canvas.area = Some(area);
            let front = canvas.front.unwrap_or(want);
            let visible = keys(front, area, 0);
            if visible.iter().all(|k| canvas.drawn(k)) {
                canvas.back = None;
            }
            // What's missing: in view first, nearest the middle first, then
            // the ring of tiles round it.
            let (cx, cy) = (area.x + area.width / 2., area.y + area.height / 2.);
            let in_view: HashSet<TileKey> = visible.iter().copied().collect();
            for key in keys(front, area, 1) {
                let tile = canvas.tiles.entry(key).or_default();
                tile.used = clock;
                if !tile.needed() {
                    continue;
                }
                tile.wanted = tile.wanted.max(scene.version);
                tile.coming = Some(scene.version);
                let r = key.rect();
                let far = ((r.x + r.width / 2. - cx).powi(2) + (r.y + r.height / 2. - cy).powi(2)).sqrt() / r.width;
                let priority = far + if in_view.contains(&key) { 0. } else { 1000. };
                jobs.push(Job { tab: tab.to_owned(), key, scene: scene.clone(), priority });
            }
            if let Some(back) = canvas.back {
                for key in keys(back, area, 0) {
                    if let Some(tile) = canvas.tiles.get_mut(&key) {
                        tile.used = clock;
                    }
                }
            }
        }
        self.pool.ask(jobs);
        self.evict();
        self.publish(tab);
    }

    /// Lets go of what no tab shows now: every tile with nothing to hold
    /// (where nothing draws, or not drawn and not coming), then those shown
    /// longest ago while every tab's together are over [`CACHE_BYTES`].
    fn evict(&self) {
        let mut tabs = self.tabs.borrow_mut();
        let in_use: HashMap<String, HashSet<TileKey>> = tabs.iter().map(|(tab, c)| (tab.clone(), c.in_use())).collect();
        for (tab, canvas) in tabs.iter_mut() {
            let keep = &in_use[tab];
            canvas.tiles.retain(|key, t| t.image.is_some() || t.coming.is_some() || keep.contains(key));
        }
        let mut held: Vec<(u64, String, TileKey)> = tabs
            .iter()
            .flat_map(|(tab, c)| {
                let keep = &in_use[tab];
                c.tiles
                    .iter()
                    .filter(|(k, t)| t.image.is_some() && !keep.contains(k))
                    .map(move |(k, t)| (t.used, tab.clone(), *k))
            })
            .collect();
        let shown: usize = tabs.values().map(|c| c.tiles.values().filter(|t| t.image.is_some()).count()).sum();
        let most = CACHE_BYTES / TILE_BYTES;
        if shown <= most {
            return;
        }
        held.sort();
        for (_, tab, key) in held.into_iter().take(shown - most) {
            if let Some(canvas) = tabs.get_mut(&tab) {
                canvas.tiles.remove(&key);
                self.host.image(&key.image_name(&tab), None);
            }
        }
    }

    fn publish(&self, tab: &str) {
        // (Set once the tabs aren't borrowed: what reads it may call back.)
        let made = self.tabs.borrow().get(tab).map(|canvas| (canvas.shared.clone(), canvas.value(tab, self.dpr.get())));
        if let Some((shared, value)) = made {
            shared.set(value);
        }
    }

    /// The tile memory held now, in bytes.
    #[cfg(test)]
    pub fn cached_bytes(&self) -> usize {
        self.tabs.borrow().values().map(|c| c.tiles.values().filter(|t| t.image.is_some()).count()).sum::<usize>() * TILE_BYTES
    }

    /// The tiles `tab` shows, back to front: each node's id and its image.
    #[cfg(test)]
    pub fn shown(&self, tab: &str) -> Vec<(String, Arc<ImageData>)> {
        let tabs = self.tabs.borrow();
        let Some(canvas) = tabs.get(tab) else { return Vec::new() };
        let Some(area) = canvas.area else { return Vec::new() };
        [canvas.back, canvas.front]
            .into_iter()
            .flatten()
            .flat_map(|level| keys(level, area, 0))
            .filter_map(|key| canvas.tiles.get(&key)?.image.clone().map(|image| (key.node_id(tab), image)))
            .collect()
    }

    /// Tiles in `tab`'s view where its Design draws that show nothing: no
    /// tile of the zoom's level drawn yet, nor the level's before it.
    #[cfg(test)]
    pub fn blank(&self, tab: &str) -> usize {
        let tabs = self.tabs.borrow();
        let Some(canvas) = tabs.get(tab) else { return 0 };
        let (Some(area), Some(scene), Some(front)) = (canvas.area, &canvas.scene, canvas.front) else { return 0 };
        keys(front, area, 0)
            .into_iter()
            .filter(|key| scene.draws_in(&key.rect()))
            .filter(|key| {
                !canvas.drawn(key) && canvas.back.is_none_or(|back| !keys(back, key.rect(), 0).iter().all(|k| canvas.drawn(k)))
            })
            .count()
    }

    /// Whether `tab` shows every tile in view at its zoom's own level, each
    /// of the Design as it is now.
    #[cfg(test)]
    pub fn sharp(&self, tab: &str) -> bool {
        let tabs = self.tabs.borrow();
        let Some(canvas) = tabs.get(tab) else { return false };
        let Some(view) = canvas.view else { return false };
        let want = Level::of(view.zoom, self.dpr.get());
        // (A tile where nothing draws is drawn too, as nothing.)
        canvas.front == Some(want)
            && canvas.back.is_none()
            && keys(want, view.area(), 0)
                .iter()
                .all(|key| canvas.tiles.get(key).is_some_and(|t| t.drawn.is_some_and(|v| v >= t.wanted)))
    }

    /// `tab`'s view: its pan and zoom.
    #[cfg(test)]
    pub fn view(&self, tab: &str) -> Option<(f32, f32, f32)> {
        self.tabs.borrow().get(tab)?.view.map(|v| (v.x, v.y, v.zoom))
    }
}
