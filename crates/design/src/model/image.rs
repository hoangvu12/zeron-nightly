//! An image paint: a picture laid over a node's bounds. It refers to its
//! image, never holds it: a file by its path relative to the Design's
//! folder (in its `images/` folder or beside the `.zdesign` file), or an
//! http(s) URL, which whoever draws the Design downloads once and caches
//! ([`crate::render::Images`]). A Design saved elsewhere keeps its relative
//! paths, so its images go with it.

use serde_json::{Map, Value, json};

use super::warn::{choice, name_of};

#[derive(Debug, Clone, PartialEq)]
pub struct ImageFill {
    /// A relative path (either separator) or an http(s) URL.
    pub url: String,
    pub mode: ImageMode,
}

/// How an image fits the node's bounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImageMode {
    /// Scaled to cover them, centered, its overflow cut off.
    #[default]
    Cover,
    /// Scaled to fit within them, centered, the rest left empty.
    Contain,
    /// Stretched to them, out of its proportions.
    Stretch,
}

const MODES: &[(&str, ImageMode)] = &[
    ("cover", ImageMode::Cover),
    ("contain", ImageMode::Contain),
    ("stretch", ImageMode::Stretch),
];

impl ImageFill {
    /// Whether an image's `url` is downloaded, rather than a file in the
    /// Design's folder.
    pub fn is_remote(url: &str) -> bool {
        remote(url)
    }

    pub(super) fn from_props(paint: &Map<String, Value>) -> Result<Self, String> {
        let url = match paint.get("url") {
            Some(Value::String(url)) => url.trim().to_owned(),
            _ => return Err("an image paint needs a \"url\": a path relative to the Design's folder, or an http(s) URL".into()),
        };
        check(&url)?;
        Ok(Self {
            url,
            mode: choice(paint, "mode", "image mode", MODES)?.unwrap_or_default(),
        })
    }

    pub(super) fn write(&self, props: &mut Map<String, Value>) {
        props.insert("type".into(), json!("image"));
        props.insert("url".into(), json!(self.url));
        if self.mode != ImageMode::Cover {
            props.insert("mode".into(), json!(name_of(MODES, self.mode)));
        }
    }
}

fn remote(url: &str) -> bool {
    let scheme = url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https"))
}

/// Refuses what isn't a relative path or an http(s) URL: an absolute path
/// breaks when the Design moves, and an image's data is never kept in the
/// file.
fn check(url: &str) -> Result<(), String> {
    let relative = "an image's url is a path relative to the Design's folder (such as images/photo.png: copy the file into the images folder beside the .zdesign file) or an http(s) URL";
    if url.is_empty() {
        return Err(relative.into());
    }
    if remote(url) {
        return Ok(());
    }
    // A drive (`C:`) or another scheme (`data:`, `file:`) has a colon.
    if url.contains(':') || url.starts_with(['/', '\\']) {
        return Err(format!("{relative}, not \"{}\"", shortened(url)));
    }
    Ok(())
}

/// `url` cut short for a message, as a data URL may be long.
fn shortened(url: &str) -> String {
    match url.char_indices().nth(80) {
        Some((at, _)) => format!("{}…", &url[..at]),
        None => url.to_owned(),
    }
}
