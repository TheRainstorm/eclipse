//! Explicit terminal metadata export; no per-frame calls or background writer.
//! A missing/partial file is unknown. `complete` closes this Trace only:
//! retained/evicted/missing counts still govern any observation-window claim.
use super::{Row, State, STAGES};
use serde::Serialize;
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

const DIRECTORY_ENV: &str = "ECLIPSE_PIPELINE_TRACE_DIR";
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct Export {
    directory: PathBuf,
}

pub(super) fn failure(error: &io::Error) {
    // Do not expose the selected path or panic if stderr itself is closed.
    let _ = writeln!(
        io::stderr().lock(),
        "pipeline terminal export unavailable ({:?})",
        error.kind()
    );
}

impl Export {
    pub(super) fn configured(enabled: bool) -> Option<Self> {
        Self::configuration(enabled, || std::env::var_os(DIRECTORY_ENV))
    }
    fn configuration(enabled: bool, read: impl FnOnce() -> Option<OsString>) -> Option<Self> {
        if !enabled {
            return None;
        }
        let directory = PathBuf::from(read()?);
        match Self::new(directory) {
            Ok(export) => Some(export),
            Err(error) => {
                failure(&error);
                None
            }
        }
    }
    fn new(directory: PathBuf) -> io::Result<Self> {
        private_directory(&directory)?;
        Ok(Self { directory })
    }
    pub(super) fn write(
        &self,
        started: Instant,
        limit: usize,
        state: &State,
        poisoned: bool,
    ) -> io::Result<()> {
        if state.total == 0 {
            return Ok(());
        }
        let trace_id = uuid::Uuid::new_v4().to_string();
        let retained = state.rows.len() as u64;
        let snapshot = Snapshot {
            event: "video_pipeline_trace_end",
            version: 1,
            pid: std::process::id(),
            trace_id: &trace_id,
            endpoint: "local_api_observation_only",
            terminal_scope: "this_trace_last_owner_drop",
            reason: if poisoned {
                "state_poisoned"
            } else {
                "trace_drop"
            },
            clock: "client_monotonic",
            qpc_anchor: crate::socket_trace::clock_anchor(started),
            stages: STAGES,
            limit,
            total_frames: state.total,
            retained_frames: retained,
            evicted_frames: state.evicted,
            unmatched_events: state.missing,
            retained_all_frames: state.evicted == 0,
            retained_first_observation_us: state
                .rows
                .iter()
                .flat_map(|row| row.at_us)
                .flatten()
                .min(),
            retained_last_observation_us: state
                .rows
                .iter()
                .flat_map(|row| row.at_us)
                .flatten()
                .max(),
            frames: &state.rows,
            complete: !poisoned && state.total == retained.saturating_add(state.evicted),
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(io::Error::other)?;
        if bytes.len() > MAX_BYTES {
            return Err(io::Error::other("pipeline snapshot exceeds byte limit"));
        }
        let name = format!("pipeline-{}-{trace_id}.json", std::process::id());
        self.write_new(&name, &bytes)
    }
    fn write_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.write_owned(name, |file| file.write_all(bytes))
    }
    fn write_owned(
        &self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
    ) -> io::Result<()> {
        private_directory(&self.directory)?;
        let path = self.directory.join(name);
        let mut file = create_new(&path)?;
        // create_new succeeded, so only this owned file may be removed on
        // failure. Never use write_atomic: it replaces an existing target.
        let result = write(&mut file);
        drop(file);
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }
}

#[derive(Serialize)]
struct Snapshot<'a> {
    event: &'static str,
    version: u8,
    pid: u32,
    trace_id: &'a str,
    endpoint: &'static str,
    terminal_scope: &'static str,
    reason: &'static str,
    clock: &'static str,
    qpc_anchor: Option<crate::socket_trace::ClockAnchor>,
    stages: [&'static str; 9],
    limit: usize,
    total_frames: u64,
    retained_frames: u64,
    evicted_frames: u64,
    unmatched_events: u64,
    retained_all_frames: bool,
    retained_first_observation_us: Option<u64>,
    retained_last_observation_us: Option<u64>,
    frames: &'a VecDeque<Row>,
    // Last in the serialized object: an interrupted write cannot be parsed
    // as a successful terminal. This is not a whole-session lossless claim.
    complete: bool,
}

