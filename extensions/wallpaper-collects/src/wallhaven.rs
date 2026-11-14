use crate::{
    config::{Config, Page},
    history::History,
};
use anyhow::{ensure, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

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
struct Cached {
    revision: String,
    fetched: Instant,
    next_page: u32,
    complete: bool,
    candidates: Vec<Candidate>,
}
pub struct State {
    history: History,
    cached: Option<Cached>,
}
impl State {
    pub fn new(history: History) -> Self {
        Self {
            history,
            cached: None,
        }
    }
    fn cached_fresh(&self, revision: &str) -> Option<&Cached> {
        self.cached.as_ref().filter(|cache| {
            cache.revision == revision && cache.fetched.elapsed() < Duration::from_secs(300)
        })
    }
    fn pick(&mut self, storage: &uu_plugin_sdk::engine::Config) -> anyhow::Result<String> {
        let candidates = &self
            .cached
            .as_ref()
            .context("Wallhaven candidates unavailable")?
            .candidates;
        let ids: Vec<_> = candidates.iter().map(|item| item.id.clone()).collect();
        let id = self
            .history
            .choose(&ids, rand::random::<u32>() as usize)
            .context("Wallhaven returned no candidate")?
            .to_owned();
        let image = candidates
            .iter()
            .find(|item| item.id == id)
            .context("Wallhaven candidate disappeared")?
            .url
            .clone();
        self.history.remember(storage, &id)?;
        Ok(image)
    }
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

pub async fn resolve(
    client: reqwest::Client,
    config: Config,
    storage: &uu_plugin_sdk::engine::Config,
    state: &Mutex<State>,
) -> anyhow::Result<String> {
    resolve_from(client, config, storage, state, SEARCH_URL).await
}
async fn resolve_from(
    client: reqwest::Client,
    config: Config,
    storage: &uu_plugin_sdk::engine::Config,
    state: &Mutex<State>,
    base: &str,
) -> anyhow::Result<String> {
    let revision = config.revision();
    let first_page = {
        let mut state = state.lock().unwrap();
        if let Some(cache) = state.cached_fresh(&revision) {
            if cache.complete
                || cache
                    .candidates
                    .iter()
                    .any(|item| state.history.unseen(&item.id))
            {
                return state.pick(storage);
            }
            cache.next_page
        } else {
            1
        }
    };
    let mut available = Vec::new();
    let mut last_page = first_page;
    let mut fetched_page = first_page;
    // A hundred recent IDs can exhaust four normal pages. Five pages bound
    // requests while still giving refreshes a substantially larger new pool.
    for page in first_page..first_page.saturating_add(5) {
        if page > last_page {
            break;
        }
        let response = fetch(&client, search_url(base, &config, page)?).await?;
        last_page = response.meta.last_page.max(1);
        fetched_page = page;
        available.extend(candidates(response.data, &config));
        let state = state.lock().unwrap();
        if available.iter().any(|item| state.history.unseen(&item.id)) {
            break;
        }
    }
    ensure!(
        !available.is_empty(),
        "Wallhaven returned no matching wallpaper"
    );
    let mut state = state.lock().unwrap();
    state.cached = Some(Cached {
        revision,
        fetched: Instant::now(),
        next_page: if fetched_page >= last_page {
            1
        } else {
            fetched_page + 1
        },
        complete: first_page == 1 && fetched_page >= last_page,
        candidates: available,
    });
    // Reserve before returning to the downloader, including failed image URLs.
    // A broken candidate must not be selected again on every manual refresh.
    state.pick(storage)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn an_exhausted_first_page_fetches_new_candidates_and_remembers_them() {
        use std::{
            io::{BufRead, Write},
            net::TcpListener,
        };
        uu_plugin_sdk::engine::install_crypto_provider();
        let directory =
            std::env::temp_dir().join(format!("eclipse-wallpaper-pages-{}", rand::random::<u64>()));
        let storage = uu_plugin_sdk::engine::Config {
            data_dir: directory.clone(),
            state_dir: directory.join("state"),
            signaling_host: String::new(),
        };
        let history = Mutex::new(State::new(History::default()));
        history
            .lock()
            .unwrap()
            .history
            .remember(&storage, "first")
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (page, id) in [(1, "first"), (2, "second")] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                assert!(
                    first.contains(&format!("page={page}")),
                    "unexpected request: {first}"
                );
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let bytes = serde_json::to_vec(&serde_json::json!({"data":[{"id":id,"category":"general","purity":"sfw","path":format!("https://w.wallhaven.cc/{id}.jpg"),"file_size":100,"dimension_x":100,"dimension_y":100}],"meta":{"last_page":2}})).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        let runtime = uu_plugin_sdk::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let base = format!("http://{address}/search");
        let url = runtime
            .block_on(resolve_from(
                client.clone(),
                Config::default(),
                &storage,
                &history,
                &base,
            ))
            .unwrap();
        assert_eq!(url, "https://w.wallhaven.cc/second.jpg");
        assert!(!History::read(&storage.at("wallhaven-history.json"))
            .unwrap()
            .unseen("second"));
        server.join().unwrap();
        // The complete two-item pool rotates from memory. The mock server is
        // already closed, so any redundant API request would fail this test.
        for expected in ["first", "second", "first"] {
            let url = runtime
                .block_on(resolve_from(
                    client.clone(),
                    Config::default(),
                    &storage,
                    &history,
                    &base,
                ))
                .unwrap();
            assert_eq!(url, format!("https://w.wallhaven.cc/{expected}.jpg"));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
