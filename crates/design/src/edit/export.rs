//! `Export`: nodes as PNG files into a folder, or Screens as one HTML file.
//! The call checks now that it could write where the export goes, so a
//! path it can't write fails the call with why (and leaves nothing
//! behind); the folders and files are made once the call is kept, from the
//! Design as it left it.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{Draft, EditError};
use crate::export::HtmlOptions;

/// What a PNG export is drawn at unless asked otherwise: twice its size.
const DEFAULT_SCALE: f32 = 2.;

/// Largest scale a PNG export is drawn at.
const MAX_SCALE: f32 = 8.;

/// An export the call asked for, its paths resolved and checked.
#[derive(Debug, Clone, PartialEq)]
pub enum Export {
    /// Each node as a PNG file, at `scale`, into `files` (one per node).
    Png { ids: Vec<String>, files: Vec<PathBuf>, scale: f32 },
    /// The Screens `ids` (all of them, without any) as the HTML `file`.
    Html { ids: Vec<String>, file: PathBuf, options: HtmlOptions },
}

impl Export {
    /// The files it writes.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Export::Png { files, .. } => files.iter().map(PathBuf::as_path).collect(),
            Export::Html { file, .. } => vec![file.as_path()],
        }
    }
}

/// `Export`'s options, as the snippet gives them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Options {
    format: String,
    /// A folder for PNG files, a file for HTML: absolute, or from the
    /// Design's folder.
    path: String,
    #[serde(default)]
    scale: Option<f32>,
    /// `tailwind` or `css` (plain), for HTML.
    #[serde(default)]
    css: Option<String>,
    #[serde(default)]
    scaffold: Option<bool>,
    #[serde(default)]
    layer_names: Option<bool>,
}

impl Draft {
    /// Asks for the export of the nodes `ids` (for HTML, none for every
    /// Screen) with the call's result, as `options` says. Returns the paths
    /// of the files it will write.
    pub fn export(&mut self, ids: Vec<String>, options: Value) -> Result<Vec<String>, EditError> {
        let invalid = |e: String| EditError::Invalid(format!("Export: {e}"));
        let options: Options = serde_json::from_value(options).map_err(|e| {
            invalid(format!(
                "it takes {{ format: \"png\" | \"html\", path, scale?, css?: \"tailwind\" | \"css\", scaffold?, layerNames? }}: {e}"
            ))
        })?;
        for id in &ids {
            if self.design.find(id).is_none() && super::inside::expanded(&self.design).find(id).is_none() {
                return Err(EditError::NotFound(id.clone()));
            }
        }
        let path = self.export_path(&options.path).map_err(invalid)?;
        let export = match options.format.as_str() {
            "png" => {
                if ids.is_empty() {
                    return Err(invalid("a PNG export needs the ids of the nodes to draw".into()));
                }
                let scale = options.scale.unwrap_or(DEFAULT_SCALE);
                if !(scale > 0. && scale <= MAX_SCALE) {
                    return Err(invalid(format!("its scale is above 0 and at most {MAX_SCALE}, not {scale}")));
                }
                writable(&path).map_err(invalid)?;
                let files = file_names(&self.design, &ids).into_iter().map(|name| path.join(name)).collect();
                Export::Png { ids, files, scale }
            }
            "html" => {
                if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm")) {
                    return Err(invalid(format!("an HTML export's path is a .html file, not {}", path.display())));
                }
                let folder = path.parent().ok_or_else(|| invalid(format!("{} has no folder", path.display())))?;
                writable(folder).map_err(invalid)?;
                let tailwind = match options.css.as_deref() {
                    None | Some("css") => false,
                    Some("tailwind") => true,
                    Some(other) => return Err(invalid(format!("css is \"tailwind\" or \"css\", not \"{other}\""))),
                };
                let options = HtmlOptions {
                    tailwind,
                    scaffold: options.scaffold.unwrap_or(true),
                    layer_names: options.layer_names.unwrap_or(false),
                };
                Export::Html { ids, file: path, options }
            }
            other => return Err(invalid(format!("its format is \"png\" or \"html\", not \"{other}\""))),
        };
        let paths = export.paths().iter().map(|path| path.display().to_string()).collect();
        self.exports.push(export);
        Ok(paths)
    }

    /// `path` as written to: absolute, or from the Design's folder.
    fn export_path(&self, path: &str) -> Result<PathBuf, String> {
        let path = Path::new(path);
        if path.as_os_str().is_empty() {
            return Err("its path is empty".into());
        }
        let path = match (&self.folder, path.is_absolute()) {
            (_, true) => path.to_path_buf(),
            (Some(folder), false) => folder.join(path),
            (None, false) => return Err(format!("{} isn't absolute, and the Design has no folder yet", path.display())),
        };
        // Its separators this system's and its `..` taken, however written,
        // so the paths from it (to its images) are right.
        let mut normal = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::ParentDir => {
                    normal.pop();
                }
                std::path::Component::CurDir => {}
                part => normal.push(part.as_os_str()),
            }
        }
        let path = normal;
        if path.extension().is_some_and(|e| e == "zdesign") {
            return Err(format!("{} is a Design's file, which an export doesn't write over", path.display()));
        }
        Ok(path)
    }
}

/// Whether `folder` could be written in: the nearest part of it that's
/// there is a folder, and a file can be written (then removed) in it.
fn writable(folder: &Path) -> Result<(), String> {
    let there = folder.ancestors().find(|ancestor| ancestor.exists()).ok_or_else(|| {
        format!("can't write in {}: none of its folders is there", folder.display())
    })?;
    if !there.is_dir() {
        return Err(format!(
            "can't make the folder {}: {} is a file, not a folder",
            folder.display(),
            there.display()
        ));
    }
    let probe = there.join(format!(".zeron-export-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&probe, b"").map_err(|e| format!("can't write in {}: {e}", there.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// A PNG file name for each node: its name, made safe, and numbered past
/// the first of a name.
fn file_names(design: &crate::model::Design, ids: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for id in ids {
        let name = design.find(id).map(|node| node.name.as_str()).unwrap_or(id);
        let base: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let base = match base.trim_matches('-') {
            "" => "node".to_owned(),
            base => base.to_owned(),
        };
        let mut file = format!("{base}.png");
        let mut n = 1;
        while names.contains(&file) {
            n += 1;
            file = format!("{base}-{n}.png");
        }
        names.push(file);
    }
    names
}
