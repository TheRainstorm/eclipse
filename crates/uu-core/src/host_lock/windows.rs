use super::{Backend, Desktop};
use anyhow::{ensure, Context, Result};
use std::{
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::System::{
    RemoteDesktop::*, Shutdown::LockWorkStation, Threading::GetCurrentProcessId,
};

pub(super) struct System;
impl Backend for System {
    fn desktop(&self) -> Result<Desktop> {
        let mut session = 0;
        ensure!(
            unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } != 0,
            "无法查询主机桌面会话"
        );
        ensure!(
            session != 0 && session == unsafe { WTSGetActiveConsoleSessionId() },
            "主机当前不在物理控制台桌面"
        );
        let mut buffer = ptr::null_mut();
        let mut bytes = 0;
        ensure!(
            unsafe {
                WTSQuerySessionInformationW(
                    WTS_CURRENT_SERVER_HANDLE,
                    session,
                    WTSSessionInfoEx,
                    &mut buffer,
                    &mut bytes,
                )
            } != 0,
            "无法查询 Windows 锁屏状态：{}",
            std::io::Error::last_os_error()
        );
        struct Buffer(*mut u16);
        impl Drop for Buffer {
            fn drop(&mut self) {
                unsafe {
                    WTSFreeMemory(self.0.cast());
                }
            }
        }
        let buffer = Buffer(buffer);
        ensure!(
            !buffer.0.is_null() && bytes as usize >= std::mem::size_of::<WTSINFOEXW>(),
            "Windows 锁屏状态不完整"
        );
        let info = unsafe { &*buffer.0.cast::<WTSINFOEXW>() };
        ensure!(info.Level == 1, "Windows 锁屏状态版本不支持");
        let info = unsafe { info.Data.WTSInfoExLevel1 };
        ensure!(
            info.SessionId == session && matches!(info.SessionFlags, 0 | 1),
            "Windows 锁屏状态尚未就绪"
        );
        Ok(Desktop {
            session,
            logon: info.LogonTime,
            locked: info.SessionFlags == WTS_SESSIONSTATE_LOCK as i32,
        })
    }
    fn lock(&self, expected: Desktop) -> Result<()> {
        let current = self.desktop()?;
        ensure!(
            current.session == expected.session
                && (expected.logon == 0 || current.logon == expected.logon),
            "主机登录会话已改变，未对其他用户执行锁屏"
        );
        if current.locked {
            return Ok(());
        }
        // This dedicated windowless worker follows the interactive input
        // desktop. Never invoke a shell or synthesize Win+L/Ctrl+Alt+Delete.
        uu_input::windows_desktop::sync_input_desktop().context("无法绑定待锁定的桌面")?;
        ensure!(
            unsafe { LockWorkStation() } != 0,
            "Windows 拒绝锁屏请求：{}",
            std::io::Error::last_os_error()
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let now = self.desktop()?;
            ensure!(
                now.session == current.session && now.logon == current.logon,
                "锁屏期间主机登录会话已改变"
            );
            if now.locked {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "Windows 尚未确认锁屏，请检查主机状态"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
