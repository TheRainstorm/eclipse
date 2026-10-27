//! Account-scoped local navigation; never stores login tokens or device names.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    io::Read,
    path::{Path, PathBuf},
};

const MAX_RECENT: usize = 20;
const MAX_FAVORITES: usize = 256;
const MAX_FILE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Filter {
    #[default]
    All,
    Recent,
    Favorites,
}
impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => uu_i18n::text("全部设备"),
            Self::Recent => uu_i18n::text("最近使用"),
            Self::Favorites => uu_i18n::text("收藏设备"),
        }
    }
    pub fn empty_message(self) -> &'static str {
        match self {
            Self::All => uu_i18n::text("没有匹配的设备"),
            Self::Recent => uu_i18n::text("还没有最近使用的设备"),
            Self::Favorites => uu_i18n::text("还没有收藏设备"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    version: u32,
    favorites: BTreeSet<String>,
    recent: Vec<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            favorites: BTreeSet::new(),
            recent: Vec::new(),
        }
    }
}
impl Preferences {
    fn load(path: &Path) -> anyhow::Result<Self> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file() && metadata.len() <= MAX_FILE_BYTES,
            "invalid device preferences size"
        );
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_FILE_BYTES,
            "device preferences exceeded read limit"
        );
        let mut value: Self =
            serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))?;
        anyhow::ensure!(value.version == 1, "unsupported device preferences version");
        value.favorites.retain(|id| valid_id(id));
        let mut seen = HashSet::new();
        value
            .recent
            .retain(|id| valid_id(id) && seen.insert(id.clone()));
        value.recent.truncate(MAX_RECENT);
        Ok(value)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

fn account_namespace(user_id: Option<&str>, token: &str) -> String {
    let mut hash = Sha256::new();
    if let Some(id) = user_id.filter(|id| !id.is_empty()) {
        hash.update(b"uu-client-device-account-v1\0");
        hash.update(id.as_bytes());
    } else {
        hash.update(b"uu-client-device-session-v1\0");
        hash.update(token.as_bytes());
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Default)]
pub(super) struct Library {
    preferences: Preferences,
    path: Option<PathBuf>,
    writable: bool,
    notice: Option<String>,
}

impl Library {
    pub fn for_account(config: Option<&uu_core::Config>, token: Option<&str>) -> Self {
        let (Some(config), Some(token)) = (config, token.filter(|token| !token.is_empty())) else {
            return Self::default();
        };
        let state = uu_core::ClientState::read(&config.at("state.json")).ok().flatten();
        let owner = state.as_ref().filter(|state|state.token.as_deref() == Some(token)).and_then(|state| {
            state
                .user_id
                .as_deref()
                .or(state.identity.user_id.as_deref())
        });
        Self::from_path(config.at(format!(
            "device-list.{}.json",
            account_namespace(owner, token)
        )))
    }

