use crate::config::{Config, Page};
use anyhow::{ensure, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Mutex;
#[cfg(test)]
use std::time::Duration;

const SEARCH_URL: &str = "https://wallhaven.cc/api/v1/search";
const MAX_RESPONSE: usize = 256 * 1024;

#[derive(Deserialize)]
struct Response {
    data: Vec<Item>,
    #[serde(default)]
    meta: Meta,
}
#[derive(Deserialize)]
struct Meta {
    #[serde(default = "one")]
    last_page: u32,
}
fn one() -> u32 {
    1
}
impl Default for Meta {
    fn default() -> Self {
        Self { last_page: 1 }
    }
}
#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    id: String,
    category: String,
    purity: String,
    path: String,
    file_size: u64,
    dimension_x: u32,
    dimension_y: u32,
}
#[derive(Clone)]
struct Candidate {
    id: String,
    url: String,
}
#[derive(Default)]
pub struct State {
    ordered: Option<(String, Vec<Candidate>, u32, bool)>,
}

fn search_url(base: &str, config: &Config, page: u32) -> anyhow::Result<reqwest::Url> {
    config.validate()?;
    let mut url = reqwest::Url::parse(base)?;
    let mut query = url.query_pairs_mut();
    query.append_pair("page", &page.to_string());
    query.append_pair("categories", &config.categories_mask());
    query.append_pair("purity", &config.purity_mask());
    match config.page {
        Page::Toplist => {
            query
                .append_pair("sorting", "toplist")
                .append_pair("topRange", "1M");
        }
        Page::Hot => {
            query.append_pair("sorting", "hot");
        }
        Page::Latest => {
            query.append_pair("sorting", "date_added");
        }
    }
    query.append_pair("order", "desc");
    drop(query);
    Ok(url)
}

fn image_url(value: &str) -> bool {
    if value.len() > 4096 {
        return false;
    }
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url
                .host_str()
                .is_some_and(|host| host == "wallhaven.cc" || host.ends_with(".wallhaven.cc"))
    })
}
fn candidates(items: Vec<Item>, config: &Config) -> Vec<Candidate> {
    items
        .into_iter()
        .filter(|item| {
            let category = match item.category.as_str() {
                "general" => 0,
                "anime" => 1,
                "people" => 2,
                _ => return false,
            };
            let purity = match item.purity.as_str() {
                "sfw" => 0,
                "sketchy" => 1,
                _ => return false,
            };
            config.categories[category]
                && config.purity[purity]
                && item.file_size <= 16 * 1024 * 1024
                && (1..=8192).contains(&item.dimension_x)
                && (1..=8192).contains(&item.dimension_y)
                && image_url(&item.path)
        })
        .map(|item| Candidate {
            id: if item.id.is_empty() {
                format!("{:x}", Sha256::digest(item.path.as_bytes()))
            } else {
                item.id
            },
            url: item.path,
        })
        .collect()
}
async fn fetch(client: &reqwest::Client, url: reqwest::Url) -> anyhow::Result<Response> {
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::USER_AGENT, "Eclipse")
        .send()
        .await
        .context("Wallhaven search request failed")?
        .error_for_status()
        .context("Wallhaven search returned an error")?;
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_RESPONSE as u64),
        "Wallhaven response is too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        ensure!(
            chunk.len() <= MAX_RESPONSE.saturating_sub(bytes.len()),
            "Wallhaven response is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes).context("Wallhaven search response is invalid")?)
}

