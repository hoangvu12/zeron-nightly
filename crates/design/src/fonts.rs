//! Where a Design's text gets its fonts, the same in the engine and the
//! window so both draw alike. A family is looked for, in order:
//!
//! - in font files beside the Design (its folder and a `fonts/` in it);
//! - in the one family bundled, Geist ([`BUNDLED_FAMILY`]);
//! - in the computer's font cache, a folder shared by the engine and the
//!   window;
//! - from a [`FontSource`], once, into that cache: Google Fonts
//!   ([`GoogleFonts`]), or a folder of font files in tests ([`FontFolder`]).
//!
//! The window doesn't fetch: the engine tells it the font files a Design
//! uses ([`Fonts::files`]) and it draws with those ([`Fonts::of_files`]).
//!
//! Never the computer's own fonts, so text draws the same everywhere. A
//! family that can't be had draws in Geist, with a warning ([`Fonts::warnings`]).
//! Characters a text's family lacks fall back to Geist, then to a Noto family
//! for their script, fetched like any other ([`families`]); characters none
//! of those cover draw nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use skia_safe::textlayout::TypefaceFontProvider;
use skia_safe::{FontMgr, Typeface};

use crate::model::{Design, Kind, Text};
use crate::skia::Typesetter;

/// The family bundled with Zeron: drawn without a fetch, and in place of a
/// family that can't be had.
pub const BUNDLED_FAMILY: &str = "Geist";

/// The Zeron app's own Geist.
const BUNDLED: &[&[u8]] = &[
    include_bytes!("../../../apps/zgx/fonts/Geist.ttf"),
    include_bytes!("../../../apps/zgx/fonts/Geist-Medium.ttf"),
    include_bytes!("../../../apps/zgx/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../../apps/zgx/fonts/Geist-Bold.ttf"),
];

/// How long a fetch that failed (offline, say) is left before it's tried
/// again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Font files read from beside a Design.
const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc"];

/// A font file a Design's text uses, as the engine found it: in its cache
/// or beside the Design.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FontFile {
    /// The family the Design names it by.
    pub family: String,
    pub path: PathBuf,
}

/// A typeface, and the file it was read from (none for the bundled one).
type Face = (Typeface, Option<PathBuf>);

/// Where fonts not yet cached come from.
pub trait FontSource: Send + Sync {
    /// A font file of `family` at the weight nearest `weight`, or `None`
    /// when the source has no such family.
    fn fetch(&self, family: &str, weight: u16) -> Result<Option<Vec<u8>>, String>;
}

/// The fonts of one computer, optionally with those beside one Design.
/// Cheap to clone: clones share what's loaded.
#[derive(Clone, Default)]
pub struct Fonts {
    library: Arc<Library>,
    /// The folder of the Design these fonts are for.
    beside: Option<PathBuf>,
}

#[derive(Default)]
struct Library {
    /// The font cache: `{family}/{weight}.ttf`, as fetched.
    cache: Option<PathBuf>,
    source: Option<Box<dyn FontSource>>,
    /// Font files given by family, looked in first ([`Fonts::of_files`]).
    given: Vec<(String, Typeface, PathBuf)>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Typefaces from the cache or the source, by family and weight.
    loaded: HashMap<(String, u16), Face>,
    /// Families the source doesn't have.
    unknown: HashSet<String>,
    /// Fetches that failed: when, and why.
    failed: HashMap<(String, u16), (Instant, String)>,
    /// Font files beside Designs, by folder.
    beside: HashMap<PathBuf, Beside>,
}

/// One folder's font files: the listing they were read from, so a change
/// to it reads them again, and their typefaces by family.
struct Beside {
    listing: Vec<(PathBuf, SystemTime)>,
    families: Vec<(String, Typeface, PathBuf)>,
}

/// Why a family couldn't be had.
enum Missing {
    /// No such family, from the source or beside the Design.
    Unknown,
    /// Fetching it failed.
    Failed(String),
}

impl Fonts {
    /// Fonts cached in `cache` (none without one), fetched from `source`
    /// (none without one).
    pub fn new(cache: Option<PathBuf>, source: Option<Box<dyn FontSource>>) -> Self {
        Self {
            library: Arc::new(Library {
                cache,
                source,
                given: Vec::new(),
                state: Mutex::default(),
            }),
            beside: None,
        }
    }

    /// Fonts of these files only (and the bundled family), fetching
    /// nothing: the window's, from what the engine says a Design uses.
    /// Files that can't be read are skipped.
    pub fn of_files(files: &[FontFile]) -> Self {
        let given = files
            .iter()
            .filter_map(|file| {
                let typeface = decode(&std::fs::read(&file.path).ok()?)?;
                Some((file.family.clone(), typeface, file.path.clone()))
            })
            .collect();
        Self {
            library: Arc::new(Library {
                cache: None,
                source: None,
                given,
                state: Mutex::default(),
            }),
            beside: None,
        }
    }

