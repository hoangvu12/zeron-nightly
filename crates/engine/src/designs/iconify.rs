//! Iconify's API, where Designs' icons come from on a cache miss.

use std::sync::OnceLock;
use std::time::Duration;

use zeron_design::icons::IconSource;

const API: &str = "https://api.iconify.design";

/// One fetch's longest; a call's fetches together stop at the snippet's
/// `Limits::fetching`, off its own time limit.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Fetches `<set>/<name>.svg` from Iconify's API.
pub struct Iconify;

/// One runtime and client for every fetch, so connections are reused.
/// Static, so they're never dropped inside the engine's own runtime.
fn http() -> &'static (tokio::runtime::Runtime, reqwest::Client) {
    static HTTP: OnceLock<(tokio::runtime::Runtime, reqwest::Client)> = OnceLock::new();
    HTTP.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("design-icons")
            .enable_all()
            .build()
            .expect("the icon fetcher's runtime starts");
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("the icon fetcher's client builds");
        (runtime, client)
    })
}

impl IconSource for Iconify {
    fn fetch(&self, set: &str, name: &str) -> Result<Option<String>, String> {
        let url = format!("{API}/{set}/{name}.svg");
        // Waited on from a thread of its own: callers are the snippet's
        // thread and the engine's blocking pool, inside a runtime or not.
        std::thread::spawn(move || {
            let (runtime, client) = http();
            runtime.block_on(get(client, &url))
        })
        .join()
        .map_err(|_| "the icon fetch panicked".to_owned())?
    }
}

async fn get(client: &reqwest::Client, url: &str) -> Result<Option<String>, String> {
    let reply = client.get(url).send().await.map_err(|e| e.to_string())?;
    if reply.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let reply = reply.error_for_status().map_err(|e| e.to_string())?;
    reply.text().await.map(Some).map_err(|e| e.to_string())
}
