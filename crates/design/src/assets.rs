//! Assets: what a Design draws with beyond its own file, as icons and
//! images are (and fonts will be). The engine resolves them, fetching, caching
//! and checking each ([`Cache`]); it tells the window which a Design uses
//! and the file holding each ([`Resolved`]), and the window reads only
//! those files, never the network ([`Files`]). A painter asks either for an
//! asset's bytes through [`Assets`], so a Design tab draws what an agent's
//! screenshot does. An image in the Design's folder is found through the
//! Design's own view of the cache ([`Beside`]).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::icons::{IconSource, Icons};
use crate::images::{self, ImageSource, Images};
use crate::model::{Design, ImageFill};

/// The kinds of asset; a kind is added with a variant here and its arms in
/// [`Cache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// An Iconify icon's SVG, named `set:name` (its weight's variant).
    Icon,
    /// An image fill's file, named by its url as written: a path relative
    /// to the Design's folder, or an http(s) URL.
    Image,
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

    pub fn image(url: impl Into<String>) -> Self {
        Self { kind: AssetKind::Image, name: url.into() }
    }

    /// An image in the Design's folder, which only [`Beside`] finds.
    fn is_beside(&self) -> bool {
        self.kind == AssetKind::Image && !ImageFill::is_remote(&self.name)
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
    images: Images,
}

impl Cache {
    /// Assets cached in `dir`, icons fetched from `icons`; images aren't
    /// downloaded until [`with_images`](Self::with_images).
    pub fn new(dir: &Path, icons: Arc<dyn IconSource>) -> Self {
        Self { icons: Icons::new(dir.join("icons"), icons), images: Images::new(dir.join("images")) }
    }

    /// Images downloaded from `images`.
    pub fn with_images(self, images: Arc<dyn ImageSource>) -> Self {
        self.images.set_source(images);
        self
    }

    pub fn icons(&self) -> &Icons {
        &self.icons
    }

    pub fn images(&self) -> &Images {
        &self.images
    }

    /// The assets of a Design in `folder`: this cache's, and the images
    /// beside it.
    pub fn beside(self: &Arc<Self>, folder: impl Into<PathBuf>) -> Beside {
        Beside { cache: self.clone(), folder: folder.into() }
    }

    /// Fetches what `design` uses that isn't cached (a Design read from a
    /// file made on another computer). One that can't be fetched draws
    /// nothing, and isn't asked for again for a while.
    pub fn fetch(&self, design: &Design) {
        for key in used(design) {
            match key.kind {
                AssetKind::Icon => {
                    let _ = self.icons.load(&key.name);
                }
                AssetKind::Image if !key.is_beside() => {
                    let _ = self.images.load(&key.name);
                }
                AssetKind::Image => {}
            }
        }
    }

    /// Each asset `design` uses that's cached, and its file; one that isn't
    /// is left out, and draws nothing. Never fetches.
    pub fn resolved(&self, design: &Design) -> Vec<Resolved> {
        used(design)
            .into_iter()
            .filter_map(|key| {
                let path = match key.kind {
                    AssetKind::Icon => self.icons.file_of(&key.name).ok()?,
                    AssetKind::Image if !key.is_beside() => self.images.file_of(&key.name),
                    AssetKind::Image => return None,
                };
                path.is_file().then_some(Resolved { key, path })
            })
            .collect()
    }
}

/// What's cached only: drawing never fetches.
impl Assets for Cache {
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>> {
        match key.kind {
            AssetKind::Icon => self.icons.cached(&key.name).map(Arc::from),
            AssetKind::Image if !key.is_beside() => std::fs::read(self.images.file_of(&key.name)).ok().map(Arc::from),
            AssetKind::Image => None,
        }
    }
}

/// One Design's assets: the [`Cache`]'s, and the images in its folder.
pub struct Beside {
    cache: Arc<Cache>,
    folder: PathBuf,
}

impl Beside {
    pub fn cache(&self) -> &Arc<Cache> {
        &self.cache
    }

    /// Fetches what `design` uses that isn't cached ([`Cache::fetch`]).
    pub fn fetch(&self, design: &Design) {
        self.cache.fetch(design);
    }

    /// Each asset `design` uses that's here, and its file
    /// ([`Cache::resolved`]), with the images in its folder. Never fetches.
    pub fn resolved(&self, design: &Design) -> Vec<Resolved> {
        let mut resolved = self.cache.resolved(design);
        for key in used(design).into_iter().filter(AssetKey::is_beside) {
            let path = self.folder.join(key.name.replace('\\', "/"));
            if path.is_file() {
                resolved.push(Resolved { key, path });
            }
        }
        resolved
    }

    /// The file of the image at `url`, downloaded now if it's a URL not
    /// cached yet, or why it can't be drawn.
    pub fn image(&self, url: &str) -> Result<PathBuf, String> {
        if ImageFill::is_remote(url) {
            self.cache.images.load(url)
        } else {
            images::beside(&self.folder, url)
        }
    }
}

/// What's here only: drawing never fetches.
impl Assets for Beside {
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>> {
        if !key.is_beside() {
            return self.cache.get(key);
        }
        std::fs::read(self.folder.join(key.name.replace('\\', "/"))).ok().map(Arc::from)
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

    /// Each asset's file.
    pub fn paths(&self) -> &HashMap<AssetKey, PathBuf> {
        &self.paths
    }
}

impl Assets for Files {
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>> {
        let read = || self.read.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(bytes) = read().get(key) {
            return bytes.clone();
        }
        // (Read without the lock, so tiles drawn at once don't wait on it.)
        let bytes: Option<Arc<[u8]>> = self.paths.get(key).and_then(|path| std::fs::read(path).ok()).map(Arc::from);
        read().insert(key.clone(), bytes.clone());
        bytes
    }
}
