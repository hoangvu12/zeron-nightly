//! What agents each template had the last time it ran (spec #37, "Composer
//! agents"; #46): asked of a sandbox's engine when it's Ready, kept on this
//! device's disk, and what a composer is offered for that template from then
//! on, without asking (or waking) any sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::registry::HarnessDescriptor;

/// `{data}/place-catalogs.json`: `{"<provider>:<template>": PlaceCatalog}`.
const FILE: &str = "place-catalogs.json";
/// The key of a provider's default template (none named).
const DEFAULT_TEMPLATE: &str = "";

/// A template's agents and each one's models, as its sandbox listed them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceCatalog {
    pub harnesses: Vec<HarnessDescriptor>,
    /// By harness slug; an agent whose models couldn't be listed has none.
    #[serde(default)]
    pub models: BTreeMap<String, Vec<zeron_proto::Model>>,
    /// Epoch ms it was read.
    pub at: i64,
}

#[derive(Default)]
pub(super) struct Catalogs {
    path: Option<PathBuf>,
    kept: Mutex<BTreeMap<String, PlaceCatalog>>,
}

fn key(provider: &str, template: Option<&str>) -> String {
    format!("{provider}:{}", template.unwrap_or(DEFAULT_TEMPLATE))
}

impl Catalogs {
    pub(super) fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let kept = std::fs::read(&path)
            .ok()
            .and_then(|bytes| match serde_json::from_slice(&bytes) {
                Ok(kept) => Some(kept),
                Err(err) => {
                    tracing::warn!(error = %err, "sandboxes: unreadable {FILE}; starting empty");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            path: Some(path),
            kept: Mutex::new(kept),
        }
    }

    pub(super) fn get(&self, provider: &str, template: Option<&str>) -> Option<PlaceCatalog> {
        super::lock(&self.kept).get(&key(provider, template)).cloned()
    }

    pub(super) fn keep(&self, provider: &str, template: Option<&str>, catalog: PlaceCatalog) {
        let bytes = {
            let mut kept = super::lock(&self.kept);
            kept.insert(key(provider, template), catalog);
            serde_json::to_vec_pretty(&*kept)
        };
        let Some(path) = &self.path else {
            return;
        };
        let outcome = bytes.map_err(std::io::Error::other).and_then(|bytes| {
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, path)
        });
        if let Err(err) = outcome {
            tracing::warn!(error = %err, "sandboxes: couldn't save {FILE}");
        }
    }
}

/// Ask the sandbox's engine (awake: it just answered) what it has: its
/// agents, then each installed one's models. `None` when it couldn't say.
pub(super) async fn read(
    links: &std::sync::Arc<zeron_rpc::LinkCache>,
    device_id: &str,
) -> Option<PlaceCatalog> {
    use std::time::Duration;
    const LIST_TIMEOUT: Duration = Duration::from_secs(15);
    // As `ListModels`' own deadline: a CLI's first listing can be slow.
    const MODELS_TIMEOUT: Duration = Duration::from_secs(100);

    let client = links.client(device_id).await.ok()?;
    let listed = tokio::time::timeout(
        LIST_TIMEOUT,
        client.call(
            zeron_rpc::methods::LIST_HARNESSES,
            serde_json::json!({ "includeRegistry": true }),
        ),
    )
    .await
    .ok()?
    .ok()?;
    let harnesses: Vec<HarnessDescriptor> = serde_json::from_value(listed).ok()?;
    let asks = harnesses.iter().filter(|d| d.installed).map(|d| {
        let client = client.clone();
        let harness = d.id;
        async move {
            let models = tokio::time::timeout(
                MODELS_TIMEOUT,
                client.call(
                    zeron_rpc::methods::LIST_MODELS,
                    serde_json::json!({ "harness": harness }),
                ),
            )
            .await
            .ok()?
            .ok()?;
            let models: Vec<zeron_proto::Model> = serde_json::from_value(models).ok()?;
            (!models.is_empty()).then(|| (harness.as_str().to_owned(), models))
        }
    });
    let models = futures::future::join_all(asks)
        .await
        .into_iter()
        .flatten()
        .collect();
    Some(PlaceCatalog {
        harnesses,
        models,
        at: crate::now_ms(),
    })
}
