//! Designs (apps/zgx/docs/design): this engine owns every live Design on
//! its computer (ADR-0003) and applies agents' snippets to each in order.
//!
//! A call ([`Designs::execute`]) finds its Design ([`Designs::target`]),
//! runs its snippet against a draft of it in the QuickJS sandbox, and on
//! success keeps the draft ([`Live::commit`]): the file is saved, then the
//! call's screenshots drawn (one that can't be drawn says why in its
//! place). Each kept call is one step of the Design's undo stack.
//!
//! A failed call changes nothing, on disk or in memory, nor the chat's
//! globals: it returns a [`Failure`] with an edit id, and its snippet is
//! kept for the chat to retry with find-and-replace edits ([`retry`]).
//!
//! New Designs are made in the managed folder, `designs/` in the
//! engine's data dir, one folder each with an `images/` folder beside the
//! `.zdesign` file. Assets (icons) are fetched as a call places them and
//! cached in `design-cache/` beside it ([`zeron_design::assets::Cache`]);
//! a Design's tabs hear the files of those it uses.
//!
//! Several chats may change one Design: they share its one live copy, and
//! a call is told when someone else changed it since that chat's last. The
//! engine also keeps which Designs each chat's Design tabs show
//! ([`Designs::set_tabs`]), and the Designs touched most recently on this
//! computer ([`Designs::recent`]), in `recent-designs.json`. Design tabs
//! follow a Design ([`Designs::watch`]): it whole, then again after each
//! change.

mod iconify;
mod retry;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeron_design::Design;
use zeron_design::assets::{Cache, Resolved};
use zeron_design::edit::{Draft, Edited};
use zeron_design::history::History;
use zeron_design::icons::Icons;
use zeron_design::snippet::{self, Globals, Limits, SnippetError};

pub use retry::{Edit, Failure};
use retry::{Failed, Kept, Retarget};

use self::iconify::Iconify;

/// A `.zdesign` file's extension.
pub const EXTENSION: &str = "zdesign";

/// Longest name a managed Design's folder takes from its Design's name.
const MAX_NAME_CHARS: usize = 64;

/// How many recent Designs are kept.
const MAX_RECENT: usize = 50;

/// What `design_state` reminds an agent of.
pub const GUIDE_REMINDER: &str = "Design with the design guide loaded: the core guide comes with your first design call in a chat; call design_guide for it again (after your context was compacted, say) or for a topic guide.";

/// What a call is told when its Design changed since that chat's last call
/// (a chat's first call on a Design has nothing to compare, so isn't).
const CHANGED_NOTICE: &str = "This Design changed since your last call (another chat or the person changed it): read nodes again before changing what you didn't just make.";

/// `DesignExecute`'s params, as Zeron's `design_execute` tool sends them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteParams {
    /// The calling chat (the agent's, set by the engine for an agent).
    #[serde(default)]
    pub chat_id: Option<String>,
    /// Left out when retrying a failed call.
    #[serde(default)]
    pub snippet: Option<String>,
    /// What the call does, in a few words.
    #[serde(default)]
    pub description: Option<String>,
    /// A `.zdesign` file to change.
    #[serde(default)]
    pub path: Option<String>,
    /// The name of a new managed Design to make.
    #[serde(default)]
    pub name: Option<String>,
    /// The failed call to retry, with `edits` to its snippet.
    #[serde(default)]
    pub edit_id: Option<String>,
    #[serde(default)]
    pub edits: Vec<Edit>,
}

/// `WatchDesign`'s params.
#[derive(Debug, Deserialize)]
pub struct WatchParams {
    /// The `.zdesign` file to follow.
    pub path: String,
}

/// `SetDesignTabs`'s params: a chat's Design tabs, the front one first.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabsParams {
    pub chat_id: String,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

/// `DesignState`'s params, as Zeron's `design_state` tool sends them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateParams {
    /// The calling chat (the agent's, set by the engine for an agent).
    #[serde(default)]
    pub chat_id: Option<String>,
}

