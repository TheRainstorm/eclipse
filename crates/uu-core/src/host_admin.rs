//! Authenticated host management over the existing native control channel.
//! Remote clients only inspect; local service management owns all mutations.
use crate::{device_book::{Sharing,Store},device_trust::{InputPermissions,Permissions},shared_host::SessionContext};
use anyhow::{ensure,Context,Result};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use std::sync::{atomic::{AtomicBool,AtomicU64,Ordering},Arc,Mutex};
use tokio::sync::mpsc;

#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(tag="action",rename_all="snake_case",deny_unknown_fields)]
pub enum Request {
    Inspect,
    Save {revision:String,settings:Sharing,restart:bool},
    Permissions {peer:String,permissions:Permissions},
    SessionControls,
    SessionPermissions {session:String,input:InputPermissions},
    SessionQuality {session:String,update:crate::native_quality::Update},
    SessionDisplay {session:String,id:i32},
    Disconnect {session:String},
}
impl Request {
    pub fn is_mutation(&self)->bool { !matches!(self,Self::Inspect|Self::SessionControls) }
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub id:String,
    pub peer:String,
    pub name:String,
    pub status:String,
    pub permissions:Permissions,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionControl {
    pub id:String,
    pub peer:String,
    pub name:String,
    pub status:String,
    pub permissions:Permissions,
    pub control:Option<crate::shared_host::SessionControlSnapshot>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    pub active:Sharing,
    pub saved:Sharing,
    pub revision:String,
    pub monitors:Vec<(String,String)>,
    pub portal:bool,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub own_permissions:Permissions,
    pub connections:Vec<Connection>,
    pub server:Option<ServerSettings>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(tag="result",rename_all="snake_case",deny_unknown_fields)]
pub enum Response {
    Snapshot {snapshot:Snapshot},
    SessionControls {sessions:Vec<SessionControl>},
    Saved {restart:bool},
    Applied,
    Error {message:String},
}
pub(crate) struct Command {pub id:u64,pub request:Request}
#[derive(Default)]
pub struct Client {
    pub available:AtomicBool,
    sender:Mutex<Option<mpsc::Sender<Command>>>,
    next:AtomicU64,
    pub snapshot:Mutex<Option<Snapshot>>,
    pub response:Mutex<Option<(u64,Response)>>,
}
impl Client {
    pub fn request(&self,request:Request)->Result<u64> {
        ensure!(self.available.load(Ordering::Acquire),"{}", uu_i18n::format!("主机未提供连接管理，请更新两端并重新连接"));
        let id=self.next.fetch_add(1,Ordering::Relaxed)+1;
        self.sender.lock().unwrap().as_ref().context(uu_i18n::text("控制通路未连接"))?
            .try_send(Command{id,request}).map_err(|_|anyhow::anyhow!("{}", uu_i18n::format!("管理请求队列已满或连接已关闭")))?;
        Ok(id)
    }
    pub(crate) fn attach(&self)->mpsc::Receiver<Command> {
        self.available.store(false,Ordering::Release);
        *self.snapshot.lock().unwrap()=None;
        *self.response.lock().unwrap()=None;
        let (sender,receiver)=mpsc::channel(8);
        *self.sender.lock().unwrap()=Some(sender);
        receiver
    }
    pub(crate) fn receive(&self,id:u64,response:Response) {
        if let Response::Snapshot{snapshot}=&response {*self.snapshot.lock().unwrap()=Some(snapshot.clone());}
        *self.response.lock().unwrap()=Some((id,response));
    }
    pub(crate) fn disconnect(&self) {
        self.available.store(false,Ordering::Release);
        self.sender.lock().unwrap().take();
    }
}

pub struct Service {
    store:Store,
    active:Mutex<Sharing>,
    restart:Arc<dyn Fn()->Result<()>+Send+Sync>,
}
fn revision(settings:&Sharing)->Result<String> {Ok(hex::encode(Sha256::digest(serde_json::to_vec(settings)?)))}
impl Service {
    pub fn new(store:Store,active:Sharing,restart:impl Fn()->Result<()>+Send+Sync+'static)->Self {
        Self {store,active:Mutex::new(active),restart:Arc::new(restart)}
    }
    /// Snapshot at admission; existing media retains its previous settings.
    pub(crate) fn defaults(&self)->Sharing {self.active.lock().unwrap().clone()}
    fn grant(context:&SessionContext)->Result<Permissions> {
        Ok(context.access.peer_permissions(&context.peer)?.context(uu_i18n::text("设备授权已撤销"))?.1)
    }
    pub(crate) fn execute(&self,context:&SessionContext,request:Request)->Result<Response> {
        let grant=Self::grant(context)?;
        self.execute_granted(&context.access, grant, request, false)
    }
    /// Only call after the local OS service-management authority has been checked.
    /// Network requests must use execute() and their current device grant.
    pub fn execute_local_manager(&self, access:&crate::shared_host::Access, request:Request)->Result<Response> {
        let mut details=Permissions::CONTROL.details(); details.manage_server=true;
        self.execute_granted(access,Permissions::CONTROL.with_details(details),request, true)
    }
    pub fn restart_local_manager(&self)->Result<()> {(self.restart)()}
    fn execute_granted(&self,access:&crate::shared_host::Access,grant:Permissions,request:Request,local:bool)->Result<Response> {
        grant.validate()?;
        if !local && !matches!(request,Request::Inspect) {
            anyhow::bail!("{}",uu_i18n::text("客户端只能查看主机管理信息；连接控制请在服务端操作"));
        }
        ensure!(!request.is_mutation()||!access.sessions.configuration_switching(),"服务正在切换配置，请稍后重试。");
        if request.is_mutation() {ensure!(grant.details().manage_server,"{}", uu_i18n::format!("本机未授予管理服务和连接权限"));}
        match request {
            Request::Inspect=>{
                let connections=access.sessions.details().into_iter().map(|(id,peer,status)| {
                    let record=access.peer_permissions(&peer).ok().flatten();
                    let name=record.as_ref().map_or_else(||uu_i18n::text("已授权设备").into(),|(device,_)|device.name.clone());
                    let permissions=record.map(|(_,p)|p).unwrap_or(Permissions::VIEW);
                    let status=match status {
                        crate::native::HostStatus::Streaming{width,height,fps,..}=>format!("{width}×{height} · {fps} FPS"),
                        _=>uu_i18n::text("正在建立串流").into(),
                    };
                    Connection{id,peer,name,status,permissions}
                }).collect();
                let server=if grant.details().manage_server {
                    let saved=self.store.read()?.sharing;
                    let monitors=crate::native::monitors().unwrap_or_default().into_iter().map(|m|(m.name.clone(),format!("{} · {}×{}",m.name,m.width,m.height))).collect();
                    Some(ServerSettings{active:self.defaults(),revision:revision(&saved)?,saved,monitors,portal:crate::native::uses_desktop_portal()})
                }else{None};
                Ok(Response::Snapshot{snapshot:Snapshot{own_permissions:grant,connections,server}})
            },
            Request::Save{revision:expected,settings,restart}=>{
                settings.validate()?;
                let mut active=self.active.lock().unwrap();
                ensure!(expected.len()==64,"{}", uu_i18n::format!("服务配置版本无效，请重新读取"));
                self.store.edit(|book| {
                    ensure!(revision(&book.sharing)?==expected,"{}", uu_i18n::format!("服务配置已被其它窗口修改，请重新读取后保存"));
                    ensure!(settings.ip_enabled||!settings.plugin_services.is_empty()||book.services.iter().any(|s|s.host_enabled),"{}", uu_i18n::format!("至少保留一个连接入口"));
                    book.sharing=settings.clone();
                    Ok(())
                })?;
                // Listener identity and ports await explicit restart. Media
                // defaults apply to new connections without ending live media.
                let mut next=settings;
                next.name=active.name.clone();
                next.ip_enabled=active.ip_enabled;
                next.ipv6_enabled=active.ipv6_enabled;
                next.service_port=active.service_port;
                next.discovery_port=active.discovery_port;
                next.plugin_services=active.plugin_services.clone();
                crate::virtual_display::Manager::shared().set_limit(next.virtual_display_limit)?;
                *active=next;
                Ok(Response::Saved{restart})
            },
            Request::Permissions{peer,permissions}=>{
                permissions.validate()?;
                access.set_permissions(&peer,permissions)?;
                Ok(Response::Applied)
            },
            Request::SessionControls=>{
                let sessions=access.sessions.details().into_iter().map(|(id,peer,status)| {
                    let record=access.peer_permissions(&peer).ok().flatten();
                    let name=record.as_ref().map_or_else(||uu_i18n::text("已授权设备").into(),|(device,_)|device.name.clone());
                    let permissions=record.map(|(_,p)|p).unwrap_or(Permissions::VIEW);
                    let status=match status {
                        crate::native::HostStatus::Streaming{width,height,fps,..}=>format!("{width}×{height} · {fps} FPS"),
                        _=>uu_i18n::text("正在建立串流").into(),
                    };
                    SessionControl{id:id.clone(),peer,name,status,permissions,control:access.sessions.session_control_snapshot(&id)}
                }).collect();
                Ok(Response::SessionControls{sessions})
            },
            Request::SessionPermissions{session,input}=>{
                access.sessions.set_session_input(&session,input)?;
                Ok(Response::Applied)
            },
            Request::SessionQuality{session,update}=>{
                update.validate()?;
                access.sessions.set_session_quality(&session,update)?;
                Ok(Response::Applied)
            },
            Request::SessionDisplay{session,id}=>{
                access.sessions.set_session_display(&session,id)?;
                Ok(Response::Applied)
            },
            Request::Disconnect{session}=>{
                ensure!(access.sessions.sessions().iter().any(|(id,_)|id==&session),"{}", uu_i18n::format!("会话已经结束"));
                access.sessions.disconnect_session(&session);
                Ok(Response::Applied)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device_auth::PairingControl,device_trust::{PermissionDetails,TrustStore},shared_host::{Access,SessionGate}};
    #[tokio::test]
    async fn management_requires_a_current_grant_and_saving_detects_conflicting_writers() {
        let root=std::env::temp_dir().join(format!("eclipse-host-admin-{}",uuid::Uuid::new_v4()));
        let trust=TrustStore::open(root.join("host")).unwrap();
        let client=TrustStore::open(root.join("client")).unwrap().card("Generated client");
        trust.authorize(client.clone(),Permissions::VIEW,None).unwrap();
        let store=Store::open(root.join("host")).unwrap();
        let (sender,_)=mpsc::unbounded_channel();
        let access=Arc::new(Access {store:trust.clone(),name:"Generated host".into(),pairing:Arc::new(PairingControl::new(sender)),sessions:Arc::new(SessionGate::default())});
        let context=SessionContext{access,peer:client.id(),permissions:Permissions::VIEW,session_id:None};
        let service=Service::new(store.clone(),Sharing::default(),||Ok(()));
        let Response::Snapshot{snapshot}=service.execute(&context,Request::Inspect).unwrap() else{panic!()};
        assert!(snapshot.server.is_none());
        assert!(service.execute(&context,Request::Save{revision:"0".repeat(64),settings:Sharing::default(),restart:true}).is_err());
        let manager=Permissions::VIEW.with_details(PermissionDetails{manage_server:true,..Default::default()});
        trust.authorize(client.clone(),manager,None).unwrap();
        let Response::Snapshot{snapshot}=service.execute(&context,Request::Inspect).unwrap() else{panic!()};
        let server=snapshot.server.unwrap();
        assert!(service.execute(&context,Request::Save{revision:server.revision.clone(),settings:Sharing::default(),restart:false}).is_err(),"remote clients must not mutate host settings");
        assert!(service.execute(&context,Request::SessionControls).is_err(),"remote clients must not enumerate or control sessions");
        assert!(service.execute(&context,Request::SessionPermissions{session:"generated".into(),input:InputPermissions::NONE}).is_err());
        assert!(service.execute(&context,Request::SessionQuality{session:"generated".into(),update:crate::native_quality::Update{fps:60,bitrate_kbps:Some(8_000),rate_control:None}}).is_err());
        assert!(service.execute(&context,Request::SessionDisplay{session:"generated".into(),id:0}).is_err());
        assert!(service.execute(&context,Request::Permissions{peer:client.id(),permissions:Permissions::VIEW}).is_err());
        assert!(service.execute(&context,Request::Disconnect{session:"generated".into()}).is_err());
        let lease=context.access.sessions.claim_named(&client.id(),"Generated client").unwrap();
        let control=crate::native_control::HostControl::new(false,None,None).await;
        context.access.sessions.attach_control(lease.id(),control);
        let Response::SessionControls{sessions}=service.execute_local_manager(&context.access,Request::SessionControls).unwrap() else{panic!()};
        assert_eq!(sessions.len(),1);assert_eq!(sessions[0].id,lease.id());
        assert!(service.execute_local_manager(&context.access,Request::SessionPermissions{session:lease.id().into(),input:InputPermissions::NONE}).is_ok());
        drop(lease);
        let mut changed=server.saved;changed.audio=false;
        changed.virtual_display_limit=1;
        changed.service_port=changed.service_port.saturating_sub(1);
        changed.ipv6_enabled=false;
        let initial=service.defaults();
        let switching=uuid::Uuid::new_v4().to_string();
        context.access.sessions.reserve_configuration_switch(&switching,false).unwrap();
        assert!(service.execute(&context,Request::Inspect).is_ok());
        assert!(service.execute_local_manager(&context.access,Request::Save{revision:server.revision.clone(),settings:changed.clone(),restart:false}).is_err());
        assert!(store.read().unwrap().sharing.audio,"switching must preserve the original host settings");
        context.access.sessions.reserve_configuration_switch(&switching,true).unwrap();
        assert!(matches!(service.execute_local_manager(&context.access,Request::Save{revision:server.revision.clone(),settings:changed.clone(),restart:false}).unwrap(),Response::Saved{restart:false}));
        assert!(!store.read().unwrap().sharing.audio);
        let live=service.defaults();
        assert!(!live.audio,"new media admissions must use the saved default without restarting the host");
        assert_eq!(live.virtual_display_limit,1);
        assert_eq!(live.service_port,initial.service_port,"a saved port must not claim the listener has moved");
        assert!(!store.read().unwrap().sharing.ipv6_enabled);
        assert!(live.ipv6_enabled,"a saved IPv6 setting must not claim the listener has changed before restart");
        assert!(initial.audio,"already admitted media retains its immutable settings");
        assert!(service.execute_local_manager(&context.access,Request::Save{revision:server.revision,settings:Sharing::default(),restart:false}).unwrap_err().to_string().contains("其它窗口"));
        trust.revoke(&client.id()).unwrap();
        assert!(service.execute(&context,Request::Inspect).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
