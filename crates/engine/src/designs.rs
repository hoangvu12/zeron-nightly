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
//! `.zdesign` file.

mod retry;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use zeron_design::Design;
use zeron_design::edit::{Draft, Edited};
use zeron_design::history::History;
use zeron_design::snippet::{self, Globals, Limits, SnippetError};

pub use retry::{Edit, Failure};
use retry::{Failed, Kept, Retarget};

/// A `.zdesign` file's extension.
pub const EXTENSION: &str = "zdesign";

/// Longest name a managed Design's folder takes from its Design's name.
const MAX_NAME_CHARS: usize = 64;

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
    /// Designs held live, by file: one owner each, so calls apply in order.
    live: Mutex<HashMap<PathBuf, Arc<Mutex<Live>>>>,
    /// Each chat's snippet globals, carried to its next call.
    globals: Mutex<HashMap<String, Globals>>,
    /// Failed calls, kept to retry.
    failed: Mutex<Failed>,
}

/// One Design as the engine holds it.
struct Live {
    path: PathBuf,
    design: Design,
    history: History,
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
        Self {
            inner: Arc::new(Inner {
                root,
                limits: Limits::default(),
                live: Mutex::new(HashMap::new()),
                globals: Mutex::new(HashMap::new()),
                failed: Mutex::new(Failed::default()),
            }),
        }
    }

    /// The managed folder new Designs are made in.
    pub fn root(&self) -> &Path {
        &self.inner.root
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
        self.step(path, History::undo, History::redo)
    }

    /// Does the latest undone change to the Design at `path` again.
    pub fn redo(&self, path: &Path) -> Result<bool, DesignError> {
        self.step(path, History::redo, History::undo)
    }

    /// Takes one step through the Design's history with `step`, or back
    /// with `back` when the Design can't be saved.
    fn step(
        &self,
        path: &Path,
        step: fn(&mut History, &Design) -> Option<(Design, String)>,
        back: fn(&mut History, &Design) -> Option<(Design, String)>,
    ) -> Result<bool, DesignError> {
        let live = self.open(path)?;
        let mut live = lock(&live);
        let live = &mut *live;
        let Some((design, _)) = step(&mut live.history, &live.design) else {
            return Ok(false);
        };
        if let Err(e) = save(&live.path, &design) {
            back(&mut live.history, &design);
            return Err(e);
        }
        live.design = design;
        Ok(true)
    }

    /// Runs `code` with the calling chat's globals, keeping the globals it
    /// leaves when it succeeds. Read once the Design is held, so calls
    /// queued on one Design see each other's; calls on two Designs at once
    /// each start from what was there, and the later to end wins.
    fn run(&self, live: &Mutex<Live>, code: &str, params: &ExecuteParams) -> Result<Value, Ran> {
        let chat = params.chat_id.as_deref();
        let mut live = lock(live);
        let globals = chat
            .and_then(|chat| lock(&self.inner.globals).get(chat).cloned())
            .unwrap_or_default();
        let (draft, globals) = snippet::run(Draft::new(live.design.clone()), globals, code, self.inner.limits)
            .map_err(Ran::Snippet)?;
        let edited = draft.finish();
        let description = params.description.as_deref().unwrap_or(DEFAULT_DESCRIPTION);
        let screenshots = live.commit(&edited, description).map_err(Ran::Design)?;
        if let Some(chat) = chat {
            lock(&self.inner.globals).insert(chat.to_owned(), globals);
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
        Ok(result)
    }

    /// The Design a call is for: the file it names, or a new managed
    /// Design by the name it gives (made on disk when the call succeeds).
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
            (None, None) => Err(DesignError::Refused(
                "name a Design: the path of a .zdesign file, or a name for a new one".into(),
            )),
        }
    }

    /// The live Design of the `.zdesign` file at `path`, read on first use.
    fn open(&self, path: &Path) -> Result<Arc<Mutex<Live>>, DesignError> {
        if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
            return Err(DesignError::Refused(format!(
                "{} is not a .{EXTENSION} file",
                path.display()
            )));
        }
        let path = canonical(path)
            .map_err(|e| DesignError::Refused(format!("can't open {}: {e}", path.display())))?;
        if let Some(design) = lock(&self.inner.live).get(&path) {
            return Ok(design.clone());
        }
        // Read without holding every Design up; the first read held wins.
        let text = std::fs::read_to_string(&path)
            .map_err(|e| DesignError::Refused(format!("can't read {}: {e}", path.display())))?;
        let design = Design::from_json(&text).map_err(|e| {
            DesignError::Refused(format!("{} is not a Design: {e}", path.display()))
        })?;
        let design = Arc::new(Mutex::new(Live {
            path: path.clone(),
            design,
            history: History::default(),
        }));
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
        let design = Arc::new(Mutex::new(Live {
            path: path.clone(),
            design: Design::default(),
            history: History::default(),
        }));
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
    /// Keeps a successful call's draft as one undo step, described as
    /// `description`: saves the file, then draws the screenshots the call
    /// asked for.
    fn commit(&mut self, edited: &Edited, description: &str) -> Result<Vec<Value>, DesignError> {
        save(&self.path, &edited.design)?;
        self.history.record(description, &self.design, &edited.design);
        self.design = edited.design.clone();
        Ok(edited
            .screenshots
            .iter()
            .map(|id| {
                let shot = zeron_design::render::screenshot(
                    &self.design,
                    id,
                    zeron_design::render::SCREENSHOT_MAX_SIDE,
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
    let mut file = tempfile::NamedTempFile::new_in(folder).map_err(io)?;
    std::io::Write::write_all(&mut file, design.to_json().as_bytes()).map_err(io)?;
    file.persist(path).map_err(|e| io(e.error))?;
    Ok(())
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
