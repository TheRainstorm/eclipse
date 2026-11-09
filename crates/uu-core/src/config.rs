//! One process-wide profile selection, with legacy XDG paths as the default.

use std::{ffi::OsStr, path::{Path, PathBuf}, sync::OnceLock};

use crate::error::CoreError;

/// Human-readable default for a newly registered device, independent of its ids.
pub fn local_device_name() -> String {
    let hostname = gethostname::gethostname();
    let name: String = hostname.to_string_lossy().trim().chars()
        .filter(|c| !c.is_control()).take(64).collect();
    if name.is_empty() { "Eclipse".into() } else { name }
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
    /// An existing `eclipse-data` directory beside the executable selects a
    /// portable profile. ECLIPSE_PROFILE_DIR explicitly selects a profile root.
    /// Select once: adding/removing the directory while running must not split
    /// settings, device identities and child windows across different stores.
    pub fn from_env() -> Result<Self, CoreError> {
        static PROFILE: OnceLock<Result<Option<PathBuf>, String>> = OnceLock::new();
        let profile = PROFILE.get_or_init(|| {
            let selected = std::env::var_os("ECLIPSE_PROFILE_DIR");
            let executable = std::env::current_exe().map_err(|e| e.to_string())?;
            profile_directory(selected.as_deref(), &executable).map_err(|e| match e {
                CoreError::Config(message) => message,
                other => other.to_string(),
            })
        });
        match profile {
            Ok(Some(root)) => return Ok(Self::in_profile(root)),
            Err(error) => return Err(CoreError::Config(error.clone())),
            Ok(None) => {},
        }
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
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let state_home = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        #[cfg(windows)]
        let default_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local")).join("Eclipse");
        #[cfg(not(windows))]
        let default_data = data_home.join("eclipse");
        #[cfg(windows)]
        let data_dir = if std::env::var_os("XDG_DATA_HOME").is_some() { data_home.join("eclipse") } else { default_data.clone() };
        #[cfg(not(windows))]
        let data_dir = default_data;
        #[cfg(windows)]
        let state_dir = if std::env::var_os("XDG_STATE_HOME").is_some() { state_home.join("eclipse") } else { default_data.join("state") };
        #[cfg(not(windows))]
        let state_dir = state_home.join("eclipse");
        crate::profile_storage::migrate(&data_home.join("uu-remote"), &data_dir)?;
        crate::profile_storage::migrate(&state_home.join("uu-remote"), &state_dir)?;
        Ok(Self {
            data_dir,
            state_dir,
            signaling_host: String::new(),
        })
    }

    /// Both persistent settings and runtime state travel with this root.
    fn in_profile(root: &Path) -> Self {
        Self {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        }
    }

    /// None for legacy user storage and the protected Windows service store.
    pub fn profile_dir(&self) -> Option<&Path> {
        let root = self.data_dir.parent()?;
        (self.data_dir.file_name()? == "data"
            && self.state_dir == root.join("state"))
            .then_some(root)
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

fn profile_directory(explicit: Option<&OsStr>, executable: &Path) -> Result<Option<PathBuf>, CoreError> {
    let root = if let Some(value) = explicit {
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return Err(CoreError::Config("ECLIPSE_PROFILE_DIR must be an absolute, nonempty path".into()));
        }
        root
    } else {
        let root = executable.parent()
            .ok_or_else(|| CoreError::Config("application directory unavailable".into()))?
            .join("eclipse-data");
        if !root.try_exists()? { return Ok(None); }
        root
    };
    match std::fs::metadata(&root) {
        Ok(meta) if !meta.is_dir() => return Err(CoreError::Config(format!("profile must be a directory: {}", root.display()))),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {},
    }
    Ok(Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("eclipse-profiles-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Directory { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn portable_selection_is_relative_to_executable_and_explicit_profile_wins() {
        let directory = Directory::new();
        let exe = directory.0.join("app").join("eclipse");
        let portable = exe.parent().unwrap().join("eclipse-data");
        assert_eq!(profile_directory(None, &exe).unwrap(), None);
        assert!(!exe.parent().unwrap().exists(), "reading the default must not create portable storage");
        std::fs::create_dir_all(&portable).unwrap();
        assert_eq!(profile_directory(None, &exe).unwrap(), Some(portable.clone()));
        let explicit = directory.0.join("separate profile");
        assert_eq!(profile_directory(Some(explicit.as_os_str()), &exe).unwrap(), Some(explicit));
        for invalid in ["", "relative/profile"] {
            assert!(profile_directory(Some(OsStr::new(invalid)), &exe).is_err());
        }
        std::fs::remove_dir(&portable).unwrap();
        std::fs::write(&portable, b"not a directory").unwrap();
        assert!(profile_directory(None, &exe).is_err(), "invalid portable storage must not fall back to user data");
    }

    #[tokio::test]
    async fn independent_profiles_keep_settings_identity_and_logs_separate() {
        let directory = Directory::new();
        let original = Config::in_profile(&directory.0.join("original"));
        let fresh = Config::in_profile(&directory.0.join("first run"));
        original.ensure_dirs().await.unwrap();
        std::fs::write(original.at("gui-settings.json"), b"original preferences").unwrap();
        std::fs::write(original.at("identity.json"), b"original identity").unwrap();
        fresh.ensure_dirs().await.unwrap();
        assert!(!fresh.at("gui-settings.json").exists());
        assert!(!fresh.at("identity.json").exists());
        assert_ne!(fresh.session_log(), original.session_log());
        std::fs::write(fresh.at("gui-settings.json"), b"new preferences").unwrap();
        assert_eq!(std::fs::read(original.at("gui-settings.json")).unwrap(), b"original preferences");
        assert_eq!(fresh.profile_dir(), Some(directory.0.join("first run").as_path()));
    }
}
