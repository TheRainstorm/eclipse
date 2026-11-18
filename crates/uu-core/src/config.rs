//! One process-wide profile selection: Windows portable storage and Linux XDG.

use std::{ffi::OsStr, io::Read, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Human-readable default for a newly registered device, independent of its ids.
pub fn local_device_name() -> String {
    let hostname = gethostname::gethostname();
    let name: String = hostname.to_string_lossy().trim().chars()
        .filter(|c| !c.is_control()).take(64).collect();
    if name.is_empty() { "Eclipse".into() } else { name }
}

/// Where we keep installer caches, the Wine-less client state, and logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Override for the signaling host (tests / mirrors).
    pub signaling_host: String,
}

/// Publish only a successful configuration. The lock serializes cold callers;
/// subsequent reads do not repeat environment queries or migration filesystem IO.
struct ConfigCache {
    value: OnceLock<Config>,
    initialize: Mutex<()>,
}

impl ConfigCache {
    const fn new() -> Self {
        Self { value: OnceLock::new(), initialize: Mutex::new(()) }
    }

    fn get_or_try_init(&self, factory: impl FnOnce() -> Result<Config, CoreError>) -> Result<Config, CoreError> {
        if let Some(config) = self.value.get() { return Ok(config.clone()); }
        // No partially initialized state is published if the factory panics.
        let _guard = self.initialize.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(config) = self.value.get() { return Ok(config.clone()); }
        let config = factory()?;
        Ok(self.value.get_or_init(|| config).clone())
    }
}

impl Config {
    /// Windows uses `eclipse-data` beside the executable by default; Linux keeps
    /// XDG storage unless a portable directory or saved selection exists.
    /// Select once: adding/removing the directory while running must not split
    /// settings, device identities and child windows across different stores.
    /// After successful resolution, reuse the whole configuration. Recoverable
    /// default-storage migration errors leave the cache empty so callers can retry.
    pub fn from_env() -> Result<Self, CoreError> {
        static CONFIG: ConfigCache = ConfigCache::new();
        CONFIG.get_or_try_init(Self::resolve_from_env)
    }

    fn resolve_from_env() -> Result<Self, CoreError> {
        // A relocation helper uses this one-shot override only when reopening
        // the original process after a failed move. It carries paths, no secrets.
        if std::env::var_os("ECLIPSE_PROFILE_DIR").is_none() {
            if let Some(source) = std::env::var_os("ECLIPSE_CONFIG_SOURCE") {
                let config: Self = serde_json::from_str(&source.to_string_lossy())
                    .map_err(|e| CoreError::Config(format!("invalid profile restart source: {e}")))?;
                if !config.data_dir.is_absolute() || !config.state_dir.is_absolute() {
                    return Err(CoreError::Config("profile restart paths must be absolute".into()));
                }
                return config.with_process_lease();
            }
        }
        static PROFILE: OnceLock<Result<Option<Config>, String>> = OnceLock::new();
        let profile = PROFILE.get_or_init(|| {
            let selected = std::env::var_os("ECLIPSE_PROFILE_DIR");
            let executable = std::env::current_exe().map_err(|e| e.to_string())?;
            selected_profile(
                selected.as_deref(),
                &executable,
                &Self::profile_selection_file(&executable).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())
        });
        match profile {
            Ok(Some(config)) => return config.clone().with_process_lease(),
            Err(error) => return Err(CoreError::Config(error.clone())),
            Ok(None) => {}
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
        Self {
            data_dir,
            state_dir,
            signaling_host: String::new(),
        }.with_process_lease()
    }

    /// Both persistent settings and runtime state travel with this root.
    pub fn in_profile(root: &Path) -> Self {
        Self {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        }
    }

    fn with_process_lease(self) -> Result<Self, CoreError> {
        static LEASE: OnceLock<std::fs::File> = OnceLock::new();
        static PROCESS: OnceLock<std::fs::File> = OnceLock::new();
        if LEASE.get().is_none() {
            let file = crate::profile_storage::profile_lease(&self.data_dir, false)?;
            let process = crate::profile_storage::process_lease(&self.data_dir)?;
            let _ = LEASE.set(file);
            let _ = PROCESS.set(process);
        }
        Ok(self)
    }

    /// The portable default is a Windows product choice. Linux keeps XDG data
    /// and state directories and therefore has no single default profile root.
    #[cfg(windows)]
    pub fn default_profile_directory(executable: &Path) -> anyhow::Result<PathBuf> {
        Ok(executable.parent().ok_or_else(||anyhow::anyhow!("application directory unavailable"))?.join("eclipse-data"))
    }

    pub fn profile_selection_file(executable: &Path) -> anyhow::Result<PathBuf> {
        #[cfg(windows)]
        { Ok(executable.parent().ok_or_else(||anyhow::anyhow!("application directory unavailable"))?.join("eclipse-profile.json")) }
        #[cfg(not(windows))]
        {
            let _ = executable;
            let root = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
                .or_else(||std::env::var_os("HOME").map(|h|PathBuf::from(h).join(".config")))
                .ok_or_else(||anyhow::anyhow!("user configuration directory unavailable"))?;
            anyhow::ensure!(root.is_absolute(), "configuration directory must be absolute");
            Ok(root.join("eclipse/profile.json"))
        }
    }

    /// Reopen this exact source after a failed relocation, including old XDG
    /// layouts which do not have a common data/state root.
    pub fn apply_to_command(&self, command: &mut std::process::Command) {
        command.env_remove("ECLIPSE_PROFILE_DIR").env("ECLIPSE_CONFIG_SOURCE", serde_json::to_string(self).expect("Config contains only serializable paths"));
    }

    pub fn save_profile_selection(executable: &Path, root: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(root.is_absolute(), "profile directory must be absolute");
        save_selection(
            &Self::profile_selection_file(executable)?,
            &ProfileSelection {
                version: 1,
                directory: Some(root.to_path_buf()),
                data_dir: None,
                state_dir: None,
            },
        )
    }

    /// Explicitly reuse an existing layout without copying files or changing
    /// the Windows host's directory-derived profile identity.
    pub fn save_config_selection(executable: &Path, source: &Config) -> anyhow::Result<()> {
        save_selection(
            &Self::profile_selection_file(executable)?,
            &ProfileSelection {
                version: 2,
                directory: None,
                data_dir: Some(source.data_dir.clone()),
                state_dir: Some(source.state_dir.clone()),
            },
        )
    }

    /// The old Windows user layout stores settings directly in this folder.
    /// This is an explicit recovery choice, never a default-storage fallback.
    pub fn existing_flat_profile(directory: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            directory.is_absolute() && directory.is_dir(),
            "已有配置目录不存在或不是绝对目录"
        );
        anyhow::ensure!(
            directory.join("gui-settings.json").is_file()
                || directory.join("device-trust").is_dir(),
            "所选目录中没有已有 Eclipse 配置"
        );
        Ok(Self {
            data_dir: directory.to_path_buf(),
            state_dir: directory.join("state"),
            signaling_host: String::new(),
        })
    }

