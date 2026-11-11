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
//! `.zdesign` file. Assets (icons, fonts) are fetched as a call places them
//! and cached in `design-cache/` beside it ([`zeron_design::assets::Cache`]);
//! a Design's tabs hear the files of those it uses.
//!
//! Several chats may change one Design: they share its one live copy, and
//! a call is told when someone else changed it since that chat's last. The
//! engine also keeps which Designs each chat's Design tabs show
//! ([`Designs::set_tabs`]), and the Designs touched most recently on this
//! computer ([`Designs::recent`]), in `recent-designs.json`. Design tabs
//! follow a Design ([`Designs::watch`]): it whole, then again after each
//! change.
//!
//! Saving follows pen.dev: a Design in the managed folder saves itself a
//! few seconds after a change ([`AUTOSAVE_AFTER`]). One opened from
//! anywhere else changes on disk only when it's saved ([`Designs::save`]);
//! until then each change goes to a backup in `design-backups/` in the
//! engine's data dir, which is the Design again, marked recovered, when
//! it's next opened after Zeron stopped without saving it (and stale when
//! its file changed on disk since). Save as
//! ([`Designs::save_as`]) writes the Design anywhere with the images and
//! fonts beside it, and the Design is that file from then on.
//!
//! Text is set in Google Fonts, fetched on first use as assets, or in font
//! files beside its Design ([`zeron_design::fonts`]). An image fill's file
//! is in the Design's folder, or downloaded ([`images`]) as an asset.

pub mod guide;
mod iconify;
mod export;
mod images;
mod retry;

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::Engine as _;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeron_design::Design;
use zeron_design::assets::{Cache, Resolved};
use zeron_design::edit::{Draft, Edited, Warning};
use zeron_design::fonts::{Fonts, GoogleFonts};
use zeron_design::history::History;
use sha2::Digest as _;
use zeron_design::icons::Icons;
use zeron_design::model::{ImageFill, Kind};
use zeron_design::snippet::{self, Globals, Limits, SnippetError};
use zeron_rpc::methods::GUIDE_TEXTS;

pub use retry::{Edit, Failure};
use retry::{Failed, Kept, Retarget};

use self::iconify::Iconify;
use self::images::Download;

/// A `.zdesign` file's extension.
pub const EXTENSION: &str = "zdesign";

/// What New design names a Design it isn't given a name for.
const UNTITLED: &str = "Untitled design";

/// Longest name a managed Design's folder takes from its Design's name.
const MAX_NAME_CHARS: usize = 64;

/// How many recent Designs are kept.
const MAX_RECENT: usize = 50;

/// How long after a change a managed Design saves itself; the changes made
/// meanwhile save with it.
pub const AUTOSAVE_AFTER: Duration = Duration::from_secs(3);

/// What an agent's call on a Design that isn't managed is told.
const UNSAVED_NOTICE: &str = "Kept in a backup: the file changes when the person saves the Design.";

/// The undo step of discarding a Design's unsaved changes.
const DISCARD_DESCRIPTION: &str = "Discard unsaved changes";

/// The undo step of Save as renaming images it copied.
const SAVE_AS_DESCRIPTION: &str = "Save as, renaming images";

/// What `design_state` reminds an agent of.
pub const GUIDE_REMINDER: &str = "Design with the design guide loaded: the core guide came with your first design call in this chat. If it's no longer in your context (after compaction, say), call design_guide for it again, and design_guide with a topic for components, design systems, landing pages, mobile apps, web apps, slides, tables, code or Tailwind.";

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

/// `SaveDesign`'s, `DiscardDesign`'s and `DesignSaving`'s params.
#[derive(Debug, Deserialize)]
pub struct PathParams {
    /// The `.zdesign` file.
    pub path: PathBuf,
}

/// `NewDesign`'s params.
#[derive(Debug, Deserialize)]
pub struct NewParams {
    #[serde(default)]
    pub name: Option<String>,
}

/// `SaveDesignAs`'s params.
#[derive(Debug, Deserialize)]
pub struct SaveAsParams {
    /// The `.zdesign` file the Design is now.
    pub path: PathBuf,
    /// The `.zdesign` file to write it to (its folder is made if need be).
    pub to: PathBuf,
}

/// `DesignGuide`'s params, as Zeron's `design_guide` tool sends them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideParams {
    /// The calling chat (the agent's, set by the engine for an agent).
    #[serde(default)]
    pub chat_id: Option<String>,
    /// A topic guide's name; none for the core guide.
    #[serde(default)]
    pub topic: Option<String>,
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

/// The stack a Design call runs on ([`on_design_thread`]): resolving,
/// laying out and drawing recurse once a nesting level (48 at most,
/// `edit::MAX_DEPTH`), which a debug build's frames take about 1 MiB for;
/// this leaves room many times over in any build.
pub const DESIGN_STACK: usize = 8 << 20;

