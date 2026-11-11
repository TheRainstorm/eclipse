//! A profile-scoped restart intention, containing only the saved configuration
//! revision. Reopening the UI must inspect the real host before acting on it.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use uu_core::Config;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    revision: String,
}
fn path(config: &Config) -> std::path::PathBuf {
    config.state_dir.join("connection-service-pending.json")
}
fn valid(revision: &str) -> bool {
    revision.len() == 64 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn mutation(config: &Config) -> Result<std::fs::File> {
    use fs2::FileExt;
    std::fs::create_dir_all(&config.state_dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path(config).with_extension("lock"))?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}
pub(super) fn load(config: &Config) -> Result<Option<String>> {
    let bytes = match crate::settings::read_bounded(&path(config), 1024) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let intent: Intent = serde_json::from_slice(&bytes)?;
    ensure!(
        intent.version == 1 && valid(&intent.revision),
        "invalid connection-service activation intent"
    );
    Ok(Some(intent.revision))
}
pub(super) fn save(config: &Config, revision: &str) -> Result<()> {
    let _guard = mutation(config)?;
    ensure!(
        valid(revision),
        "invalid connection-service configuration revision"
    );
    crate::settings::save_json(
        &Intent {
            version: 1,
            revision: revision.into(),
        },
        &path(config),
    )
}
pub(super) fn clear(config: &Config, revision: &str) -> Result<()> {
    let _guard = mutation(config)?;
    // A completion from an older window must not erase a newer import intent.
    if load(config)?.as_deref() != Some(revision) {
        return Ok(());
    }
    match std::fs::remove_file(path(config)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reopening_retains_the_profile_intent_and_stale_completion_keeps_newer_intent() {
        let root = std::env::temp_dir().join(format!("eclipse-pending-{}", uuid::Uuid::new_v4()));
        let config = Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        };
        let other = Config {
            state_dir: root.join("other-state"),
            ..config.clone()
        };
        let first = "a".repeat(64);
        let second = "b".repeat(64);
        save(&config, &first).unwrap();
        assert_eq!(load(&config).unwrap(), Some(first.clone()));
        assert!(load(&other).unwrap().is_none());
        save(&config, &second).unwrap();
        clear(&config, &first).unwrap();
        assert_eq!(load(&config).unwrap(), Some(second.clone()));
        clear(&config, &second).unwrap();
        assert!(load(&config).unwrap().is_none());
        let invalid = vec![b'x'; 1025];
        std::fs::write(path(&config), &invalid).unwrap();
        assert!(load(&config).is_err());
        assert_eq!(std::fs::read(path(&config)).unwrap(), invalid);
        std::fs::remove_dir_all(root).unwrap();
    }
}
