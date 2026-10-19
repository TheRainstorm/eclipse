//! The launcher owns device browsing; each stream owns its own process, window,
//! input state and controller. Closing one stream cannot stop another device.
use std::{
    collections::HashMap,
    path::Path,
    process::{Child, Command},
};

#[derive(Clone, Debug, PartialEq)]
pub struct StreamTarget {
    pub device_id: String,
    pub alias: String,
    pub view_only: bool,
}
impl StreamTarget {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.into_iter();
        let (mut id, mut alias, mut view_only) = (None, None, false);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--stream-device" => {
                    id = Some(args.next().filter(|s| !s.is_empty()).ok_or("缺少设备 ID")?)
                }
                "--stream-alias" => alias = Some(args.next().ok_or("缺少设备名称")?),
                "--view-only" => view_only = true,
                _ => return Err(format!("未知参数：{arg}")),
            }
        }
        match id {
            Some(device_id) => Ok(Some(Self {
                alias: alias.unwrap_or_else(|| device_id.clone()),
                device_id,
                view_only,
            })),
            None if alias.is_some() || view_only => Err("串流参数需要 --stream-device".into()),
            None => Ok(None),
        }
    }
    fn command(&self, exe: &Path) -> Command {
        let mut command = Command::new(exe);
        command
            .arg("--stream-device")
            .arg(&self.device_id)
            .arg("--stream-alias")
            .arg(&self.alias)
            // A child connects directly to this target, never launches another child.
            .env_remove("UU_AUTOCONNECT");
        if self.view_only {
            command.arg("--view-only");
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // No console window for a GUI child.
        }
        command
    }
}

#[derive(Default)]
pub struct Sessions {
    children: HashMap<String, Child>,
}
impl Sessions {
    pub fn reap(&mut self) {
        self.children
            .retain(|_, child| !matches!(child.try_wait(), Ok(Some(_))));
    }
    pub fn open(&mut self, exe: &Path, target: &StreamTarget) -> std::io::Result<bool> {
        self.reap();
        if let Some(child) = self.children.get(&target.device_id) {
            focus_process(child.id());
            return Ok(false);
        }
        let child = target.command(exe).spawn()?;
        self.children.insert(target.device_id.clone(), child);
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
            device_id: "device-a".into(),
            alias: "工作电脑 A 中文".into(),
            view_only: true,
        };
        let command = target.command(Path::new("uu-gui.exe"));
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(StreamTarget::parse(args).unwrap(), Some(target));
        assert!(command
            .get_envs()
            .any(|(k, v)| k == "UU_AUTOCONNECT" && v.is_none()));
        assert!(StreamTarget::parse(Vec::new()).unwrap().is_none());
        assert!(StreamTarget::parse(["--stream-device".into()]).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn finished_device_is_reaped_without_terminating_other_streams() {
        let mut completed = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        completed.wait().unwrap();
        let running = Command::new("sleep").arg("30").spawn().unwrap();
        let mut sessions = Sessions {
            children: HashMap::from([("finished".into(), completed), ("running".into(), running)]),
        };
        sessions.reap();
        assert!(!sessions.children.contains_key("finished"));
        assert!(sessions.children["running"].id() != 0);
        let child = sessions.children.get_mut("running").unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
