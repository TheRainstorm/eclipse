use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    Toplist,
    Hot,
    Latest,
}
impl Page {
    pub const ALL: [Self; 3] = [Self::Toplist, Self::Hot, Self::Latest];
    pub fn label(self) -> &'static str {
        match self {
            Self::Toplist => "榜单",
            Self::Hot => "热门",
            Self::Latest => "最新",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub page: Page,
    pub categories: [bool; 3],
    pub purity: [bool; 2],
}
impl Default for Config {
    fn default() -> Self {
        Self {
            page: Page::Toplist,
            categories: [true; 3],
            purity: [true, false],
        }
    }
}
impl Config {
    pub fn previous(application: &uu_plugin_sdk::engine::Config) -> Self {
        use std::io::Read;
        let read = (|| -> anyhow::Result<Self> {
            let mut bytes = Vec::new();
            std::fs::File::open(application.at("gui-settings.json"))?
                .take(65537)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() <= 65536,
                "previous wallpaper settings too large"
            );
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            let mut config = Self::default();
            if let Some(page) = value.get("wallhaven_page") {
                config.page = serde_json::from_value(page.clone())?;
            }
            if let Some(categories) = value.get("wallhaven_categories") {
                for (index, key) in ["general", "anime", "people"].into_iter().enumerate() {
                    if let Some(v) = categories.get(key).and_then(serde_json::Value::as_bool) {
                        config.categories[index] = v;
                    }
                }
            }
            if let Some(purity) = value.get("wallhaven_purity") {
                for (index, key) in ["sfw", "sketchy"].into_iter().enumerate() {
                    if let Some(v) = purity.get(key).and_then(serde_json::Value::as_bool) {
                        config.purity[index] = v;
                    }
                }
            }
            config.validate()?;
            Ok(config)
        })();
        read.unwrap_or_default()
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.categories.contains(&true) && self.purity.contains(&true),
            "{}",
            uu_plugin_sdk::i18n::text("至少选择一种 Wallhaven 分类和内容筛选")
        );
        Ok(())
    }
    pub fn revision(&self) -> String {
        serde_json::to_string(self).expect("wallpaper settings serialize")
    }
    pub fn categories_mask(&self) -> String {
        self.categories
            .map(|v| if v { '1' } else { '0' })
            .into_iter()
            .collect()
    }
    pub fn purity_mask(&self) -> String {
        format!("{}{}0", u8::from(self.purity[0]), u8::from(self.purity[1]))
    }
}
