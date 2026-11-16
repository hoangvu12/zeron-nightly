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
                    tracing::warn!(error = %err, file = FILE, "sandboxes: unreadable; starting empty");
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
        super::lock(&self.kept)
            .get(&key(provider, template))
            .cloned()
    }

    pub(super) fn keep(&self, provider: &str, template: Option<&str>, catalog: PlaceCatalog) {
        super::lock(&self.kept).insert(key(provider, template), catalog);
        self.save();
    }

    /// Every template of `provider`'s, by its key (to hand over).
    pub(super) fn of_provider(&self, provider: &str) -> BTreeMap<String, PlaceCatalog> {
        let prefix = key(provider, Some(""));
        super::lock(&self.kept)
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(key, catalog)| (key.clone(), catalog.clone()))
            .collect()
    }

    /// Take `provider`'s catalogs another device kept (handed over): each
    /// replaces one read before it here. Other providers' are left alone.
    pub(super) fn take(&self, provider: &str, taken: BTreeMap<String, PlaceCatalog>) {
        let prefix = key(provider, Some(""));
        {
            let mut kept = super::lock(&self.kept);
            for (key, catalog) in taken {
                if !key.starts_with(&prefix) {
                    continue;
                }
                if kept.get(&key).is_none_or(|here| here.at <= catalog.at) {
                    kept.insert(key, catalog);
                }
            }
        }
        self.save();
    }

    fn save(&self) {
        let bytes = serde_json::to_vec_pretty(&*super::lock(&self.kept));
        let Some(path) = &self.path else {
            return;
        };
        let outcome = bytes.map_err(|e| e.to_string()).and_then(|bytes| {
            crate::runner::write_private_atomic(path, &bytes).map_err(|e| e.to_string())
        });
        if let Err(err) = outcome {
            tracing::warn!(error = %err, file = FILE, "sandboxes: couldn't save");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_template_had_is_kept_across_a_restart_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        Catalogs::open(dir.path()).keep("docker", Some("dev"), PlaceCatalog::default());
        let again = Catalogs::open(dir.path());
        assert!(again.get("docker", Some("dev")).is_some());
        assert!(again.get("docker", None).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
