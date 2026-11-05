//! The launcher hands connections to the streaming workspace. Each session
//! retains its own input state and controller inside that process.
use std::{
    collections::HashMap,
    hash::Hash,
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
    NativeLoopback,
    NativeDevice(String),
    NativeInternet(PathBuf),
    NativeProfile {id:String,snapshot:PathBuf},
    NativeDisplay {connection:Box<ConnectionTarget>,display:DisplayTarget},
    NativeVirtual {connection:Box<ConnectionTarget>,virtual_display:VirtualDisplayTarget},
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualDisplayTarget {
    pub id:uuid::Uuid,
    pub spec:uu_core::virtual_display::Spec,
    #[serde(default,skip_serializing_if="std::ops::Not::not")]
    pub persistent:bool,
}
impl VirtualDisplayTarget {
    pub fn new(spec:uu_core::virtual_display::Spec)->Self {
        Self{id:uuid::Uuid::new_v4(),spec,persistent:false}
    }
    fn validate(&self)->Result<(),String> {
        if self.id.is_nil() {return Err("temporary display launch identity is empty".into());}
        // The host owns the canonical Spec validation and creation rules.
        Ok(())
    }
}
impl Hash for VirtualDisplayTarget {
    fn hash<H:std::hash::Hasher>(&self,state:&mut H) {
        self.id.hash(state);
        self.spec.width.hash(state);self.spec.height.hash(state);
        self.spec.refresh_millihz.hash(state);self.spec.hdr.hash(state);
        self.persistent.hash(state);
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash,serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayTarget {
    pub id:i32,
    pub source:String,
}
impl DisplayTarget {
    pub fn validate(&self)->Result<(),String> {
        if self.id<0 || self.source.len()>256 || self.source.chars().any(char::is_control) {
            return Err("invalid native display target".into());
        }
        Ok(())
    }
    pub fn resolve(&self,screens:&[uu_core::screen_control::RemoteScreen])->Result<i32,&'static str> {
        let mut matches=screens.iter().filter(|screen| if self.source.is_empty(){screen.id==self.id}else{screen.display_name==self.source});
        let screen=matches.next().ok_or_else(||uu_i18n::text("指定显示器不存在，请重新选择显示器"))?;
        if matches.next().is_some(){return Err(uu_i18n::text("指定显示器名称不唯一，请重新选择显示器"));}
        Ok(screen.id)
    }
}
impl ConnectionTarget {
    pub fn base(&self)->&Self {
        match self {Self::NativeDisplay{connection,..}|Self::NativeVirtual{connection,..}=>connection.base(),other=>other}
    }
    pub fn display(&self)->Option<&DisplayTarget> {
        match self {Self::NativeDisplay{display,..}=>Some(display),_=>None}
    }
    pub fn virtual_display(&self)->Option<&VirtualDisplayTarget> {
        match self {Self::NativeVirtual{virtual_display,..}=>Some(virtual_display),_=>None}
    }
    /// Durable key for per-display client settings. Physical views use the
    /// stable source name; explicit virtual views use their launch UUID.
    pub fn display_client_key(&self)->Option<String> {
        match self {
            Self::NativeDisplay{display,..} if !display.source.is_empty()=>Some(format!("display:{}",display.source)),
            Self::NativeDisplay{display,..}=>Some(format!("display-id:{}",display.id)),
            Self::NativeVirtual{virtual_display,..}=>Some(format!("virtual:{}",virtual_display.id)),
            _=>None,
        }
    }
    pub fn is_auxiliary_view(&self)->bool {
        matches!(self,Self::NativeDisplay{..}|Self::NativeVirtual{..})
    }
    /// Explicit views keep one display source for their entire connection.
    /// A normal connection may also acquire a runtime binding when detached.
    pub fn is_fixed_display_view(&self)->bool {self.is_auxiliary_view()}
    pub fn validate(&self)->Result<(),String> {
        let connection=match self {
            Self::NativeDisplay{connection,display}=>{display.validate()?;Some(connection)},
            Self::NativeVirtual{connection,virtual_display}=>{
                virtual_display.validate()?;
                if matches!(connection.as_ref(),Self::NativeLoopback) {return Err("temporary displays require an authenticated remote connection".into());}
                Some(connection)
            },
            _=>None,
        };
        if let Some(connection)=connection {
            if matches!(connection.as_ref(),Self::Plugin{..}|Self::NativeDisplay{..}|Self::NativeVirtual{..}) {
                return Err("native display target requires an unwrapped native connection".into());
            }
        }
        Ok(())
    }
    fn key(&self)->Self {
        match self {
            Self::NativeProfile{id,..}=>Self::NativeProfile{id:id.clone(),snapshot:PathBuf::new()},
            Self::NativeInvitation(path)=>Self::NativeInvitation(path.canonicalize().unwrap_or_else(|_|path.clone())),
            Self::NativeDisplay{connection,display}=>Self::NativeDisplay {
                connection:Box::new(connection.key()),
                // Host IDs may change after reconnect; a unique source name
                // identifies the explicit window independently of that ID.
                display:DisplayTarget{id:if display.source.is_empty(){display.id}else{0},source:display.source.clone()},
            },
            Self::NativeVirtual{connection,virtual_display}=>Self::NativeVirtual {
                connection:Box::new(connection.key()),virtual_display:virtual_display.clone(),
            },
            other=>other.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq,serde::Serialize,serde::Deserialize)]
pub struct StreamTarget {
    pub connection: ConnectionTarget,
    pub alias: String,
    pub view_only: bool,
}
impl StreamTarget {
    pub fn key(&self)->ConnectionTarget {
        self.connection.key()
    }
    pub fn is_native(&self) -> bool {
        !matches!(self.connection.base(),ConnectionTarget::Plugin{..})
    }
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.into_iter();
        let (mut connection, mut alias, mut view_only) = (None, None, false);
        let (mut display_id,mut display_source)=(None,None);
        let mut virtual_display=None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--self-stream" => {
                    if connection.is_some() {return Err(uu_i18n::text("一次只能选择一种连接方式").into());}
                    connection=Some(ConnectionTarget::NativeLoopback);
                    view_only=true;
                }
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
                "--display-id" => {
                    if display_id.is_some(){return Err("duplicate native display id".into());}
                    display_id=Some(args.next().ok_or("missing native display id")?.parse::<i32>().map_err(|_|"invalid native display id")?);
                }
                "--display-source" => {
                    if display_source.is_some(){return Err("duplicate native display source".into());}
                    display_source=Some(args.next().ok_or("missing native display source")?);
                }
                "--virtual-display" => {
                    if virtual_display.is_some(){return Err("duplicate temporary display launch".into());}
                    let value=args.next().ok_or("missing temporary display launch")?;
                    virtual_display=Some(serde_json::from_str::<VirtualDisplayTarget>(&value).map_err(|_|"invalid temporary display launch")?);
                }
                "--view-only" => view_only = true,
                _ => return Err(uu_i18n::format!("未知参数：{arg}")),
            }
        }
        match connection {
            Some(connection) => {
                let connection=if let Some(virtual_display)=virtual_display {
                    if display_id.is_some()||display_source.is_some(){return Err("temporary and existing display targets are mutually exclusive".into());}
                    ConnectionTarget::NativeVirtual{connection:Box::new(connection),virtual_display}
                }else if let Some(id)=display_id {
                    ConnectionTarget::NativeDisplay{connection:Box::new(connection),display:DisplayTarget{id,source:display_source.unwrap_or_default()}}
                }else{
                    if display_source.is_some(){return Err("native display source requires a display id".into());}
                    connection
                };
                connection.validate()?;
                view_only|=matches!(connection.base(),ConnectionTarget::NativeLoopback);
                let default = match connection.base() {
                    ConnectionTarget::Plugin {identity,..} => identity.clone(),
                    ConnectionTarget::NativeLoopback => uu_i18n::text("本机串流测试").into(),
                    ConnectionTarget::NativeInvitation(_) | ConnectionTarget::NativeDevice(_) | ConnectionTarget::NativeInternet(_) | ConnectionTarget::NativeProfile{..} => uu_i18n::text("原生设备").into(),
                    ConnectionTarget::NativeDisplay{..}|ConnectionTarget::NativeVirtual{..}=>unreachable!("base connection is unwrapped"),
                };
                Ok(Some(Self {
                    connection,
                    alias: alias.unwrap_or(default),
                    view_only,
                }))
            }
            None if alias.is_some() || view_only || display_id.is_some() || display_source.is_some() || virtual_display.is_some() => Err(uu_i18n::text("串流参数需要设备或原生邀请").into()),
            None => Ok(None),
        }
    }
    fn command(&self, exe: &Path, origin: Option<(i32, i32)>) -> Command {
        let mut command = Command::new(exe);
        match self.connection.base() {
            ConnectionTarget::NativeLoopback => {command.arg("--self-stream");}
            ConnectionTarget::Plugin{plugin,identity}=>{command.arg("--plugin").arg(plugin).arg(identity).stdin(Stdio::piped());}
            ConnectionTarget::NativeInvitation(path) => {
                command.arg("--native-invitation").arg(path);
            }
            ConnectionTarget::NativeDevice(id) => {command.arg("--native-device").arg(id);}
            ConnectionTarget::NativeInternet(path) => {command.arg("--native-internet").arg(path);}
            ConnectionTarget::NativeProfile{snapshot,..}=>{command.arg("--device-profile").arg(snapshot);}
            ConnectionTarget::NativeDisplay{..}|ConnectionTarget::NativeVirtual{..}=>unreachable!("base connection is unwrapped"),
        }
        if let Some(display)=self.connection.display(){command.arg("--display-id").arg(display.id.to_string()).arg("--display-source").arg(&display.source);}
        if let Some(virtual_display)=self.connection.virtual_display(){command.arg("--virtual-display").arg(serde_json::to_string(virtual_display).expect("temporary display launch serializes"));}
        command
            .arg("--stream-alias")
            .arg(&self.alias)
            .env_remove("UU_AUTOCONNECT");
        if let Some((x, y)) = origin {
            command.env("UU_WINDOW_ORIGIN", format!("{x},{y}"));
        } else {
            command.env_remove("UU_WINDOW_ORIGIN");
        }
        if self.view_only || matches!(self.connection.base(),ConnectionTarget::NativeLoopback) {
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
        target.connection.validate().map_err(|error|std::io::Error::new(std::io::ErrorKind::InvalidInput,error))?;
        if matches!(target.connection,ConnectionTarget::Plugin{..}) != payload.is_some() || payload.is_some_and(|s|s.is_empty()||s.len()>MAX_PLUGIN_PAYLOAD) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,"invalid plugin launch payload"));
        }
        let key = target.key();
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
    fn temporary_display_identity_and_arguments_preserve_launch_intent() {
        let virtual_display=VirtualDisplayTarget::new(uu_core::virtual_display::Spec{width:2560,height:1440,refresh_millihz:120_000,hdr:false});
        let wrap=|snapshot:&str,virtual_display:VirtualDisplayTarget|StreamTarget {
            connection:ConnectionTarget::NativeVirtual {
                connection:Box::new(ConnectionTarget::NativeProfile{id:"generated-profile".into(),snapshot:snapshot.into()}),virtual_display,
            },alias:"Generated computer".into(),view_only:true,
        };
        let first=wrap("first.json",virtual_display.clone());
        assert!(first.connection.is_auxiliary_view());
        assert!(first.connection.display().is_none());
        assert_eq!(first.connection.virtual_display(),Some(&virtual_display));
        assert_eq!(first.key(),wrap("second.json",virtual_display.clone()).key());
        assert_ne!(first.key(),wrap("first.json",VirtualDisplayTarget::new(virtual_display.spec)).key());
        assert_ne!(first.key(),first.connection.base().key());
        assert_eq!(serde_json::from_slice::<StreamTarget>(&serde_json::to_vec(&first).unwrap()).unwrap(),first);
        // Normal process handoff must not turn a temporary view into an ordinary
        // device connection by dropping its UUID or requested display spec.
        let device=StreamTarget{connection:ConnectionTarget::NativeVirtual {
            connection:Box::new(ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into())),virtual_display,
        },..first};
        let args=device.command(Path::new("generated-gui"),None).get_args().map(|arg|arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(StreamTarget::parse(args).unwrap(),Some(device));
    }
    #[test]
    fn temporary_display_targets_reject_nested_wrappers_plugins_and_loopback() {
        let wrap=|connection|ConnectionTarget::NativeVirtual {
            connection:Box::new(connection),virtual_display:VirtualDisplayTarget::new(Default::default()),
        };
        let base=ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into());
        for invalid in [
            wrap(ConnectionTarget::NativeLoopback),
            wrap(ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"generated".into()}),
            wrap(wrap(base.clone())),
            wrap(display(base.clone(),1,"Generated source")),
            display(wrap(base.clone()),1,"Generated source"),
            ConnectionTarget::NativeVirtual{connection:Box::new(base),virtual_display:VirtualDisplayTarget{id:uuid::Uuid::nil(),spec:Default::default(),persistent:false}},
        ] {
            let decoded:ConnectionTarget=serde_json::from_slice(&serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(decoded.validate().is_err());
        }
        let virtual_display=serde_json::to_string(&VirtualDisplayTarget::new(Default::default())).unwrap();
        for args in [
            vec!["--virtual-display".into(),virtual_display.clone()],
            vec!["--self-stream".into(),"--virtual-display".into(),virtual_display.clone()],
            vec!["--native-device".into(),"01234567-89ab-4cde-8fab-0123456789ab".into(),"--virtual-display".into(),virtual_display,"--display-id".into(),"1".into()],
        ] {assert!(StreamTarget::parse(args).is_err());}
    }
    fn display(connection:ConnectionTarget,id:i32,source:&str)->ConnectionTarget {
        ConnectionTarget::NativeDisplay{connection:Box::new(connection),display:DisplayTarget{id,source:source.into()}}
    }
    #[test]
    fn native_display_arguments_roundtrip_and_loopback_remains_read_only() {
        for args in [
            vec!["--native-invitation","generated.eclipse-invite.json","--display-id","7","--display-source","Stable source"],
            vec!["--display-id","7","--self-stream"],
            vec!["--native-internet","generated.json","--display-id","2","--display-source",""],
        ] {
            let target=StreamTarget::parse(args.into_iter().map(str::to_owned)).unwrap().unwrap();
            assert!(target.is_native());
            assert!(target.connection.display().is_some());
            let command=target.command(Path::new("generated-gui"),None);
            let args=command.get_args().map(|arg|arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
            assert_eq!(StreamTarget::parse(args).unwrap(),Some(target.clone()));
            if matches!(target.connection.base(),ConnectionTarget::NativeLoopback){assert!(target.view_only);}
        }
        let target=StreamTarget{connection:display(ConnectionTarget::NativeLoopback,0,""),alias:"Generated".into(),view_only:false};
        assert!(target.command(Path::new("generated-gui"),None).get_args().any(|arg|arg=="--view-only"));
    }
    #[test]
    fn per_display_settings_keys_follow_stable_display_identity_not_transient_ids() {
        let base=ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into());
        let a=display(base.clone(),1,"Stable source");
        let b=display(base.clone(),9,"Stable source");
        assert_eq!(a.display_client_key(),b.display_client_key());
        assert_eq!(display(base.clone(),7,"").display_client_key().as_deref(),Some("display-id:7"));
        let virtual_display=VirtualDisplayTarget::new(Default::default());
        assert_eq!(ConnectionTarget::NativeVirtual{connection:Box::new(base),virtual_display:virtual_display.clone()}.display_client_key(),Some(format!("virtual:{}",virtual_display.id)));
    }
    #[test]
    fn display_arguments_and_workspace_targets_reject_invalid_or_nested_selection() {
        for args in [
            vec!["--display-id","0"],
            vec!["--self-stream","--display-source","Source"],
            vec!["--self-stream","--display-id","-1"],
            vec!["--self-stream","--display-id","0","--display-id","1"],
            vec!["--self-stream","--display-id","0","--display-source","A","--display-source","B"],
            vec!["--plugin","example-addon","device","--display-id","0"],
        ] {assert!(StreamTarget::parse(args.into_iter().map(str::to_owned)).is_err());}
        assert!(display(ConnectionTarget::NativeLoopback,0,&"A".repeat(257)).validate().is_err());
        assert!(display(ConnectionTarget::NativeLoopback,0,"A\nB").validate().is_err());
        assert!(display(display(ConnectionTarget::NativeLoopback,0,"Source"),1,"Other").validate().is_err());
        let malformed=display(ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"device".into()},0,"");
        let decoded:ConnectionTarget=serde_json::from_slice(&serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert!(decoded.validate().is_err());
    }
    #[test]
    fn display_identity_normalizes_only_the_wrapped_connection_and_stable_source() {
        let target=|connection|StreamTarget{connection,alias:"Generated".into(),view_only:false};
        let profile=|snapshot:&str|ConnectionTarget::NativeProfile{id:"device-a".into(),snapshot:snapshot.into()};
        let first=target(display(profile("one.json"),7,"Source A"));
        assert_eq!(first.key(),target(display(profile("two.json"),42,"Source A")).key());
        assert_ne!(first.key(),target(display(profile("one.json"),7,"Source B")).key());
        assert_ne!(first.key(),target(profile("one.json")).key());
        assert_ne!(target(display(profile("one.json"),7,"")).key(),target(display(profile("one.json"),42,"")).key());
        // Existing unwrapped enum bytes are the persisted workspace/layout key.
        assert_eq!(serde_json::to_string(&target(profile("one.json")).key()).unwrap(),r#"{"NativeProfile":{"id":"device-a","snapshot":""}}"#);
        assert_eq!(serde_json::to_string(&target(ConnectionTarget::NativeLoopback).key()).unwrap(),r#""NativeLoopback""#);
        assert_eq!(serde_json::to_string(&target(ConnectionTarget::NativeDevice("device-a".into())).key()).unwrap(),r#"{"NativeDevice":"device-a"}"#);
        let executable=std::env::current_exe().unwrap();
        let equivalent=executable.parent().unwrap().join(".").join(executable.file_name().unwrap());
        assert_eq!(target(display(ConnectionTarget::NativeInvitation(equivalent),7,"Source A")).key(),target(display(ConnectionTarget::NativeInvitation(executable),42,"Source A")).key());
    }
    #[test]
    fn self_stream_is_an_explicit_read_only_native_target() {
        let target=StreamTarget::parse(["--self-stream".into()]).unwrap().unwrap();
        assert_eq!(target.connection,ConnectionTarget::NativeLoopback);
        assert!(target.is_native() && target.view_only);
        let args=target.command(Path::new("generated-gui"),None).get_args().map(|s|s.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(StreamTarget::parse(args).unwrap(),Some(target));
        assert!(StreamTarget::parse(["--self-stream".into(),"--native-invitation".into(),"generated.json".into()]).is_err());
        assert!(StreamTarget::parse(["--native-invitation".into(),"generated.json".into(),"--self-stream".into()]).is_err());
    }

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
        let command = target.command(Path::new("eclipse.exe"), Some((1540, -564)));
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
