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
pub(super) async fn resolve(client: reqwest::Client) -> Result<String> {
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
}
#[cfg(test)]
mod tests {
    use super::*;
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
