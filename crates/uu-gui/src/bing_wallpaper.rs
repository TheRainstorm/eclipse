//! Built-in Bing daily-image metadata resolution; image I/O stays in wallpapers.
use anyhow::{ensure, Context as _, Result};
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
async fn fetch(client: reqwest::Client, idx: usize) -> Result<Vec<String>> {
    let mut response = client
        .get(ENDPOINT.replace("idx=0&n=1", &format!("idx={idx}&n=8")))
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
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let images = value
        .get("images")
        .and_then(|v| v.as_array())
        .context("Bing returned no image list")?;
    images
        .iter()
        .map(|item| image_url(&serde_json::to_vec(&serde_json::json!({"images":[item]}))?))
        .collect()
}
#[derive(Default)]
pub(super) struct Catalog(std::sync::Mutex<(Vec<String>, bool)>);
impl Catalog {
    pub fn contains_or_can_extend(&self, position: usize) -> bool {
        let state = self.0.lock().unwrap();
        position < 15 && (position < state.0.len() || !state.1)
    }

    pub async fn resolve(&self, client: reqwest::Client, position: usize) -> Result<String> {
        ensure!(position < 15, "已到必应可用历史的末尾");
        if self.0.lock().unwrap().0.is_empty() {
            let items = fetch(client.clone(), 0).await?;
            let mut state = self.0.lock().unwrap();
            if state.0.is_empty() {
                state.0 = items;
            }
        }
        let older = {
            let state = self.0.lock().unwrap();
            position >= state.0.len() && !state.1
        };
        if older {
            let items = fetch(client, 7).await?;
            let mut state = self.0.lock().unwrap();
            for item in items {
                if !state.0.contains(&item) {
                    state.0.push(item);
                }
            }
            state.1 = true;
        }
        self.0
            .lock()
            .unwrap()
            .0
            .get(position)
            .cloned()
            .context("已到必应可用历史的末尾")
    }
}
#[cfg(test)]
async fn resolve(client: reqwest::Client) -> Result<String> {
    Catalog::default().resolve(client, 0).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_positions_are_stable_and_end_without_wrapping() {
        let catalog = Catalog(std::sync::Mutex::new((
            (0..15)
                .map(|i| format!("https://cn.bing.com/th?id={i}"))
                .collect(),
            true,
        )));
        assert!(!catalog.contains_or_can_extend(15));
        assert!(catalog.contains_or_can_extend(14));
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let client = reqwest::Client::new();
            for p in [0, 14, 3, 0] {
                assert_eq!(
                    catalog.resolve(client.clone(), p).await.unwrap(),
                    format!("https://cn.bing.com/th?id={p}")
                );
            }
            assert!(catalog.resolve(client, 15).await.is_err());
        });
    }
    #[test]
    #[ignore = "requires the live Bing homepage and image endpoint"]
    fn live_daily_metadata_resolves_to_a_decodable_image() {
        uu_core::install_crypto_provider();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap();
            let url = resolve(client.clone()).await.unwrap();
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
            let image = image::load_from_memory(&bytes).unwrap();
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
