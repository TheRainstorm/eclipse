//! Resolve the current Bing homepage image; the host owns image I/O and cache.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use uu_plugin_sdk::{
    anyhow::{ensure, Context as _, Result},
    wallpaper, Context, Plugin,
};
const ENDPOINT: &str = "https://cn.bing.com/HPImageArchive.aspx?format=js&idx=0&n=1&mkt=zh-CN";
const LIMIT: usize = 64 * 1024;
fn image_url(bytes: &[u8]) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("invalid Bing wallpaper metadata")?;
    let path = value
        .get("images")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("url"))
        .and_then(|v| v.as_str())
        .context("Bing returned no daily image")?;
    ensure!(path.len() <= 4096, "Bing image URL is too large");
    let resolved = reqwest::Url::parse("https://cn.bing.com")?.join(path)?;
    ensure!(
        resolved.scheme() == "https"
            && resolved.username().is_empty()
            && resolved.password().is_none()
            && resolved
                .host_str()
                .is_some_and(|host| host == "bing.com" || host.ends_with(".bing.com")),
        "unexpected Bing image origin"
    );
    Ok(resolved.into())
}
struct Source {
    enabled: AtomicBool,
}
impl wallpaper::Provider for Source {
    fn sources(&self) -> Vec<wallpaper::Source> {
        vec![wallpaper::Source {
            id: "daily".into(),
            name: "必应每日壁纸".into(),
            revision: "zh-CN:1".into(),
        }]
    }
    fn resolve<'a>(
        &'a self,
        client: wallpaper::Client,
        source: &'a str,
    ) -> wallpaper::Resolution<'a> {
        Box::pin(async move {
            ensure!(
                self.enabled.load(Ordering::Acquire),
                "Bing wallpaper plugin is disabled"
            );
            ensure!(source == "daily", "unknown Bing wallpaper source");
            let mut response = client
                .get(ENDPOINT)
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
                .context("Bing metadata request failed")?
                .error_for_status()
                .context("Bing metadata returned an error")?;
            ensure!(
                response.content_length().is_none_or(|n| n <= LIMIT as u64),
                "Bing metadata is too large"
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    chunk.len() <= LIMIT.saturating_sub(bytes.len()),
                    "Bing metadata is too large"
                );
                bytes.extend_from_slice(&chunk);
            }
            image_url(&bytes)
        })
    }
}
struct Extension(Arc<Source>);
impl Plugin for Extension {
    fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.0.enabled.store(enabled, Ordering::Release);
        Ok(())
    }
    fn wallpaper_provider(&self) -> Option<Arc<dyn wallpaper::Provider>> {
        Some(self.0.clone())
    }
}
fn create(context: Context) -> Result<Box<dyn Plugin>> {
    ensure!(
        context.id == "wallpaper-bing",
        "Bing plugin manifest ID must be wallpaper-bing"
    );
    Ok(Box::new(Extension(Arc::new(Source {
        enabled: AtomicBool::new(false),
    }))))
}
#[no_mangle]
pub extern "C" fn eclipse_plugin_entry_v1() -> *const uu_plugin_sdk::Entry {
    static ENTRY: uu_plugin_sdk::Entry = uu_plugin_sdk::entry(create);
    &ENTRY
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the live Bing homepage and image endpoint"]
    fn live_daily_metadata_resolves_to_a_decodable_image() {
        uu_plugin_sdk::engine::install_crypto_provider();
        uu_plugin_sdk::tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async {
                let source = Source {
                    enabled: AtomicBool::new(true),
                };
                let client = wallpaper::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap();
                let url = wallpaper::Provider::resolve(&source, client.clone(), "daily")
                    .await
                    .unwrap();
                let bytes = client
                    .get(url)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap();
                assert!(bytes.len() <= 16 * 1024 * 1024);
                let image = uu_plugin_sdk::image::load_from_memory(&bytes).unwrap();
                assert!(image.width() > 0 && image.height() > 0);
            });
    }
    #[test]
    fn resolves_relative_daily_image() {
        assert_eq!(
            image_url(br#"{"images":[{"url":"/th?id=OHR.Example_1920x1080.jpg"}]}"#).unwrap(),
            "https://cn.bing.com/th?id=OHR.Example_1920x1080.jpg"
        );
    }
    #[test]
    fn rejects_empty_invalid_and_foreign_metadata() {
        for data in [
            r#"{"images":[]}"#,
            r#"{"images":[{"url":"https://bing.com.example.invalid/picture"}]}"#,
            r#"{"images":[{"url":"http://cn.bing.com/picture"}]}"#,
            "not JSON",
        ] {
            assert!(image_url(data.as_bytes()).is_err());
        }
    }
}
