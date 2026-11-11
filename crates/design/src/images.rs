//! Images for image fills ([`crate::model::ImageFill`]): a file by its path
//! relative to the Design's folder, read where it is, or an http(s) URL,
//! downloaded from an [`ImageSource`] on first use and cached on the
//! computer, one file per URL, so the engine and the window draw the same
//! image offline. Nothing is copied beside the Design or into its file: a
//! Design saved elsewhere takes its `images/` folder along.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// Where images come from on a cache miss: the web for the engine, a local
/// server in tests.
pub trait ImageSource: Send + Sync {
    /// The bytes at `url`, or why they can't be had.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// No source: nothing is downloaded.
struct NoSource;

impl ImageSource for NoSource {
    fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
        Err("images aren't downloaded here".into())
    }
}

/// How long a download that failed isn't tried again, so a Design drawn
/// often doesn't ask a server that's down each time.
const RETRY_AFTER: Duration = Duration::from_secs(60);

const NOT_AN_IMAGE: &str = "it isn't an image Zeron can draw (PNG, JPEG, WebP, GIF, BMP or ICO)";

/// Downloaded images: on disk, then the source.
pub struct Images {
    dir: PathBuf,
    source: RwLock<Arc<dyn ImageSource>>,
    /// Downloads that failed, why, and when.
    failed: Mutex<HashMap<String, (String, Instant)>>,
}

impl Images {
    /// Images cached in `dir`, downloaded from nowhere until
    /// [`set_source`](Self::set_source).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            source: RwLock::new(Arc::new(NoSource)),
            failed: Mutex::new(HashMap::new()),
        }
    }

    /// Downloads cache misses from `source` from now on.
    pub fn set_source(&self, source: Arc<dyn ImageSource>) {
        *self.source.write().unwrap_or_else(|e| e.into_inner()) = source;
    }

    /// The file the image at `url` is cached in, whether it is yet or not.
    pub fn file_of(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{:x}", Sha256::digest(url.as_bytes())))
    }

    /// The file holding the image at `url` if it's cached, or why it isn't:
    /// never downloads.
    pub fn had(&self, url: &str) -> Result<PathBuf, String> {
        let file = self.file_of(url);
        if file.is_file() {
            return Ok(file);
        }
        match lock(&self.failed).get(url) {
            Some((why, _)) => Err(why.clone()),
            None => Err("it isn't downloaded yet (the call waited on the network as long as it may); the next call may download it".into()),
        }
    }

    /// The file holding the image at `url`, downloaded now if it isn't
    /// cached. A download's wait doesn't count against the running
    /// snippet's time ([`crate::waits`]).
    pub fn load(&self, url: &str) -> Result<PathBuf, String> {
        let file = self.file_of(url);
        if file.is_file() {
            return Ok(file);
        }
        if let Some((why, at)) = lock(&self.failed).get(url)
            && at.elapsed() < RETRY_AFTER
        {
            return Err(why.clone());
        }
        let source = self.source.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Ok(fetched) = crate::waits::network(|| source.fetch(url)) else {
            // (Not remembered: the next call may have time for it.)
            return Err("this call has waited on the network too long already; the next may download it".into());
        };
        let kept = fetched
            .and_then(|bytes| if crate::skia::is_image(&bytes) { Ok(bytes) } else { Err(NOT_AN_IMAGE.into()) })
            .and_then(|bytes| {
                write_whole(&file, &bytes).map_err(|e| format!("can't keep the download in {}: {e}", self.dir.display()))
            });
        match kept {
            Ok(()) => {
                lock(&self.failed).remove(url);
                Ok(file)
            }
            Err(why) => {
                lock(&self.failed).insert(url.into(), (why.clone(), Instant::now()));
                Err(why)
            }
        }
    }
}

/// The file of the image at `url`, a path relative to `folder`, or why it
/// can't be drawn.
pub fn beside(folder: &Path, url: &str) -> Result<PathBuf, String> {
    let path = folder.join(url.replace('\\', "/"));
    let bytes = std::fs::read(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("there's no file at {}", path.display()),
        _ => format!("can't read {}: {e}", path.display()),
    })?;
    if !crate::skia::is_image(&bytes) {
        return Err(NOT_AN_IMAGE.into());
    }
    Ok(path)
}

/// Writes `bytes` whole or not at all, so a reader never sees half a file.
fn write_whole(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