/// A Design touched recently on this computer, by an agent or the person.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub name: String,
    pub path: PathBuf,
    /// When, in ms since the Unix epoch.
    pub touched_at: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DesignError {
    /// The request can't be met as asked; the agent can fix it and retry.
    #[error("{0}")]
    Refused(String),
    /// The call ran and failed, changing nothing; the agent can retry it
    /// with edits.
    #[error("{}", .0.error)]
    Failed(Box<Failure>),
    #[error("{0}")]
    Io(String),
}

#[derive(Clone)]
pub struct Designs {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    limits: Limits,
    assets: Arc<Cache>,
    /// Designs held live, by file: one owner each, so calls apply in order.
    live: Mutex<HashMap<PathBuf, Arc<Mutex<Live>>>>,
    /// Each chat's snippet globals, carried to its next call.
    globals: Mutex<HashMap<String, Globals>>,
    /// Failed calls, kept to retry.
    failed: Mutex<Failed>,
    /// The Designs each chat's Design tabs show, front first.
    tabs: Mutex<HashMap<String, Vec<PathBuf>>>,
    /// The Designs touched most recently, newest first, as kept in
    /// `recent_file`.
    recent: Mutex<Vec<Recent>>,
    recent_file: PathBuf,
}

/// One Design as the engine holds it.
struct Live {
    path: PathBuf,
    design: Design,
    history: History,
    /// Counts the changes kept, whoever made them.
    version: u64,
    /// The version each chat's last successful call left.
    seen: HashMap<String, u64>,
    /// What the Design tabs following it hear: it after each change.
    followers: tokio::sync::watch::Sender<Told>,
    /// Where its tabs are told the files of its assets.
    assets: Arc<Cache>,
}

/// A Design as the tabs following it hear it: its version, the changes
/// that made the latest versions (a tab that falls behind hears each
/// change it missed, up to [`MAX_TOLD_CHANGES`]), and the files of the
/// assets it uses (`None` until resolved for a tab).
#[derive(Clone)]
struct Told {
    version: u64,
    design: Arc<Design>,
    changes: Vec<Change>,
    assets: Option<Vec<Resolved>>,
}

/// One change to a Design, as its tabs hear of it.
#[derive(Clone, Serialize)]
struct Change {
    version: u64,
    /// The nodes it made or changed.
    changed: Vec<String>,
    by: Changer,
}

/// How many of the latest changes a Design's tabs are told of.
const MAX_TOLD_CHANGES: usize = 32;

/// Who changed a Design: its tabs flash an agent's changes. (The person's
/// own edits in a Design tab come with #33.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Changer {
    /// An agent's `design_execute` call.
    Agent,
    /// An undo or redo of an earlier change.
    Undo,
}

