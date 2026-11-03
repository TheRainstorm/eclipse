//! Optional management channel for connection-owned virtual display leases.
//! The authenticated connection owns the lease across all media epochs. Driver
//! operations run on a worker and require the current manage_server grant.
use crate::{
    shared_host::SessionContext,
    virtual_display::{Display, LeaseId, Manager, Owner, Slot, Spec, Status},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::{
    data_channel::{
        data_channel_init::RTCDataChannelInit, data_channel_state::RTCDataChannelState,
        RTCDataChannel,
    },
    peer_connection::{sdp::session_description::RTCSessionDescription, RTCPeerConnection},
};

const ATTRIBUTE: &str = "a=x-eclipse-virtual-display:1";
const LABEL: &str = "eclipse-virtual-display-v1";
const MAX_JSON: usize = 16 * 1024;
/// A UI notice deadline, not permission to cancel or repeat a submitted
/// topology change. Windows class installers cannot be cancelled safely.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Inspect,
    Create { spec: Spec },
    Remove { id: LeaseId },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalRequest {
    Manage { request: Request },
    /// Host-local administrator IPC only; remote Create has its own ManageServer check.
    Prepare,
    Recover,
    PrepareSlot { id: String },
}
impl From<Request> for LocalRequest {
    fn from(request: Request) -> Self {
        Self::Manage { request }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Snapshot {
        status: Status,
        displays: Vec<Display>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        slots: Vec<Slot>,
    },
    Created {
        display: Display,
    },
    Removed {
        id: LeaseId,
    },
    Error {
        message: String,
    },
}
pub fn error_message(error: &anyhow::Error) -> String {
    use crate::virtual_display::Error;
    match error.downcast_ref::<Error>() {
        Some(Error::Unsupported) => uu_i18n::text("虚拟显示器仅支持 Windows").into(),
        Some(Error::DriverMissing) => uu_i18n::text("源机尚未安装支持的签名虚拟显示器驱动").into(),
        Some(Error::Preparation(status))=>preparation_message(status),
        Some(Error::NoAvailableSlot) => {
            uu_i18n::text("驱动没有可用虚拟屏，请在源机管理窗口准备驱动后重试").into()
        }
        Some(Error::Claimed) => uu_i18n::text("虚拟显示器由其它共享进程使用").into(),
        Some(Error::AdministratorRequired) => uu_i18n::text("启用虚拟显示器需要管理员权限，请在源机管理窗口准备虚拟显示器").into(),
        Some(Error::InvalidSpec) => uu_i18n::text("虚拟屏分辨率或刷新率无效").into(),
        Some(Error::WrongOwner) => uu_i18n::text("此虚拟屏属于其它连接或由源机手动创建").into(),
        Some(Error::SessionClosed | Error::HostClosing) => {
            uu_i18n::text("所属连接或共享进程已停止，不能再新建虚拟屏").into()
        }
        Some(Error::UnknownSlot) => {
            uu_i18n::text("该虚拟屏槽位已不可用或正在被使用，请刷新列表").into()
        }
        Some(Error::LastActiveOutput) => {
            uu_i18n::text("不能停用最后一个活动显示器，请先保留另一个桌面输出").into()
        }
        _ => format!("{error:#}"),
    }
}
/// Stable user-facing result for both first-offer and in-session Create.
/// The status still carries the detailed provider error for the local details UI.
pub fn preparation_message(status:&crate::virtual_display::preparation::Status)->String {
    use crate::virtual_display::preparation::Phase;
    match status.phase {
        Phase::Unsupported=>uu_i18n::text("此电脑暂不支持虚拟副屏").into(),
        Phase::NeedsDriver=>uu_i18n::text("首次使用虚拟副屏时，后台可以准备签名驱动。").into(),
        Phase::NeedsCapacity=>uu_i18n::text("可用虚拟副屏数量不足，可在空闲时准备更多。").into(),
        Phase::NeedsElevation=>uu_i18n::text("请在源机打开后台准备虚拟副屏；首次安装时按系统提示授权。").into(),
        Phase::WaitingForIdle if status.queued=>uu_i18n::text("当前有连接或虚拟屏正在使用，准备已排队；空闲后会自动补充虚拟副屏。").into(),
        Phase::WaitingForIdle=>uu_i18n::text("当前有连接或虚拟屏正在使用，可登记准备并在空闲时补充。").into(),
        Phase::Preparing=>uu_i18n::text("正在准备虚拟副屏，请等待本次操作完成。").into(),
        Phase::Ready=>uu_i18n::text("虚拟副屏已就绪，可以创建。").into(),
        Phase::Failed=>uu_i18n::text("虚拟副屏准备未完成，请查看诊断详情后重试。").into(),
        Phase::RecoveryPending=>uu_i18n::text("上次准备尚未完成恢复，已停止自动重试；请在源机后台检查并恢复。").into(),
    }
}
/// Local UI operation. An installed service must call this only after its OS
/// administrator IPC boundary; it is never the remote connection entry point.
pub fn execute_local_owner(request: LocalRequest) -> Result<Response> {
    let manager = Manager::shared();
    let request = match request {
        LocalRequest::Manage { request } => request,
        LocalRequest::Prepare => {
            match crate::virtual_display::prepare() {
                Ok(()) | Err(crate::virtual_display::Error::Preparation(_))=>{},
                Err(error)=>return Err(error.into()),
            }
            Request::Inspect
        }
        LocalRequest::Recover => {
            manager.recover_preparation()?;
            Request::Inspect
        }
        LocalRequest::PrepareSlot { id } => {
            manager.prepare_slot(&id)?;
            Request::Inspect
        }
    };
    Ok(match request {
        Request::Inspect => Response::Snapshot {
            status: manager.status()?,
            displays: manager.list()?,
            slots: manager.slots()?,
        },
        Request::Create { spec } => Response::Created {
            display: manager.create(Owner::Manual, spec)?,
        },
        Request::Remove { id } => {
            manager.remove(id)?;
            Response::Removed { id }
        }
    })
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Packet {
    Request { id: u64, command: Request },
    Response { id: u64, response: Response },
}
fn parse(bytes: &[u8]) -> Option<Packet> {
    if bytes.len() > MAX_JSON {
        return None;
    }
    let packet: Packet = serde_json::from_slice(bytes).ok()?;
    match &packet {
        Packet::Request { id, .. } | Packet::Response { id, .. } if *id == 0 => None,
        Packet::Response {
            response: Response::Snapshot { displays, .. },
            ..
        } if displays.len() > 16 => None,
        _ => Some(packet),
    }
}
async fn send(channel: &RTCDataChannel, packet: Packet) -> Result<()> {
    let bytes = serde_json::to_vec(&packet)?;
    ensure!(
        bytes.len() <= MAX_JSON,
        "virtual display response is too large"
    );
    ensure!(
        channel.ready_state() == RTCDataChannelState::Open,
        "virtual display channel closed"
    );
    let length = bytes.len();
    let written =
        tokio::time::timeout(Duration::from_secs(2), channel.send(&bytes.into())).await??;
    ensure!(written == length, "partial virtual display response");
    Ok(())
}
pub(crate) fn offered(description: &RTCSessionDescription) -> bool {
    let mut values = description
        .sdp
        .lines()
        .filter(|line| line.starts_with("a=x-eclipse-virtual-display:"));
    values.next() == Some(ATTRIBUTE) && values.next().is_none()
}
pub(crate) fn advertise(description: &mut RTCSessionDescription) {
    description.sdp.push_str(ATTRIBUTE);
    description.sdp.push_str("\r\n");
}
pub(crate) fn accepts(channel: &RTCDataChannel) -> bool {
    channel.label() == LABEL
        && channel.ordered()
        && channel.max_retransmits() == 0
        && channel.max_packet_lifetime() == 0
}

struct Command {
    id: u64,
    request: Request,
}
#[derive(Default)]
pub struct Client {
    pub available: AtomicBool,
    next: AtomicU64,
    state: Mutex<ClientState>,
}
#[derive(Default)]
struct ClientState {
    epoch: u64,
    sender: Option<mpsc::Sender<Command>>,
    responses: std::collections::VecDeque<(u64, Response)>,
}
impl Client {
    pub fn request(&self, request: Request) -> Result<u64> {
        let state = self.state.lock().unwrap();
        ensure!(
            self.available.load(Ordering::Acquire),
            "{}",
            uu_i18n::text("远端未提供虚拟显示器管理，请更新后台并授予管理服务权限")
        );
        let id = self
            .next
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .context("virtual display request ID overflow")?;
        state
            .sender
            .as_ref()
            .context("virtual display channel closed")?
            .try_send(Command { id, request })
            .context("virtual display request queue full or closed")?;
        Ok(id)
    }
    pub fn take_response(&self, id: u64) -> Option<Response> {
        let mut state = self.state.lock().unwrap();
        let responses = &mut state.responses;
        let at = responses.iter().position(|r| r.0 == id)?;
        responses.remove(at).map(|r| r.1)
    }
    fn attach(&self) -> (u64, mpsc::Receiver<Command>) {
        let mut state = self.state.lock().unwrap();
        self.available.store(false, Ordering::Release);
        state.responses.clear();
        state.epoch += 1;
        let (sender, receiver) = mpsc::channel(4);
        state.sender = Some(sender);
        (state.epoch, receiver)
    }
    fn opened(&self, epoch: u64) {
        let state = self.state.lock().unwrap();
        if state.epoch == epoch {
            self.available.store(true, Ordering::Release);
        }
    }
    fn disconnect(&self, epoch: u64) {
        let mut state = self.state.lock().unwrap();
        if state.epoch == epoch {
            self.available.store(false, Ordering::Release);
            state.sender.take();
        }
    }
    fn receive(&self, epoch: u64, id: u64, response: Response) {
        let mut state = self.state.lock().unwrap();
        if state.epoch != epoch {
            return;
        }
        let responses = &mut state.responses;
        if responses.len() == 16 {
            responses.pop_front();
        }
        responses.push_back((id, response));
    }
}

pub(crate) struct Host {
    manager: Arc<Manager>,
    owner: uuid::Uuid,
    context: SessionContext,
    stopped: AtomicBool,
    busy: AtomicBool,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    initial: InitialLease,
}
#[derive(Default)]
struct InitialLease(Mutex<Option<(Spec,std::result::Result<Display,String>)>>);
impl InitialLease {
    fn prepare(&self,spec:Spec,create:impl FnOnce()->Result<Display>)->Result<Display> {
        spec.validate()?;
        let mut state=self.0.lock().unwrap();
        if let Some((previous,result))=&*state {
            ensure!(*previous==spec,"initial virtual display specification cannot change");
            return result.clone().map_err(anyhow::Error::msg);
        }
        // A failed mutation may have a partially applied OS transaction. Cache
        // its terminal outcome as well, never silently repeat an accepted Create.
        let result=create().map_err(|error|error_message(&error));
        *state=Some((spec,result.clone()));
        result.map_err(anyhow::Error::msg)
    }
}
impl Host {
    /// Called on a blocking worker after authentication and before capture.
    /// The outer connection lifetime owns cleanup, including failed startup.
    pub fn prepare_initial(&self,spec:Spec)->Result<Display> {
        self.authorize()?;
        self.initial.prepare(spec,|| {
            self.authorize()?;
            let display=self.manager.create_with_context(Owner::Session(self.owner),spec,self.context.session_id.as_deref())?;
            if let Err(error)=self.authorize() {
                self.manager.remove_session(self.owner,display.id)?;
                return Err(error);
            }
            Ok(display)
        })
    }
    pub fn permitted(context: &SessionContext) -> bool {
        context
            .access
            .peer_permissions(&context.peer)
            .ok()
            .flatten()
            .is_some_and(|(_, permissions)| permissions.details().manage_server)
    }
    fn authorize(&self) -> Result<()> {
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "virtual display connection ended"
        );
        ensure!(
            Self::permitted(&self.context),
            "{}",
            uu_i18n::text("本机未授予管理服务和连接权限")
        );
        Ok(())
    }
    fn execute(
        &self,
        request: Request,
        displays: &crate::native_displays::Host,
    ) -> Result<Response> {
        self.authorize()?;
        let response = match request {
            Request::Inspect => Response::Snapshot {
                status: self.manager.status()?,
                displays: self
                    .manager
                    .list()?
                    .into_iter()
                    .filter(|d| d.owner == Owner::Session(self.owner))
                    .collect(),
                slots: Vec::new(),
            },
            Request::Create { spec } => {
                let display = self.manager.create_with_context(Owner::Session(self.owner), spec,self.context.session_id.as_deref())?;
                // Revocation or cancellation while Windows was applying the
                // target cannot leave a new display owned by an expired grant.
                if let Err(error) = self.authorize() {
                    self.manager.remove_session(self.owner, display.id)?;
                    return Err(error);
                }
                if let Err(error) = displays.refresh_source(&display.source) {
                    self.manager.remove_session(self.owner, display.id)?;
                    return Err(error);
                }
                Response::Created { display }
            }
            Request::Remove { id } => {
                let owned = self.manager.list()?.into_iter().find(|display| {
                    display.id == id && display.owner == Owner::Session(self.owner)
                });
                ensure!(
                    !owned
                        .as_ref()
                        .is_some_and(|display| displays.references_source(&display.source)),
                    "{}",
                    uu_i18n::text("请先切换到另一显示器，再移除当前或正在切换的虚拟屏")
                );
                self.manager.remove_session(self.owner, id)?;
                displays.refresh()?;
                Response::Removed { id }
            }
        };
        Ok(response)
    }
}

/// Held by the outer authenticated host future, independently of channel
/// callbacks. Dropping one media epoch must not remove connection-owned screens.
pub(crate) struct Lifetime {
    host: Option<Arc<Host>>,
    guard: Option<crate::virtual_display::SessionGuard>,
}
impl Lifetime {
    pub fn new(context: Option<SessionContext>, desktop: bool) -> Self {
        if !cfg!(windows) || !desktop {
            return Self {
                host: None,
                guard: None,
            };
        }
        let Some(context) = context else {
            return Self {
                host: None,
                guard: None,
            };
        };
        let manager = Manager::shared();
        let owner = uuid::Uuid::new_v4();
        let guard = manager.session_guard(owner);
        Self {
            host: Some(Arc::new(Host {
                manager,
                owner,
                context,
                stopped: AtomicBool::new(false),
                busy: AtomicBool::new(false),
                worker: Mutex::new(None),
                initial: InitialLease::default(),
            })),
            guard: Some(guard),
        }
    }
    pub fn host(&self) -> Option<Arc<Host>> {
        self.host.clone()
    }
    pub async fn shutdown(&self) {
        if let Some(host) = &self.host {
            let worker = {
                let mut worker = host.worker.lock().unwrap();
                host.stopped.store(true, Ordering::Release);
                worker.take()
            };
            if let Some(worker) = worker {
                if let Err(error) = worker.await {
                    tracing::warn!(%error,"virtual display request worker failed");
                }
            }
            let manager = host.manager.clone();
            let owner = host.owner;
            match tokio::task::spawn_blocking(move || manager.release_session(owner)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error,"virtual display cleanup remains pending"),
                Err(error) => tracing::warn!(%error,"virtual display cleanup worker failed"),
            }
        }
    }
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        if let Some(host) = &self.host {
            host.stopped.store(true, Ordering::Release);
        }
        self.guard.take();
    }
}
pub(crate) fn host_channel(
    channel: Arc<RTCDataChannel>,
    host: Arc<Host>,
    displays: Arc<crate::native_displays::Host>,
) {
    let weak = Arc::downgrade(&channel);
    channel.on_message(Box::new(move |message| {
        let host = host.clone();
        let weak = weak.clone();
        let displays = displays.clone();
        let packet = parse(&message.data);
        Box::pin(async move {
            let Some(Packet::Request { id, command }) = packet else {
                return;
            };
            let Some(channel) = weak.upgrade() else {
                return;
            };
            if host.busy.load(Ordering::Acquire) {
                let _ = send(
                    &channel,
                    Packet::Response {
                        id,
                        response: Response::Error {
                            message: uu_i18n::text("另一项虚拟显示器操作尚未完成").into(),
                        },
                    },
                )
                .await;
                return;
            }
            let mut slot = host.worker.lock().unwrap();
            if host.stopped.load(Ordering::Acquire) || host.busy.swap(true, Ordering::AcqRel) {
                return;
            }
            let owner = host.clone();
            // Never hold the SCTP read callback while Windows changes topology.
            // At most one owned task is outstanding per connection; normal
            // shutdown waits for it before closing its display leases.
            *slot = Some(tokio::spawn(async move {
                let worker = owner.clone();
                let result =
                    tokio::task::spawn_blocking(move || worker.execute(command, &displays))
                        .await
                        .map_err(anyhow::Error::from)
                        .and_then(|r| r);
                if !owner.stopped.load(Ordering::Acquire) {
                    let response = result.unwrap_or_else(|error| Response::Error {
                        message: error_message(&error)
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(512)
                            .collect(),
                    });
                    let _ = send(&channel, Packet::Response { id, response }).await;
                }
                owner.busy.store(false, Ordering::Release);
            }));
        })
    }));
}
pub(crate) async fn viewer_channel(
    pc: &RTCPeerConnection,
    stats: Arc<crate::media::MediaStats>,
) -> Result<Viewer> {
    let channel = pc
        .create_data_channel(
            LABEL,
            Some(RTCDataChannelInit {
                ordered: Some(true),
                ..Default::default()
            }),
        )
        .await?;
    let (epoch, mut commands) = stats.virtual_displays.attach();
    let weak = Arc::downgrade(&channel);
    let incoming = Arc::downgrade(&stats);
    channel.on_message(Box::new(move |message| {
        if let Some(Packet::Response { id, response }) = parse(&message.data) {
            if let Some(stats) = incoming.upgrade() {
                stats.virtual_displays.receive(epoch, id, response);
            }
        }
        Box::pin(async {})
    }));
    let opened = Arc::downgrade(&stats);
    channel.on_open(Box::new(move || {
        if let Some(stats) = opened.upgrade() {
            stats.virtual_displays.opened(epoch);
        }
        Box::pin(async {})
    }));
    let closed = Arc::downgrade(&stats);
    channel.on_close(Box::new(move || {
        if let Some(stats) = closed.upgrade() {
            stats.virtual_displays.disconnect(epoch);
        }
        Box::pin(async {})
    }));
    let client = Arc::downgrade(&stats);
    let task = tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            let Some(channel) = weak.upgrade() else {
                break;
            };
            if let Err(error) = send(
                &channel,
                Packet::Request {
                    id: command.id,
                    command: command.request,
                },
            )
            .await
            {
                stats.virtual_displays.receive(
                    epoch,
                    command.id,
                    Response::Error {
                        message: error.to_string(),
                    },
                );
                break;
            }
        }
    });
    Ok(Viewer {
        _channel: channel,
        task: Some(task),
        client,
        epoch,
    })
}
pub(crate) struct Viewer {
    _channel: Arc<RTCDataChannel>,
    task: Option<tokio::task::JoinHandle<()>>,
    client: std::sync::Weak<crate::media::MediaStats>,
    epoch: u64,
}
impl Viewer {
    pub async fn close(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(stats) = self.client.upgrade() {
            stats.virtual_displays.disconnect(self.epoch);
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        if let Some(stats) = self.client.upgrade() {
            stats.virtual_displays.disconnect(self.epoch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_lease_is_connection_scoped_idempotent_and_never_repeats_an_accepted_mutation() {
        use std::sync::atomic::AtomicUsize;
        let calls=AtomicUsize::new(0);let spec=Spec::default();let owner=uuid::Uuid::new_v4();
        let display=Display{id:LeaseId(uuid::Uuid::new_v4()),owner:Owner::Session(owner),spec,actual:Some(spec),name:"generated".into(),source:"display-b".into(),active:true,cleanup_pending:false,last_error:None};
        let lease=InitialLease::default();
        assert_eq!(lease.prepare(spec,||{calls.fetch_add(1,Ordering::SeqCst);Ok(display.clone())}).unwrap(),display);
        assert_eq!(lease.prepare(spec,||panic!("same connection must reuse its lease")).unwrap(),display);
        assert!(lease.prepare(Spec{width:1280,..spec},||panic!("different intent must fail before creation")).is_err());
        assert_eq!(calls.load(Ordering::SeqCst),1);
        let another=InitialLease::default();
        another.prepare(spec,||{calls.fetch_add(1,Ordering::SeqCst);Ok(Display{id:LeaseId(uuid::Uuid::new_v4()),owner:Owner::Session(uuid::Uuid::new_v4()),..display})}).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst),2);
        let failed=InitialLease::default();assert!(failed.prepare(spec,||anyhow::bail!("generated partial transaction failure")).is_err());
        assert!(failed.prepare(spec,||panic!("failed accepted request must not be repeated")).is_err());
        assert!(failed.prepare(Spec{width:0,..spec},||panic!("invalid spec")).is_err());
    }
    #[test]
    fn invalid_or_oversized_wire_and_duplicate_capabilities_are_rejected() {
        assert!(parse(
            br#"{"type":"request","id":1,"command":{"action":"prepare_slot","id":"untrusted"}}"#
        )
        .is_none());
        assert!(parse(br#"{"type":"request","id":1,"command":{"action":"create","spec":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}},"unknown":true}"#).is_none());
        assert!(parse(br#"{"type":"request","id":0,"command":{"action":"inspect"}}"#).is_none());
        assert!(parse(&vec![b' '; MAX_JSON + 1]).is_none());
        let mut description = RTCSessionDescription::default();
        assert!(!offered(&description));
        advertise(&mut description);
        assert!(offered(&description));
        advertise(&mut description);
        assert!(!offered(&description));
    }
    #[test]
    fn unavailable_client_cannot_queue_work_and_responses_keep_their_request_identity() {
        let client = Client::default();
        assert!(client.request(Request::Inspect).is_err());
        let (epoch, mut requests) = client.attach();
        client.opened(epoch);
        let id = client.request(Request::Inspect).unwrap();
        assert_eq!(requests.try_recv().unwrap().id, id);
        client.receive(
            epoch,
            id,
            Response::Error {
                message: "generated failure".into(),
            },
        );
        assert!(client.take_response(id + 1).is_none());
        assert!(matches!(
            client.take_response(id),
            Some(Response::Error { .. })
        ));
        client.disconnect(epoch);
        assert!(client.request(Request::Inspect).is_err());
        let (new_epoch, _) = client.attach();
        client.opened(new_epoch);
        client.disconnect(epoch);
        client.receive(
            epoch,
            12,
            Response::Error {
                message: "stale".into(),
            },
        );
        assert!(client.available.load(Ordering::Acquire));
        assert!(client.take_response(12).is_none());
    }
}
