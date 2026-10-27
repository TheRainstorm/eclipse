//! Read-only, bounded hardware inventory for user-initiated UU registration.
//! Does not capture pixels, read serial numbers or change the device identity.
use serde::{Deserialize, Serialize};
use std::{io::Read, process::{Command, Stdio}, time::{Duration, Instant}};

#[derive(Serialize, Deserialize)]
pub(crate) struct ComputerInfo {
    pub os: String,
    pub base_board: String,
    pub cpu: String,
    pub video: Vec<String>,
    pub mac: String,
    pub memory: u64,
    pub screen: String,
}
impl Default for ComputerInfo {
    fn default() -> Self {
        Self {
            os: if cfg!(windows) { "Windows" } else { std::env::consts::OS }.into(),
            base_board: "unavailable".into(), cpu: "unavailable".into(),
            video: vec!["unavailable".into()], mac: String::new(), memory: 0,
            screen: "unavailable".into(),
        }
    }
}
impl ComputerInfo {
    pub fn collect() -> Self {
        let mut info = platform_info();
        fn clean(value: &str) -> String {
            value.trim().chars().filter(|c| !c.is_control()).take(256).collect()
        }
        for field in [&mut info.os, &mut info.base_board, &mut info.cpu, &mut info.screen] {
            *field = clean(field);
            if field.is_empty() { *field = "unavailable".into(); }
        }
        info.video = info.video.iter().take(8).map(|v| clean(v)).filter(|v| !v.is_empty()).collect();
        if info.video.is_empty() { info.video.push("unavailable".into()); }
        info.mac = clean(&info.mac);
        info
    }
}

/// Installation-specific GUID, stable even for account files from older builds.
/// Never borrow the official client's ids or the OS machine-wide identity.
pub(crate) fn installation_guid(client_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(format!("eclipse-uu-registration:{client_id}"));
    let mut bytes = [0;16]; bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // RFC 9562 application-specific UUID.
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn command_text(mut command: Command, limit: Duration) -> Option<String> {
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn().ok()?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() { return None; }
                let mut output = String::new();
                child.stdout.take()?.take(64*1024).read_to_string(&mut output).ok()?;
                return Some(output.trim_start_matches('\u{feff}').trim().to_owned());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => { let _=child.kill(); let _=child.wait(); return None; }
        }
    }
}
#[cfg(windows)]
fn platform_info() -> ComputerInfo {
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", include_str!("registration/windows.ps1")]);
    command_text(command, Duration::from_secs(8)).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
#[cfg(target_os="linux")]
fn platform_info() -> ComputerInfo {
    fn read(path: impl AsRef<std::path::Path>) -> String {
        let mut text=String::new();
        let _=std::fs::File::open(path).and_then(|f| f.take(64*1024).read_to_string(&mut text));
        text.trim().into()
    }
    let mut info=ComputerInfo::default();
    if let Some(name)=read("/etc/os-release").lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")) {
        info.os=name.trim_matches('"').into();
    }
    if let Some(name)=read("/proc/cpuinfo").lines().find_map(|l| {
        let (key,value)=l.split_once(':')?;
        (key.trim()=="model name" || key.trim()=="Hardware").then(||value.trim())
    }) { info.cpu=name.into(); }
    info.base_board=format!("{} {}",read("/sys/class/dmi/id/board_vendor"),read("/sys/class/dmi/id/board_name"));
    info.memory=read("/proc/meminfo").lines().find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0)/1024;
    let mut pci=Command::new("lspci");pci.arg("-mm");
    if let Some(output)=command_text(pci, Duration::from_secs(2)) {
        info.video=output.lines().filter_map(|l| {
            let fields:Vec<_>=l.split('"').collect();
            let class=*fields.get(1)?;
            (class.contains("VGA") || class.contains("3D controller") || class.contains("Display controller"))
                .then(||format!("{} {}",fields.get(3).unwrap_or(&""),fields.get(5).unwrap_or(&"")))
        }).collect();
    }
    if let Ok(entries)=std::fs::read_dir("/sys/class/net") {
        let mut devices:Vec<_>=entries.flatten().map(|e|e.path()).filter(|p| p.join("device").exists() && read(p.join("operstate"))=="up").collect();
        devices.sort();
        info.mac=devices.iter().map(|p|read(p.join("address"))).find(|mac|!mac.is_empty() && mac!="00:00:00:00:00:00").unwrap_or_default();
    }
    if let Ok(monitors)=crate::native::monitors() {
        if let Some(m)=monitors.iter().find(|m|m.primary).or_else(||monitors.first()) {
            info.screen=format!("{}x{}",m.width,m.height);
        }
    }
    info
}
#[cfg(not(any(windows,target_os="linux")))]
fn platform_info() -> ComputerInfo { ComputerInfo::default() }

#[cfg(test)]
mod tests {
    #[test]
    fn registration_guid_is_stable_per_installation_and_not_a_shared_placeholder() {
        let a=super::installation_guid("generated-client-one");
        assert_eq!(a, super::installation_guid("generated-client-one"));
        assert_ne!(a, super::installation_guid("generated-client-two"));
        assert!(uuid::Uuid::parse_str(&a).is_ok());
    }
}
