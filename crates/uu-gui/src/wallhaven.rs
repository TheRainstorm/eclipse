//! Public Wallhaven search-to-image resolution.
//!
//! The API is only a discovery step; the image itself is still downloaded by
//! the existing bounded wallpaper worker and retains the configured source
//! identity for cache validation.

use crate::settings::{WallhavenCategories, WallhavenPage, WallhavenPurity};
use anyhow::{Context, ensure};
use serde::Deserialize;
const SEARCH_URL: &str = "https://wallhaven.cc/api/v1/search";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_IMAGE_URL: usize = 4096;
const MAX_WALLPAPER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Query {
    pub page: WallhavenPage,
    pub categories: WallhavenCategories,
    pub purity: WallhavenPurity,
}

impl Query {
    pub(super) fn identity(&self) -> String {
        format!(
            "wallhaven://{}/{}/{}",
            self.page.key(),
            self.categories.mask().unwrap_or("000"),
            self.purity.mask().unwrap_or("000")
        )
    }

    fn search_url(&self) -> anyhow::Result<reqwest::Url> {
        let categories = self
            .categories
            .mask()
            .context("Wallhaven requires at least one category")?;
        let purity = self
            .purity
            .mask()
            .context("Wallhaven requires SFW or Sketchy")?;
        let mut url = reqwest::Url::parse(SEARCH_URL)?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("page", "1");
            query.append_pair("categories", categories);
            query.append_pair("purity", purity);
            match self.page {
                WallhavenPage::Toplist => {
                    query.append_pair("sorting", "toplist");
                    query.append_pair("topRange", "1M");
                    query.append_pair("order", "desc");
                }
                WallhavenPage::Hot => {
                    // Wallhaven currently accepts this as a listing sort even
                    // though the public parameter table only documents the
                    // standard sorts. Keep the fixed public endpoint.
                    query.append_pair("sorting", "hot");
                    query.append_pair("order", "desc");
                }
                WallhavenPage::Latest => {
                    query.append_pair("sorting", "date_added");
                    query.append_pair("order", "desc");
                }
            }
        }
        Ok(url)
    }
}

pub(super) async fn resolve(client: &reqwest::Client, query: &Query) -> anyhow::Result<String> {
    let url = query.search_url()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::USER_AGENT, "Eclipse")
        .send()
        .await
        .context("Wallhaven search request failed")?
        .error_for_status()
        .context("Wallhaven search returned an error")?;
    let bytes = response_bytes(response, MAX_RESPONSE_BYTES).await?;
    let parsed: SearchResponse =
        serde_json::from_slice(&bytes).context("Wallhaven search response is invalid")?;
    select_candidate(parsed.data, query, uuid::Uuid::new_v4().as_bytes()[0])
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    category: String,
    purity: String,
    path: String,
    file_size: u64,
    dimension_x: u32,
    dimension_y: u32,
}

fn select_candidate(
    items: Vec<SearchItem>,
    query: &Query,
    selector: u8,
) -> anyhow::Result<String> {
    let candidates: Vec<_> = items
        .into_iter()
        .filter(|item| {
            category_allowed(&item.category, query.categories)
                && purity_allowed(&item.purity, query.purity)
                && item.file_size <= MAX_WALLPAPER_BYTES
                && item.dimension_x <= MAX_SOURCE_DIMENSION
                && item.dimension_y <= MAX_SOURCE_DIMENSION
                && valid_image_url(&item.path).is_ok()
        })
        .collect();
    ensure!(!candidates.is_empty(), "Wallhaven returned no matching wallpaper");
    let index = usize::from(selector) % candidates.len();
    let path = candidates[index].path.clone();
    valid_image_url(&path)?;
    Ok(path)
}

fn category_allowed(value: &str, categories: WallhavenCategories) -> bool {
    match value {
        "general" => categories.general,
        "anime" => categories.anime,
        "people" => categories.people,
        _ => false,
    }
}

fn purity_allowed(value: &str, purity: WallhavenPurity) -> bool {
    match value {
        "sfw" => purity.sfw,
        "sketchy" => purity.sketchy,
        _ => false,
    }
}