    /// These fonts, and those in font files beside the Design in `folder`.
    pub fn beside(&self, folder: &Path) -> Self {
        Self {
            library: self.library.clone(),
            beside: Some(folder.to_owned()),
        }
    }

    /// Lays out and draws `design`'s text, with every font it uses loaded
    /// (fetched first when needed).
    pub fn typesetter(&self, design: &Design) -> Typesetter {
        let mut provider = TypefaceFontProvider::new();
        for typeface in bundled() {
            provider.register_typeface(typeface.clone(), Some(BUNDLED_FAMILY));
        }
        self.each_face(design, &mut |family, (typeface, _)| {
            provider.register_typeface(typeface, Some(family));
        });
        Typesetter::new(provider)
    }

    /// The font files `design`'s text uses (fetched first when needed), for
    /// the window to draw it with ([`Fonts::of_files`]).
    pub fn files(&self, design: &Design) -> Vec<FontFile> {
        let mut files = Vec::new();
        self.each_face(design, &mut |family, (_, path)| {
            if let Some(path) = path {
                let file = FontFile { family: family.to_owned(), path };
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        });
        files
    }

    /// Calls `visit` with each typeface `design`'s text uses, and the
    /// family it's for.
    fn each_face(&self, design: &Design, visit: &mut dyn FnMut(&str, Face)) {
        let mut done = HashSet::new();
        design.walk(&mut |node| {
            let Kind::Text(text) = &node.kind else {
                return;
            };
            let weight = snapped(text.weight().0);
            for family in families(text) {
                if !done.insert((family.clone(), weight)) {
                    continue;
                }
                for face in self.load(&family, weight).unwrap_or_default() {
                    visit(&family, face);
                }
            }
        });
    }

    /// What an agent should hear of the fonts `texts` use: families that
    /// couldn't be had, once each.
    pub fn warnings<'a>(&self, texts: impl IntoIterator<Item = &'a Text>) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut warnings = Vec::new();
        for text in texts {
            let weight = snapped(text.weight().0);
            for (i, family) in families(text).into_iter().enumerate() {
                if !seen.insert(family.clone()) {
                    continue;
                }
                let drawn = if i == 0 {
                    format!("its text draws in {BUNDLED_FAMILY}")
                } else {
                    "the characters it was for draw nothing".to_owned()
                };
                match self.load(&family, weight) {
                    Ok(_) => {}
                    Err(Missing::Unknown) => warnings.push(format!(
                        "there's no font family \"{family}\" on Google Fonts or in a font file beside the Design: {drawn}"
                    )),
                    Err(Missing::Failed(why)) => warnings.push(format!(
                        "couldn't get the font family \"{family}\" ({why}): {drawn} until it can be fetched"
                    )),
                }
            }
        }
        warnings
    }

    /// `family`'s typefaces nearest `weight`: all of a family given or
    /// beside the Design, else the bundled one, else one from the cache or
    /// the source.
    fn load(&self, family: &str, weight: u16) -> Result<Vec<Face>, Missing> {
        let given: Vec<Face> = self
            .library
            .given
            .iter()
            .filter(|(name, ..)| name == family)
            .map(|(_, typeface, path)| (typeface.clone(), Some(path.clone())))
            .collect();
        if !given.is_empty() {
            return Ok(given);
        }
        if let Some(folder) = &self.beside {
            let typefaces = self.beside_design(folder, family);
            if !typefaces.is_empty() {
                return Ok(typefaces);
            }
        }
        if family.eq_ignore_ascii_case(BUNDLED_FAMILY) {
            return Ok(bundled().iter().map(|typeface| (typeface.clone(), None)).collect());
        }
        let key = (family.to_owned(), weight);
        {
            let state = lock(&self.library.state);
            if let Some(face) = state.loaded.get(&key) {
                return Ok(vec![face.clone()]);
            }
            if state.unknown.contains(family) {
                return Err(Missing::Unknown);
            }
            if let Some((when, why)) = state.failed.get(&key)
                && when.elapsed() < RETRY_AFTER
            {
                return Err(Missing::Failed(why.clone()));
            }
        }
        let cached = self.library.cache.as_ref().map(|cache| cached_file(cache, family, weight));
        if let Some(file) = &cached
            && let Some(typeface) = std::fs::read(file).ok().and_then(|bytes| decode(&bytes))
        {
            let face = (typeface, Some(file.clone()));
            lock(&self.library.state).loaded.insert(key, face.clone());
            return Ok(vec![face]);
        }
        let Some(source) = &self.library.source else {
            return Err(Missing::Unknown);
        };
        // Fetched without holding the fonts up for other Designs' text.
        let fetched = source.fetch(family, weight);
        let mut state = lock(&self.library.state);
        match fetched {
            Ok(Some(bytes)) => match decode(&bytes) {
                Some(typeface) => {
                    // A font that can't be cached still serves this run
                    // (but not the window, which reads it from the cache).
                    let file = cached.filter(|file| match write_whole(file, &bytes) {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::warn!("can't cache the font {}: {e}", file.display());
                            false
                        }
                    });
                    let face = (typeface, file);
                    state.failed.remove(&key);
                    state.loaded.insert(key, face.clone());
                    Ok(vec![face])
                }
                None => {
                    let why = "what was fetched isn't a font file".to_owned();
                    state.failed.insert(key, (Instant::now(), why.clone()));
                    Err(Missing::Failed(why))
                }
            },
            Ok(None) => {
                state.unknown.insert(family.to_owned());
                Err(Missing::Unknown)
            }
            Err(why) => {
                state.failed.insert(key, (Instant::now(), why.clone()));
                Err(Missing::Failed(why))
            }
        }
    }

    /// The typefaces of `family` in font files in `folder` and its
    /// `fonts/`, read again when the files there change.
    fn beside_design(&self, folder: &Path, family: &str) -> Vec<Face> {
        let listing = font_files(&[folder.to_owned(), folder.join("fonts")]);
        let mut state = lock(&self.library.state);
        let read = state.beside.get(folder).is_some_and(|beside| beside.listing == listing);
        if !read {
            let families = listing
                .iter()
                .filter_map(|(path, _)| {
                    let typeface = decode(&std::fs::read(path).ok()?)?;
                    Some((typeface.family_name(), typeface, path.clone()))
                })
                .collect();
            state.beside.insert(folder.to_owned(), Beside { listing, families });
        }
        state.beside[folder]
            .families
            .iter()
            .filter(|(name, ..)| name.eq_ignore_ascii_case(family))
            .map(|(_, typeface, path)| (typeface.clone(), Some(path.clone())))
            .collect()
    }
}

