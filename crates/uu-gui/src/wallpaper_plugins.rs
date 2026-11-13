//! Selection and discovery of enabled wallpaper extensions.
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uu_plugin_sdk::wallpaper::{Provider, Source};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Selection {
    pub plugin: String,
    pub source: String,
}
impl Selection {
    pub fn identity(&self) -> String {
        format!("plugin://{}/{}", self.plugin, self.source)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            [&self.plugin, &self.source]
                .into_iter()
                .all(|id| !id.is_empty()
                    && id.len() <= 64
                    && id.bytes().all(|b| b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b == b'-'
                        || b == b'_')),
            "invalid wallpaper plugin selection"
        );
        Ok(())
    }
}
#[derive(Clone)]
pub(super) struct Available {
    pub selection: Selection,
    pub source: Source,
    pub provider: Arc<dyn Provider>,
}

impl super::plugin_ui::Manager {
    pub fn wallpapers(&self) -> Vec<Available> {
        self.active_plugins()
            .into_iter()
            .flat_map(|(id, loaded)| {
                let Some(provider) = loaded.plugin.wallpaper_provider() else {
                    return Vec::new();
                };
                provider
                    .sources()
                    .into_iter()
                    .filter_map(|source| {
                        let selection = Selection {
                            plugin: id.clone(),
                            source: source.id.clone(),
                        };
                        (selection.validate().is_ok()
                            && !source.name.is_empty()
                            && source.name.len() <= 256
                            && source.revision.len() <= 4096)
                            .then(|| Available {
                                selection,
                                source,
                                provider: provider.clone(),
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}