/// Freeze result order for this filter revision, extending it page by page.
pub async fn resolve_at(
    client: reqwest::Client,
    config: Config,
    state: &Mutex<State>,
    position: usize,
) -> anyhow::Result<String> {
    ordered_from(client, config, state, position, SEARCH_URL).await
}
async fn ordered_from(
    client: reqwest::Client,
    config: Config,
    state: &Mutex<State>,
    position: usize,
    base: &str,
) -> anyhow::Result<String> {
    ensure!(position < 4096, "已到壁纸列表的浏览上限");
    let revision = config.revision();
    for _ in 0..5 {
        let page = {
            let mut state = state.lock().unwrap();
            if state.ordered.as_ref().is_none_or(|v| v.0 != revision) {
                state.ordered = Some((revision.clone(), Vec::new(), 1, false));
            }
            let (_, items, page, complete) = state.ordered.as_ref().unwrap();
            if let Some(item) = items.get(position) {
                return Ok(item.url.clone());
            }
            ensure!(!complete, "已到壁纸列表末尾");
            *page
        };
        let response = fetch(&client, search_url(base, &config, page)?).await?;
        let last = response.meta.last_page.max(1);
        let items = candidates(response.data, &config);
        let mut state = state.lock().unwrap();
        let ordered = state.ordered.as_mut().context("wallpaper list retired")?;
        ensure!(ordered.0 == revision, "wallpaper filters changed");
        if ordered.2 == page {
            for item in items {
                if !ordered.1.iter().any(|old| old.id == item.id) && ordered.1.len() < 4096 {
                    ordered.1.push(item);
                }
            }
            ordered.2 = page + 1;
            ordered.3 = page >= last;
        }
    }
    let state = state.lock().unwrap();
    state
        .ordered
        .as_ref()
        .and_then(|v| v.1.get(position))
        .map(|v| v.url.clone())
        .context("未找到此位置的可用壁纸")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_browsing_preserves_api_rank_and_extends_pages() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        uu_plugin_sdk::engine::install_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (page, ids) in [(1, vec!["a", "b"]), (2, vec!["c"])] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0; 4096];
                let n = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..n]).contains(&format!("page={page}")));
                let data:Vec<_>=ids.into_iter().map(|id|serde_json::json!({"id":id,"category":"general","purity":"sfw","path":format!("https://w.wallhaven.cc/{id}.jpg"),"file_size":10,"dimension_x":100,"dimension_y":100})).collect();
                let bytes =
                    serde_json::to_vec(&serde_json::json!({"data":data,"meta":{"last_page":2}}))
                        .unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        let runtime = uu_plugin_sdk::tokio::runtime::Runtime::new().unwrap();
        let state = Mutex::new(State::default());
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = format!("http://{address}/search");
        for (position, id) in [(0, "a"), (1, "b"), (2, "c"), (1, "b")] {
            let value = runtime
                .block_on(ordered_from(
                    client.clone(),
                    Config::default(),
                    &state,
                    position,
                    &base,
                ))
                .unwrap();
            assert!(value.ends_with(&format!("/{id}.jpg")));
        }
        server.join().unwrap();
    }
    #[test]
    fn filters_and_pagination_stay_in_the_discovery_request() {
        let config = Config::default();
        let url = search_url(SEARCH_URL, &config, 3).unwrap();
        assert!(url.query_pairs().any(|(k, v)| k == "page" && v == "3"));
        assert!(url
            .query_pairs()
            .any(|(k, v)| k == "categories" && v == "111"));
        assert!(url.query_pairs().any(|(k, v)| k == "purity" && v == "100"));
        assert!(!image_url("https://wallhaven.cc.example.invalid/image.jpg"));
        assert!(!image_url("https://user@w.wallhaven.cc/image.jpg"));
    }
    #[test]
    fn unknown_purity_and_mismatched_categories_are_not_admitted() {
        let items = r#"[{"id":"a","category":"general","purity":"nsfw","path":"https://w.wallhaven.cc/a.jpg","file_size":1,"dimension_x":100,"dimension_y":100},{"id":"b","category":"anime","purity":"sfw","path":"https://w.wallhaven.cc/b.jpg","file_size":1,"dimension_x":100,"dimension_y":100}]"#;
        let mut config = Config::default();
        config.categories = [true, false, false];
        assert!(candidates(serde_json::from_str(items).unwrap(), &config).is_empty());
    }
}
