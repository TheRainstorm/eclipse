//! File ownership for the capacity transaction. No global desktop snapshot is
//! ever replayed; durable records are evidence and a fail-closed startup gate.
use super::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::FromRawHandle,
    },
    path::{Path, PathBuf},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ,
        GENERIC_WRITE,
    },
    Security::{
        Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
        *,
    },
    Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, GetFileAttributesW, CREATE_NEW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        INVALID_FILE_ATTRIBUTES,
    },
};

fn fail(message: impl Into<String>) -> Error {
    Error::Platform(message.into())
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}
fn io(operation: &str, error: std::io::Error) -> Error {
    fail(format!("{operation}: {error}"))
}

fn ordinary(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata =
            std::fs::symlink_metadata(ancestor).map_err(|e| io("capacity path metadata", e))?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(fail("capacity path contains a reparse point"));
        }
    }
    Ok(())
}
pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    ordinary(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| io("open capacity file", e))?;
    read_handle(&mut file)
}
fn read_handle(file: &mut File) -> Result<Vec<u8>> {
    let meta = file
        .metadata()
        .map_err(|e| io("capacity file metadata", e))?;
    if !meta.is_file()
        || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || meta.len() > 1024 * 1024
    {
        return Err(fail("capacity file is not a bounded ordinary file"));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io("read capacity file", e))?;
    if bytes.len() > 1024 * 1024 {
        return Err(fail("capacity file grew beyond its size bound"));
    }
    Ok(bytes)
}
fn write_handle(file: &mut File, bytes: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| io("seek capacity file", e))?;
    file.write_all(bytes)
        .map_err(|e| io("write capacity file", e))?;
    file.set_len(bytes.len() as u64)
        .map_err(|e| io("truncate capacity file", e))?;
    file.sync_all().map_err(|e| io("sync capacity file", e))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| io("seek capacity readback", e))?;
    if read_handle(file)? != bytes {
        return Err(fail(
            "capacity file readback does not match its written bytes",
        ));
    }
    Ok(())
}
pub(super) fn replace_exact(path: &Path, expected: &[u8], replacement: &[u8]) -> Result<()> {
    ordinary(path)?;
    // Sharing is disabled while comparing, writing, syncing and reading back.
    // Keep the file/ACL/inode; close before UMDF may reopen its XML. A write
    // error attempts byte-exact compensation on this same exclusive handle.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| io("exclusive capacity CAS", e))?;
    let current = read_handle(&mut file)?;
    if current != expected {
        return Err(fail(
            "capacity XML changed before CAS; no bytes were overwritten",
        ));
    }
    if let Err(error) = write_handle(&mut file, replacement) {
        return match write_handle(&mut file, expected) {
            Ok(()) => Err(fail(format!("{error}; original file bytes were restored"))),
            Err(rollback) => Err(fail(format!(
                "{error}; exact file recovery remains pending: {rollback}"
            ))),
        };
    }
    Ok(())
}