    pub fn prepare_existing_selection(&self) -> anyhow::Result<()> {
        crate::profile_storage::no_other_processes(&self.data_dir)
    }

    /// Called by the restart helper after the old launcher has actually exited.
    pub fn select_existing_profile(
        &self,
        executable: &Path,
        destination: &Config,
    ) -> anyhow::Result<()> {
        let existing = Self::existing_flat_profile(&destination.data_dir)?;
        anyhow::ensure!(
            existing.state_dir == destination.state_dir,
            "旧版配置状态目录不匹配"
        );
        let _lease = crate::profile_storage::profile_lease(&self.data_dir, true)?;
        Self::save_config_selection(executable, destination)
    }

    #[cfg(windows)]
    pub fn legacy_user_profile() -> anyhow::Result<Self> {
        let directory = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("无法确定旧版用户配置目录"))?
            .join("Eclipse");
        Self::existing_flat_profile(&directory)
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
    /// Atomically persist data in this selected application or plugin profile.
    pub fn write_data(&self, rel: impl AsRef<Path>, bytes: &[u8]) -> std::io::Result<()> {
        crate::private_file::write_atomic(&self.at(rel), bytes)
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
        if !cfg!(windows) && !root.try_exists()? { return Ok(None); }
        root
    };
    match std::fs::metadata(&root) {
        Ok(meta) if !meta.is_dir() => return Err(CoreError::Config(format!("profile must be a directory: {}", root.display()))),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {},
    }
    Ok(Some(root))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileSelection {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state_dir: Option<PathBuf>,
}
impl ProfileSelection {
    fn config(&self) -> anyhow::Result<Config> {
        let config = match (
            self.version,
            &self.directory,
            &self.data_dir,
            &self.state_dir,
        ) {
            (1, Some(root), None, None) if root.is_absolute() => Config::in_profile(root),
            (2, None, Some(data), Some(state)) if data.is_absolute() && state.is_absolute() => {
                Config {
                    data_dir: data.clone(),
                    state_dir: state.clone(),
                    signaling_host: String::new(),
                }
            }
            _ => anyhow::bail!("invalid profile selection"),
        };
        Ok(config)
    }
}

fn save_selection(path: &Path, selected: &ProfileSelection) -> anyhow::Result<()> {
    selected.config()?;
    crate::private_file::write_atomic(path, &serde_json::to_vec_pretty(selected)?)?;
    Ok(())
}

fn selected_profile(
    explicit: Option<&OsStr>,
    executable: &Path,
    selection_file: &Path,
) -> anyhow::Result<Option<Config>> {
    if explicit.is_none() {
        if let Some(config) = read_profile_selection(selection_file)? {
            return Ok(Some(config));
        }
    }
    Ok(profile_directory(explicit, executable)?.map(|root| Config::in_profile(&root)))
}

fn read_profile_selection(path: &Path) -> anyhow::Result<Option<Config>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 16 * 1024,
        "profile selection file is too large"
    );
    let selected: ProfileSelection = serde_json::from_slice(&bytes)?;
    Ok(Some(selected.config()?))
}

