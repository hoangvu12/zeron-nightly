//! Icons: any Iconify icon by set and name (`lucide:house`), fetched as SVG
//! from an [`IconSource`] on first use and cached on the computer, one file
//! per icon, so the engine and the window draw the same icon offline.
//!
//! Iconify's SVGs are sized in em, which Skia's SVG module draws as
//! nothing, so [`prepare`] sets the root's size in pixels before drawing,
//! along with the icon's colour and weight.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::model::Color;

/// Where icons come from on a cache miss: Iconify's API for the engine, a
/// local fixture in tests.
pub trait IconSource: Send + Sync {
    /// The icon's SVG as Iconify serves it, or `Ok(None)` when the set has
    /// no icon of that name (or there's no such set).
    fn fetch(&self, set: &str, name: &str) -> Result<Option<String>, String>;
}

#[derive(Debug, thiserror::Error)]
pub enum IconError {
    #[error("\"{0}\" is not an icon name: write the Iconify set and name, as in \"lucide:house\"")]
    BadName(String),
    #[error("Iconify has no icon \"{0}\": check the set and the name at icon-sets.iconify.design")]
    Unknown(String),
    #[error("couldn't fetch the icon \"{0}\": {1}")]
    Unavailable(String, String),
}

/// The icons cache: in memory, then on disk, then the source.
pub struct Icons {
    dir: PathBuf,
    source: RwLock<Arc<dyn IconSource>>,
    loaded: Mutex<HashMap<String, Arc<str>>>,
}

impl Icons {
    /// Icons cached in `dir` (as `<set>/<name>.svg`), fetched from `source`.
    pub fn new(dir: impl Into<PathBuf>, source: Arc<dyn IconSource>) -> Self {
        Self {
            dir: dir.into(),
            source: RwLock::new(source),
            loaded: Mutex::new(HashMap::new()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Fetches cache misses from `source` from now on.
    pub fn set_source(&self, source: Arc<dyn IconSource>) {
        *self.source.write().unwrap_or_else(|e| e.into_inner()) = source;
    }

    /// The SVG of the icon `icon` (`set:name`) at `weight`, as its source
    /// served it. A weight picks the set's variant where it has them by
    /// name (Phosphor's `-bold`); see [`prepare`] for the rest.
    pub fn svg(&self, icon: &str, weight: Option<u16>) -> Result<Arc<str>, IconError> {
        let (set, name) = split(icon)?;
        let name = variant(set, name, weight);
        let key = format!("{set}:{name}");
        if let Some(svg) = lock(&self.loaded).get(&key) {
            return Ok(svg.clone());
        }
        let file = self.dir.join(set).join(format!("{name}.svg"));
        let svg: Arc<str> = match std::fs::read_to_string(&file) {
            Ok(svg) => svg.into(),
            Err(_) => {
                let source = self.source.read().unwrap_or_else(|e| e.into_inner()).clone();
                let svg = source
                    .fetch(set, &name)
                    .map_err(|e| IconError::Unavailable(key.clone(), e))?
                    .filter(|svg| svg.trim_start().starts_with("<svg"))
                    .ok_or_else(|| IconError::Unknown(key.clone()))?;
                // A cache that can't be written is only slower next time.
                let _ = write_whole(&file, &svg);
                svg.into()
            }
        };
        lock(&self.loaded).insert(key, svg.clone());
        Ok(svg)
    }
}

/// `set:name`, both made of Iconify's characters (which also keeps a name
/// from reaching outside the cache folder).
fn split(icon: &str) -> Result<(&str, &str), IconError> {
    let valid = |part: &str| {
        !part.is_empty()
            && !part.starts_with('-')
            && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    match icon.split_once(':') {
        Some((set, name)) if valid(set) && valid(name) => Ok((set, name)),
        _ => Err(IconError::BadName(icon.into())),
    }
}

/// Phosphor's weights, as the suffixes its icons are named with.
const PHOSPHOR_WEIGHTS: &[&str] = &["thin", "light", "bold", "fill", "duotone"];

/// The icon's name at `weight`, for sets whose weights are separate icons.
fn variant(set: &str, name: &str, weight: Option<u16>) -> String {
    let Some(weight) = weight else {
        return name.into();
    };
    let named = name
        .rsplit_once('-')
        .is_some_and(|(_, suffix)| PHOSPHOR_WEIGHTS.contains(&suffix));
    if set != "ph" || named {
        return name.into();
    }
    match weight {
        0..=150 => format!("{name}-thin"),
        151..=350 => format!("{name}-light"),
        351..=550 => name.into(),
        _ => format!("{name}-bold"),
    }
}

/// Whether a weight changes `icon` (`set:name`, served as `svg`): in
/// Phosphor, by name; elsewhere, where it's drawn with strokes.
pub fn takes_weight(icon: &str, svg: &str) -> bool {
    icon.starts_with("ph:") || svg.contains("stroke-width=")
}

/// `svg` ready to draw at `width` × `height` pixels: the root sized in
/// pixels, `currentColor` as `color` (its alpha is the painter's to apply)
/// and, for a weight in a set drawn with strokes, its strokes scaled from
/// the regular 400.
pub fn prepare(svg: &str, color: Color, weight: Option<u16>, width: f32, height: f32) -> String {
    let Color { r, g, b, .. } = color;
    let mut svg = svg.replace("currentColor", &format!("#{r:02X}{g:02X}{b:02X}"));
    if let Some(weight) = weight {
        svg = scale_strokes(&svg, f32::from(weight.max(1)) / 400.);
    }
    let Some(start) = svg.find("<svg") else {
        return svg;
    };
    let Some(end) = svg[start..].find('>').map(|end| start + end) else {
        return svg;
    };
    let mut root = svg[start..end].to_owned();
    root = set_attribute(&root, "width", &format!("{width}px"));
    root = set_attribute(&root, "height", &format!("{height}px"));
    format!("{}{root}{}", &svg[..start], &svg[end..])
}

/// Every `stroke-width="n"` as `n × factor`.
fn scale_strokes(svg: &str, factor: f32) -> String {
    const ATTRIBUTE: &str = "stroke-width=\"";
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find(ATTRIBUTE) {
        let (before, after) = rest.split_at(at + ATTRIBUTE.len());
        out.push_str(before);
        let Some(close) = after.find('"') else {
            rest = after;
            break;
        };
        match after[..close].trim().parse::<f32>() {
            Ok(width) => out.push_str(&(width * factor).to_string()),
            Err(_) => out.push_str(&after[..close]),
        }
        rest = &after[close..];
    }
    out.push_str(rest);
    out
}

/// `tag` (an opening tag without its `>`) with `name="value"`, replacing
/// the attribute it had.
fn set_attribute(tag: &str, name: &str, value: &str) -> String {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(at) = tag[from..].find(&needle).map(|at| from + at) {
        // A whole attribute name: `width`, not `stroke-width`.
        if tag[..at].ends_with(char::is_whitespace) {
            let start = at + needle.len();
            if let Some(len) = tag[start..].find('"') {
                return format!("{}{value}{}", &tag[..start], &tag[start + len..]);
            }
        }
        from = at + needle.len();
    }
    let tag = tag.trim_end_matches('/');
    format!("{tag} {name}=\"{value}\"")
}

/// Writes `text` whole or not at all, so a reader never sees half a file.
fn write_whole(file: &Path, text: &str) -> std::io::Result<()> {
    let folder = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