fn private_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "absolute diagnostic directory required",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "existing non-link directory required",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private diagnostic directory required",
            ));
        }
    }
    #[cfg(windows)]
    {
        windows::private_directory(path, &metadata)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn create_new(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
#[cfg(windows)]
fn create_new(path: &Path) -> io::Result<File> {
    windows::create_new(path)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        os::windows::{ffi::OsStrExt, fs::MetadataExt, io::FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{LocalFree, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
                SE_FILE_OBJECT,
            },
            *,
        },
        Storage::FileSystem::{
            CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_FLAG_OPEN_REPARSE_POINT,
        },
    };
    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    fn denied() -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private diagnostic DACL required",
        )
    }
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub(super) fn private_directory(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(denied());
        }
        let (mut owner, mut dacl, mut descriptor) =
            (ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        // Returned owner/ACL point into descriptor and remain valid until LocalFree.
        // https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-getnamedsecurityinfow
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _descriptor = Descriptor(descriptor);
        if owner.is_null() || dacl.is_null() {
            return Err(denied());
        }
        for index in 0..unsafe { (*dacl).AceCount } as u32 {
            let mut raw = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut raw) } == 0 || raw.is_null() {
                return Err(denied());
            }
            let header = unsafe { &*(raw as *const ACE_HEADER) };
            // winnt.h ACCESS_DENIED_ACE_TYPE=1; ACCESS_ALLOWED_ACE_TYPE=0.
            if header.AceType == 1 {
                continue;
            }
            if header.AceType != 0
                || (header.AceSize as usize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            {
                return Err(denied());
            }
            let ace = unsafe { &*(raw as *const ACCESS_ALLOWED_ACE) };
            let sid = ptr::addr_of!(ace.SidStart) as PSID;
            let private = unsafe {
                EqualSid(sid, owner) != 0
                    || [
                        WinLocalSystemSid,
                        WinBuiltinAdministratorsSid,
                        WinCreatorOwnerSid,
                        WinCreatorOwnerRightsSid,
                    ]
                    .into_iter()
                    .any(|kind| IsWellKnownSid(sid, kind) != 0)
            };
            if !private {
                return Err(denied());
            }
        }
        Ok(())
    }
    pub(super) fn create_new(path: &Path) -> io::Result<File> {
        let sddl: Vec<u16> = "D:P(A;;FA;;;OW)\0".encode_utf16().collect();
        let mut raw = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut raw,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let descriptor = Descriptor(raw);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // CREATE_NEW rejects every existing target; the private DACL is applied
        // at creation rather than after exposing any metadata bytes.
        let handle = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                GENERIC_WRITE,
                0,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Trace, EXPORT_LIMIT, LIMIT};
    use super::*;
    use std::sync::Arc;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "uu-pipeline-terminal-test-{}",
                uuid::Uuid::new_v4()
            ));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            builder.create(&path).unwrap();
            #[cfg(windows)]
            {
                use std::{os::windows::ffi::OsStrExt, ptr};
                use windows_sys::Win32::{
                    Foundation::LocalFree,
                    Security::{
                        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
                        SetFileSecurityW, DACL_SECURITY_INFORMATION,
                        PROTECTED_DACL_SECURITY_INFORMATION,
                    },
                };
                let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)\0".encode_utf16().collect();
                let mut raw = ptr::null_mut();
                assert_ne!(
                    unsafe {
                        ConvertStringSecurityDescriptorToSecurityDescriptorW(
                            sddl.as_ptr(),
                            1,
                            &mut raw,
                            ptr::null_mut(),
                        )
                    },
                    0
                );
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let ok = unsafe {
                    SetFileSecurityW(
                        wide.as_ptr(),
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        raw,
                    )
                };
                unsafe {
                    LocalFree(raw);
                }
                assert_ne!(ok, 0);
            }
            Self(path)
        }
        fn files(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        }
        fn trace(&self, enabled: bool) -> Trace {
            Trace::with_export(enabled, Some(Export::new(self.0.clone()).unwrap()))
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn disabled_does_not_read_export_configuration_and_empty_does_not_write() {
        assert!(Export::configuration(false, || panic!("disabled configuration read")).is_none());
        let root = Directory::new();
        let disabled = root.trace(false);
        assert_eq!(disabled.limit, LIMIT);
        disabled.packet(1, 0, 1, 1, true, Instant::now());
        drop(disabled);
        let empty = root.trace(true);
        assert_eq!(empty.limit, EXPORT_LIMIT);
        drop(empty);
        assert!(root.files().is_empty());
    }
    #[test]
    fn only_last_owner_exports_each_nonempty_trace_to_its_own_file() {
        let root = Directory::new();
        let trace = Arc::new(root.trace(true));
        trace.packet(1, 0, 9, 1, true, Instant::now());
        let last = trace.clone();
        drop(trace);
        assert!(root.files().is_empty());
        last.packet(1, 0, 9, 2, true, Instant::now());
        drop(last);
        let second = root.trace(true);
        second.packet(2, 0, 10, 3, true, Instant::now());
        drop(second);
        let files = root.files();
        assert_eq!(files.len(), 2);
        let values: Vec<serde_json::Value> = files
            .iter()
            .map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap())
            .collect();
        assert_ne!(values[0]["trace_id"], values[1]["trace_id"]);
        assert!(values.iter().any(|v| v["total_frames"] == 2));
        for value in values {
            assert_eq!(value["complete"], true);
            assert_eq!(value["terminal_scope"], "this_trace_last_owner_drop");
            assert_eq!(value["limit"], EXPORT_LIMIT);
        }
    }
    #[test]
    fn existing_file_is_never_replaced() {
        let root = Directory::new();
        let export = Export::new(root.0.clone()).unwrap();
        export.write_new("collision.json", b"original").unwrap();
        assert!(export.write_new("collision.json", b"replace").is_err());
        assert_eq!(
            fs::read(root.0.join("collision.json")).unwrap(),
            b"original"
        );
    }
    #[test]
    fn interrupted_write_removes_only_the_file_this_export_created() {
        let root = Directory::new();
        let export = Export::new(root.0.clone()).unwrap();
        export.write_new("preserve.json", b"original").unwrap();
        let result = export.write_owned("partial.json", |file| {
            file.write_all(b"{\"event\":\"partial\"")?;
            Err(io::Error::other("injected failure after partial write"))
        });
        assert!(result.is_err());
        assert!(!root.0.join("partial.json").exists());
        assert_eq!(fs::read(root.0.join("preserve.json")).unwrap(), b"original");
    }
    #[test]
    fn rejected_or_removed_directory_does_not_panic_or_interrupt_other_traces() {
        let root = Directory::new();
        assert!(Export::new(root.0.join("absent")).is_err());
        assert!(Export::new(PathBuf::from("relative")).is_err());
        let trace = root.trace(true);
        trace.packet(1, 0, 9, 1, true, Instant::now());
        fs::remove_dir(&root.0).unwrap();
        drop(trace);
        let ordinary = Trace::with_export(true, None);
        ordinary.packet(1, 0, 9, 2, true, Instant::now());
        assert_eq!(ordinary.report().unwrap()["total_frames"], 1);
    }
    #[test]
    fn terminal_preserves_evictions_and_marks_poisoned_state_incomplete() {
        let root = Directory::new();
        let mut trace = root.trace(true);
        trace.limit = 3;
        for rtp in 0..5 {
            trace.packet(1, 0, 9, rtp, true, Instant::now());
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lock = trace.state.lock().unwrap();
            panic!("test poison");
        }));
        drop(trace);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&root.files()[0]).unwrap()).unwrap();
        assert_eq!(value["total_frames"], 5);
        assert_eq!(value["retained_frames"], 3);
        assert_eq!(value["evicted_frames"], 2);
        assert_eq!(value["retained_all_frames"], false);
        assert_eq!(value["complete"], false);
        assert_eq!(value["reason"], "state_poisoned");
    }
    #[cfg(unix)]
    #[test]
    fn unix_links_public_directories_and_public_output_permissions_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = Directory::new();
        let link = root.0.with_extension("link");
        symlink(&root.0, &link).unwrap();
        assert!(Export::new(link.clone()).is_err());
        fs::remove_file(link).unwrap();
        let trace = root.trace(true);
        trace.packet(1, 0, 9, 1, true, Instant::now());
        drop(trace);
        assert_eq!(
            fs::metadata(&root.files()[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Export::new(root.0.clone()).is_err());
    }
}
