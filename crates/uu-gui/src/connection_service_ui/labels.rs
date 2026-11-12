//! Local aliases label an endpoint; invitation names continue to label its group.
use super::received;
use eframe::egui;
use std::{collections::HashMap, sync::mpsc};
use uu_core::Config;

pub(crate) type Names = HashMap<String, String>;
pub(crate) fn address_key(address: &str) -> String {
    url::Url::parse(address)
        .ok()
        .and_then(|url| {
            let group =
                uu_core::internet::server::groups::query_group(url.query().unwrap_or("")).ok()?;
            uu_core::internet::enrollment::group_url(address, &group).ok()
        })
        .unwrap_or_else(|| address.trim().to_owned())
}
pub(crate) fn display_name(names: &Names, address: &str) -> String {
    if let Some(alias) = names
        .get(&address_key(address))
        .filter(|name| !name.trim().is_empty())
    {
        return alias.clone();
    }
    let parsed = url::Url::parse(address).or_else(|_| url::Url::parse(&format!("wss://{address}")));
    if let Ok(url) = parsed {
        if let Some(host) = url.host() {
            let host = match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            };
            return match uu_core::internet::server::groups::query_group(url.query().unwrap_or("")) {
                Ok(group) => format!("{host} · {}", &group.to_string()[..8]),
                Err(_) => host,
            };
        }
    }
    uu_i18n::text("连接服务器").into()
}
/// A route and an entrance describe the same configured service. Keep their
/// presentation independent of the runtime endpoint's identity and group name.
pub(crate) fn route_name(names: &Names, address: &str, group_name: &str) -> String {
    let parsed = url::Url::parse(address)
        .or_else(|_| url::Url::parse(&format!("wss://{address}")));
    let location = parsed.as_ref().ok().and_then(|url| {
        let host = url.host()?;
        Some(match url.port() {
            Some(port) => format!("{}://{host}:{port}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        })
    }).unwrap_or_else(|| uu_i18n::text("连接服务器").into());
    let group = if group_name.trim().is_empty() {
        parsed.as_ref().ok().and_then(|url| {
            uu_core::internet::server::groups::query_group(url.query().unwrap_or("")).ok()
        }).map(|group| group.to_string()[..8].to_owned()).unwrap_or_default()
    } else { group_name.trim().to_owned() };
    let prefix = uu_i18n::text("自建服务器");
    match names.get(&address_key(address)).filter(|alias| !alias.trim().is_empty()) {
        Some(alias) if !group.is_empty() => format!("{prefix} · {alias} ({location} | {group})"),
        Some(alias) => format!("{prefix} · {alias} ({location})"),
        None if !group.is_empty() => format!("{prefix} · {location} ({group})"),
        None => format!("{prefix} · {location}"),
    }
}
pub(crate) fn load_names(config: &Config) -> anyhow::Result<Names> {
    match crate::settings::read_bounded(&config.at("connection-service-aliases.json"), 64 * 1024) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Names::new()),
        Err(error) => Err(error.into()),
    }
}
#[derive(Default)]
pub(crate) struct Labels {
    names: Names,
    loaded: bool,
    pending: Option<mpsc::Receiver<(bool, Result<Names, String>)>>,
    next_read: Option<std::time::Instant>,
    changed: bool,
    saved_generation: u64,
    save_failed: bool,
    pub(crate) error: String,
}
impl Labels {
    pub(crate) fn poll(&mut self, ctx: &egui::Context, config: Option<&Config>) {
        if let Some(result) = received(&self.pending) {
            self.pending = None;
            let loaded = self.loaded;
            self.loaded = true;
            match result {
                Ok((save, Ok(names))) => {
                    self.changed |= loaded && self.names != names;
                    self.names = names;
                    if save {
                        self.saved_generation += 1;
                        self.save_failed = false;
                    }
                    if !self.save_failed {
                        self.error.clear();
                    }
                }
                Ok((save, Err(error))) => {
                    self.error = error;
                    self.save_failed |= save;
                }
                Err(error) => self.error = error,
            }
            self.next_read = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        }
        if self.pending.is_none()
            && self
                .next_read
                .is_none_or(|at| std::time::Instant::now() >= at)
        {
            if let Some(config) = config.cloned() {
                let context = ctx.clone();
                let (send, receive) = mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let _ = send.send((false, load_names(&config).map_err(|e| e.to_string())));
                    context.request_repaint();
                });
                self.pending = Some(receive);
            }
        }
        if self.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs(5));
        }
    }
    pub(crate) fn names(&self) -> &Names {
        &self.names
    }
    pub(crate) fn display_name(&self, address: &str) -> String {
        display_name(&self.names, address)
    }
    pub(crate) fn alias(&self, address: &str) -> &str {
        self.names
            .get(&address_key(address))
            .map(String::as_str)
            .unwrap_or("")
    }
    pub(crate) fn ready(&self) -> bool {
        self.loaded && self.pending.is_none()
    }
    pub(crate) fn saved_generation(&self) -> u64 {
        self.saved_generation
    }
    pub(crate) fn clear_error(&mut self) {
        self.error.clear();
        self.save_failed = false;
    }
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
    pub(crate) fn refresh(&mut self) {
        self.next_read = None;
    }
    pub(crate) fn save(
        &mut self,
        ctx: &egui::Context,
        config: &Config,
        address: String,
        alias: String,
    ) {
        if !self.ready() {
            return;
        }
        let config = config.clone();
        let context = ctx.clone();
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Names> {
                anyhow::ensure!(
                    alias.chars().count() <= 64 && !alias.chars().any(char::is_control),
                    "{}",
                    uu_i18n::text("别名最多64个字符")
                );
                std::fs::create_dir_all(&config.data_dir)?;
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true).create(true).truncate(false);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let lock = options.open(config.at("connection-service-aliases.lock"))?;
                fs2::FileExt::lock_exclusive(&lock)?;
                let mut names = load_names(&config)?;
                let key = address_key(&address);
                if alias.trim().is_empty() {
                    names.remove(&key);
                } else {
                    names.insert(key, alias.trim().into());
                }
                crate::settings::save_json(&names, &config.at("connection-service-aliases.json"))?;
                Ok(names)
            })()
            .map_err(|e| e.to_string());
            let _ = send.send((true, result));
            context.request_repaint();
        });
        self.pending = Some(receive);
    }
    #[cfg(test)]
    pub(crate) fn fixture(names: Names) -> Self {
        Self {
            names,
            loaded: true,
            next_read: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_context_uses_group_identity_and_hides_url_credentials_and_query() {
        let group = uuid::Uuid::new_v4();
        let address = uu_core::internet::enrollment::group_url(
            "wss://signal.example:9443/v1/signal?extra=private-context", &group,
        ).unwrap();
        let mut names = Names::new();
        names.insert(address_key(&address), "家庭服务器".into());
        assert_eq!(route_name(&names, &address, "我的设备"), "自建服务器 · 家庭服务器 (wss://signal.example:9443 | 我的设备)");
        assert_eq!(route_name(&Names::new(), &address, "我的设备"), "自建服务器 · wss://signal.example:9443 (我的设备)");
        let other = uu_core::internet::enrollment::group_url("wss://signal.example:9443/v1/signal", &uuid::Uuid::new_v4()).unwrap();
        assert!(!route_name(&names, &other, "其他设备").contains("家庭服务器"));
        let ipv6 = route_name(&Names::new(), "wss://user:secret@[2001:db8::1]:9443/v1/signal?extra=hidden", "工作设备");
        assert_eq!(ipv6, "自建服务器 · wss://[2001:db8::1]:9443 (工作设备)");
    }
    fn settled(labels: &mut Labels, config: &Config, ctx: &egui::Context) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            labels.poll(ctx, Some(config));
            if labels.ready() {
                break;
            }
            assert!(
                std::time::Instant::now() < until,
                "alias worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn aliases_refresh_between_windows_and_preserve_other_groups() {
        let root =
            std::env::temp_dir().join(format!("eclipse-server-aliases-{}", uuid::Uuid::new_v4()));
        let config = Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        };
        let endpoint = |group| {
            uu_core::internet::enrollment::group_url("wss://203.0.113.10:9443/v1/signal", &group)
                .unwrap()
        };
        let a = endpoint(uuid::Uuid::new_v4());
        let b = endpoint(uuid::Uuid::new_v4());
        let ctx = egui::Context::default();
        let mut first = Labels::default();
        let mut second = Labels::default();
        settled(&mut first, &config, &ctx);
        assert!(!first.take_changed());
        first.save(&ctx, &config, a.clone(), "家庭服务器".into());
        settled(&mut first, &config, &ctx);
        assert_eq!(first.display_name(&a), "家庭服务器");
        settled(&mut second, &config, &ctx);
        second.save(&ctx, &config, b.clone(), "工作服务器".into());
        settled(&mut second, &config, &ctx);
        first.next_read = None;
        first.poll(&ctx, Some(&config));
        settled(&mut first, &config, &ctx);
        assert_eq!(first.display_name(&b), "工作服务器");
        assert_eq!(first.display_name(&a), "家庭服务器");
        assert!(first.take_changed());
        let original_generation = first.saved_generation();
        let path = config.at("connection-service-aliases.json");
        std::fs::write(&path, b"invalid fixture JSON").unwrap();
        first.save(&ctx, &config, a, "new unsaved alias".into());
        settled(&mut first, &config, &ctx);
        assert_eq!(first.saved_generation(), original_generation);
        assert!(!first.error.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid fixture JSON");
        std::fs::remove_dir_all(root).unwrap();
    }
}