impl Designs {
    /// Designs whose managed folder is `designs/` in `data_dir`.
    pub fn new(data_dir: &Path) -> Self {
        // Made now and in its canonical form, so a managed Design named by
        // its path later is the one already held.
        let root = data_dir.join("designs");
        let root = std::fs::create_dir_all(&root)
            .and_then(|()| canonical(&root))
            .unwrap_or(root);
        let recent_file = data_dir.join("recent-designs.json");
        let recent = std::fs::read_to_string(&recent_file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                root,
                limits: Limits::default(),
                assets: Arc::new(Cache::new(&data_dir.join("design-cache"), Arc::new(Iconify))),
                live: Mutex::new(HashMap::new()),
                globals: Mutex::new(HashMap::new()),
                failed: Mutex::new(Failed::default()),
                tabs: Mutex::new(HashMap::new()),
                recent: Mutex::new(recent),
                recent_file,
            }),
        }
    }

    /// The managed folder new Designs are made in.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The icons Designs draw, cached in `design-cache/icons/` in the
    /// engine's data dir.
    pub fn icons(&self) -> &Icons {
        self.inner.assets.icons()
    }

    /// Runs one `design_execute` call to its end (blocking: run it off the
    /// async runtime). Returns the Design's name and path, the name-to-id
    /// map of the nodes the call made, its screenshots as base64 PNG, the
    /// lines it printed and any warnings about its edits. A call that
    /// fails once it runs is a [`DesignError::Failed`], kept to retry.
    pub fn execute(&self, params: ExecuteParams) -> Result<Value, DesignError> {
        let (params, edit_id) = match params.edit_id.clone() {
            Some(edit_id) => (self.retried(params, &edit_id)?, Some(edit_id)),
            None => (params, None),
        };
        let code = params.snippet.as_deref().unwrap_or_default();
        if code.trim().is_empty() {
            return Err(DesignError::Refused("the snippet is empty".into()));
        }
        let target = self.target(&params)?;
        let ran = self.run(&target.live, code, &params);
        if ran.is_err() && target.new {
            // Nothing was made: the name is free again.
            lock(&self.inner.live).retain(|_, live| !Arc::ptr_eq(live, &target.live));
        }
        let (error, line) = match ran {
            Ok(result) => {
                if let Some(edit_id) = &edit_id {
                    lock(&self.inner.failed).remove(edit_id);
                }
                return Ok(result);
            }
            Err(Ran::Snippet(SnippetError::Failed { message, line })) => (message, line),
            Err(Ran::Snippet(e)) => (e.to_string(), None),
            Err(Ran::Design(e)) => (e.to_string(), None),
        };
        let retarget = match (&params.name, target.new) {
            (Some(name), true) => Retarget::Name(name.clone()),
            _ => Retarget::Path(lock(&target.live).path.clone()),
        };
        let kept = Kept {
            chat: params.chat_id.clone(),
            snippet: code.to_owned(),
            description: params.description.clone(),
            target: retarget,
        };
        let edit_id = lock(&self.inner.failed).keep(edit_id, kept);
        Err(DesignError::Failed(Box::new(Failure::new(error, line, code, edit_id))))
    }

    /// A retry's call: the failed call `edit_id`'s, its snippet patched
    /// with the retry's edits and kept so, should this try fail too.
    fn retried(&self, retry: ExecuteParams, edit_id: &str) -> Result<ExecuteParams, DesignError> {
        if retry.snippet.is_some() {
            return Err(DesignError::Refused(
                "a retry sends edits to the failed call's snippet, not a snippet: leave out snippet".into(),
            ));
        }
        if retry.path.is_some() || retry.name.is_some() {
            return Err(DesignError::Refused(
                "a retry runs on the failed call's Design: leave out path and name".into(),
            ));
        }
        let mut failed = lock(&self.inner.failed);
        let kept = failed.get(edit_id, retry.chat_id.as_deref()).cloned().ok_or_else(|| {
            DesignError::Refused(format!(
                "no failed call of this chat has the edit id \"{edit_id}\" (a retry that succeeds is done with it)"
            ))
        })?;
        let snippet = retry::patched(&kept.snippet, &retry.edits).map_err(|error| {
            DesignError::Failed(Box::new(Failure::new(error, None, &kept.snippet, edit_id.to_owned())))
        })?;
        failed.keep(Some(edit_id.to_owned()), Kept { snippet: snippet.clone(), ..kept.clone() });
        let (path, name) = match kept.target {
            Retarget::Path(path) => (Some(path.to_string_lossy().into_owned()), None),
            Retarget::Name(name) => (None, Some(name)),
        };
        Ok(ExecuteParams {
            chat_id: retry.chat_id,
            snippet: Some(snippet),
            description: retry.description.or(kept.description),
            path,
            name,
            edit_id: None,
            edits: Vec::new(),
        })
    }

    /// Undoes the latest change to the Design at `path`, whoever made it,
    /// and saves it. False with nothing to undo.
    pub fn undo(&self, path: &Path) -> Result<bool, DesignError> {
        lock(&*self.open(path)?).step(History::undo, History::redo)
    }

    /// Does the latest undone change to the Design at `path` again.
    pub fn redo(&self, path: &Path) -> Result<bool, DesignError> {
        lock(&*self.open(path)?).step(History::redo, History::undo)
    }

    /// Follows the `.zdesign` file at `path` (`WatchDesign`, a Design
    /// tab's): it whole as it is now, then after each change, as `{version,
    /// design, changes: [{version, changed, by}], assets}`: the changes
    /// since the item before (none in the first), `by` "agent", and the
    /// file of each asset it uses (`{kind, name, path}`), for the tab to
    /// draw from without fetching. A tab that falls behind hears the latest only.
    /// Blocking (it may read the file): run it off the async runtime.
    pub fn watch(&self, path: &str) -> Result<impl Stream<Item = Value> + Send + use<>, DesignError> {
        let live = self.open(Path::new(path))?;
        let told = {
            let live = lock(&live);
            // (Changes with nobody following weren't copied for them.)
            live.followers.send_if_modified(|told| {
                let stale = told.version != live.version || told.assets.is_none();
                if stale {
                    *told = Told {
                        version: live.version,
                        design: Arc::new(live.design.clone()),
                        changes: Vec::new(),
                        assets: Some(live.assets.resolved(&live.design)),
                    };
                }
                stale
            });
            live.followers.subscribe()
        };
        // (The version it last told, so each item tells the changes since.)
        Ok(futures::stream::unfold((told, None::<u64>), |(mut told, last)| async move {
            if last.is_some() && told.changed().await.is_err() {
                return None;
            }
            let now = told.borrow_and_update().clone();
            let since: Vec<&Change> = match last {
                Some(last) => now.changes.iter().filter(|change| change.version > last).collect(),
                None => Vec::new(),
            };
            let item = json!({
                "version": now.version,
                "design": &*now.design,
                "changes": since,
                "assets": now.assets.as_deref().unwrap_or_default(),
            });
            Some((item, (told, Some(now.version))))
        }))
    }

    /// Runs `code` with the calling chat's globals, keeping the globals it
    /// leaves when it succeeds. Read once the Design is held, so calls
    /// queued on one Design see each other's; calls on two Designs at once
    /// each start from what was there, and the later to end wins.
    fn run(&self, live: &Mutex<Live>, code: &str, params: &ExecuteParams) -> Result<Value, Ran> {
        let mut live = lock(live);
        let chat = params.chat_id.as_deref();
        let globals = chat
            .and_then(|chat| lock(&self.inner.globals).get(chat).cloned())
            .unwrap_or_default();
        let (draft, globals) = snippet::run(Draft::new(live.design.clone()).with_assets(self.inner.assets.clone()), globals, code, self.inner.limits)
            .map_err(Ran::Snippet)?;
        let edited = draft.finish();
        let changed = chat
            .and_then(|chat| live.seen.get(chat))
            .is_some_and(|&seen| seen != live.version);
        let description = params.description.as_deref().unwrap_or(DEFAULT_DESCRIPTION);
        let screenshots = live.commit(&edited, description, Changer::Agent).map_err(Ran::Design)?;
        if let Some(chat) = chat {
            lock(&self.inner.globals).insert(chat.to_owned(), globals);
            let version = live.version;
            live.seen.insert(chat.to_owned(), version);
        }
        let mut result = json!({
            "design": { "name": design_name(&live.path), "path": live.path },
            "ids": edited.names,
            "screenshots": screenshots,
        });
        if !edited.printed.is_empty() {
            result["printed"] = json!(edited.printed);
        }
        if !edited.warnings.is_empty() {
            result["warnings"] = json!(edited.warnings);
        }
        if changed {
            result["notice"] = json!(CHANGED_NOTICE);
        }
        // Not holding the Design up for the recent list's write.
        let path = live.path.clone();
        drop(live);
        self.touch(&path);
        Ok(result)
    }

    /// `design_state` for `chat_id`: the Designs its Design tabs show,
    /// front first, the recent ones, and the guide reminder.
    pub fn state(&self, chat_id: Option<&str>) -> Value {
        let tabs = chat_id
            .and_then(|chat| lock(&self.inner.tabs).get(chat).cloned())
            .unwrap_or_default();
        let designs: Vec<Value> = tabs
            .iter()
            .enumerate()
            .map(|(i, path)| json!({ "name": design_name(path), "path": path, "front": i == 0 }))
            .collect();
        json!({ "designs": designs, "recent": self.recent(), "guide": GUIDE_REMINDER })
    }

    /// Sets the Designs `chat_id`'s Design tabs show, the one in front
    /// first (none when it has no Design tab): the app's Design tabs say so
    /// as they open, close and come to the front (`SetDesignTabs`). A call
    /// that names no Design changes the front one. A Design newly open is
    /// touched.
    pub fn set_tabs(&self, chat_id: &str, paths: Vec<PathBuf>) {
        let paths: Vec<PathBuf> = paths.into_iter().map(|p| canonical(&p).unwrap_or(p)).collect();
        let mut tabs = lock(&self.inner.tabs);
        let before = tabs.get(chat_id).cloned().unwrap_or_default();
        let opened: Vec<PathBuf> = paths.iter().filter(|p| !before.contains(p)).cloned().collect();
        if paths.is_empty() {
            tabs.remove(chat_id);
        } else {
            tabs.insert(chat_id.to_owned(), paths);
        }
        drop(tabs);
        // The front one last, so it's the newest.
        for path in opened.iter().rev() {
            self.touch(path);
        }
    }

    /// The Designs touched most recently on this computer, newest first,
    /// leaving out any whose file is gone.
    pub fn recent(&self) -> Vec<Recent> {
        let recent = lock(&self.inner.recent).clone();
        recent.into_iter().filter(|recent| recent.path.is_file()).collect()
    }

    /// Makes the Design at `path` the most recent, and keeps the list.
    fn touch(&self, path: &Path) {
        let mut recent = lock(&self.inner.recent);
        recent.retain(|recent| recent.path != path);
        let touched_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        recent.insert(
            0,
            Recent {
                name: design_name(path),
                path: path.to_owned(),
                touched_at,
            },
        );
        recent.truncate(MAX_RECENT);
        let text = serde_json::to_string_pretty(&*recent).unwrap_or_default();
        // A list that can't be kept still serves this run.
        if let Err(e) = write_whole(&self.inner.recent_file, text.as_bytes()) {
            tracing::warn!("can't keep the recent Designs: {e}");
        }
    }

    /// The Design a call is for: the file it names, a new managed Design by
    /// the name it gives (made on disk when the call succeeds), or else the
    /// one in the calling chat's front Design tab.
    fn target(&self, params: &ExecuteParams) -> Result<Target, DesignError> {
        match (params.path.as_deref(), params.name.as_deref()) {
            (Some(path), None) => Ok(Target {
                live: self.open(Path::new(path))?,
                new: false,
            }),
            (None, Some(name)) => Ok(Target {
                live: self.create(name)?,
                new: true,
            }),
            (Some(_), Some(_)) => Err(DesignError::Refused(
                "give either the path of a Design or the name of a new one, not both".into(),
            )),
            (None, None) => {
                let front = params
                    .chat_id
                    .as_deref()
                    .and_then(|chat| lock(&self.inner.tabs).get(chat)?.first().cloned());
                match front {
                    Some(path) if !path.is_file() => Err(DesignError::Refused(format!(
                        "your front Design tab shows {}, which is no longer on disk: give the `path` of a .{EXTENSION} file, or a `name` for a new Design",
                        path.display()
                    ))),
                    Some(path) => Ok(Target {
                        live: self.open(&path)?,
                        new: false,
                    }),
                    None => Err(self.untargeted()),
                }
            }
        }
    }

    /// Why a call that names no Design, from a chat with no Design tab,
    /// can't run, with the Designs it could name.
    fn untargeted(&self) -> DesignError {
        let mut open: Vec<PathBuf> = lock(&self.inner.tabs).values().flatten().cloned().collect();
        open.sort();
        open.dedup();
        let recent: Vec<PathBuf> = self
            .recent()
            .into_iter()
            .map(|recent| recent.path)
            .filter(|path| !open.contains(path))
            .collect();
        let mut text = String::from(
            "name a Design: this chat has no Design tab open, so give the `path` of a .zdesign file, or a `name` for a new Design.",
        );
        if open.is_empty() && recent.is_empty() {
            text.push_str(" There are no Designs yet on this computer.");
        }
        for (title, paths) in [("Open in Design tabs", open), ("Recent", recent)] {
            if !paths.is_empty() {
                text.push_str(&format!("\n{title}:"));
                for path in paths {
                    text.push_str(&format!("\n- {}: {}", design_name(&path), path.display()));
                }
            }
        }
        DesignError::Refused(text)
    }

    /// The live Design of the `.zdesign` file at `path`, read on first use.
    fn open(&self, path: &Path) -> Result<Arc<Mutex<Live>>, DesignError> {
        if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
            return Err(DesignError::Refused(format!(
                "{} is not a .{EXTENSION} file",
                path.display()
            )));
        }
        // A Design already held, named as it was given back, without opening
        // its file: on Windows, a handle open on it (as canonicalizing holds
        // one) fails another call's save, which replaces the file.
        if let Some(design) = lock(&self.inner.live).get(path) {
            return Ok(design.clone());
        }
        let path = canonical(path).map_err(|e| {
            DesignError::Refused(match e.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "there's no file at {}: give the path of a .{EXTENSION} file on this computer, or a name for a new Design",
                    path.display()
                ),
                _ => format!("can't open {}: {e}", path.display()),
            })
        })?;
        if let Some(design) = lock(&self.inner.live).get(&path) {
            return Ok(design.clone());
        }
        // Read without holding every Design up; the first read held wins.
        let text = std::fs::read_to_string(&path)
            .map_err(|e| DesignError::Refused(format!("can't read {}: {e}", path.display())))?;
        let design = Design::from_json(&text).map_err(|e| {
            DesignError::Refused(format!("{} is not a Design: {e}", path.display()))
        })?;
        let design = Arc::new(Mutex::new(Live::new(path.clone(), design, self.inner.assets.clone())));
        Ok(lock(&self.inner.live).entry(path).or_insert(design).clone())
    }

    /// A new managed Design named `name`, held from now on, in a folder of
    /// its own named after it (`Name 2` and so on when that's taken). Its
    /// file is written when the call making it succeeds.
    fn create(&self, name: &str) -> Result<Arc<Mutex<Live>>, DesignError> {
        let base = folder_name(name)?;
        let mut live = lock(&self.inner.live);
        // Without regard to case, as Windows and macOS file systems compare.
        let taken = |folder: &str| {
            self.inner.root.join(folder).exists()
                || live.keys().any(|path| {
                    path.parent()
                        .and_then(Path::file_name)
                        .is_some_and(|held| held.to_string_lossy().eq_ignore_ascii_case(folder))
                })
        };
        let mut folder = base.clone();
        let mut n = 1;
        while taken(&folder) {
            n += 1;
            folder = format!("{base} {n}");
        }
        let path = self.inner.root.join(&folder).join(format!("{folder}.{EXTENSION}"));
        let design = Arc::new(Mutex::new(Live::new(path.clone(), Design::default(), self.inner.assets.clone())));
        live.insert(path, design.clone());
        Ok(design)
    }
}

