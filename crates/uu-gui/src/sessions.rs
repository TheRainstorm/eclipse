//! The launcher hands connections to the streaming workspace. Each session
//! retains its own input state and controller inside that process.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};
pub const MAX_PLUGIN_PAYLOAD:usize=16*1024;
pub fn read_plugin_payload(reader:impl std::io::Read)->anyhow::Result<zeroize::Zeroizing<String>> {
    use std::io::Read;
    let mut bytes=zeroize::Zeroizing::new(Vec::new());reader.take(MAX_PLUGIN_PAYLOAD as u64+1).read_to_end(&mut bytes)?;
    anyhow::ensure!(!bytes.is_empty()&&bytes.len()<=MAX_PLUGIN_PAYLOAD,"invalid plugin launch payload size");
    Ok(zeroize::Zeroizing::new(std::str::from_utf8(&bytes)?.to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash,serde::Serialize,serde::Deserialize)]
pub enum ConnectionTarget {
    Plugin {plugin:String,identity:String},
    NativeInvitation(PathBuf),
    NativeDevice(String),
    NativeInternet(PathBuf),
    NativeProfile {id:String,snapshot:PathBuf},
}
#[derive(Clone, Debug, PartialEq,serde::Serialize,serde::Deserialize)]
pub struct StreamTarget {
    pub connection: ConnectionTarget,
    pub alias: String,
    pub view_only: bool,
}
impl StreamTarget {
    pub fn key(&self)->ConnectionTarget {
        match &self.connection {
            ConnectionTarget::NativeProfile{id,..}=>ConnectionTarget::NativeProfile{id:id.clone(),snapshot:PathBuf::new()},
            ConnectionTarget::NativeInvitation(path)=>ConnectionTarget::NativeInvitation(path.canonicalize().unwrap_or_else(|_|path.clone())),
            other=>other.clone(),
        }
    }
    pub fn is_native(&self) -> bool {
        matches!(self.connection, ConnectionTarget::NativeInvitation(_) | ConnectionTarget::NativeDevice(_) | ConnectionTarget::NativeInternet(_) | ConnectionTarget::NativeProfile{..})
    }
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.into_iter();
        let (mut connection, mut alias, mut view_only) = (None, None, false);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--plugin" | "--native-invitation" | "--native-device" | "--native-internet" | "--device-profile" => {
                    if connection.is_some() {
                        return Err(uu_i18n::text("一次只能选择一种连接方式").into());
                    }
                    let value = args
                        .next()
                        .filter(|s| !s.is_empty())
                        .ok_or(uu_i18n::text("缺少连接目标"))?;
                    connection = Some(if arg == "--plugin" {
                        if value.len()>64||!value.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-'){return Err("invalid plugin id".into());}
                        let identity=args.next().filter(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control)).ok_or("missing public plugin target identity")?;
                        ConnectionTarget::Plugin{plugin:value,identity}
                    } else if arg=="--device-profile" {
                        let path=PathBuf::from(value);
                        let launch=uu_core::device_book::Launch::read(&path).map_err(|e|uu_i18n::format!("无法读取设备配置：{e}"))?;
                        ConnectionTarget::NativeProfile{id:launch.profile,snapshot:path}
                    } else if arg=="--native-internet" {
                        ConnectionTarget::NativeInternet(value.into())
                    } else if arg=="--native-device" {
                        if uuid::Uuid::parse_str(&value).is_err(){return Err(uu_i18n::text("无效的原生设备编号").into());}
                        ConnectionTarget::NativeDevice(value)
                    } else {
                        ConnectionTarget::NativeInvitation(value.into())
                    });
                }
                "--stream-alias" => alias = Some(args.next().ok_or(uu_i18n::text("缺少设备名称"))?),
                "--view-only" => view_only = true,
                _ => return Err(uu_i18n::format!("未知参数：{arg}")),
            }
        }
        match connection {
            Some(connection) => {
                let default = match &connection {
                    ConnectionTarget::Plugin {identity,..} => identity.clone(),
                    ConnectionTarget::NativeInvitation(_) | ConnectionTarget::NativeDevice(_) | ConnectionTarget::NativeInternet(_) | ConnectionTarget::NativeProfile{..} => uu_i18n::text("原生设备").into(),
                };
                Ok(Some(Self {
                    connection,
                    alias: alias.unwrap_or(default),
                    view_only,
                }))
            }
            None if alias.is_some() || view_only => Err(uu_i18n::text("串流参数需要设备或原生邀请").into()),
            None => Ok(None),
        }
    }
    fn command(&self, exe: &Path, origin: Option<(i32, i32)>) -> Command {
        let mut command = Command::new(exe);
        match &self.connection {
            ConnectionTarget::Plugin{plugin,identity}=>{command.arg("--plugin").arg(plugin).arg(identity).stdin(Stdio::piped());}
            ConnectionTarget::NativeInvitation(path) => {
                command.arg("--native-invitation").arg(path);
            }
            ConnectionTarget::NativeDevice(id) => {command.arg("--native-device").arg(id);}
            ConnectionTarget::NativeInternet(path) => {command.arg("--native-internet").arg(path);}
            ConnectionTarget::NativeProfile{snapshot,..}=>{command.arg("--device-profile").arg(snapshot);}
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
        self.open_inner(exe,target,origin,None)
    }
    pub fn open_plugin(&mut self,exe:&Path,target:&StreamTarget,payload:&str,origin:Option<(i32,i32)>)->std::io::Result<bool> {
        self.open_inner(exe,target,origin,Some(payload))
    }
    fn open_inner(&mut self,exe:&Path,target:&StreamTarget,origin:Option<(i32,i32)>,payload:Option<&str>)->std::io::Result<bool> {
        self.reap();
        if matches!(target.connection,ConnectionTarget::Plugin{..}) != payload.is_some() || payload.is_some_and(|s|s.is_empty()||s.len()>MAX_PLUGIN_PAYLOAD) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,"invalid plugin launch payload"));
        }
        let key = match &target.connection {
            ConnectionTarget::NativeProfile{id,..}=>ConnectionTarget::NativeProfile{id:id.clone(),snapshot:PathBuf::new()},
            ConnectionTarget::NativeInvitation(path) => ConnectionTarget::NativeInvitation(
                path.canonicalize().unwrap_or_else(|_| path.clone()),
            ),
            other => other.clone(),
        };
        // Even an existing target goes through IPC: only the workspace knows
        // which tab and detached native window currently contain that session.
        let mut child = target.command(exe, origin).spawn()?;
        if let Some(payload)=payload {
            use std::io::Write;
            let result=child.stdin.take().ok_or_else(||std::io::Error::new(std::io::ErrorKind::BrokenPipe,"missing plugin launch pipe"))
                .and_then(|mut pipe|pipe.write_all(payload.as_bytes()));
            if let Err(error)=result {let _=child.kill();let _=child.wait();return Err(error);}
        }
        #[cfg(windows)]
        unsafe {
            // Called as part of the user's launch action while the launcher
            // owns foreground permission. The child may show its window later.
            if windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(child.id()) == 0 {
                eprintln!("stream window activation permission: {}", std::io::Error::last_os_error());
            }
        }
        self.children.insert(key, child);
        Ok(true)
    }
    // Child's Drop does not terminate the process. Streams may continue when
    // the user closes the launcher; every stream performs its own cleanup.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_arguments_contain_only_public_identity_and_receive_a_bounded_opaque_payload() {
        let target=StreamTarget {connection:ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"generated-target".into()},alias:"Generated extension".into(),view_only:false};
        let command=target.command(Path::new("generated-gui"),None);
        let args:Vec<String>=command.get_args().map(|s|s.to_string_lossy().into_owned()).collect();
        assert_eq!(StreamTarget::parse(args).unwrap(),Some(target));
        assert_eq!(&*read_plugin_payload(std::io::Cursor::new(b"{\"credential\":\"generated-secret\"}")).unwrap(),"{\"credential\":\"generated-secret\"}");
        assert!(read_plugin_payload(std::io::Cursor::new(vec![b'A';MAX_PLUGIN_PAYLOAD+1])).is_err());
        assert!(read_plugin_payload(std::io::Cursor::new(Vec::<u8>::new())).is_err());
    }
    #[test]
    fn child_arguments_preserve_device_alias_and_mode_without_recursive_autoconnect() {
        let target = StreamTarget {
            connection: ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"device-a".into()},
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
    fn native_invitation_is_an_explicit_target_and_roundtrips_without_an_account_device()
    {
        let target = StreamTarget::parse([
            "--native-invitation".into(),
            "session.eclipse-invite.json".into(),
        ])
        .unwrap()
        .unwrap();
        assert!(target.is_native() && !target.view_only);
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
                (ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"finished".into()}, completed),
                (ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"running".into()}, running),
            ]),
        };
        sessions.reap();
        assert!(!sessions
            .children
            .contains_key(&ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"finished".into()}));
        assert!(sessions.children[&ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"running".into()}].id() != 0);
        let child = sessions
            .children
            .get_mut(&ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"running".into()})
            .unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

