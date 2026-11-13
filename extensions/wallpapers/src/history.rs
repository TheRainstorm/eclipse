//! One recent list per installed plugin profile, persisted across restarts.
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, io::Read, path::Path};

const LIMIT: usize = 100;
#[derive(Default, Serialize, Deserialize)]
pub struct History {
    recent: VecDeque<String>,
}
impl History {
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(32 * 1024 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 32 * 1024, "wallpaper history is too large");
        let mut value: Self = serde_json::from_slice(&bytes)?;
        value.recent.truncate(LIMIT);
        Ok(value)
    }
    pub fn unseen(&self, id: &str) -> bool {
        !self.recent.iter().any(|old| old == id)
    }
    pub fn choose<'a>(&self, ids: &'a [String], random: usize) -> Option<&'a str> {
        let fresh: Vec<_> = ids.iter().filter(|id| self.unseen(id)).collect();
        if !fresh.is_empty() {
            return Some(fresh[random % fresh.len()]);
        }
        // A finite result set may be smaller than the recent list. Revisit the
        // least recently used item only after all alternatives have been seen.
        self.recent
            .iter()
            .rev()
            .find_map(|old| ids.iter().find(|id| *id == old))
            .or_else(|| ids.first())
            .map(String::as_str)
    }
    pub fn remember(
        &mut self,
        config: &uu_plugin_sdk::engine::Config,
        id: &str,
    ) -> anyhow::Result<()> {
        self.recent.retain(|old| old != id);
        self.recent.push_front(id.to_owned());
        self.recent.truncate(LIMIT);
        config.write_data("wallhaven-history.json", &serde_json::to_vec(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_avoids_recent_items_and_cycles_only_after_exhaustion() {
        let mut history = History::default();
        let ids = vec!["one".into(), "two".into(), "three".into()];
        history.recent = ["one".into(), "two".into()].into();
        assert_eq!(history.choose(&ids, 0), Some("three"));
        history.recent.push_front("three".into());
        assert_eq!(history.choose(&ids, 0), Some("two"));
        assert!(!history.unseen("one"));
    }
    #[test]
    fn history_survives_restart_and_retains_only_a_hundred_distinct_ids() {
        let directory = std::env::temp_dir().join(format!(
            "eclipse-wallpaper-history-{}",
            rand::random::<u64>()
        ));
        let config = uu_plugin_sdk::engine::Config {
            data_dir: directory.clone(),
            state_dir: directory.join("state"),
            signaling_host: String::new(),
        };
        let mut history = History::default();
        for id in 0..105 {
            history.remember(&config, &id.to_string()).unwrap();
        }
        let mut reopened = History::read(&config.at("wallhaven-history.json")).unwrap();
        assert_eq!(reopened.recent.len(), 100);
        assert!(reopened.unseen("0"));
        assert!(!reopened.unseen("104"));
        reopened.remember(&config, "100").unwrap();
        assert_eq!(reopened.recent.len(), 100);
        assert_eq!(reopened.recent.front().map(String::as_str), Some("100"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