/// How a call that ran failed.
enum Ran {
    Snippet(SnippetError),
    Design(DesignError),
}

/// A call's undo step's description when the agent gave none.
const DEFAULT_DESCRIPTION: &str = "An agent's change";

/// The Design a call is for, and whether the call is making it.
struct Target {
    live: Arc<Mutex<Live>>,
    new: bool,
}

impl Live {
    fn new(path: PathBuf, design: Design, assets: Arc<Cache>) -> Self {
        let told = Told {
            version: 0,
            design: Arc::new(design.clone()),
            changes: Vec::new(),
            assets: None,
        };
        Self {
            path,
            design,
            history: History::default(),
            version: 0,
            seen: HashMap::new(),
            followers: tokio::sync::watch::channel(told).0,
            assets,
        }
    }

    /// Makes `design` the Design, as every change does: saves it, then
    /// tells other chats' next calls and the Design's tabs, with the nodes
    /// `changed`. Returns the Design as it was.
    fn keep(&mut self, design: Design, changed: Vec<String>, by: Changer) -> Result<Design, DesignError> {
        save(&self.path, &design)?;
        let before = std::mem::replace(&mut self.design, design);
        self.version += 1;
        self.tell(changed, by);
        Ok(before)
    }

    /// Takes one step through the Design's history with `step` (undo or
    /// redo), or back with `back` when the Design can't be saved. False
    /// with no step to take.
    fn step(
        &mut self,
        step: fn(&mut History, &Design) -> Option<(Design, String)>,
        back: fn(&mut History, &Design) -> Option<(Design, String)>,
    ) -> Result<bool, DesignError> {
        let Some((design, _)) = step(&mut self.history, &self.design) else {
            return Ok(false);
        };
        if let Err(e) = self.keep(design.clone(), Vec::new(), Changer::Undo) {
            back(&mut self.history, &design);
            return Err(e);
        }
        Ok(true)
    }

