//! Copy legacy user data once, publish it atomically, and retain the original
//! for older releases. An incomplete copy never becomes a new device identity.
use std::{collections::BTreeMap, fs, io::{self, Read}, path::{Path, PathBuf}};
use sha2::{Digest, Sha256};

const MAX_FILES: usize = 8192;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, [u8; 32]>> {
    snapshot_with_cache(root, false)
}

fn snapshot_with_cache(root: &Path, include_cache: bool) -> io::Result<BTreeMap<PathBuf, [u8; 32]>> {
    for ancestor in root.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() { return Err(io::Error::other("configuration path contains a symbolic link")); }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 { return Err(io::Error::other("configuration path contains a reparse point")); }
        }
    }
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, [u8; 32]>, total: &mut u64, include_cache: bool) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Cache and IPC/lock state have process ownership, not profile ownership.
            if (!include_cache && relative == Path::new("cache")) || relative == Path::new(".eclipse-snapshot.json") || name.ends_with(".lock")
                || (relative.components().count() == 1 && (name.starts_with("native-stream-workspace-") || name.starts_with("stream-workspace-v3-")))
                || (relative.parent() == Some(Path::new("device-trust")) && name.starts_with("launch-") && name.ends_with(".json")) { continue; }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("legacy configuration contains a symbolic link; original data was preserved"));
            }
            #[cfg(windows)]
            { use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 { return Err(io::Error::other("legacy configuration contains a reparse point")); }
            }
            if metadata.is_dir() { walk(root, &path, files, total, include_cache)?; }
            else if metadata.is_file() {
                *total = total.saturating_add(metadata.len());
                if files.len() >= MAX_FILES || *total > MAX_BYTES { return Err(io::Error::other("legacy configuration exceeds migration limits")); }
                let mut file = fs::File::open(&path)?;
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 16 * 1024];
                let mut read = 0u64;
                loop {
                    let length = file.read(&mut buffer)?;
                    if length == 0 { break; }
                    read += length as u64;
                    if read > metadata.len() { return Err(io::Error::other("legacy configuration changed while reading")); }
                    digest.update(&buffer[..length]);
                }
                files.insert(relative.to_path_buf(), digest.finalize().into());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files, &mut 0, include_cache)?;
    Ok(files)
}

pub(crate) fn profile_lease(data: &Path, exclusive: bool) -> io::Result<fs::File> {
    fs::create_dir_all(data)?;
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(data.join(".eclipse-profile-use.lock"))?;
    if exclusive {
        fs2::FileExt::try_lock_exclusive(&file).map_err(|e|io::Error::new(e.kind(),
            format!("当前配置仍有其它 Eclipse 窗口或命令运行，请退出后再迁移：{e}")))?;
    } else {
        fs2::FileExt::try_lock_shared(&file).map_err(|e|io::Error::new(e.kind(),
            format!("当前配置正在迁移，请等待迁移完成后重新打开：{e}")))?;
    }
    Ok(file)
}

pub(crate) fn process_lease(data: &Path) -> io::Result<fs::File> {
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(data.join(format!(".eclipse-process-{}.lock", std::process::id())))?;
    fs2::FileExt::try_lock_exclusive(&file)?;
    Ok(file)
}

pub(crate) fn no_other_processes(data: &Path) -> anyhow::Result<()> {
    if !data.try_exists()? { return Ok(()); }
    let own = format!(".eclipse-process-{}.lock", std::process::id());
    for entry in fs::read_dir(data)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == own || !name.starts_with(".eclipse-process-") || !name.ends_with(".lock") { continue; }
        anyhow::ensure!(entry.file_type()?.is_file(), "配置进程标记不是普通文件");
        let file = fs::OpenOptions::new().read(true).write(true).open(entry.path())?;
        anyhow::ensure!(fs2::FileExt::try_lock_exclusive(&file).is_ok(), "当前配置还有其它 Eclipse 窗口或命令，请先退出后再迁移");
    }
    Ok(())
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(path.is_absolute(), "profile directory must be absolute");
    let mut tail = Vec::new();
    let mut parent = path;
    while !parent.try_exists()? {
        let name = parent.file_name().ok_or_else(||anyhow::anyhow!("profile directory unavailable"))?;
        anyhow::ensure!(name != "." && name != "..", "profile directory must be normalized");
        tail.push(name);
        parent = parent.parent().ok_or_else(||anyhow::anyhow!("profile directory unavailable"))?;
    }
    let mut normalized = parent.canonicalize()?;
    for name in tail.into_iter().rev() { normalized.push(name); }
    Ok(normalized)
}

