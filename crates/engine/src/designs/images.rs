//! Image fills' downloads (#24), where http(s) images come from on a cache
//! miss ([`zeron_design::images`]), and the warnings for images a call
//! placed or looked at that can't be drawn.

use std::sync::OnceLock;
use std::time::Duration;

use zeron_design::assets::Beside;
use zeron_design::edit::Warning;
use zeron_design::images::ImageSource;
use zeron_design::model::Node;
use zeron_design::render;

/// Largest image downloaded.
const MAX_DOWNLOAD_BYTES: usize = 32 * 1024 * 1024;

/// One download's longest; a call's downloads together stop at the
/// snippet's `Limits::fetching`, off its own time limit.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Downloads images over http(s).
pub struct Download;

/// One runtime and client for every download, so connections are reused.
/// Static, so they're never dropped inside the engine's own runtime.
fn http() -> &'static (tokio::runtime::Runtime, reqwest::Client) {
    static HTTP: OnceLock<(tokio::runtime::Runtime, reqwest::Client)> = OnceLock::new();
    HTTP.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("design-images")
            .enable_all()
            .build()
            .expect("the image downloader's runtime starts");
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("the image downloader's client builds");
        (runtime, client)
    })
}

impl ImageSource for Download {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let url = url.to_owned();
        // Waited on from a thread of its own: callers are the snippet's
        // thread and the engine's blocking pool, inside a runtime or not.
        std::thread::spawn(move || {
            let (runtime, client) = http();
            runtime.block_on(get(client, &url))
        })
        .join()
        .map_err(|_| "the image download panicked".to_owned())?
    }
}

async fn get(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut reply = client.get(url).send().await.map_err(|e| format!("can't download it: {e}"))?;
    if !reply.status().is_success() {
        return Err(format!("the server answered {}", reply.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = reply.chunk().await.map_err(|e| format!("can't download it: {e}"))? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_DOWNLOAD_BYTES {
            return Err(format!("it's over {} MB", MAX_DOWNLOAD_BYTES / (1024 * 1024)));
        }
    }
    Ok(bytes)
}

/// A warning for each image the `nodes` (and their descendants) draw that
/// can't be had from `assets`, once per node and image.
pub fn warnings<'a>(nodes: impl Iterator<Item = &'a Node>, assets: &Beside) -> Vec<Warning> {
    let mut found = Vec::new();
    for node in nodes {
        render::images_in(node, &mut found);
    }
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|(node, image)| seen.insert((node.id.as_str(), image.url.as_str())))
        .filter_map(|(node, image)| {
            let why = assets.image(&image.url).err()?;
            Some(Warning {
                node: Some(node.id.clone()),
                property: None,
                message: format!(
                    "\"{}\" ({}): can't draw the image {}: {why}; a placeholder is drawn in its place",
                    node.name,
                    node.kind.name(),
                    image.url
                ),
            })
        })
        .collect()
}