/// The families a text draws with, in order: its own, then Geist, then a
/// Noto family for each script among its characters that Geist lacks.
/// Each character draws in the first that has it, so a text draws the same
/// whatever else its Design holds.
pub fn families(text: &Text) -> Vec<String> {
    let mut families = vec![text.family().to_owned()];
    let mut scripts = [false; FALLBACKS.len()];
    for c in text.content.chars() {
        if let Some(i) = fallback(c) {
            scripts[i] = true;
        }
    }
    let fallbacks = FALLBACKS.iter().zip(scripts).filter(|(_, used)| *used).map(|((family, _), _)| *family);
    for family in std::iter::once(BUNDLED_FAMILY).chain(fallbacks) {
        if !families.iter().any(|f| f.eq_ignore_ascii_case(family)) {
            families.push(family.to_owned());
        }
    }
    families
}

/// Google Fonts' families for characters Geist lacks, by the code points
/// they're for, in the order they're tried. Japanese's comes before
/// Chinese's, so Han characters in Japanese text take Japanese forms; the
/// last takes every other character past ASCII.
const FALLBACKS: &[(&str, &[(u32, u32)])] = &[
    (
        "Noto Color Emoji",
        &[(0x200D, 0x200D), (0x2600, 0x27BF), (0x2B00, 0x2BFF), (0xFE0F, 0xFE0F), (0x1F000, 0x1FAFF)],
    ),
    ("Noto Sans JP", &[(0x3040, 0x30FF), (0x31F0, 0x31FF), (0xFF66, 0xFF9F)]),
    ("Noto Sans KR", &[(0x1100, 0x11FF), (0x3130, 0x318F), (0xAC00, 0xD7AF)]),
    (
        "Noto Sans SC",
        &[
            (0x2E80, 0x2FDF),
            (0x3000, 0x303F),
            (0x3400, 0x4DBF),
            (0x4E00, 0x9FFF),
            (0xF900, 0xFAFF),
            (0xFF00, 0xFFEF),
            (0x20000, 0x2FFFF),
        ],
    ),
    ("Noto Sans Arabic", &[(0x0600, 0x06FF), (0x0750, 0x077F), (0xFB50, 0xFDFF), (0xFE70, 0xFEFF)]),
    ("Noto Sans Hebrew", &[(0x0590, 0x05FF)]),
    ("Noto Sans Devanagari", &[(0x0900, 0x097F)]),
    ("Noto Sans Thai", &[(0x0E00, 0x0E7F)]),
    ("Noto Sans", &[(0x0080, 0x10FFFF)]),
];

