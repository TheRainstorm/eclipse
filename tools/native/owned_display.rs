//! Shared guard for input examples. Only the launcher-created Xvfb is a target.
pub fn check(display: &str) -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
    let pid: u32 = std::env::var("UU_INPUT_TEST_SERVER_PID")
        .context("run this probe through tools/native/input_smoke.py")?
        .parse()?;
    let number: u16 = display
        .strip_prefix(':')
        .context("owned Unix display required")?
        .parse()?;
    ensure!(
        std::env::var("DISPLAY").ok().as_deref() == Some(display),
        "owned display mismatch"
    );
    ensure!(
        fs::read_to_string(format!("/proc/{pid}/comm"))?.trim() == "Xvfb",
        "input fixture must use Xvfb"
    );
    ensure!(
        fs::read_to_string(format!("/tmp/.X{number}-lock"))?.trim() == pid.to_string(),
        "Xvfb ownership mismatch"
    );
    let authority =
        PathBuf::from(std::env::var_os("XAUTHORITY").context("private Xauthority required")?);
    let metadata = fs::metadata(authority)?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "private Xauthority permissions required"
    );
    Ok(())
}