    fn from_path(path: PathBuf) -> Self {
        match Preferences::load(&path) {
            Ok(preferences) => Self {
                preferences,
                path: Some(path),
                writable: true,
                notice: None,
            },
            Err(_) => Self {
                path: Some(path),
                notice: Some(uu_i18n::text("本地收藏读取失败；原文件已保留，当前更改仅本次有效。").into()),
                ..Self::default()
            },
        }
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn is_favorite(&self, id: &str) -> bool {
        self.preferences.favorites.contains(id)
    }

    pub fn toggle_favorite(&mut self, id: &str) {
        if !valid_id(id) {
            return;
        }
        let desired = !self.is_favorite(id);
        self.refresh_before_change();
        if desired && !self.is_favorite(id) {
            if self.preferences.favorites.len() >= MAX_FAVORITES {
                self.notice = Some(uu_i18n::text("收藏数量已达上限，请先取消部分收藏。").into());
                return;
            }
            self.preferences.favorites.insert(id.to_owned());
        } else if !desired {
            self.preferences.favorites.remove(id);
        }
        self.persist();
    }

    pub fn record_opened(&mut self, id: &str) {
        if !valid_id(id) {
            return;
        }
        self.refresh_before_change();
        self.preferences.recent.retain(|old| old != id);
        self.preferences.recent.insert(0, id.to_owned());
        self.preferences.recent.truncate(MAX_RECENT);
        self.persist();
    }

    fn refresh_before_change(&mut self) {
        if !self.writable {
            return;
        }
        if let Some(path) = &self.path {
            match Preferences::load(path) {
                Ok(preferences) => self.preferences = preferences,
                Err(_) => {
                    self.writable = false;
                    self.notice =
                        Some(uu_i18n::text("本地收藏读取失败；原文件已保留，当前更改仅本次有效。").into());
                }
            }
        }
    }

    fn persist(&mut self) {
        let Some(path) = &self.path else { return };
        if !self.writable {
            return;
        }
        self.notice = match super::settings::save_json(&self.preferences, path) {
            Ok(()) => None,
            Err(_) => {
                self.writable = false;
                Some(uu_i18n::text("本地收藏未能保存，当前更改仅本次有效。").into())
            }
        };
    }

    pub fn visible_indices(
        &self,
        devices: &[super::Device],
        filter: Filter,
        query: &str,
    ) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let mut result = devices
            .iter()
            .enumerate()
            .filter(|(_, device)| {
                let allowed = match filter {
                    Filter::All => true,
                    Filter::Recent => self.preferences.recent.contains(&device.device_id),
                    Filter::Favorites => self.is_favorite(&device.device_id),
                };
                allowed
                    && (query.is_empty()
                        || device.alias.to_lowercase().contains(&query)
                        || device.device_id.to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if filter == Filter::Recent {
            result.sort_by_key(|&index| {
                self.preferences
                    .recent
                    .iter()
                    .position(|id| id == &devices[index].device_id)
            });
        }
        result
    }

    pub fn retain_selection(selected: Option<usize>, visible: &[usize]) -> Option<usize> {
        selected
            .filter(|selected| visible.contains(selected))
            .or_else(|| visible.first().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("uu-device-library-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("device-list.fixture.json")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn device(id: &str, alias: &str) -> super::super::Device {
        super::super::Device {
            device_id: id.into(),
            alias: alias.into(),
            status: "CONNECTED".into(),
            controllable: true,
            wallpaper_url: String::new(),
            platform: 0,
        }
    }

    #[test]
    fn favorites_and_recent_survive_reload_without_storing_names_or_tokens() {
        let directory = Directory::new();
        let path = directory.path();
        let mut library = Library::from_path(path.clone());
        library.toggle_favorite("fixture-a");
        library.record_opened("fixture-b");
        library.record_opened("fixture-a");
        let loaded = Library::from_path(path.clone());
        assert!(loaded.is_favorite("fixture-a"));
        assert_eq!(loaded.preferences.recent, ["fixture-a", "fixture-b"]);
        let data = std::fs::read_to_string(&path).unwrap();
        assert!(!data.contains("token"));
        assert!(!data.contains("alias"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }

    #[test]
    fn recent_is_bounded_unique_and_moves_the_last_opened_device_first() {
        let mut library = Library::default();
        for n in 0..40 {
            library.record_opened(&format!("fixture-{n}"));
        }
        library.record_opened("fixture-25");
        assert_eq!(library.preferences.recent.len(), MAX_RECENT);
        assert_eq!(library.preferences.recent[0], "fixture-25");
        assert_eq!(
            library
                .preferences
                .recent
                .iter()
                .filter(|id| *id == "fixture-25")
                .count(),
            1
        );
    }

    #[test]
    fn corrupted_and_future_preferences_are_preserved_after_user_changes() {
        let directory = Directory::new();
        let path = directory.path();
        for bytes in [
            b"not JSON".as_slice(),
            br#"{"version":999,"favorites":[],"recent":[]}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            let mut library = Library::from_path(path.clone());
            library.toggle_favorite("fixture-a");
            library.record_opened("fixture-a");
            assert!(library.is_favorite("fixture-a"));
            assert!(library.notice().is_some());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn owner_namespaces_are_stable_for_an_account_and_do_not_contain_credentials() {
        let a = account_namespace(Some("fixture-account-a"), "fixture-token-one");
        assert_eq!(
            a,
            account_namespace(Some("fixture-account-a"), "fixture-token-two")
        );
        assert_ne!(
            a,
            account_namespace(Some("fixture-account-b"), "fixture-token-one")
        );
        assert_ne!(
            account_namespace(None, "fixture-token-one"),
            account_namespace(None, "fixture-token-two")
        );
        assert_eq!(a.len(), 64);
        assert!(!a.contains("fixture"));
    }

    #[test]
    fn sequential_windows_preserve_each_others_saved_changes() {
        let directory = Directory::new();
        let path = directory.path();
        let mut first = Library::from_path(path.clone());
        let mut second = Library::from_path(path.clone());
        first.toggle_favorite("fixture-a");
        second.toggle_favorite("fixture-b");
        first.record_opened("fixture-a");
        let saved = Library::from_path(path);
        assert!(saved.is_favorite("fixture-a") && saved.is_favorite("fixture-b"));
        assert_eq!(saved.preferences.recent, ["fixture-a"]);
    }

    #[test]
    fn filtering_uses_current_account_inventory_and_recent_order() {
        let devices = vec![
            device("fixture-a", "Work"),
            device("fixture-b", "Home"),
            device("fixture-c", "Laptop"),
        ];
        let mut library = Library::default();
        library.toggle_favorite("fixture-a");
        library.toggle_favorite("removed-from-account");
        library.record_opened("fixture-b");
        library.record_opened("fixture-a");
        assert_eq!(
            library.visible_indices(&devices, Filter::Favorites, ""),
            [0]
        );
        assert_eq!(
            library.visible_indices(&devices, Filter::Recent, ""),
            [0, 1]
        );
        assert_eq!(
            library.visible_indices(&devices, Filter::All, " HOME "),
            [1]
        );
        assert_eq!(
            library.visible_indices(&devices, Filter::All, "fixture-c"),
            [2]
        );
        assert!(library
            .visible_indices(&devices, Filter::Favorites, "Home")
            .is_empty());
        assert_eq!(Library::retain_selection(Some(1), &[0]), Some(0));
        assert_eq!(Library::retain_selection(Some(1), &[]), None);
    }
}