/// A relocation request is prepared while the GUI is alive and performed only
/// after it exits. The process-wide Config cache is deliberately never changed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRelocation {
    pub source: Config,
    pub destination: PathBuf,
}

impl ProfileRelocation {
    pub fn prepare(source: &Config, destination: &Path) -> anyhow::Result<Self> {
        let value = Self { source: source.clone(), destination: destination.to_path_buf() };
        value.validate()?;
        crate::profile_storage::no_other_processes(&source.data_dir)?;
        Ok(value)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        crate::profile_storage::validate_relocation(&self.source, &self.destination)
    }

    /// Call only after the original GUI exits. Other live processes keep a
    /// shared profile lease, so the helper refuses to copy under active writers.
    pub fn execute(&self, executable: &Path) -> anyhow::Result<Config> {
        self.validate()?;
        #[cfg(windows)]
        if crate::service_admin::windows::relocate_selected_profile(self, executable)? {
            return Ok(Config::in_profile(&self.destination));
        }
        let _lease = self.exclusive_lease()?;
        let config = self.copy()?;
        Config::save_profile_selection(executable, &self.destination)?;
        Ok(config)
    }

    /// Shared by the ordinary helper and the privileged service relocation.
    /// The caller keeps this guard through copying and pointer publication.
    pub fn exclusive_lease(&self) -> anyhow::Result<std::fs::File> {
        Ok(crate::profile_storage::profile_lease(&self.source.data_dir, true)?)
    }

