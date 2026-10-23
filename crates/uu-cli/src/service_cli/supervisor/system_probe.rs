//! A temporary System task invokes the production session launcher. It sends
//! generated pixels only; neither test process ever requests a real desktop.
use super::*;
use std::fs;

pub(crate) fn run(paths: &Paths) -> Result<()> {
    let directory = paths.programs.join("owned-fixture");
    ensure!(
        !directory.exists(),
        "refuse existing System probe directory"
    );
    super::super::install::directory(&directory)?;
    fs::copy(std::env::current_exe()?, directory.join("probe.exe"))?;
    let installed = paths.manifest()?;
    for name in ["libvpl.dll", "libgcc_s_seh-1.dll", "libstdc++-6.dll"] {
        fs::copy(
            paths.binary_directory(&installed).join(name),
            directory.join(name),
        )?;
    }
    let action = r#"$ErrorActionPreference='Stop'
$p=$null
try {
 $p=Start-Process -FilePath (Join-Path $PSScriptRoot 'probe.exe') -ArgumentList @('--ignored','--exact','service_cli::supervisor::system_probe::owned_system_generated_media','--test-threads=1','--nocapture') -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $PSScriptRoot 'probe.out') -RedirectStandardError (Join-Path $PSScriptRoot 'probe.err')
 [System.IO.File]::WriteAllText((Join-Path $PSScriptRoot 'probe.pid'),[string]$p.Id)
 if(!$p.WaitForExit(35000)){$p.Kill();$p.WaitForExit();throw 'owned System media probe timed out'}
 $p.WaitForExit();[System.IO.File]::WriteAllText((Join-Path $PSScriptRoot 'probe.result'),[string]$p.ExitCode)
} finally {if($null -ne $p -and !$p.HasExited){$p.Kill();$p.WaitForExit()}}
"#;
    let scheduler = r#"$ErrorActionPreference='Stop'
