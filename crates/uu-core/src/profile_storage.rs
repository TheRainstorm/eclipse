//! Copy legacy user data once, publish it atomically, and retain the original
//! for older releases. An incomplete copy never becomes a new device identity.
use std::{collections::BTreeMap, fs, io::{self, Read}, path::{Path, PathBuf}};
use sha2::{Digest, Sha256};

const MAX_FILES: usize = 8192;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;

fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, [u8; 32]>> {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, [u8; 32]>, total: &mut u64) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Cache and IPC/lock state have process ownership, not profile ownership.
            if relative == Path::new("cache") || name.ends_with(".lock")
                || (relative.components().count() == 1 && name.starts_with("native-stream-workspace-"))
                || (relative.parent() == Some(Path::new("device-trust")) && name.starts_with("launch-") && name.ends_with(".json")) { continue; }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("legacy configuration contains a symbolic link; original data was preserved"));
            }
            #[cfg(windows)]
            { use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 { return Err(io::Error::other("legacy configuration contains a reparse point")); }
            }
            if metadata.is_dir() { walk(root, &path, files, total)?; }
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
    walk(root, root, &mut files, &mut 0)?;
    Ok(files)
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
}