    pub fn copy(&self) -> anyhow::Result<Config> {
        crate::profile_storage::relocate(&self.source, &self.destination)?;
        Ok(Config::in_profile(&self.destination))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::{AtomicUsize, Ordering}, Barrier};
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
    fn successful_config_resolution_runs_once_for_concurrent_callers() {
        let cache = ConfigCache::new();
        let calls = AtomicUsize::new(0);
        let start = Barrier::new(9);
        let root = Path::new("selected-profile");
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| {
                start.wait();
                cache.get_or_try_init(|| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    Ok(Config::in_profile(root))
                }).unwrap()
            })).collect();
            start.wait();
            for worker in workers {
                assert_eq!(worker.join().unwrap().profile_dir(), Some(root));
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_migration_can_retry_before_the_configuration_is_cached() {
        let directory = Directory::new();
        let legacy = directory.0.join("legacy");
        let destination = directory.0.join("current");
        let cache = ConfigCache::new();
        let resolve = || {
            crate::profile_storage::migrate(&legacy, &destination)?;
            Ok(Config::in_profile(&destination))
        };
        std::fs::write(&legacy, b"not a directory").unwrap();
        assert!(matches!(cache.get_or_try_init(resolve), Err(CoreError::Io(_))));
        std::fs::remove_file(&legacy).unwrap();
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("settings.json"), b"preserved").unwrap();
        let config = cache.get_or_try_init(resolve).unwrap();
        assert_eq!(std::fs::read(destination.join("settings.json")).unwrap(), b"preserved");
        assert_eq!(config.profile_dir(), Some(destination.as_path()));
        let cached = cache.get_or_try_init(|| panic!("successful migration must not run again")).unwrap();
        assert_eq!(cached.data_dir, config.data_dir);
    }

    #[test]
    fn changing_a_returned_config_does_not_change_the_cached_selection() {
        let cache = ConfigCache::new();
        let root = Path::new("selected-profile");
        let mut caller = cache.get_or_try_init(|| Ok(Config::in_profile(root))).unwrap();
        caller.data_dir = "another-data-directory".into();
        caller.state_dir = "another-state-directory".into();
        caller.signaling_host = "caller override".into();
        let cached = cache.get_or_try_init(|| panic!("configuration already resolved")).unwrap();
        assert_eq!(cached.profile_dir(), Some(root));
        assert!(cached.signaling_host.is_empty());
    }

    #[test]
    fn portable_selection_is_relative_to_executable_and_explicit_profile_wins() {
        let directory = Directory::new();
        let exe = directory.0.join("app").join("eclipse");
        let portable = exe.parent().unwrap().join("eclipse-data");
        assert_eq!(profile_directory(None, &exe).unwrap(), cfg!(windows).then_some(portable.clone()));
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

    #[test]
    fn saved_profile_selection_is_explicit_bounded_and_does_not_switch_a_cache() {
        let directory = Directory::new();
        let selection = directory.0.join("selection.json");
        let selected = directory.0.join("my profile");
        assert!(read_profile_selection(&selection).unwrap().is_none());
        std::fs::write(&selection, serde_json::to_vec(&ProfileSelection { version:1, directory:Some(selected.clone()), data_dir:None, state_dir:None }).unwrap()).unwrap();
        assert_eq!(read_profile_selection(&selection).unwrap().unwrap().profile_dir(), Some(selected.as_path()));
        std::fs::write(&selection, br#"{"version":1,"directory":"relative"}"#).unwrap();
        assert!(read_profile_selection(&selection).is_err());
        std::fs::write(&selection, vec![b' ';16*1024+1]).unwrap();
        assert!(read_profile_selection(&selection).is_err());
    }

    #[test]
    fn exact_selection_preserves_flat_identity_and_never_imports_it_by_default() {
        let directory = Directory::new();
        let legacy = directory.0.join("legacy-user-data");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("gui-settings.json"), b"existing settings").unwrap();
        let existing = Config::existing_flat_profile(&legacy).unwrap();
        let exe = directory.0.join("app/eclipse");
        std::fs::create_dir_all(exe.parent().unwrap().join("eclipse-data")).unwrap();
        let selector = exe.parent().unwrap().join("selection.json");
        let fresh = selected_profile(None, &exe, &selector).unwrap().unwrap();
        assert_ne!(
            fresh.data_dir, legacy,
            "an unrelated installation must not adopt user storage"
        );
        save_selection(
            &selector,
            &ProfileSelection {
                version: 2,
                directory: None,
                data_dir: Some(existing.data_dir.clone()),
                state_dir: Some(existing.state_dir.clone()),
            },
        )
        .unwrap();
        let reopened = selected_profile(None, &exe, &selector).unwrap().unwrap();
        assert_eq!(reopened.data_dir, legacy);
        assert_eq!(reopened.state_dir, legacy.join("state"));
        assert!(reopened.profile_dir().is_none());
        assert_eq!(
            crate::service_profile::directory_key(&reopened.data_dir).unwrap(),
            crate::service_profile::directory_key(&existing.data_dir).unwrap()
        );
        assert!(!legacy.join("data").exists());
        assert_eq!(
            std::fs::read(legacy.join("gui-settings.json")).unwrap(),
            b"existing settings"
        );
        let other = directory.0.join("other/eclipse");
        std::fs::create_dir_all(other.parent().unwrap().join("eclipse-data")).unwrap();
        let separate = selected_profile(
            None,
            &other,
            &other.parent().unwrap().join("selection.json"),
        )
        .unwrap()
        .unwrap();
        assert_ne!(separate.data_dir, reopened.data_dir);
    }

    #[test]
    fn explicit_root_overrides_even_a_broken_selector_and_mixed_layouts_are_rejected() {
        let directory = Directory::new();
        let selector = directory.0.join("selection.json");
        std::fs::write(&selector, b"broken selection").unwrap();
        let explicit = directory.0.join("isolated");
        let selected = selected_profile(
            Some(explicit.as_os_str()),
            &directory.0.join("eclipse"),
            &selector,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.profile_dir(), Some(explicit.as_path()));
        assert!(selected_profile(None, &directory.0.join("eclipse"), &selector).is_err());
        let mixed = ProfileSelection {
            version: 2,
            directory: Some(explicit.clone()),
            data_dir: Some(explicit.clone()),
            state_dir: Some(explicit.join("state")),
        };
        assert!(save_selection(&selector, &mixed).is_err());
        assert_eq!(std::fs::read(&selector).unwrap(), b"broken selection");
        assert!(Config::existing_flat_profile(&explicit).is_err());
    }

    #[test]
    fn child_command_carries_the_exact_flat_source_instead_of_adding_a_data_folder() {
        let directory = Directory::new();
        let source = Config {
            data_dir: directory.0.join("old-flat"),
            state_dir: directory.0.join("old-flat/state"),
            signaling_host: String::new(),
        };
        let mut command = std::process::Command::new("eclipse");
        command.env("ECLIPSE_PROFILE_DIR", directory.0.join("inherited-root"));
        source.apply_to_command(&mut command);
        let vars: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(vars.get(OsStr::new("ECLIPSE_PROFILE_DIR")), Some(&None));
        let payload = vars
            .get(OsStr::new("ECLIPSE_CONFIG_SOURCE"))
            .unwrap()
            .unwrap();
        let child: Config = serde_json::from_str(payload.to_str().unwrap()).unwrap();
        assert_eq!(child.data_dir, source.data_dir);
        assert_eq!(child.state_dir, source.state_dir);
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