/// Runs `work` (blocking: a Design call) on a thread of its own with
/// [`DESIGN_STACK`], not on the runtime's, whose stack is the platform's
/// default; its answer once done.
pub async fn on_design_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, DesignError> {
    let (tell, answer) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("zeron-design".into())
        .stack_size(DESIGN_STACK)
        .spawn(move || {
            let _ = tell.send(work());
        })
        .map_err(|e| DesignError::Io(format!("can't start a Design call: {e}")))?;
    answer.await.map_err(|_| DesignError::Io("a Design call stopped".into()))
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
    /// The chats given the core guide (with their first design call), and
    /// "" for calls from no chat.
    guided: Mutex<HashSet<String>>,
    /// The Designs each chat's Design tabs show, front first.
    tabs: Mutex<HashMap<String, Vec<PathBuf>>>,
    /// The Designs touched most recently, newest first, as kept in
    /// `recent_file`.
    recent: Mutex<Vec<Recent>>,
    recent_file: PathBuf,
    /// Where Designs that aren't managed keep their unsaved changes.
    backups: PathBuf,
    /// How long after a change managed Designs save themselves.
    autosave: Mutex<Duration>,
}

/// One Design as the engine holds it.
struct Live {
    path: PathBuf,
    design: Design,
    /// What its text is set in, as its last call found them.
    fonts: Fonts,
    history: History,
    /// Counts the changes kept, whoever made them.
    version: u64,
    /// The version each chat's last successful call left.
    seen: HashMap<String, u64>,
    /// What the Design tabs following it hear: it after each change.
    followers: tokio::sync::watch::Sender<Told>,
    /// Where its tabs are told the files of its assets.
    assets: Arc<Cache>,
    /// How its changes reach its file.
    saving: Saving,
    /// It has changes its file doesn't: a managed Design's until it saves
    /// itself, another's until it's saved (kept meanwhile in its backup).
    unsaved: bool,
    /// Its unsaved changes came from its backup when it was opened, as
    /// Zeron stopped without saving them; until it's saved or they're
    /// discarded.
    recovered: bool,
    /// Its file changed on disk since the recovered changes were made to
    /// it (or that can't be told); as long as it's recovered.
    stale: bool,
    /// The digest of its file as its changes began from: as it was read or
    /// last saved (none for a recovered Design whose backup didn't say).
    base: Option<String>,
    /// Itself, for its autosave to come back to.
    me: Weak<Mutex<Live>>,
}

/// How a Design's changes reach its file.
#[derive(Clone)]
enum Saving {
    /// In the managed folder: it saves itself `after` a change (at once
    /// when that's zero), `pending` while it's waiting to.
    Autosave { after: Duration, pending: bool },
    /// Opened from elsewhere: changes go to this backup until it's saved.
    Backup(PathBuf),
}

