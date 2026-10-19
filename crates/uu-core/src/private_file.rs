//! Atomic replacement for local account state. Temporary files are private
//! from creation and remain on the destination filesystem for atomic rename.
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

struct PendingFile(PathBuf);
impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "account state needs a file name",
        ));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_file_name(format!(".uu-account-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    // Construct the guard only after create_new succeeds: a collision must
    // never delete another file that this call did not create.
    let temporary = PendingFile(temporary);
    let written = (|| {
        file.write_all(bytes)?;
        file.sync_all()
    })();
    drop(file);
    written?;
    // The standard library also handles Windows POSIX rename semantics for
    // files still held by concurrent readers. A bare MoveFileExW does not.
    // https://doc.rust-lang.org/std/fs/fn.rename.html
    fs::rename(&temporary.0, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_write_failure_preserves_destination_and_removes_private_temporary_file() {
        let root = std::env::temp_dir().join(format!("uu-private-write-{}", uuid::Uuid::new_v4()));
        let destination = root.join("existing-directory");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("sentinel"), b"preserve").unwrap();
        let result = write_atomic(&destination, b"new state");
        assert!(result.is_err());
        assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"preserve");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
