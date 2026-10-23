//! Runtime configuration and XDG-correct paths.

use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// Human-readable default for a newly registered device, independent of its ids.
pub fn local_device_name() -> String {
    let hostname = gethostname::gethostname();
    let name: String = hostname.to_string_lossy().trim().chars()
        .filter(|c| !c.is_control()).take(64).collect();
    if name.is_empty() { "Eclipse".into() } else { name }
}

/// Service-facing label is derived from this computer, not shared-host preferences.
pub fn uu_device_name() -> String {
    uu_name_for_host(&local_device_name())
}
fn uu_name_for_host(host: &str) -> String {
    const SUFFIX: &str = "-Eclipse";
    let prefix: String = host.chars().take(64 - SUFFIX.chars().count()).collect();
    format!("{prefix}{SUFFIX}")
}

/// Optional routing hint. Never ship a developer's public IP as a default.
pub fn geo_exit_ip() -> String {
    std::env::var("UU_GEO_IP").unwrap_or_else(|_| "0.0.0.0".into())
}

/// Where we keep installer caches, the Wine-less client state, and logs.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Override for the signaling host (tests / mirrors).
    pub signaling_host: String,
}

impl Config {
    /// Build a configuration from XDG environment variables.
    pub fn from_env() -> Result<Self, CoreError> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .or_else(|| {
                let drive = std::env::var_os("HOMEDRIVE")?;
                let path = std::env::var_os("HOMEPATH")?;
                let mut p = PathBuf::from(drive);
                p.push(path);
                Some(p)
            })
            .ok_or_else(|| CoreError::Config("HOME/USERPROFILE is unset".into()))?;
        let data_dir = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("uu-remote");
        let state_dir = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("uu-remote");
        Ok(Self {
            data_dir,
            state_dir,
            signaling_host: uu_proto::SIGNALING_HOST.to_string(),
        })
    }

    pub async fn ensure_dirs(&self) -> Result<(), CoreError> {
        for dir in [&self.data_dir, &self.state_dir] {
            tokio::fs::create_dir_all(dir).await?;
        }
        Ok(())
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("log")
    }

    /// Path to the log file we append session diagnostics to.
    pub fn session_log(&self) -> PathBuf {
        self.log_dir().join("session.log")
    }

    pub fn at(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.data_dir.join(rel)
    }
}

#[cfg(test)]
mod device_label_tests {
    #[test]
    fn uu_suffix_is_explicit_and_fits_the_service_character_limit() {
        assert_eq!(super::uu_name_for_host("Workstation"), "Workstation-Eclipse");
        let label = super::uu_name_for_host(&"名".repeat(64));
        assert_eq!(label.chars().count(), 64);
        assert!(label.ends_with("-Eclipse"));
        assert!(crate::api::validate_device_name(&label).is_ok());
    }
}
