use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Coverage {
    pub profile: String,
    pub ipv4: bool,
    pub ipv6: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Report {
    pub program: String,
    pub warnings: Vec<String>,
    pub changed: usize,
    pub reachability: String,
    pub rules: serde_json::Value,
    pub profiles: serde_json::Value,
    pub networks: serde_json::Value,
    pub coverage: Vec<Coverage>,
}
impl Report {
    pub fn message(&self) -> String {
        let status = if self.changed > 0 {
            uu_i18n::text("已配置防火墙")
        } else {
            uu_i18n::text("已检查防火墙")
        };
        let complete = |ipv6| ["Domain", "Private"].iter().all(|profile| self.coverage.iter()
            .any(|c| c.profile == *profile && if ipv6 { c.ipv6 } else { c.ipv4 }));
        let label = |complete| uu_i18n::text(if complete { "完整覆盖" } else { "未完整覆盖" });
        let ipv4 = label(complete(false));
        let ipv6 = label(complete(true));
        let mut text = uu_i18n::format!("{status}：专用/域网络规则 IPv4 {ipv4}，IPv6 {ipv6}；包含服务端口、发现端口和串流 TCP/UDP 49000–49009。未自动开放公用网络。规则覆盖不代表实际可达，仍需从另一台电脑验证连接。");
        for warning in &self.warnings {
            text.push_str("\n");
            text.push_str(&warning_message(warning));
        }
        text
    }
}
fn warning_message(value: &str) -> String {
    match value {
        "No active network profile; remote reachability is unknown"=>uu_i18n::text("未检测到活动网络，远端可达性未知").into(),
        "Explicit inbound Block rules may apply; they were preserved"=>uu_i18n::text("存在可能适用的入站阻止规则，已保留；请检查系统或组织策略").into(),
        "Existing address/interface/service/security restrictions were preserved; verify the remote connection"=>uu_i18n::text("已保留地址、网卡、服务或身份限制，请从目标电脑验证连接").into(),
        _ if value.ends_with(": policy prevents local inbound rules")=>uu_i18n::format!("网络 {profile} 的策略不允许本地入站规则",profile=value.split(':').next().unwrap_or("?")),
        _ if value.ends_with(": ports are missing or restricted")=>uu_i18n::format!("{scope} 端口规则缺失或受限，可点击修复补齐双栈规则",scope=value.trim_end_matches(": ports are missing or restricted")),
        _=>value.into(),
    }
}
#[derive(Deserialize)]
struct Reply {
    ok: bool,
    error: Option<String>,
    report: Option<Report>,
    #[serde(default)]
    changes: Vec<serde_json::Value>,
}
/// Ordinary-error compensation only, with full rule/filter postimage checks.
/// No global firewall snapshot is ever imported.
pub struct Changes(Vec<serde_json::Value>);
impl Changes {
    pub fn rollback(&self) -> Result<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        execute(serde_json::json!({"action":"rollback","changes":self.0}))?;
        Ok(())
    }
}
struct ChildProcess(std::process::Child);
impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn execute(input: serde_json::Value) -> Result<Reply> {
    use std::os::windows::{ffi::OsStringExt, process::CommandExt};
    // Resolve the OS binary independently of PATH and caller-controlled environment.
    let mut directory = vec![0u16; 32768];
    let len = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            directory.as_mut_ptr(),
            directory.len() as u32,
        )
    } as usize;
    ensure!(
        len > 0 && len < directory.len(),
        "cannot resolve Windows system directory"
    );
    let executable = std::path::PathBuf::from(std::ffi::OsString::from_wide(&directory[..len]))
        .join("WindowsPowerShell/v1.0/powershell.exe");
    let script = include_str!("windows.ps1");
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut child = ChildProcess(
        Command::new(executable)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &encoded,
            ])
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    // A dedicated reader drains both pipes while we enforce a bounded child lifetime.
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        use std::io::Read;
        let mut b = Vec::new();
        let _ = stdout.take(4 * 1024 * 1024).read_to_end(&mut b);
        b
    });
    let errors = std::thread::spawn(move || {
        use std::io::Read;
        let mut b = Vec::new();
        let _ = stderr.take(256 * 1024).read_to_end(&mut b);
        b
    });
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input)?)?;
    let deadline = Instant::now() + super::OPERATION_TIMEOUT;
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let _ = output.join();
            let _ = errors.join();
            anyhow::bail!("firewall operation timed out; its result is unknown, inspect rules before retrying");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let bytes = output.join().unwrap_or_default();
    let errors = errors.join().unwrap_or_default();
    let reply: Reply = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "cannot read firewall result ({status}): {}",
            String::from_utf8_lossy(&errors)
        )
    })?;
    ensure!(
        reply.ok && status.success(),
        "{}",
        reply.error.as_deref().unwrap_or("firewall command failed")
    );
    Ok(reply)
}
fn run(
    action: &str,
    program: &Path,
    old: Option<&Path>,
    service_port: u16,
    discovery_port: u16,
) -> Result<(Report, Changes)> {
    ensure!(
        program.is_absolute() && program.is_file(),
        "firewall program must be an existing absolute executable"
    );
    ensure!(
        service_port != 0 && discovery_port != 0,
        "firewall service ports must be nonzero"
    );
    let reply = execute(
        serde_json::json!({"action":action,"program":program,"oldProgram":old,"servicePort":service_port,"discoveryPort":discovery_port}),
    )?;
    Ok((
        reply.report.context("firewall report missing")?,
        Changes(reply.changes),
    ))
}
pub fn inspect(program: &Path, service_port: u16, discovery_port: u16) -> Result<Report> {
    Ok(run("inspect", program, None, service_port, discovery_port)?.0)
}
pub fn ensure(program: &Path, service_port: u16, discovery_port: u16) -> Result<Report> {
    crate::service_admin::windows::require_administrator()?;
    Ok(run("ensure", program, None, service_port, discovery_port)?.0)
}
/// Explicit user repair supplements program-scoped dual-stack rules. Automatic
/// startup and migration keep using `ensure` and preserve existing scopes.
pub fn repair(program: &Path, service_port: u16, discovery_port: u16) -> Result<Report> {
    crate::service_admin::windows::require_administrator()?;
    Ok(run("repair", program, None, service_port, discovery_port)?.0)
}
pub fn migrate(
    old: Option<&Path>,
    program: &Path,
    service_port: u16,
    discovery_port: u16,
) -> Result<(Report, Changes)> {
    crate::service_admin::windows::require_administrator()?;
    run("migrate", program, old, service_port, discovery_port)
}
