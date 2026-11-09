//! Bounded download of the pinned signed driver. No downloaded code is run;
//! installation independently verifies payloads and Authenticode in protected staging.
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct Downloaded {
    root: PathBuf,
    pub directory: PathBuf,
}
impl Drop for Downloaded {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn powershell() -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    ensure!(
        length > 0 && length < buffer.len(),
        "cannot resolve the Windows system directory"
    );
    use std::os::windows::ffi::OsStringExt;
    Ok(
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
            .join("WindowsPowerShell/v1.0/powershell.exe"),
    )
}
fn download_to_owned_directory() -> Result<Downloaded> {
    let root = std::env::temp_dir().join(format!("eclipse-vdd-download-{}", uuid::Uuid::new_v4()));
    // The background host may run as System. Do not inherit a user's writable
    // temporary-directory ACL across the later privileged extraction stage.
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
    for parent in root.parent().into_iter().flat_map(Path::ancestors) {
        ensure!(
            std::fs::symlink_metadata(parent)?.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                == 0,
            "driver download path contains a reparse point"
        );
    }
    let descriptor = crate::service_admin::windows::Descriptor::administrators()?;
    let path: Vec<_> = root.as_os_str().encode_wide().chain([0]).collect();
    crate::service_admin::windows::winok(unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateDirectoryW(
            path.as_ptr(),
            &descriptor.attributes(),
        )
    })?;
    let downloaded = Downloaded {
        directory: root.join("package"),
        root,
    };
    std::fs::create_dir(&downloaded.directory)?;
    let package = super::super::driver_package();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let bytes = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("Eclipse virtual display installer")
            .build()?;
        let mut response = match client.get(&package.url).send().await.and_then(reqwest::Response::error_for_status) {
            Ok(response)=>response,
            Err(primary)=>client.get(&package.api_url).header(reqwest::header::ACCEPT,"application/octet-stream")
                .send().await.and_then(reqwest::Response::error_for_status)
                .map_err(|fallback|anyhow::anyhow!("signed driver download failed: {primary}; official asset API: {fallback}"))?,
        };
        const MAX: usize = 512 * 1024;
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX as u64),
            "driver archive exceeds size limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= MAX,
                "driver archive exceeds size limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok::<_, anyhow::Error>(bytes)
    })?;
    ensure!(
        hex_digest(&bytes) == package.zip_sha256,
        "downloaded driver archive does not match the pinned release"
    );
    let archive = downloaded.root.join("driver.zip");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&archive)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    extract(&archive, &downloaded.directory)?;
    for file in &package.files {
        let bytes = std::fs::read(downloaded.directory.join(&file.name))?;
        ensure!(
            bytes.len() == file.bytes && hex_digest(&bytes) == file.sha256,
            "extracted driver file does not match the pinned release: {}",
            file.name
        );
    }
    Ok(downloaded)
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn extract(archive: &Path, directory: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    // All paths are environment values, never interpolated into script syntax.
    // No downloaded executable or script is run. Only four fixed files from a
    // digest-verified archive can be written, all beneath this owned directory.
    const SCRIPT: &str = r#"
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$allowed=@('VirtualDisplayDriver/mttvdd.cat','VirtualDisplayDriver/MttVDD.dll','VirtualDisplayDriver/MttVDD.inf','VirtualDisplayDriver/vdd_settings.xml')
$seen=@{}
$zip=[System.IO.Compression.ZipFile]::OpenRead($env:ECLIPSE_VDD_ARCHIVE)
try {
  foreach($entry in $zip.Entries) {
    if([string]::IsNullOrEmpty($entry.Name)){continue}
    if(-not ($allowed -ccontains $entry.FullName)){throw 'Unexpected archive member'}
    if($seen.ContainsKey($entry.FullName)){throw 'Duplicate archive member'}
    if($entry.Length -gt 1048576){throw 'Archive member is too large'}
    $seen[$entry.FullName]=$true
    $destinationStream=[System.IO.File]::Open([System.IO.Path]::Combine($env:ECLIPSE_VDD_OUTPUT,$entry.Name),[System.IO.FileMode]::CreateNew,[System.IO.FileAccess]::Write,[System.IO.FileShare]::None)
    $entryStream=$entry.Open()
    try{$entryStream.CopyTo($destinationStream)}finally{$entryStream.Dispose();$destinationStream.Dispose()}
  }
  if($seen.Count -ne 4){throw 'Driver archive is incomplete'}
} finally {$zip.Dispose()}
"#;
    let mut child = Command::new(powershell()?)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("ECLIPSE_VDD_ARCHIVE", archive)
        .env("ECLIPSE_VDD_OUTPUT", directory)
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("driver archive extraction timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "driver archive extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2048)
            .collect::<String>()
    );
    Ok(())
}
pub(super) fn download() -> super::super::preparation::Result<Vec<(String, Vec<u8>)>> {
    let result = (|| -> Result<_> {
        let downloaded = download_to_owned_directory()?;
        super::super::read_pinned_package(&downloaded.directory).map_err(anyhow::Error::new)
    })();
    result.map_err(|error| super::super::preparation::Failure::unchanged(error.to_string()))
}