$service=New-Object -ComObject Schedule.Service;$service.Connect();$folder=$service.GetFolder('\')
$definition=$service.NewTask(0);$definition.Principal.UserId='SYSTEM';$definition.Principal.LogonType=5;$definition.Principal.RunLevel=1
$definition.Settings.Enabled=$true;$definition.Settings.AllowDemandStart=$true;$definition.Settings.DisallowStartIfOnBatteries=$false;$definition.Settings.StopIfGoingOnBatteries=$false;$definition.Settings.ExecutionTimeLimit='PT45S'
$action=$definition.Actions.Create(0);$action.Path=Join-Path $PSHOME 'powershell.exe';$action.Arguments='-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "'+(Join-Path $PSScriptRoot 'action.ps1')+'"'
$name='EclipseOwnedMedia-'+[guid]::NewGuid().ToString('N');$registered=$false;$task=$null
try {
 $task=$folder.RegisterTaskDefinition($name,$definition,2,'SYSTEM',$null,5,$null);$registered=$true;$null=$task.Run($null)
 $deadline=[DateTime]::UtcNow.AddSeconds(45)
 while(!(Test-Path -LiteralPath (Join-Path $PSScriptRoot 'probe.result'))){if([DateTime]::UtcNow -gt $deadline){throw 'owned System task timed out'};Start-Sleep -Milliseconds 100}
 if([System.IO.File]::ReadAllText((Join-Path $PSScriptRoot 'probe.result')).Trim() -ne '0') {throw 'owned System media test failed'}
} finally {
 if($registered){try{$task.Stop(0)}catch{};$folder.DeleteTask($name,0)}
 $pidFile=Join-Path $PSScriptRoot 'probe.pid'
 if(Test-Path -LiteralPath $pidFile){$ownedPid=[int][System.IO.File]::ReadAllText($pidFile);$owned=Get-Process -Id $ownedPid -ErrorAction SilentlyContinue;if($null -ne $owned -and $owned.Path -eq (Join-Path $PSScriptRoot 'probe.exe')){$owned.Kill();$owned.WaitForExit()}}
}
"#;
    fs::write(directory.join("action.ps1"), action)?;
    fs::write(directory.join("scheduler.ps1"), scheduler)?;
    for entry in fs::read_dir(&directory)? {
        super::super::install::protect(&entry?.path())?;
    }
    let mut windows = vec![0u16; 32768];
    let length = unsafe { GetSystemWindowsDirectoryW(windows.as_mut_ptr(), windows.len() as u32) };
    ensure!(
        length > 0 && (length as usize) < windows.len(),
        "missing Windows system directory"
    );
    use std::os::windows::ffi::OsStringExt;
    let powershell = PathBuf::from(std::ffi::OsString::from_wide(&windows[..length as usize]))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let result = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(directory.join("scheduler.ps1"))
        .output()?;
    let output = fs::read_to_string(directory.join("probe.out")).unwrap_or_default();
    let errors = fs::read_to_string(directory.join("probe.err")).unwrap_or_default();
    ensure!(
        result.status.success(),
        "System generated-media fixture failed: {} / {output} / {errors}",
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{output}");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match fs::remove_dir_all(&directory) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) if Instant::now() >= deadline => {
                return Err(error).context("owned System task log handles did not close")
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the owned protected fixture and a temporary System task; generated pixels only"]
async fn owned_system_generated_media() {
    assert_eq!(
        process_user(unsafe { GetCurrentProcess() }).unwrap(),
        "S-1-5-18"
    );
    let paths = Paths::discover().unwrap();
    assert!(std::env::current_exe()
        .unwrap()
        .canonicalize()
        .unwrap()
        .starts_with(paths.programs.join("owned-fixture").canonicalize().unwrap()));
    assert_eq!(
        uu_core::device_book::Store::open(paths.config().at("device-trust"))
            .unwrap()
            .read()
            .unwrap()
            .sharing
            .monitor,
        "eclipse-owned-no-capture"
    );
    for name in [
        "SeTcbPrivilege",
        "SeAssignPrimaryTokenPrivilege",
        "SeIncreaseQuotaPrivilege",
    ] {
        enable(name).unwrap();
    }
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    assert!(session > 0 && session != u32::MAX);
    let invitation = paths
        .root
        .join("generated-system-probe.eclipse-invite.json");
    let child = launch_entry(
        &paths,
        &paths.manifest().unwrap(),
        session,
        Entry::Generated(invitation.clone()),
    )
    .unwrap();
    let invite = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(invite) = uu_core::native_pairing::Invitation::load(&invitation) {
                break invite;
            }
            assert_ne!(
                unsafe { WaitForSingleObject(child.process.0, 0) },
                WAIT_OBJECT_0,
                "generated child exited before invitation"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let stats = std::sync::Arc::new(uu_core::media::MediaStats::default());
    stats
        .decoder_preference
        .set(uu_media::DecoderKind::Software)
        .unwrap();
    stats.audio.output_enabled.store(false, Ordering::Release);
    let (frames, mut received) = uu_core::media::frame_channel();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let copied = stats.clone();
    let viewer =
        tokio::spawn(async move { uu_core::native::view(&invite, frames, copied, receiver).await });
    let observed = tokio::time::timeout(Duration::from_secs(15), async {
        while stats.decoded_total.load(Ordering::Relaxed) < 12 {
            while let Ok((_, width, height, _)) = received.try_recv() {
                assert_eq!((width, height), (1280, 720));
            }
            if viewer.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    stop.send_replace(true);
    let viewed = tokio::time::timeout(Duration::from_secs(4), viewer)
        .await
        .unwrap()
        .unwrap();
    observed.unwrap();
    viewed.unwrap();
    assert!(stats.decoded_total.load(Ordering::Relaxed) >= 12);
    assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
    assert_eq!(
        unsafe { WaitForSingleObject(child.process.0, 4000) },
        WAIT_OBJECT_0
    );
    let mut code = 0;
    winok(unsafe { GetExitCodeProcess(child.process.0, &mut code) }).unwrap();
    assert_eq!(code, 0);
    drop(child);
    fs::remove_file(invitation).unwrap();
    // Holding the exact protected service process handle prevents PID reuse.
    let service = open_service(SERVICE_QUERY_STATUS).unwrap();
    fn status(service: &ServiceHandle) -> SERVICE_STATUS_PROCESS {
        let mut state: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
        let mut size = 0;
        winok(unsafe {
            QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&mut state as *mut SERVICE_STATUS_PROCESS).cast(),
                std::mem::size_of_val(&state) as u32,
                &mut size,
            )
        })
        .unwrap();
        state
    }
    let original = status(&service);
    assert_eq!(original.dwCurrentState, SERVICE_RUNNING);
    let process = Handle(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_TERMINATE
                | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
            0,
            original.dwProcessId,
        )
    });
    assert!(!process.0.is_null());
    assert_eq!(process_user(process.0).unwrap(), "S-1-5-18");
    let mut image = vec![0u16; 32768];
    let mut length = image.len() as u32;
    winok(unsafe { QueryFullProcessImageNameW(process.0, 0, image.as_mut_ptr(), &mut length) })
        .unwrap();
    use std::os::windows::ffi::OsStringExt;
    assert_eq!(
        PathBuf::from(std::ffi::OsString::from_wide(&image[..length as usize]))
            .canonicalize()
            .unwrap(),
        paths
            .binary_directory(&paths.manifest().unwrap())
            .join("uu.exe")
            .canonicalize()
            .unwrap()
    );
    winok(unsafe { TerminateProcess(process.0, 71) }).unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(process.0, 2000) },
        WAIT_OBJECT_0
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = status(&service);
            if current.dwCurrentState == SERVICE_RUNNING
                && current.dwProcessId != 0
                && current.dwProcessId != original.dwProcessId
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("SCM did not recover the owned crashed service");
    println!("System console-session hardware encoder delivered {} generated frames with zero decode errors and SCM recovered its owned crashed service; no desktop was captured",stats.decoded_total.load(Ordering::Relaxed));
}
