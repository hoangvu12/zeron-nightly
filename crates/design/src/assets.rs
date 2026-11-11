//! Assets: what a Design draws with beyond its own file, as icons are (and
//! fonts and images will be). The engine resolves them, fetching, caching
//! and checking each ([`Cache`]); it tells the window which a Design uses
//! and the file holding each ([`Resolved`]), and the window reads only
//! those files, never the network ([`Files`]). A painter asks either for an
//! asset's bytes through [`Assets`], so a Design tab draws what an agent's
//! screenshot does.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::icons::{IconSource, Icons};
use crate::model::Design;

/// The kinds of asset; a kind is added with a variant here and its arms in
/// [`Cache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// An Iconify icon's SVG, named `set:name` (its weight's variant).
    Icon,
}

/// One asset, by kind and name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetKey {
    pub kind: AssetKind,
    pub name: String,
}

impl AssetKey {
    pub fn icon(name: impl Into<String>) -> Self {
        Self { kind: AssetKind::Icon, name: name.into() }
    }
}

/// An asset a Design uses and the file holding it, as the engine tells
/// the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolved {
    #[serde(flatten)]
    pub key: AssetKey,
    /// Absolute, in the engine's cache on this computer.
    pub path: PathBuf,
}

/// Where a painter gets assets' bytes.
pub trait Assets: Send + Sync {
    /// The asset's bytes, or `None` when it isn't available here (it then
    /// draws nothing).
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>>;
}

/// No assets: what draws with them draws nothing.
pub struct NoAssets;

impl Assets for NoAssets {
    fn get(&self, _: &AssetKey) -> Option<Arc<[u8]>> {
        None
    }
}

/// The assets `design` draws with, each once.
pub fn used(design: &Design) -> BTreeSet<AssetKey> {
    let mut used = BTreeSet::new();
    design.walk(&mut |node| used.extend(node.kind.get().assets()));
    used
}

/// The engine's assets: fetched on first use and cached on this computer,
/// in a folder per kind.
pub struct Cache {
    icons: Icons,
}

impl Cache {
    /// Assets cached in `dir`, icons fetched from `icons`.
    pub fn new(dir: &Path, icons: Arc<dyn IconSource>) -> Self {
        Self { icons: Icons::new(dir.join("icons"), icons) }
    }

    pub fn icons(&self) -> &Icons {
        &self.icons
    }

    /// The file holding `key`, fetched first if it isn't cached.
    pub fn resolve(&self, key: &AssetKey) -> Result<PathBuf, String> {
        match key.kind {
            AssetKind::Icon => self.icons.file(&key.name).map_err(|e| e.to_string()),
        }
    }

    /// Each asset `design` uses and its file; one that can't be resolved
    /// is left out, and draws nothing.
    pub fn resolved(&self, design: &Design) -> Vec<Resolved> {
        used(design)
            .into_iter()
            .filter_map(|key| Some(Resolved { path: self.resolve(&key).ok()?, key }))
            .collect()
    }
}

impl Assets for Cache {
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>> {
        match key.kind {
            AssetKind::Icon => self.icons.load(&key.name).ok().map(Arc::from),
        }
    }
}

/// The window's assets: the files the engine named, each read once, when
/// first drawn.
pub struct Files {
    paths: HashMap<AssetKey, PathBuf>,
    read: Mutex<HashMap<AssetKey, Option<Arc<[u8]>>>>,
}

impl Files {
    pub fn new(resolved: impl IntoIterator<Item = Resolved>) -> Self {
        Self {
            paths: resolved.into_iter().map(|resolved| (resolved.key, resolved.path)).collect(),
            read: Mutex::new(HashMap::new()),
        }
    }
}

impl Assets for Files {
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>> {
        let mut read = self.read.lock().unwrap_or_else(|e| e.into_inner());
        read.entry(key.clone())
            .or_insert_with(|| std::fs::read(self.paths.get(key)?).ok().map(Arc::from))
            .clone()
    }
}
