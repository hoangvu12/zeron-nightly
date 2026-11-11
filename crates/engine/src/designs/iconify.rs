//! Iconify's API, where Designs' icons come from on a cache miss.

use std::time::Duration;

use zeron_design::icons::IconSource;

const API: &str = "https://api.iconify.design";

/// Short, as an icon is fetched inside a snippet's time limit.
const TIMEOUT: Duration = Duration::from_secs(4);

/// Fetches `<set>/<name>.svg` from Iconify's API.
pub struct Iconify;

impl IconSource for Iconify {
    fn fetch(&self, set: &str, name: &str) -> Result<Option<String>, String> {
        let url = format!("{API}/{set}/{name}.svg");
        // On a thread of its own: callers are the snippet's thread and the
        // engine's blocking pool, with and without a runtime of their own.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(get(&url))
        })
        .join()
        .map_err(|_| "the icon fetch panicked".to_owned())?
    }
}

async fn get(url: &str) -> Result<Option<String>, String> {
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let reply = http.get(url).send().await.map_err(|e| e.to_string())?;
    if reply.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let reply = reply.error_for_status().map_err(|e| e.to_string())?;
    reply.text().await.map(Some).map_err(|e| e.to_string())
}