/// Which of [`FALLBACKS`] `c` may need, if any.
fn fallback(c: char) -> Option<usize> {
    let c = c as u32;
    if c < 0x80 {
        return None;
    }
    FALLBACKS
        .iter()
        .position(|(_, ranges)| ranges.iter().any(|&(from, to)| (from..=to).contains(&c)))
}

/// A folder of font files as a [`FontSource`]: a family's file nearest the
/// weight. Tests serve fonts this way in place of Google Fonts.
pub struct FontFolder {
    dir: PathBuf,
}

impl FontFolder {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl FontSource for FontFolder {
    fn fetch(&self, family: &str, weight: u16) -> Result<Option<Vec<u8>>, String> {
        let nearest = font_files(std::slice::from_ref(&self.dir))
            .into_iter()
            .filter_map(|(path, _)| {
                let bytes = std::fs::read(&path).ok()?;
                let typeface = decode(&bytes)?;
                let distance = (*typeface.font_style().weight() - i32::from(weight)).abs();
                typeface.family_name().eq_ignore_ascii_case(family).then_some((distance, bytes))
            })
            .min_by_key(|(distance, _)| *distance);
        Ok(nearest.map(|(_, bytes)| bytes))
    }
}

/// Google Fonts, by family name: the font file Google's CSS API names for
/// a family and weight (or its default weight, when it hasn't that one).
#[cfg(feature = "google-fonts")]
pub struct GoogleFonts {
    agent: ureq::Agent,
}

#[cfg(feature = "google-fonts")]
impl Default for GoogleFonts {
    fn default() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build();
        let config = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Self { agent: config.into() }
    }
}

#[cfg(feature = "google-fonts")]
impl GoogleFonts {
    /// Most a font file may take; CJK families' run to about 20 MB.
    const MAX_FILE: u64 = 64 * 1024 * 1024;

    /// The CSS API's stylesheet for `query`, or `None` when it has no such
    /// family or weight (it answers 400).
    fn css(&self, query: &str) -> Result<Option<String>, String> {
        let url = format!("https://fonts.googleapis.com/css2?family={query}");
        match self.agent.get(&url).call() {
            Ok(mut response) => response.body_mut().read_to_string().map(Some).map_err(|e| e.to_string()),
            Err(ureq::Error::StatusCode(400)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(feature = "google-fonts")]
impl FontSource for GoogleFonts {
    fn fetch(&self, family: &str, weight: u16) -> Result<Option<Vec<u8>>, String> {
        let name: String = family
            .trim()
            .bytes()
            .map(|b| match b {
                b' ' => "+".to_owned(),
                b if b.is_ascii_alphanumeric() || b == b'-' => (b as char).to_string(),
                b => format!("%{b:02X}"),
            })
            .collect();
        let css = match self.css(&format!("{name}:wght@{weight}"))? {
            Some(css) => css,
            None => match self.css(&name)? {
                Some(css) => css,
                None => return Ok(None),
            },
        };
        // Asked without a browser's user agent, the API names TrueType files.
        let url = css
            .split("url(")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .ok_or_else(|| "Google Fonts named no font file".to_owned())?;
        let mut response = self.agent.get(url).call().map_err(|e| e.to_string())?;
        response
            .body_mut()
            .with_config()
            .limit(Self::MAX_FILE)
            .read_to_vec()
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

/// The bundled Geist, read once.
fn bundled() -> &'static [Typeface] {
    static BUNDLED_TYPEFACES: OnceLock<Vec<Typeface>> = OnceLock::new();
    BUNDLED_TYPEFACES.get_or_init(|| BUNDLED.iter().filter_map(|file| decode(file)).collect())
}

fn decode(bytes: &[u8]) -> Option<Typeface> {
    FontMgr::new().new_from_data(skia_safe::Data::new_copy(bytes), None)
}

/// `weight` to the nearest hundred Google Fonts serves, 100 to 900.
fn snapped(weight: u16) -> u16 {
    ((weight + 50) / 100 * 100).clamp(100, 900)
}

/// Where the cache keeps `family` at `weight`.
fn cached_file(cache: &Path, family: &str, weight: u16) -> PathBuf {
    let folder: String = family
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' })
        .collect();
    cache.join(folder).join(format!("{weight}.ttf"))
}

/// The font files in `folders`, with when each last changed, sorted.
fn font_files(folders: &[PathBuf]) -> Vec<(PathBuf, SystemTime)> {
    let mut files: Vec<_> = folders
        .iter()
        .filter_map(|folder| std::fs::read_dir(folder).ok())
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            let extension = path.extension()?.to_str()?.to_ascii_lowercase();
            if !FONT_EXTENSIONS.contains(&extension.as_str()) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort();
    files
}

/// Writes `path` whole or not at all, making its folder.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let temp = folder.join(format!(".{}.{}", uuid::Uuid::new_v4(), "part"));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
