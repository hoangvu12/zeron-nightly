//! The images a Design's image paints refer to (#24): files by their path
//! relative to the Design's folder, read when drawn, and http(s) URLs,
//! downloaded once into `image-cache/` in the engine's data dir and read
//! from there after. Nothing is copied beside the Design or into its file,
//! so a Design saved elsewhere takes its `images/` folder along and finds
//! its URLs in the cache again. Design tabs are told each image's file
//! ([`DesignImages::files`]) and read it there: only the engine downloads.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use zeron_design::Design;
use zeron_design::model::{ImageFill, Node};
use zeron_design::render::{self, Images};

use super::{lock, write_whole};

/// Largest image downloaded.
const MAX_DOWNLOAD_BYTES: usize = 32 * 1024 * 1024;

/// How long a download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a failed download is remembered before it's tried again, so a
/// Design drawn often doesn't ask a server that's down each time.
const RETRY_AFTER: Duration = Duration::from_secs(60);

const NOT_AN_IMAGE: &str = "it isn't an image Zeron can draw (PNG, JPEG, WebP, GIF, BMP or ICO)";

/// Downloaded images, shared by every Design on this computer.
pub struct ImageCache {
    dir: PathBuf,
    /// Downloads that failed, why, and when.
    failed: Mutex<HashMap<String, (String, Instant)>>,
}

impl ImageCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            failed: Mutex::new(HashMap::new()),
        }
    }

    /// The cache file holding the image at `url`, downloaded now if it
    /// isn't yet.
    fn file(&self, url: &str) -> Result<PathBuf, String> {
        let file = self.dir.join(format!("{:x}", Sha256::digest(url.as_bytes())));
        if file.is_file() {
            return Ok(file);
        }
        if let Some((why, at)) = lock(&self.failed).get(url)
            && at.elapsed() < RETRY_AFTER
        {
            return Err(why.clone());
        }
        let downloaded = download(url).and_then(|bytes| {
            if zeron_design::skia::is_image(&bytes) {
                Ok(bytes)
            } else {
                Err(NOT_AN_IMAGE.to_owned())
            }
        });
        let kept = downloaded.and_then(|bytes| {
            std::fs::create_dir_all(&self.dir)
                .and_then(|()| write_whole(&file, &bytes))
                .map_err(|e| format!("can't keep the download in {}: {e}", self.dir.display()))
        });
        match kept {
            Ok(()) => {
                lock(&self.failed).remove(url);
                Ok(file)
            }
            Err(why) => {
                lock(&self.failed).insert(url.to_owned(), (why.clone(), Instant::now()));
                Err(why)
            }
        }
    }
}

/// The images one Design's paints refer to.
pub struct DesignImages<'a> {
    /// The folder the Design's file is in.
    pub folder: PathBuf,
    pub cache: &'a ImageCache,
}

impl DesignImages<'_> {
    /// The file the image at `url` is in: in the Design's folder, or the
    /// cache's, downloaded now if it's a URL not had yet.
    fn file(&self, url: &str) -> Result<PathBuf, String> {
        if ImageFill::is_remote(url) {
            return self.cache.file(url);
        }
        let path = self.folder.join(url.replace('\\', "/"));
        if !path.is_file() {
            return Err(format!("there's no file at {}", path.display()));
        }
        Ok(path)
    }

    /// The file each image `design` draws is in, for those it has.
    pub fn files(&self, design: &Design) -> BTreeMap<String, PathBuf> {
        render::image_urls(design)
            .into_iter()
            .filter_map(|url| Some((url.clone(), self.file(&url).ok()?)))
            .collect()
    }
}

impl Images for DesignImages<'_> {
    fn get(&self, url: &str) -> Result<Arc<[u8]>, String> {
        let path = self.file(url)?;
        let bytes = std::fs::read(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        if !zeron_design::skia::is_image(&bytes) {
            return Err(NOT_AN_IMAGE.to_owned());
        }
        Ok(bytes.into())
    }
}

/// A warning for each image the `nodes` (and their descendants) draw that
/// can't be had, once per node and image.
pub fn warnings<'a>(nodes: impl Iterator<Item = &'a Node>, images: &dyn Images) -> Vec<String> {
    let mut found = Vec::new();
    for node in nodes {
        render::images_in(node, &mut found);
    }
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|(node, image)| seen.insert((node.id.as_str(), image.url.as_str())))
        .filter_map(|(node, image)| {
            let why = images.get(&image.url).err()?;
            Some(format!(
                "\"{}\" ({}): can't draw the image {}: {why}; a placeholder is drawn in its place",
                node.name,
                node.kind.name(),
                image.url
            ))
        })
        .collect()
}

/// Downloads `url` (blocking: called off the async runtime).
fn download(url: &str) -> Result<Vec<u8>, String> {
    let fetch = async {
        let client = reqwest::Client::builder()
            .timeout(DOWNLOAD_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        let mut response = client.get(url).send().await.map_err(|e| format!("can't download it: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("the server answered {}", response.status()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| format!("can't download it: {e}"))? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_DOWNLOAD_BYTES {
                return Err(format!("it's over {} MB", MAX_DOWNLOAD_BYTES / (1024 * 1024)));
            }
        }
        Ok(bytes)
    };
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => runtime.block_on(fetch),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(fetch),
    }
}