    /// Tells the Design's tabs what a change (`by` whom) made of it, and
    /// the nodes it `changed` (copied for them only while some follow it).
    fn tell(&self, changed: Vec<String>, by: Changer) {
        if self.followers.receiver_count() == 0 {
            return;
        }
        let change = Change {
            version: self.version,
            changed,
            by,
        };
        let design = Arc::new(self.design.clone());
        let assets = self.assets.resolved(&self.design);
        self.followers.send_modify(|told| {
            told.version = change.version;
            told.design = design;
            told.assets = Some(assets);
            told.changes.push(change);
            let over = told.changes.len().saturating_sub(MAX_TOLD_CHANGES);
            told.changes.drain(..over);
        });
    }

    /// Keeps a successful change's draft (`by` whom) as one undo step,
    /// described as `description`: saves the file, then draws the
    /// screenshots it asked for.
    fn commit(&mut self, edited: &Edited, description: &str, by: Changer) -> Result<Vec<Value>, DesignError> {
        let before = self.keep(edited.design.clone(), edited.changed.iter().cloned().collect(), by)?;
        self.history.record(description, &before, &self.design);
        Ok(edited
            .screenshots
            .iter()
            .map(|id| {
                let shot = zeron_design::render::screenshot(
                    &self.design,
                    id,
                    zeron_design::render::SCREENSHOT_MAX_SIDE,
                    &*self.assets,
                );
                match shot {
                    Ok(shot) => json!({
                        "id": shot.id,
                        "width": shot.width,
                        "height": shot.height,
                        "mimeType": "image/png",
                        "data": base64::engine::general_purpose::STANDARD.encode(&shot.png),
                    }),
                    Err(e) => json!({ "id": id, "error": e.to_string() }),
                }
            })
            .collect())
    }
}