fn valid_image_url(value: &str) -> anyhow::Result<reqwest::Url> {
    ensure!(!value.is_empty() && value.len() <= MAX_IMAGE_URL, "invalid Wallhaven image URL");
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some_and(|host| host == "wallhaven.cc" || host.ends_with(".wallhaven.cc")),
        "untrusted Wallhaven image URL"
    );
    Ok(url)
}

async fn response_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        ensure!(length <= limit as u64, "Wallhaven response exceeds byte limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)
        .context("Wallhaven response interrupted")?
    {
        ensure!(
            chunk.len() <= limit.saturating_sub(bytes.len()),
            "Wallhaven response exceeds byte limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(page: WallhavenPage) -> Query {
        Query {
            page,
            categories: WallhavenCategories::default(),
            purity: WallhavenPurity::default(),
        }
    }

    #[test]
    fn query_url_uses_fixed_endpoint_and_selected_listing_filters() {
        let url = query(WallhavenPage::Toplist).search_url().unwrap();
        assert_eq!(url.host_str(), Some("wallhaven.cc"));
        assert_eq!(url.path(), "/api/v1/search");
        let pairs: Vec<_> = url.query_pairs().collect();
        assert!(pairs.iter().any(|(k, v)| k == "sorting" && v == "toplist"));
        assert!(pairs.iter().any(|(k, v)| k == "topRange" && v == "1M"));
        assert!(pairs.iter().any(|(k, v)| k == "categories" && v == "111"));
        assert!(pairs.iter().any(|(k, v)| k == "purity" && v == "100"));
        let latest = query(WallhavenPage::Latest).search_url().unwrap();
        assert!(latest.query_pairs().any(|(k, v)| k == "sorting" && v == "date_added"));
    }

    #[test]
    fn candidate_selection_filters_category_purity_and_untrusted_hosts() {
        let mut allowed = query(WallhavenPage::Hot);
        allowed.categories = WallhavenCategories {
            general: true,
            anime: false,
            people: false,
        };
        allowed.purity = WallhavenPurity {
            sfw: true,
            sketchy: false,
        };
        let items = vec![
            SearchItem {
                category: "anime".into(),
                purity: "sfw".into(),
                path: "https://w.wallhaven.cc/full/an/wallhaven-anime.jpg".into(),
                file_size: 1024,
                dimension_x: 1920,
                dimension_y: 1080,
            },
            SearchItem {
                category: "general".into(),
                purity: "sketchy".into(),
                path: "https://w.wallhaven.cc/full/sk/wallhaven-sketchy.jpg".into(),
                file_size: 1024,
                dimension_x: 1920,
                dimension_y: 1080,
            },
            SearchItem {
                category: "general".into(),
                purity: "sfw".into(),
                path: "https://evil.example.invalid/wallhaven.jpg".into(),
                file_size: 1024,
                dimension_x: 1920,
                dimension_y: 1080,
            },
            SearchItem {
                category: "general".into(),
                purity: "sfw".into(),
                path: "https://w.wallhaven.cc/full/ok/wallhaven-ok.jpg".into(),
                file_size: 1024,
                dimension_x: 1920,
                dimension_y: 1080,
            },
        ];
        assert_eq!(
            select_candidate(items, &allowed, 0).unwrap(),
            "https://w.wallhaven.cc/full/ok/wallhaven-ok.jpg"
        );
        assert!(select_candidate(Vec::new(), &allowed, 0).is_err());
    }

    #[tokio::test]
    #[ignore = "requires the public Wallhaven API"]
    async fn public_api_resolves_a_real_filtered_wallpaper() {
        let client=reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap();
        let path=resolve(&client,&query(WallhavenPage::Toplist)).await.unwrap();
        let url=valid_image_url(&path).unwrap();
        assert_eq!(url.host_str(),Some("w.wallhaven.cc"));
    }

    #[test]
    fn identity_changes_with_page_and_filters() {
        let top = query(WallhavenPage::Toplist).identity();
        let latest = query(WallhavenPage::Latest).identity();
        assert_ne!(top, latest);
        let mut changed = query(WallhavenPage::Toplist);
        changed.purity.sketchy = true;
        assert_ne!(top, changed.identity());
    }
}
