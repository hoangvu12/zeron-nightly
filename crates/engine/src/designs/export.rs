//! Writing a call's exports once it's kept ([`zeron_design::edit::Export`]):
//! PNG files drawn as screenshots are, at the scale asked, and HTML whose
//! images are found from the page.

use std::path::{Component, Path, PathBuf};

use zeron_design::edit::{Export, Warning};
use zeron_design::export::{self, Sources};
use zeron_design::render;

use super::{Live, write_whole};

impl Live {
    /// Writes `exports`. Returns the files written, and a warning for each
    /// that couldn't be (the call's edits are kept all the same).
    pub(super) fn export(&self, exports: &[Export]) -> (Vec<String>, Vec<Warning>) {
        let (mut written, mut warnings) = (Vec::new(), Vec::new());
        let mut wrote = |path: &Path, bytes: Result<Vec<u8>, String>| match bytes
            .and_then(|bytes| write_whole(path, &bytes).map_err(|e| e.to_string()))
        {
            Ok(()) => written.push(path.display().to_string()),
            Err(e) => warnings.push(Warning::export(format!("Export couldn't write {}: {e}", path.display()))),
        };
        for export in exports {
            match export {
                Export::Png { ids, files, scale } => {
                    for (id, file) in ids.iter().zip(files) {
                        let png = render::png(&self.design, id, *scale, &self.fonts, &self.images, &*self.assets)
                            .map(|shot| shot.png)
                            .map_err(|e| e.to_string());
                        wrote(file, png);
                    }
                }
                Export::Html { ids, file, options } => {
                    let page_folder = file.parent().unwrap_or(Path::new("."));
                    let design_folder = self.path.parent().unwrap_or(Path::new("."));
                    let image_href = |url: &str| href(url, design_folder, page_folder);
                    let sources = Sources { fonts: &self.fonts, assets: &*self.assets, image_href: &image_href };
                    let html = export::html(&self.design, ids, *options, &sources).map(String::into_bytes);
                    wrote(file, html);
                }
            }
        }
        (written, warnings)
    }
}

/// Where a page in `page_folder` finds the image `url` names: a URL as it
/// is, a file (from the Design's folder) by its path from the page.
fn href(url: &str, design_folder: &Path, page_folder: &Path) -> String {
    if url.contains("://") || url.starts_with("data:") {
        return url.to_owned();
    }
    let file = design_folder.join(url);
    match relative(page_folder, &file) {
        Some(path) => path.to_string_lossy().replace('\\', "/"),
        None => format!("file:///{}", file.to_string_lossy().replace('\\', "/").trim_start_matches('/')),
    }
}

/// `to` as a path from the folder `from`, when they share a root.
fn relative(from: &Path, to: &Path) -> Option<PathBuf> {
    let (from, to): (Vec<Component>, Vec<Component>) = (from.components().collect(), to.components().collect());
    if from.first() != to.first() {
        return None;
    }
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in shared..from.len() {
        path.push("..");
    }
    for part in &to[shared..] {
        path.push(part.as_os_str());
    }
    Some(path)
}