/// Writes the file whole or not at all, making a new managed Design's
/// folder (with its `images/`) on its first save.
fn save(path: &Path, design: &Design) -> Result<(), DesignError> {
    let io = |e: std::io::Error| DesignError::Io(format!("can't save {}: {e}", path.display()));
    let folder = path.parent().ok_or_else(|| DesignError::Io("a Design's path has no folder".into()))?;
    if !path.exists() {
        std::fs::create_dir_all(folder.join("images")).map_err(io)?;
    }
    write_whole(path, design.to_json().as_bytes()).map_err(io)
}

/// Writes `path` whole or not at all.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    std::io::Write::write_all(&mut file, bytes)?;
    // Windows refuses to replace a file another process (a virus scanner,
    // the indexer) has open for a moment after it was written: try again.
    let mut tries = 0;
    loop {
        match file.persist(path) {
            Ok(_) => return Ok(()),
            Err(e) if e.error.kind() == std::io::ErrorKind::PermissionDenied && tries < PERSIST_TRIES => {
                tries += 1;
                file = e.file;
                std::thread::sleep(std::time::Duration::from_millis(10 * tries));
            }
            Err(e) => return Err(e.error),
        }
    }
}

/// How many times a save tries again to replace its file.
const PERSIST_TRIES: u64 = 8;

/// A Design's name: its file's, without the extension.
fn design_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `name` as a folder name every file system takes: characters Windows
/// refuses become `-`, a name Windows keeps for a device (`CON`, `COM1`)
/// gets a `-` after it, and it's cut to [`MAX_NAME_CHARS`].
fn folder_name(name: &str) -> Result<String, DesignError> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).trim().to_owned();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '-' || c == '.') {
        return Err(DesignError::Refused(format!(
            "\"{name}\" can't name a Design: use letters or digits"
        )));
    }
    let stem = cleaned.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    Ok(if device { format!("{cleaned}-") } else { cleaned })
}

fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    // Without Windows' `\?\` prefix, as the agent and the person write paths.
    #[cfg(windows)]
    let path = dunce::simplified(&path).to_path_buf();
    Ok(path)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
