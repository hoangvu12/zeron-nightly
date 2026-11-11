//! Assets: what a Design draws with beyond its own file, as icons and
//! fonts are (and images will be). The engine resolves them, fetching, caching
//! and checking each ([`Cache`]); it tells the window which a Design uses
//! and the file holding each ([`Resolved`]), and the window reads only
//! those files, never the network ([`Files`]). A painter asks either for an
//! asset's bytes through [`Assets`], so a Design tab draws what an agent's
//! screenshot does.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::fonts::{FontFile, FontSource, Fonts};
use crate::icons::{IconSource, Icons};
use crate::model::Design;

/// The kinds of asset; a kind is added with a variant here and its arms in
/// [`Cache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// An Iconify icon's SVG, named `set:name` (its weight's variant).
    Icon,
    /// A font file, named `family@weight` as text asks for it, or
    /// `family@file` for one of a family beside the Design.
    Font,
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

    /// `family` at `weight` (100 to 900).
    pub fn font(family: &str, weight: u16) -> Self {
        Self { kind: AssetKind::Font, name: format!("{family}@{weight}") }
    }

    /// A font's family and what follows it (its weight, or its file):
    /// split at the first `@`, which no family name holds.
    fn font_parts(&self) -> Option<(&str, &str)> {
        (self.kind == AssetKind::Font).then(|| self.name.split_once('@')).flatten()
    }

    /// A font's family and weight, as text asks for it.
    fn font_weight(&self) -> Option<(&str, u16)> {
        let (family, weight) = self.font_parts()?;
        Some((family, weight.parse().ok()?))
    }
}

impl Resolved {
    /// The font file this is, for [`Fonts::of_files`].
    pub fn font_file(&self) -> Option<FontFile> {
        let (family, _) = self.key.font_parts()?;
        Some(FontFile { family: family.to_owned(), path: self.path.clone() })
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

/// The assets `design` draws with, each once: as drawn, so icons inside
/// Instances, and those Tokens name, count.
pub fn used(design: &Design) -> BTreeSet<AssetKey> {
    let design = &crate::resolve::drawn(design);
    let mut used = BTreeSet::new();
    design.walk(&mut |node| used.extend(node.kind.get().assets()));
    used
}

/// The engine's assets: fetched on first use and cached on this computer,
/// in a folder per kind.
pub struct Cache {
    dir: PathBuf,
    icons: Icons,
    fonts: RwLock<Fonts>,
}

impl Cache {
    /// Assets cached in `dir`, icons fetched from `icons`; fonts only from
    /// the cache until given a source ([`Cache::set_font_source`]).
    pub fn new(dir: &Path, icons: Arc<dyn IconSource>) -> Self {
        Self {
            dir: dir.to_owned(),
            icons: Icons::new(dir.join("icons"), icons),
            fonts: RwLock::new(Fonts::new(Some(dir.join("fonts")), None)),
        }
    }

    pub fn icons(&self) -> &Icons {
        &self.icons
    }

    /// This computer's fonts.
    pub fn fonts(&self) -> Fonts {
        self.fonts.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Fetches fonts the cache hasn't got from `source` from now on.
    pub fn set_font_source(&self, source: Box<dyn FontSource>) {
        self.set_fonts(Fonts::new(Some(self.dir.join("fonts")), Some(source)));
    }

    /// Sets text in `fonts` from now on, cache and all (for tests).
    pub fn set_fonts(&self, fonts: Fonts) {
        *self.fonts.write().unwrap_or_else(|e| e.into_inner()) = fonts;
    }

    /// Fetches what `design` (beside it, in `folder`) uses that isn't
    /// cached (a Design read from a file made on another computer). One
    /// that can't be fetched draws nothing, and isn't asked for again for a
    /// while; a font family beside the Design isn't fetched.
    pub fn fetch(&self, design: &Design, folder: Option<&Path>) {
        let fonts = self.fonts_beside(folder);
        for key in used(design) {
            match key.kind {
                AssetKind::Icon => {
                    let _ = self.icons.load(&key.name);
                }
                AssetKind::Font => {
                    if let Some((family, weight)) = key.font_weight() {
                        fonts.fetch(family, weight);
                    }
                }
            }
        }
    }

    /// Each asset `design` (beside it, in `folder`) uses that's cached, and
    /// its file; one that isn't is left out, and draws nothing (text in
    /// Geist). A family beside the Design is each of its files. Never
    /// fetches.
    pub fn resolved(&self, design: &Design, folder: Option<&Path>) -> Vec<Resolved> {
        let fonts = self.fonts_beside(folder);
        let mut resolved: Vec<Resolved> = used(design)
            .into_iter()
            .flat_map(|key| match key.kind {
                AssetKind::Icon => self
                    .icons
                    .file_of(&key.name)
                    .ok()
                    .filter(|path| path.is_file())
                    .map(|path| Resolved { key, path })
                    .into_iter()
                    .collect(),
                AssetKind::Font => {
                    let Some((family, weight)) = key.font_weight() else {
                        return Vec::new();
                    };
                    fonts
                        .found(family, weight)
                        .into_iter()
                        .map(|path| {
                            let file = path.file_stem().unwrap_or_default().to_string_lossy();
                            let key = AssetKey { kind: AssetKind::Font, name: format!("{family}@{file}") };
                            Resolved { key, path }
                        })
                        .collect::<Vec<_>>()
                }
            })
            .collect();
        // (A family beside the Design is all of its files for each weight
        // asked for: each once.)
        let mut seen = BTreeSet::new();
        resolved.retain(|resolved| seen.insert(resolved.key.clone()));
        resolved
    }

    /// This computer's fonts, and those beside the Design in `folder`.
    fn fonts_beside(&self, folder: Option<&Path>) -> Fonts {
        match folder {
            Some(folder) => self.fonts().beside(folder),
            None => self.fonts(),
        }
    }
}

/// What's cached only: drawing never fetches. (A font beside a Design,
/// named by its file, isn't among them: text gets it from [`Fonts`].)
impl Assets for Cache {
    fn get(&self, key: &AssetKey) -> Option<Arc<[u8]>> {
        match key.kind {
            AssetKind::Icon => self.icons.cached(&key.name).map(Arc::from),
            AssetKind::Font => {
                let (family, weight) = key.font_weight()?;
                let path = self.fonts().found(family, weight).into_iter().next()?;
                std::fs::read(path).ok().map(Arc::from)
            }
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