#[cfg(test)]mod lan_tests{
 use super::*;
 #[test]fn native_device_child_arguments_contain_only_public_identity_and_alias(){
  let id="01234567-89ab-4cde-8fab-0123456789ab";
  let target=StreamTarget::parse(["--native-device",id,"--stream-alias","Generated server"].into_iter().map(str::to_owned)).unwrap().unwrap();assert!(target.is_native());
  let command=target.command(Path::new("generated-gui"),None);let args:Vec<_>=command.get_args().map(|v|v.to_str().unwrap()).collect();assert_eq!(args,["--native-device",id,"--stream-alias","Generated server"]);
  assert!(StreamTarget::parse(["--native-device","invalid"].into_iter().map(str::to_owned)).is_err());assert!(StreamTarget::parse(["--native-device",id,"--native-invitation","generated.eclipse-invite.json"].into_iter().map(str::to_owned)).is_err());
 }
}

#[cfg(test)]
mod internet_target_tests {
 use super::*;
 #[test] fn internet_sessions_keep_native_input_and_pass_only_profile_location(){
 let target=StreamTarget::parse(["--native-internet","generated-settings.json","--view-only"].into_iter().map(str::to_owned)).unwrap().unwrap();
 assert!(target.is_native());assert!(target.view_only);
 let command=target.command(Path::new("eclipse"),None);
 let args:Vec<_>=command.get_args().map(|v|v.to_string_lossy().into_owned()).collect();
 assert_eq!(&args[..2],["--native-internet","generated-settings.json"]);
 assert!(StreamTarget::parse(["--native-internet","generated.json","--native-device","another"].into_iter().map(str::to_owned)).is_err());
 }
}
