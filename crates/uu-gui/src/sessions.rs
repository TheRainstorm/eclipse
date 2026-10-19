//! The launcher owns device browsing; each stream owns its own process, window,
//! input state and controller. Closing one stream cannot stop another device.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConnectionTarget {
    UuDevice(String),
    NativeInvitation(PathBuf),
}
#[derive(Clone, Debug, PartialEq)]
pub struct StreamTarget {
    pub connection: ConnectionTarget,
    pub alias: String,
    pub view_only: bool,
}
impl StreamTarget {
    pub fn is_native(&self) -> bool {
        matches!(self.connection, ConnectionTarget::NativeInvitation(_))
    }
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.into_iter();
        let (mut connection, mut alias, mut view_only) = (None, None, false);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--stream-device" | "--native-invitation" => {
                    if connection.is_some() {
                        return Err("一次只能选择一种连接方式".into());
                    }
                    let value = args
                        .next()
                        .filter(|s| !s.is_empty())
                        .ok_or("缺少连接目标")?;
                    connection = Some(if arg == "--stream-device" {
                        ConnectionTarget::UuDevice(value)
                    } else {
                        ConnectionTarget::NativeInvitation(value.into())
                    });
                }
                "--stream-alias" => alias = Some(args.next().ok_or("缺少设备名称")?),
                "--view-only" => view_only = true,
                _ => return Err(format!("未知参数：{arg}")),
            }
        }
        match connection {
            Some(connection) => {
                let default = match &connection {
                    ConnectionTarget::UuDevice(id) => id.clone(),
                    ConnectionTarget::NativeInvitation(_) => "原生直连".into(),
                };
                view_only |= matches!(connection, ConnectionTarget::NativeInvitation(_));
                Ok(Some(Self {
                    connection,
                    alias: alias.unwrap_or(default),
                    view_only,
                }))
            }
            None if alias.is_some() || view_only => Err("串流参数需要设备或原生邀请".into()),
            None => Ok(None),
        }
    }
    fn command(&self, exe: &Path, origin: Option<(i32, i32)>) -> Command {
        let mut command = Command::new(exe);
        match &self.connection {
            ConnectionTarget::UuDevice(id) => {
                command.arg("--stream-device").arg(id);
            }
            ConnectionTarget::NativeInvitation(path) => {
                command.arg("--native-invitation").arg(path);
            }
        }
        command
            .arg("--stream-alias")
            .arg(&self.alias)
            .env_remove("UU_AUTOCONNECT");
        if let Some((x, y)) = origin {
            command.env("UU_WINDOW_ORIGIN", format!("{x},{y}"));
        } else {
            command.env_remove("UU_WINDOW_ORIGIN");
        }
        if self.view_only {
            command.arg("--view-only");
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
    }
}

#[derive(Default)]
pub struct Sessions {
    children: HashMap<ConnectionTarget, Child>,
}
impl Sessions {
    pub fn reap(&mut self) {
        self.children
            .retain(|_, child| !matches!(child.try_wait(), Ok(Some(_))));
    }
    pub fn open(
        &mut self,
        exe: &Path,
        target: &StreamTarget,
        origin: Option<(i32, i32)>,
    ) -> std::io::Result<bool> {
        self.reap();
        let key = match &target.connection {
            ConnectionTarget::NativeInvitation(path) => ConnectionTarget::NativeInvitation(
                path.canonicalize().unwrap_or_else(|_| path.clone()),
            ),
            other => other.clone(),
        };
        if let Some(child) = self.children.get(&key) {
            focus_process(child.id());
            return Ok(false);
        }
        let child = target.command(exe, origin).spawn()?;
        self.children.insert(key, child);
        Ok(true)
    }
    // Child's Drop does not terminate the process. Streams may continue when
    // the user closes the launcher; every stream performs its own cleanup.
}

fn focus_process(_pid: u32) {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        extern "system" {
            fn EnumWindows(
                callback: unsafe extern "system" fn(isize, isize) -> i32,
                data: isize,
            ) -> i32;
            fn GetWindowThreadProcessId(window: isize, pid: *mut u32) -> u32;
            fn IsWindowVisible(window: isize) -> i32;
            fn IsIconic(window: isize) -> i32;
            fn ShowWindow(window: isize, command: i32) -> i32;
            fn SetForegroundWindow(window: isize) -> i32;
        }
        unsafe extern "system" fn focus(window: isize, pid: isize) -> i32 {
            let mut actual = 0;
            GetWindowThreadProcessId(window, &mut actual);
            if actual == pid as u32 && IsWindowVisible(window) != 0 {
                if IsIconic(window) != 0 {
                    ShowWindow(window, 9);
                }
                SetForegroundWindow(window);
                return 0;
            }
            1
        }
        unsafe {
            EnumWindows(focus, _pid as isize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_arguments_preserve_device_alias_and_mode_without_recursive_autoconnect() {
        let target = StreamTarget {
            connection: ConnectionTarget::UuDevice("device-a".into()),
            alias: "工作电脑 A 中文".into(),
            view_only: true,
        };
        let command = target.command(Path::new("uu-gui.exe"), Some((1540, -564)));
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(StreamTarget::parse(args).unwrap(), Some(target));
        assert!(command
            .get_envs()
            .any(|(k, v)| k == "UU_AUTOCONNECT" && v.is_none()));
        assert!(command
            .get_envs()
            .any(|(k, v)| k == "UU_WINDOW_ORIGIN" && v == Some(std::ffi::OsStr::new("1540,-564"))));
        assert!(StreamTarget::parse(Vec::new()).unwrap().is_none());
        assert!(StreamTarget::parse(["--stream-device".into()]).is_err());
    }
    #[test]
    fn native_invitation_is_an_explicit_read_only_target_and_roundtrips_without_an_account_device()
    {
        let target = StreamTarget::parse([
            "--native-invitation".into(),
            "session.eclipse-invite.json".into(),
        ])
        .unwrap()
        .unwrap();
        assert!(target.is_native() && target.view_only);
        let command = target.command(Path::new("uu-gui"), None);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect();
        assert!(!args.iter().any(|arg| arg == "--stream-device"));
        assert_eq!(StreamTarget::parse(args).unwrap(), Some(target));
        assert!(StreamTarget::parse([
            "--stream-device".into(),
            "test-device".into(),
            "--native-invitation".into(),
            "session.eclipse-invite.json".into()
        ])
        .is_err());
    }
    #[cfg(unix)]
    #[test]
    fn finished_device_is_reaped_without_terminating_other_streams() {
        let mut completed = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        completed.wait().unwrap();
        let running = Command::new("sleep").arg("30").spawn().unwrap();
        let mut sessions = Sessions {
            children: HashMap::from([
                (ConnectionTarget::UuDevice("finished".into()), completed),
                (ConnectionTarget::UuDevice("running".into()), running),
            ]),
        };
        sessions.reap();
        assert!(!sessions
            .children
            .contains_key(&ConnectionTarget::UuDevice("finished".into())));
        assert!(sessions.children[&ConnectionTarget::UuDevice("running".into())].id() != 0);
        let child = sessions
            .children
            .get_mut(&ConnectionTarget::UuDevice("running".into()))
            .unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