pub(crate) fn validate_relocation(source: &crate::Config, destination: &Path) -> anyhow::Result<()> {
    let target = absolute_path(destination)?;
    for path in [&source.data_dir, &source.state_dir] {
        let original = absolute_path(path)?;
        anyhow::ensure!(!target.starts_with(&original) && !original.starts_with(&target),
            "新配置目录不能与当前数据或状态目录相同、包含或互相嵌套");
    }
    if destination.try_exists()? {
        let meta = fs::symlink_metadata(destination)?;
        anyhow::ensure!(meta.is_dir() && !meta.file_type().is_symlink(), "新配置位置必须是普通目录");
        anyhow::ensure!(fs::read_dir(destination)?.next().is_none(), "新目录已有内容，不能覆盖另一份配置；请选择空目录");
    }
    Ok(())
}

/// Relocation copies caches and logs as well as persistent data. Only lock/IPC
/// artifacts are omitted; the original tree is retained as the user's backup.
pub(crate) fn relocate(source: &crate::Config, destination: &Path) -> anyhow::Result<()> {
    validate_relocation(source, destination)?;
    let parent = destination.parent().ok_or_else(||anyhow::anyhow!("profile parent unavailable"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".eclipse-relocate-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&temporary)?;
    let result = (|| -> anyhow::Result<()> {
        for (original, name) in [(&source.data_dir, "data"), (&source.state_dir, "state")] {
            let copied = temporary.join(name);
            fs::create_dir(&copied)?;
            if !original.try_exists()? { continue; }
            let files = snapshot_with_cache(original, true)?;
            for (relative, expected) in &files {
                let bytes = fs::read(original.join(relative))?;
                anyhow::ensure!(<[u8;32]>::from(Sha256::digest(&bytes)) == *expected, "配置在复制期间发生变化，原目录已保留");
                crate::private_file::write_atomic(&copied.join(relative), &bytes)?;
            }
            anyhow::ensure!(snapshot_with_cache(original, true)? == files, "配置在复制期间发生变化，原目录已保留");
        }
        for suffix in [Path::new(""), Path::new("server")] {
            crate::service_profile::rebase_copied_connections(&source.data_dir.join(suffix), &temporary.join("data").join(suffix), &destination.join("data").join(suffix))?;
        }
        // Do not replace a directory that became nonempty while copying.
        if destination.try_exists()? {
            anyhow::ensure!(fs::read_dir(destination)?.next().is_none(), "新配置目录在迁移期间发生变化，未覆盖");
            fs::remove_dir(destination)?;
        }
        fs::rename(&temporary, destination)?;
        Ok(())
    })();
    if temporary.exists() { let _ = fs::remove_dir_all(&temporary); }
    result
}

pub(crate) fn migrate(legacy: &Path, destination: &Path) -> io::Result<()> {
    if destination.try_exists()? || !legacy.try_exists()? { return Ok(()); }
    let parent = destination.parent().ok_or_else(|| io::Error::other("configuration parent unavailable"))?;
    fs::create_dir_all(parent)?;
    let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(parent.join(".eclipse-migration.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    if destination.try_exists()? { return Ok(()); }
    let files = snapshot(legacy)?;
    let temporary = parent.join(format!(".eclipse-migration-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&temporary)?;
    let result = (|| {
        for (relative, expected) in &files {
            let bytes = fs::read(legacy.join(relative))?;
            if <[u8; 32]>::from(Sha256::digest(&bytes)) != *expected { return Err(io::Error::other("legacy configuration changed during migration; original data was preserved")); }
            crate::private_file::write_atomic(&temporary.join(relative), &bytes)?;
        }
        if snapshot(legacy)? != files { return Err(io::Error::other("legacy configuration changed during migration; original data was preserved")); }
        fs::rename(&temporary, destination)
    })();
    if temporary.exists() { let _ = fs::remove_dir_all(&temporary); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_identity_original_and_existing_new_preferences() {
        let root = std::env::temp_dir().join(format!("eclipse-legacy-migration-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("uu-remote"); let destination = root.join("eclipse");
        let original = crate::device_trust::TrustStore::open(legacy.join("device-trust")).unwrap();
        fs::write(legacy.join("gui-settings.json"), b"original preferences").unwrap();
        fs::create_dir_all(legacy.join("cache")).unwrap(); fs::write(legacy.join("cache/not-profile"), b"cache").unwrap();
        migrate(&legacy, &destination).unwrap();
        let copied = crate::device_trust::TrustStore::open(destination.join("device-trust")).unwrap();
        assert_eq!(original.card("Original").id(), copied.card("Copy").id());
        assert!(legacy.join("gui-settings.json").is_file()); assert!(!destination.join("cache").exists());
        fs::write(destination.join("gui-settings.json"), b"new preferences").unwrap();
        migrate(&legacy, &destination).unwrap();
        assert_eq!(fs::read(destination.join("gui-settings.json")).unwrap(), b"new preferences");
        assert_eq!(fs::read(legacy.join("gui-settings.json")).unwrap(), b"original preferences");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relocation_copies_data_state_cache_and_identity_without_runtime_capabilities() {
        let root=std::env::temp_dir().join(format!("eclipse-relocation-{}",uuid::Uuid::new_v4()));
        let source=crate::Config::in_profile(&root.join("original"));
        fs::create_dir_all(source.at("cache")).unwrap();
        fs::create_dir_all(source.state_dir.join("stream-workspace-v3-owned")).unwrap();
        fs::write(source.at("cache/image"),b"image cache").unwrap();
        fs::write(source.at("settings.json"),b"settings").unwrap();
        fs::write(source.at("active.lock"),b"lock").unwrap();
        fs::write(source.state_dir.join("log.jsonl"),b"diagnostics").unwrap();
        fs::write(source.state_dir.join("stream-workspace-v3-owned/endpoint.json"),b"session secret").unwrap();
        let identity=crate::device_trust::TrustStore::open(source.at("device-trust")).unwrap().card("Original").id();
        let request=crate::config::ProfileRelocation::prepare(&source,&root.join("moved")).unwrap();
        let copied=request.copy().unwrap();
        assert_eq!(fs::read(copied.at("cache/image")).unwrap(),b"image cache");
        assert_eq!(fs::read(copied.state_dir.join("log.jsonl")).unwrap(),b"diagnostics");
        assert!(!copied.at("active.lock").exists());
        assert!(!copied.state_dir.join("stream-workspace-v3-owned").exists());
        assert_eq!(crate::device_trust::TrustStore::open(copied.at("device-trust")).unwrap().card("Copied").id(),identity);
        assert_eq!(fs::read(source.at("settings.json")).unwrap(),b"settings");
        assert!(request.copy().is_err(),"an existing destination must never be overwritten");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relocation_rejects_nested_targets_and_live_profile_writers() {
        let root=std::env::temp_dir().join(format!("eclipse-relocation-live-{}",uuid::Uuid::new_v4()));
        let source=crate::Config::in_profile(&root.join("original"));
        fs::create_dir_all(&source.data_dir).unwrap();
        fs::create_dir_all(&source.state_dir).unwrap();
        for target in [&source.data_dir, &source.data_dir.join("nested"), &root] {
            assert!(crate::config::ProfileRelocation::prepare(&source,target).is_err());
        }
        let shared=profile_lease(&source.data_dir,false).unwrap();
        assert!(profile_lease(&source.data_dir,true).is_err());
        drop(shared);
        assert!(profile_lease(&source.data_dir,true).is_ok());
        let marker=source.at(".eclipse-process-another.lock");
        let other=fs::OpenOptions::new().read(true).write(true).create_new(true).open(&marker).unwrap();
        fs2::FileExt::lock_exclusive(&other).unwrap();
        assert!(crate::config::ProfileRelocation::prepare(&source,&root.join("new")).is_err());
        drop(other);
        assert!(crate::config::ProfileRelocation::prepare(&source,&root.join("new")).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