/// A Design as the tabs following it hear it: its version, the changes
/// that made the latest versions (a tab that falls behind hears each
/// change it missed, up to [`MAX_TOLD_CHANGES`]), and the files of the
/// assets it uses, its icons and fonts (none yet for a Design no change
/// was told of).
#[derive(Clone)]
struct Told {
    version: u64,
    design: Arc<Design>,
    changes: Vec<Change>,
    assets: Option<Vec<Resolved>>,
    /// Its file, unsaved, recovered and stale: as [`Designs::saving`]
    /// says.
    path: PathBuf,
    unsaved: bool,
    recovered: bool,
    stale: bool,
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
    /// Its file: unsaved changes discarded for what the file holds, or
    /// Save as renaming images it copied.
    File,
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
                assets: {
                    let assets = Cache::new(&data_dir.join("design-cache"), Arc::new(Iconify));
                    assets.set_font_source(Box::new(GoogleFonts::default()));
                    assets.set_image_source(Arc::new(Download));
                    Arc::new(assets)
                },
                live: Mutex::new(HashMap::new()),
                globals: Mutex::new(HashMap::new()),
                failed: Mutex::new(Failed::default()),
                guided: Mutex::new(HashSet::new()),
                tabs: Mutex::new(HashMap::new()),
                recent: Mutex::new(recent),
                recent_file,
                backups: data_dir.join("design-backups"),
                autosave: Mutex::new(AUTOSAVE_AFTER),
            }),
        }
    }

    /// The managed folder new Designs are made in.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Managed Designs opened from now on save themselves `after` a change
    /// in place of [`AUTOSAVE_AFTER`]; with zero, at once (for tests, which
    /// read the file each call left).
    pub fn autosave_after(&self, after: Duration) {
        *lock(&self.inner.autosave) = after;
    }

    /// Whether the Design at `path` is managed, has unsaved changes, and
    /// recovered them (`DesignSaving`): `{path, managed, unsaved,
    /// recovered, stale}`, `stale` when its file changed on disk since the
    /// recovered changes were made to it. Blocking, like every call here
    /// that may read a file.
    pub fn saving(&self, path: &Path) -> Result<Value, DesignError> {
        Ok(lock(&*self.open(path)?).saving())
    }

    /// Saves the Design at `path` to its file now (`SaveDesign`), its
    /// backup gone; says as [`Designs::saving`] does.
    pub fn save(&self, path: &Path) -> Result<Value, DesignError> {
        let live = self.open(path)?;
        let mut live = lock(&live);
        live.save_file()?;
        Ok(live.saving())
    }

    /// A new, empty managed Design named `name` ("Untitled design" without
    /// one), on disk at once and the most recent (`NewDesign`, the Design
    /// tab's New design): `{name, path}`.
    pub fn new_design(&self, name: Option<&str>) -> Result<Value, DesignError> {
        let name = name.map(str::trim).filter(|name| !name.is_empty()).unwrap_or(UNTITLED);
        let live = self.create(name)?;
        let path = {
            let mut held = lock(&live);
            let made = held.save_file();
            if let Err(e) = made {
                // Nothing was made: the name is free again (its folder, new
                // with it, gone too).
                if let Some(folder) = held.path.parent() {
                    let _ = std::fs::remove_dir_all(folder);
                }
                drop(held);
                lock(&self.inner.live).retain(|_, other| !Arc::ptr_eq(other, &live));
                return Err(e);
            }
            held.path.clone()
        };
        self.touch(&path);
        Ok(json!({ "name": design_name(&path), "path": path }))
    }

    /// Puts the Design at `path` back as its file holds it, its unsaved
    /// changes and backup gone (`DiscardDesign`), as one undo step; says as
    /// [`Designs::saving`] does. A managed Design has none to discard.
    pub fn discard(&self, path: &Path) -> Result<Value, DesignError> {
        let live = self.open(path)?;
        let mut live = lock(&live);
        live.discard()?;
        Ok(live.saving())
    }

    /// Writes the Design at `path` to `to` (`SaveDesignAs`), with the
    /// image files it refers to and the font files beside it copied along
    /// at the same paths relative to it (an image under a new name where
    /// another file has its name, and the Design's references changed to
    /// match). The Design is the file at `to` from then on: its tabs and
    /// the recent list follow it, and the file it was is left as it was
    /// last saved. Says as [`Designs::saving`] does, for `to`.
    pub fn save_as(&self, path: &Path, to: &Path) -> Result<Value, DesignError> {
        if to.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
            return Err(DesignError::Refused(format!("{} is not a .{EXTENSION} file", to.display())));
        }
        let (Some(folder), Some(file)) = (to.parent().filter(|f| !f.as_os_str().is_empty()), to.file_name()) else {
            return Err(DesignError::Refused(format!("{} names no folder to save in", to.display())));
        };
        let io = |e: std::io::Error| DesignError::Io(format!("can't save to {}: {e}", to.display()));
        std::fs::create_dir_all(folder).map_err(io)?;
        // As the file system spells it (its case, its long names), so a
        // Design held by that file is found.
        let to = canonical(to)
            .or_else(|_| canonical(folder).map(|folder| folder.join(file)))
            .map_err(io)?;
        let folder = to.parent().unwrap_or(Path::new(".")).to_owned();
        let held = self.open(path)?;
        // The files copied without holding the Design up; again if it
        // changed meanwhile.
        let ((design, renamed), mut live) = loop {
            let (version, design, from) = {
                let live = lock(&held);
                if live.path == to {
                    drop(live);
                    return self.save(&to);
                }
                let from = live.path.parent().unwrap_or(Path::new(".")).to_owned();
                (live.version, live.design.clone(), from)
            };
            let copied = copy_beside(&design, &from, &folder)?;
            let live = lock(&held);
            if live.version == version {
                break (copied, live);
            }
        };
        // The file is this Design's from now on: no other may open it.
        {
            let mut by_path = lock(&self.inner.live);
            if by_path.get(&to).is_some_and(|other| !Arc::ptr_eq(other, &held)) {
                return Err(DesignError::Refused(format!(
                    "{} is open as another Design: save this one somewhere else",
                    to.display()
                )));
            }
            by_path.insert(to.clone(), held.clone());
        }
        let json = design.to_json();
        if let Err(e) = write_whole(&to, json.as_bytes()) {
            lock(&self.inner.live).remove(&to);
            return Err(io(e));
        }
        lock(&self.inner.live).remove(&live.path);
        if let Saving::Backup(backup) = &live.saving {
            remove_backup(backup);
        }
        let old = std::mem::replace(&mut live.path, to.clone());
        live.saving = self.saving_for(&to);
        // Nor is what was unsaved in a file it replaced recovered later.
        if let Saving::Backup(backup) = &live.saving {
            remove_backup(backup);
        }
        live.fonts = self.fonts_for(&to);
        live.on_file(digest(json.as_bytes()));
        if renamed {
            // One undo step: undone, it draws its images by their old names.
            let before = std::mem::replace(&mut live.design, design);
            let Live { history, design, .. } = &mut *live;
            history.record(SAVE_AS_DESCRIPTION, &before, design);
        }
        // Its tabs draw its images and fonts from where it is now.
        live.version += 1;
        live.tell(Vec::new(), Changer::File);
        live.tell_saving();
        let saving = live.saving();
        drop(live);
        for tabs in lock(&self.inner.tabs).values_mut() {
            for tab in tabs.iter_mut().filter(|tab| **tab == old) {
                tab.clone_from(&to);
            }
        }
        lock(&self.inner.recent).retain(|recent| recent.path != old);
        self.touch(&to);
        Ok(saving)
    }

    /// Saves every managed Design's changes not saved yet (as the engine
    /// stops). Others' are in their backups.
    pub fn flush(&self) {
        let held: Vec<_> = lock(&self.inner.live).values().cloned().collect();
        for live in held {
            lock(&live).autosave();
        }
    }

    /// How changes to the Design at `path` reach its file.
    fn saving_for(&self, path: &Path) -> Saving {
        if path.starts_with(&self.inner.root) {
            Saving::Autosave {
                after: *lock(&self.inner.autosave),
                pending: false,
            }
        } else {
            let key = digest(path.to_string_lossy().as_bytes());
            Saving::Backup(self.inner.backups.join(format!("{}.{EXTENSION}", &key[..32])))
        }
    }

    /// The Design at `path` held live, as `design`, its file as [`Opened`]
    /// says.
    fn hold(&self, path: PathBuf, design: Design, opened: Opened) -> Arc<Mutex<Live>> {
        let fonts = self.fonts_for(&path);
        let saving = self.saving_for(&path);
        Arc::new_cyclic(|me| {
            let assets = self.inner.assets.clone();
            let mut live = Live::new(path, design, fonts, assets, saving, me.clone());
            live.unsaved = opened.recovered;
            live.recovered = opened.recovered;
            live.stale = opened.stale;
            live.base = opened.base;
            Mutex::new(live)
        })
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
            Ok(mut result) => {
                if let Some(edit_id) = &edit_id {
                    lock(&self.inner.failed).remove(edit_id);
                }
                self.guide_once(params.chat_id.as_deref(), &mut result, Vec::new());
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
    /// design, changes: [{version, changed, by}]}`: the changes since the
    /// item before (none in the first), `by` "agent", "undo" or "file"
    /// (unsaved changes discarded, or Save as renaming images); and `path`,
    /// `unsaved`, `recovered` and `stale`, as [`Designs::saving`] says (heard again,
    /// with no changes, when only they change). A tab that falls behind
    /// hears the latest only. Blocking (it may read the file): run it off
    /// the async runtime.
    pub fn watch(&self, path: &str) -> Result<impl Stream<Item = Value> + Send + use<>, DesignError> {
        let live = self.open(Path::new(path))?;
        let told = loop {
            let (version, design) = {
                let live = lock(&live);
                let told = live.followers.borrow();
                if told.version == live.version && told.assets.is_some() {
                    drop(told);
                    break live.followers.subscribe();
                }
                (live.version, live.design.clone())
            };
            let live = lock(&live);
            // (Changes with nobody following weren't copied for them; one
            // made meanwhile is looked at again.)
            if live.version == version {
                // (Only what's cached: no fetch under its lock.)
                let assets = live.assets.resolved(&design, live.path.parent());
                live.followers.send_replace(Told {
                    version,
                    design: Arc::new(design),
                    changes: Vec::new(),
                    assets: Some(assets),
                    path: live.path.clone(),
                    unsaved: live.unsaved,
                    recovered: live.recovered,
                    stale: live.stale,
                });
                break live.followers.subscribe();
            }
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
                "path": now.path,
                "unsaved": now.unsaved,
                "recovered": now.recovered,
                "stale": now.stale,
            });
            Some((item, (told, Some(now.version))))
        }))
    }

    /// The fonts for the Design at `path`: this computer's, and those
    /// beside it.
    fn fonts_for(&self, path: &Path) -> Fonts {
        let fonts = self.inner.assets.fonts();
        match path.parent() {
            Some(folder) => fonts.beside(folder),
            None => fonts,
        }
    }

    /// Sets text in `fonts` from now on, in place of the font cache and
    /// Google Fonts (for tests, which run offline).
    pub fn use_fonts(&self, fonts: Fonts) {
        self.inner.assets.set_fonts(fonts);
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
        live.fonts = self.fonts_for(&live.path);
        let mut draft = Draft::new(live.design.clone())
            .with_fonts(live.fonts.clone())
            .with_assets(self.inner.assets.clone());
        if let Some(folder) = live.path.parent() {
            draft = draft.with_folder(folder.to_owned());
        }
        let (draft, globals) = snippet::run(draft, globals, code, self.inner.limits).map_err(Ran::Snippet)?;
        let mut edited = draft.finish();
        // What it draws with that no edit's check fetched: icons and fonts
        // inside Instances, or named by Tokens (`assets::used` resolves).
        self.inner.assets.fetch(&edited.design, live.path.parent());
        // Fonts the call's text couldn't get, and images it can't have, as
        // drawn (inside Instances, named by Tokens).
        let drawn = zeron_design::resolve::drawn(&edited.design);
        let mut texts = Vec::new();
        drawn.walk(&mut |node| {
            let top = node.id.split('/').next().unwrap_or_default();
            if let Kind::Text(text) = &node.kind
                && (edited.changed.contains(&node.id) || edited.changed.contains(top))
            {
                texts.push(text.clone());
            }
        });
        edited.warnings.extend(live.fonts.warnings(&texts).into_iter().map(Warning::fonts));
        // Icons the call's nodes draw (named by a Token, say) that couldn't
        // be fetched, which draw nothing: every icon's, when it changed the
        // Tokens or a Component.
        let icons = self.inner.assets.icons();
        let everywhere = live.design.tokens != edited.design.tokens
            || edited.changed.iter().any(|id| edited.design.find(id).is_some_and(|node| node.is_component()));
        drawn.walk(&mut |node| {
            let top = node.id.split('/').next().unwrap_or_default();
            if let Kind::Icon(icon) = &node.kind
                && (everywhere || edited.changed.contains(&node.id) || edited.changed.contains(top))
                && let Ok(name) = zeron_design::icons::name(&icon.icon, icon.weight)
                && icons.cached(&name).is_none()
            {
                let message = format!("\"{}\" draws the icon \"{}\", which can't be had; it draws nothing", node.name, icon.icon);
                edited.warnings.push(Warning { node: Some(node.id.clone()), property: Some("icon".into()), message });
            }
        });
        // Images the call placed or looked at that can't be had (those it
        // can, downloaded now, for its screenshots and the tabs).
        let looked_at = edited.changed.iter().chain(&edited.screenshots);
        let assets = live.assets.beside(&live.folder());
        let missing = images::warnings(looked_at.filter_map(|id| drawn.find(id)), &assets);
        edited.warnings.extend(missing);
        let changed = chat
            .and_then(|chat| live.seen.get(chat))
            .is_some_and(|&seen| seen != live.version);
        let description = params.description.as_deref().unwrap_or(DEFAULT_DESCRIPTION);
        let screenshots = live.commit(&edited, description, Changer::Agent).map_err(Ran::Design)?;
        let (exported, unwritten) = live.export(&edited.exports);
        edited.warnings.extend(unwritten);
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
        if !exported.is_empty() {
            result["exported"] = json!(exported);
        }
        if !edited.warnings.is_empty() {
            result["warnings"] = json!(edited.warnings);
        }
        if changed {
            result["notice"] = json!(CHANGED_NOTICE);
        }
        if matches!(live.saving, Saving::Backup(_)) {
            result["unsaved"] = json!(UNSAVED_NOTICE);
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
        let mut state = json!({ "designs": designs, "recent": self.recent(), "guide": GUIDE_REMINDER });
        self.guide_once(chat_id, &mut state, Vec::new());
        state
    }

    /// `design_guide` for `chat_id`: the topic guide `topic`, or the core
    /// guide without one, as [`GUIDE_TEXTS`], with the topics there are.
    /// The core guide comes after a topic when the chat hasn't had it yet.
    pub fn guide(&self, chat_id: Option<&str>, topic: Option<&str>) -> Result<Value, DesignError> {
        let topics = guide::topics();
        let text = match topic {
            None => guide::CORE,
            Some(name) => guide::topic(name).ok_or_else(|| {
                DesignError::Refused(format!(
                    "there's no design guide on \"{name}\": the topics are {}, or none for the core guide",
                    topics.join(", ")
                ))
            })?,
        };
        let mut result = json!({ "topic": topic.unwrap_or("core"), "topics": topics });
        self.guide_once(chat_id, &mut result, vec![text]);
        Ok(result)
    }

    /// Puts `texts` in `result` as [`GUIDE_TEXTS`], and the core guide
    /// after them when `chat_id` hasn't had it (unless they hold it), which
    /// it now has.
    fn guide_once(&self, chat_id: Option<&str>, result: &mut Value, mut texts: Vec<&'static str>) {
        if lock(&self.inner.guided).insert(chat_id.unwrap_or_default().to_owned()) && !texts.contains(&guide::CORE) {
            texts.push(guide::CORE);
        }
        if !texts.is_empty() {
            result[GUIDE_TEXTS] = json!(texts);
        }
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
        let file = digest(text.as_bytes());
        // Changes Zeron stopped without saving, from its backup: stale when
        // the file isn't as they began from (or that can't be told).
        let backup = match self.saving_for(&path) {
            Saving::Backup(backup) => std::fs::read_to_string(&backup).ok().and_then(|text| {
                let design = Design::from_json(&text)
                    .inspect_err(|e| tracing::warn!("can't recover {} from {}: {e}", path.display(), backup.display()))
                    .ok()?;
                let base = std::fs::read_to_string(base_of(&backup)).ok().map(|base| base.trim().to_owned());
                Some((design, base))
            }),
            Saving::Autosave { .. } => None,
        };
        let (design, opened) = match backup {
            Some((recovered, base)) => (
                recovered,
                Opened {
                    recovered: true,
                    stale: base.as_ref() != Some(&file),
                    base,
                },
            ),
            None => (
                design,
                Opened {
                    base: Some(file),
                    ..Opened::default()
                },
            ),
        };
        // Fetched now, outside its lock: telling its tabs and drawing it
        // only read what's cached.
        self.inner.assets.fetch(&design, path.parent());
        let design = self.hold(path.clone(), design, opened);
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
        let design = self.hold(path.clone(), Design::default(), Opened::default());
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

/// What a Design held from its file starts with: as [`Live`]'s `base`,
/// `recovered` and `stale` say.
#[derive(Default)]
struct Opened {
    base: Option<String>,
    recovered: bool,
    stale: bool,
}

impl Live {
    fn new(
        path: PathBuf,
        design: Design,
        fonts: Fonts,
        assets: Arc<Cache>,
        saving: Saving,
        me: Weak<Mutex<Live>>,
    ) -> Self {
        let told = Told {
            version: 0,
            design: Arc::new(design.clone()),
            changes: Vec::new(),
            assets: None,
            path: path.clone(),
            unsaved: false,
            recovered: false,
            stale: false,
        };
        Self {
            path,
            design,
            fonts,
            history: History::default(),
            version: 0,
            seen: HashMap::new(),
            followers: tokio::sync::watch::channel(told).0,
            assets,
            saving,
            unsaved: false,
            recovered: false,
            stale: false,
            base: None,
            me,
        }
    }

    /// The folder its file is in.
    fn folder(&self) -> PathBuf {
        self.path.parent().unwrap_or(Path::new(".")).to_owned()
    }

    /// Makes `design` the Design, as every change does: puts it on disk as
    /// the Design saves, then tells other chats' next calls and the
    /// Design's tabs, with the nodes `changed`. Returns the Design as it
    /// was.
    fn keep(&mut self, design: Design, changed: Vec<String>, by: Changer) -> Result<Design, DesignError> {
        self.write(&design)?;
        Ok(self.put(design, changed, by))
    }

    /// Makes `design` the Design, already on disk as it should be, and
    /// tells of it as [`Live::keep`] does.
    fn put(&mut self, design: Design, changed: Vec<String>, by: Changer) -> Design {
        let before = std::mem::replace(&mut self.design, design);
        self.version += 1;
        self.tell(changed, by);
        self.tell_saving();
        before
    }

    /// Puts a change on disk as the Design saves: in its backup, or in its
    /// file at once (a managed Design's first save, or with no autosave
    /// delay), or there once its autosave delay is up.
    fn write(&mut self, design: &Design) -> Result<(), DesignError> {
        match &mut self.saving {
            Saving::Backup(backup) => {
                let io = |e: std::io::Error| DesignError::Io(format!("can't back up {}: {e}", backup.display()));
                std::fs::create_dir_all(backup.parent().unwrap_or(Path::new("."))).map_err(io)?;
                // As a backup begins, the file its changes begin from, to
                // tell later whether it changed on disk since.
                if !backup.exists() {
                    let base = base_of(backup);
                    match &self.base {
                        Some(digest) => write_whole(&base, digest.as_bytes()).map_err(io)?,
                        // (Can't be told: so it's stale if recovered.)
                        None => _ = std::fs::remove_file(&base),
                    }
                }
                write_whole(backup, design.to_json().as_bytes()).map_err(io)?;
                self.unsaved = true;
            }
            Saving::Autosave { after, .. } if after.is_zero() || !self.path.exists() => {
                save(&self.path, design)?;
                self.unsaved = false;
            }
            Saving::Autosave { after, pending } => {
                self.unsaved = true;
                if !*pending {
                    *pending = true;
                    let (me, after) = (self.me.clone(), *after);
                    // (Saving recurses through the Design: a Design call's stack.)
                    let _ = std::thread::Builder::new().stack_size(DESIGN_STACK).spawn(move || {
                        std::thread::sleep(after);
                        if let Some(live) = me.upgrade() {
                            lock(&live).autosave();
                        }
                    });
                }
            }
        }
        Ok(())
    }

    /// Saves a managed Design's changes not saved yet. One that can't be
    /// saved keeps them, for its next change's autosave.
    fn autosave(&mut self) {
        let Saving::Autosave { pending, .. } = &mut self.saving else {
            return;
        };
        *pending = false;
        if self.unsaved
            && let Err(e) = self.save_file()
        {
            tracing::warn!("{e}");
        }
    }

    /// Saves the Design to its file, its backup gone.
    fn save_file(&mut self) -> Result<(), DesignError> {
        let json = self.design.to_json();
        save_json(&self.path, &json)?;
        if let Saving::Backup(backup) = &self.saving {
            remove_backup(backup);
        }
        self.on_file(digest(json.as_bytes()));
        self.tell_saving();
        Ok(())
    }

    /// Its changes are all in its file, whose digest is `base`: it's no
    /// longer unsaved, recovered or stale.
    fn on_file(&mut self, base: String) {
        self.base = Some(base);
        self.unsaved = false;
        self.recovered = false;
        self.stale = false;
    }

    /// Puts the Design back as its file holds it, as one undo step, its
    /// backup gone.
    fn discard(&mut self) -> Result<(), DesignError> {
        let Saving::Backup(backup) = self.saving.clone() else {
            return Err(DesignError::Refused(
                "a Design in Zeron's designs folder saves itself: it has no unsaved changes to discard".into(),
            ));
        };
        if !self.unsaved {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| DesignError::Io(format!("can't read {}: {e}", self.path.display())))?;
        let design = Design::from_json(&text)
            .map_err(|e| DesignError::Refused(format!("{} is not a Design: {e}", self.path.display())))?;
        remove_backup(&backup);
        self.on_file(digest(text.as_bytes()));
        let before = self.put(design, Vec::new(), Changer::File);
        self.history.record(DISCARD_DESCRIPTION, &before, &self.design);
        Ok(())
    }

    /// Its saving state, as [`Designs::saving`] says it.
    fn saving(&self) -> Value {
        json!({
            "path": self.path,
            "managed": matches!(self.saving, Saving::Autosave { .. }),
            "unsaved": self.unsaved,
            "recovered": self.recovered,
            "stale": self.stale,
        })
    }

    /// Tells the Design's tabs its file, and whether it's unsaved,
    /// recovered or stale, when that changed.
    fn tell_saving(&self) {
        self.followers.send_if_modified(|told| self.saving_into(told));
    }

    /// Sets what `told` says of its file, unsaved, recovered and stale to
    /// how they are; true when that changed it.
    fn saving_into(&self, told: &mut Told) -> bool {
        let changed = told.path != self.path
            || told.unsaved != self.unsaved
            || told.recovered != self.recovered
            || told.stale != self.stale;
        told.path.clone_from(&self.path);
        told.unsaved = self.unsaved;
        told.recovered = self.recovered;
        told.stale = self.stale;
        changed
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

    /// Tells the Design's tabs what a change (`by` whom) made of it, the
    /// nodes it `changed` (copied for them only while some follow it), and
    /// its saving state as the change left it.
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
        // (Only what's cached: no fetch under its lock.)
        let assets = self.assets.resolved(&self.design, self.path.parent());
        self.followers.send_modify(|told| {
            told.version = change.version;
            told.design = design;
            told.assets = Some(assets);
            // With the change, so no tab hears it saved before it's told
            // otherwise.
            self.saving_into(told);
            told.changes.push(change);
            let over = told.changes.len().saturating_sub(MAX_TOLD_CHANGES);
            told.changes.drain(..over);
        });
    }

    /// Keeps a successful change's draft (`by` whom) as one undo step,
    /// described as `description`: saves the file, then draws the
    /// screenshots it asked for with its assets.
    fn commit(&mut self, edited: &Edited, description: &str, by: Changer) -> Result<Vec<Value>, DesignError> {
        let before = self.keep(edited.design.clone(), edited.changed.iter().cloned().collect(), by)?;
        self.history.record(description, &before, &self.design);
        let assets = self.assets.beside(&self.folder());
        Ok(edited
            .screenshots
            .iter()
            .map(|id| {
                let shot = zeron_design::render::screenshot(
                    &self.design,
                    id,
                    zeron_design::render::SCREENSHOT_MAX_SIDE,
                    &self.fonts,
                    &assets,
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
    save_json(path, &design.to_json())
}

/// Saves a Design's JSON as [`save`] does.
fn save_json(path: &Path, json: &str) -> Result<(), DesignError> {
    let io = |e: std::io::Error| DesignError::Io(format!("can't save {}: {e}", path.display()));
    let folder = path.parent().ok_or_else(|| DesignError::Io("a Design's path has no folder".into()))?;
    if !path.exists() {
        std::fs::create_dir_all(folder.join("images")).map_err(io)?;
    }
    write_whole(path, json.as_bytes()).map_err(io)
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

/// Removes a Design's backup (and the digest beside it), once its changes
/// are saved or discarded.
fn remove_backup(backup: &Path) {
    for file in [backup.to_owned(), base_of(backup)] {
        if let Err(e) = std::fs::remove_file(&file)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("can't remove the backup {}: {e}", file.display());
        }
    }
}

/// Beside a Design's backup: the digest of its file as its changes began
/// from.
fn base_of(backup: &Path) -> PathBuf {
    backup.with_extension("base")
}

/// A file's contents' SHA-256, in hex.
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// `design` as saved in the folder `to` from the folder `from`: the image
/// files it refers to and the font files beside it copied along at the
/// same relative paths. An image whose name another file has in `to` is
/// copied under a new one (`photo-2.png`), and the Design's references
/// changed to match: true when any were.
fn copy_beside(design: &Design, from: &Path, to: &Path) -> Result<(Design, bool), DesignError> {
    let io = |e: std::io::Error| DesignError::Io(format!("can't copy into {}: {e}", to.display()));
    if from == to {
        return Ok((design.clone(), false));
    }
    let mut json: Value = serde_json::from_str(&design.to_json()).map_err(|e| DesignError::Io(e.to_string()))?;
    let mut copied: HashMap<String, String> = HashMap::new();
    let mut failed = None;
    image_urls_mut(&mut json, &mut |url| {
        if ImageFill::is_remote(url) || failed.is_some() {
            return;
        }
        let new = match copied.get(url.as_str()) {
            Some(new) => new.clone(),
            None => match copy_image(url, from, to) {
                Ok(new) => {
                    copied.insert(url.clone(), new.clone());
                    new
                }
                Err(e) => {
                    failed = Some(e);
                    return;
                }
            },
        };
        *url = new;
    });
    if let Some(e) = failed {
        return Err(io(e));
    }
    for (folder, into) in [(from.to_owned(), to.to_owned()), (from.join("fonts"), to.join("fonts"))] {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path();
            let font = file
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["ttf", "otf", "ttc"].contains(&e.to_ascii_lowercase().as_str()));
            let dest = into.join(entry.file_name());
            if font && file.is_file() && !dest.exists() {
                std::fs::create_dir_all(&into).map_err(io)?;
                std::fs::copy(&file, &dest).map_err(io)?;
            }
        }
    }
    let renamed = copied.iter().any(|(url, new)| url != new);
    let design = Design::from_json(&json.to_string()).map_err(|e| DesignError::Io(e.to_string()))?;
    Ok((design, renamed))
}

/// Copies the image `url` refers to from the folder `from` to the same
/// path in `to`, or beside it under a new name where a different file has
/// that name. Returns how the Design refers to it in `to`: `url` itself
/// unless it was renamed. One that's not a file in `from` stays as it is.
fn copy_image(url: &str, from: &Path, to: &Path) -> std::io::Result<String> {
    let relative = PathBuf::from(url.replace('\\', "/"));
    let inside = relative.components().all(|c| matches!(c, Component::Normal(_)));
    let source = from.join(&relative);
    if !inside || !source.is_file() {
        return Ok(url.to_owned());
    }
    let stem = relative.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let extension = relative
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut named = relative.clone();
    for n in 2.. {
        let dest = to.join(&named);
        if !dest.exists() {
            std::fs::create_dir_all(dest.parent().unwrap_or(to))?;
            std::fs::copy(&source, &dest)?;
            break;
        }
        if std::fs::read(&dest)? == std::fs::read(&source)? {
            break;
        }
        named = relative.with_file_name(format!("{stem}-{n}{extension}"));
    }
    Ok(if named == relative {
        url.to_owned()
    } else {
        named.to_string_lossy().replace('\\', "/")
    })
}

/// Visits the url of every image paint in a Design's JSON, wherever it is
/// (fills, overrides, Tokens), to change it.
fn image_urls_mut(value: &mut Value, visit: &mut dyn FnMut(&mut String)) {
    match value {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("image")
                && let Some(Value::String(url)) = object.get_mut("url")
            {
                visit(url);
            }
            for value in object.values_mut() {
                image_urls_mut(value, visit);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|value| image_urls_mut(value, visit)),
        _ => {}
    }
}

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