fn root_path() -> Result<PathBuf> {
    crate::service_admin::windows::Paths::discover()
        .map(|p| p.root.with_file_name("EclipseVirtualDisplayCapacity"))
        .map_err(|e| fail(e.to_string()))
}
fn private_root(path: &Path) -> Result<()> {
    ordinary(path)?;
    let (mut owner, mut dacl, mut descriptor) = (null_mut(), null_mut(), null_mut());
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(error("capacity directory security", status));
    }
    let checked = (|| -> Result<()> {
        if owner.is_null()
            || dacl.is_null()
            || !unsafe {
                IsWellKnownSid(owner, WinLocalSystemSid) != 0
                    || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
            }
        {
            return Err(fail(
                "capacity journal requires an administrator-owned private directory",
            ));
        }
        for index in 0..unsafe { (*dacl).AceCount } as u32 {
            let mut raw = null_mut();
            win_ok(
                unsafe { GetAce(dacl, index, &mut raw) },
                "capacity journal ACL",
            )?;
            if raw.is_null() {
                return Err(fail("capacity journal ACL contains an invalid entry"));
            }
            let header = unsafe { &*(raw as *const ACE_HEADER) };
            if header.AceType == 1 {
                continue;
            }
            if header.AceType != 0 || (header.AceSize as usize) < size_of::<ACCESS_ALLOWED_ACE>() {
                return Err(fail("capacity journal ACL has an unsupported entry"));
            }
            let ace = unsafe { &*(raw as *const ACCESS_ALLOWED_ACE) };
            let sid = std::ptr::addr_of!(ace.SidStart) as PSID;
            let administrator = unsafe {
                IsWellKnownSid(sid, WinLocalSystemSid) != 0
                    || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
            };
            // Ordinary frontends may inspect marker existence, but cannot list
            // the directory or read/modify a journal or original XML backup.
            let metadata_only = unsafe { IsWellKnownSid(sid, WinBuiltinUsersSid) != 0 }
                && ace.Mask == 0xa0
                && header.AceFlags == 0;
            if !administrator && !metadata_only {
                return Err(fail("capacity journal directory grants content access outside Administrators/System"));
            }
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor);
    }
    checked
}
fn root() -> Result<PathBuf> {
    let root = root_path()?;
    ordinary(
        root.parent()
            .ok_or_else(|| fail("capacity root has no parent"))?,
    )?;
    let descriptor = descriptor("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0xa0;;;BU)")?;
    let created = unsafe { CreateDirectoryW(wide(&root).as_ptr(), &descriptor.attributes()) };
    if created == 0 && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Err(error("create private capacity journal directory", unsafe {
            GetLastError()
        }));
    }
    private_root(&root)?;
    Ok(root)
}
fn descriptor(sddl: &str) -> Result<crate::service_admin::windows::Descriptor> {
    let sddl: Vec<_> = sddl.encode_utf16().chain([0]).collect();
    let mut value = null_mut();
    win_ok(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut value,
                null_mut(),
            )
        },
        "capacity journal security descriptor",
    )?;
    Ok(crate::service_admin::windows::Descriptor(value))
}
fn create_marker(path: &Path) -> Result<File> {
    let descriptor = descriptor("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x80;;;BU)")?;
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            &descriptor.attributes(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(error("reserve capacity journal", unsafe { GetLastError() }));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}
pub(super) fn pending_exists() -> Result<bool> {
    let root = root_path()?;
    // GetFileAttributes needs only the explicitly granted READ_ATTRIBUTES;
    // do not let a standard-library file open request content or sync access.
    let attributes = unsafe { GetFileAttributesW(wide(&root).as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        let code = unsafe { GetLastError() };
        return match code {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
            code => Err(error("capacity journal directory attributes", code)),
        };
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || attributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(fail("capacity journal root is not an ordinary directory"));
    }
    // The authoritative ProgramData ancestor must also remain non-reparse.
    for ancestor in root.ancestors().skip(1) {
        let attributes = unsafe { GetFileAttributesW(wide(ancestor).as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(error("capacity journal ancestor attributes", unsafe {
                GetLastError()
            }));
        }
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(fail("capacity journal ancestor is a reparse point"));
        }
    }
    // Presence, including an unreadable/non-file/reparse marker, blocks writes.
    let attributes = unsafe { GetFileAttributesW(wide(&root.join("pending.json")).as_ptr()) };
    if attributes != INVALID_FILE_ATTRIBUTES {
        return Ok(true);
    }
    match unsafe { GetLastError() } {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
        code => Err(error("capacity journal marker attributes", code)),
    }
}

pub(super) struct Journal {
    path: PathBuf,
    last: Vec<u8>,
    token: String,
}
impl Journal {
    /// Reopen evidence only. The caller must prove the old device is gone and
    /// bind every remaining resource before retiring this startup gate.
    pub(super) fn reopen() -> Result<(Self, serde_json::Value, Vec<u8>)> {
        let root=root_path()?;
        private_root(&root)?;
        let path=root.join("pending.json");
        let last=read(&path)?;
        let value:serde_json::Value=serde_json::from_slice(&last).map_err(|e|fail(e.to_string()))?;
        let token=value["owner_token"].as_str().ok_or_else(||fail("capacity journal token missing"))?.to_owned();
        if Uuid::parse_str(&token).map(|id|id.to_string()).ok().as_deref()!=Some(token.as_str()) {
            return Err(fail("capacity journal token is not canonical"));
        }
        let backup=root.join(format!("{token}.original.xml"));
        if value["backup"].as_str()!=backup.to_str(){return Err(fail("capacity backup binding changed"));}
        let original=read(&backup)?;
        Ok((Self{path,last,token},value,original))
    }
    pub(super) fn create(original: &[u8], value: serde_json::Value) -> Result<Self> {
        let root = root()?;
        let token = Uuid::new_v4().to_string();
        let path = root.join("pending.json");
        let mut marker = create_marker(&path)?;
        // The marker exists before backup creation. If storage fails we keep it
        // as an explicit recovery gate rather than silently allowing a reload.
        let mut journal = Self {
            path,
            last: Vec::new(),
            token,
        };
        let backup = root.join(format!("{}.original.xml", journal.token));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&backup)
            .map_err(|e| io("create original XML backup", e))?;
        write_handle(&mut file, original)?;
        let mut value = value;
        value["schema"] = "eclipse-capacity-transaction/v1".into();
        value["owner_token"] = journal.token.clone().into();
        value["backup"] = backup.to_string_lossy().to_string().into();
        value["phase"] = "before_mutation".into();
        let bytes = serde_json::to_vec_pretty(&value).map_err(|e| fail(e.to_string()))?;
        write_handle(&mut marker, &bytes)?;
        journal.last = bytes;
        Ok(journal)
    }
    pub(super) fn phase(&mut self, phase: &str) -> Result<()> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&self.last).map_err(|e| fail(e.to_string()))?;
        value["phase"] = phase.into();
        let bytes = serde_json::to_vec_pretty(&value).map_err(|e| fail(e.to_string()))?;
        replace_exact(&self.path, &self.last, &bytes)?;
        self.last = bytes;
        Ok(())
    }
    pub(super) fn record(&mut self, key: &str, value: serde_json::Value) -> Result<()> {
        let mut state: serde_json::Value =
            serde_json::from_slice(&self.last).map_err(|e| fail(e.to_string()))?;
        state[key] = value;
        let bytes = serde_json::to_vec_pretty(&state).map_err(|e| fail(e.to_string()))?;
        replace_exact(&self.path, &self.last, &bytes)?;
        self.last = bytes;
        Ok(())
    }
    pub(super) fn complete(&mut self, phase: &str) -> Result<()> {
        self.phase(phase)?;
        private_root(
            self.path
                .parent()
                .ok_or_else(|| fail("capacity journal parent missing"))?,
        )?;
        if read(&self.path)? != self.last {
            return Err(fail("capacity journal ownership changed"));
        }
        let receipt = self
            .path
            .with_file_name(format!("{}.receipt.json", self.token));
        if receipt.exists() {
            return Err(fail("capacity receipt already exists"));
        }
        std::fs::rename(&self.path, receipt).map_err(|e| io("finish capacity journal", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Owned(PathBuf);
    impl Owned {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("eclipse-capacity-file-test-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            for name in ["config.xml", "pending.json"] {
                let _ = std::fs::remove_file(self.0.join(name));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn exclusive_cas_preserves_raw_bytes_and_rejects_a_stale_snapshot() {
        let owned = Owned::new();
        let path = owned.0.join("config.xml");
        let original =
            b"\xef\xbb\xbf<vdd_settings>\r\n<monitors><count>1</count></monitors></vdd_settings>";
        std::fs::write(&path, original).unwrap();
        let changed = crate::virtual_display::capacity_config::expand(original, 16).unwrap();
        replace_exact(&path, original, &changed.after).unwrap();
        assert_eq!(read(&path).unwrap(), changed.after);
        assert!(replace_exact(&path, original, b"do not overwrite").is_err());
        assert_eq!(read(&path).unwrap(), changed.after);
        replace_exact(&path, &changed.after, original).unwrap();
        assert_eq!(read(&path).unwrap(), original);
    }
    #[test]
    fn another_open_owner_prevents_cas_without_overwriting_its_file() {
        let owned = Owned::new();
        let path = owned.0.join("config.xml");
        std::fs::write(&path, b"owned bytes").unwrap();
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(replace_exact(&path, b"owned bytes", b"changed").is_err());
        drop(handle);
        assert_eq!(read(&path).unwrap(), b"owned bytes");
    }
    #[test]
    fn a_foreign_journal_edit_retains_the_marker_and_cannot_be_archived() {
        let owned = Owned::new();
        let path = owned.0.join("pending.json");
        let original = b"{\"phase\":\"before_mutation\"}".to_vec();
        std::fs::write(&path, &original).unwrap();
        let mut journal = Journal {
            path: path.clone(),
            last: original,
            token: Uuid::new_v4().to_string(),
        };
        journal.phase("enable_submitted").unwrap();
        std::fs::write(&path, b"external recovery evidence").unwrap();
        assert!(journal.phase("committed").is_err());
        assert_eq!(read(&path).unwrap(), b"external recovery evidence");
        assert!(path.exists());
    }
}
