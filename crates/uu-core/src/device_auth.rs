//! Versioned device authentication over a bounded, provisional transport.
//! Only a successful result may be used to start capture or desktop input.
use crate::device_trust::{Permissions, PublicDevice, TrustStore};
use anyhow::{ensure, Context, Result};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::sync::{Arc, Mutex,Weak};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use zeroize::Zeroizing;

pub const MAX_AUTH_MESSAGE: usize = 8 * 1024;
const WAIT: Duration = Duration::from_secs(180);

#[async_trait::async_trait]
pub trait AuthIo: Send {
    async fn send(&mut self, value: serde_json::Value) -> Result<()>;
    async fn receive(&mut self) -> Result<serde_json::Value>;
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Hello {
        version: u32,
        host: bool,
        device: PublicDevice,
        nonce: String,
        resumable: bool,
    },
    Select {
        pair: bool,
    },
    SelectMethod {method:PairingMethod},
    ReadyMethod {method:PairingMethod,salt:Option<String>},
    Ready {
        pair: bool,
    },
    Pake {
        share: String,
    },
    Confirm {
        tag: String,
    },
    Proof {
        signature: String,
    },
    Granted {
        signature: String,
        permissions: Permissions,
    },
    Finished,
    Rejected {
        reason: Rejection,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Rejection {
    PairingClosed,
    Busy,
    Declined,
    AuthenticationFailed,
    MethodUnavailable,
}
impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PairingClosed => "被控端已停止接收配对，或配对尝试过于频繁；请稍后重试",
            Self::Busy => "被控端正在确认另一台设备，请稍后重试",
            Self::Declined => "被控端拒绝或取消了配对，请重新发起",
            Self::AuthenticationFailed => "设备身份认证失败，请核对短码和被控端授权后重新连接",
            Self::MethodUnavailable => "主机未启用所选配对方式，请核对密码设置或升级两端",
        })
    }
}
impl std::error::Error for Rejection {}
async fn send(io: &mut dyn AuthIo, message: Message) -> Result<()> {
    let value = serde_json::to_value(message)?;
    ensure!(
        serde_json::to_vec(&value)?.len() <= MAX_AUTH_MESSAGE,
        "配对消息过大"
    );
    tokio::time::timeout(Duration::from_secs(10), io.send(value))
        .await
        .context("配对发送超时")?
}
async fn receive(io: &mut dyn AuthIo) -> Result<Message> {
    let value = tokio::time::timeout(WAIT, io.receive())
        .await
        .context("等待配对超时")??;
    ensure!(
        serde_json::to_vec(&value)?.len() <= MAX_AUTH_MESSAGE,
        "配对消息过大"
    );
    let message = serde_json::from_value(value).map_err(|_| anyhow::anyhow!("设备认证协议无效"))?;
    if let Message::Rejected { reason } = message {
        anyhow::bail!(reason);
    }
    Ok(message)
}
fn nonce() -> String {
    let mut b = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}
fn hello(value: Message, host: bool) -> Result<(PublicDevice, String, bool)> {
    let Message::Hello {
        version,
        host: role,
        device,
        nonce,
        resumable,
    } = value
    else {
        anyhow::bail!("缺少设备身份握手");
    };
    ensure!(
        version == 2 && role == host && nonce.len() == 64 && hex::decode(&nonce).is_ok(),
        "设备认证版本、角色或挑战无效"
    );
    device.validate()?;
    Ok((device, nonce, resumable))
}
fn context(
    binding: &[u8],
    viewer: &PublicDevice,
    host: &PublicDevice,
    a: &str,
    b: &str,
) -> Result<Vec<u8>> {
    ensure!(binding.len() == 32, "缺少安全通道绑定");
    Ok(Sha256::digest(serde_json::to_vec(&serde_json::json!({"domain":"eclipse-device-auth-v2",
        "binding":hex::encode(binding),"viewer":viewer,"host":host,"viewer_nonce":a,"host_nonce":b}))?).to_vec())
}
fn method_context(base:Vec<u8>,method:PairingMethod,salt:Option<&str>)->Result<Vec<u8>> {
    if matches!(method,PairingMethod::Code|PairingMethod::PublicKey) {return Ok(base);}
    if method==PairingMethod::TemporaryCode {ensure!(salt.is_none(),"临时配对参数无效");}
    if method==PairingMethod::Password {ensure!(salt.is_some_and(|v|v.len()==32 && hex::decode(v).is_ok()),"密码配对参数无效");}
    Ok(Sha256::digest(serde_json::to_vec(&serde_json::json!({"domain":"eclipse-device-auth-method-v1","context":hex::encode(base),"method":method,"salt":salt}))?).to_vec())
}
fn proof(context: &[u8], host: bool, permissions: Option<Permissions>) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &serde_json::json!({"context":hex::encode(context),"host":host,"permissions":permissions}),
    )?)
}
fn confirmation(
    key: &[u8],
    context: &[u8],
    a: &[u8],
    b: &[u8],
    host: bool,
) -> Result<Hmac<Sha256>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(b"eclipse-pair-confirm-v2\0");
    mac.update(context);
    mac.update(a);
    mac.update(b);
    mac.update(&[host as u8]);
    Ok(mac)
}
fn share(value: Message) -> Result<Vec<u8>> {
    let Message::Pake { share } = value else {
        anyhow::bail!("缺少短码配对消息");
    };
    ensure!(share.len() == 66, "短码配对消息长度无效");
    Ok(hex::decode(share).context("短码配对消息格式无效")?)
}
fn check_tag(value: Message, mac: Hmac<Sha256>) -> Result<()> {
    let Message::Confirm { tag } = value else {
        anyhow::bail!("缺少配对确认");
    };
    ensure!(tag.len() == 64, "配对确认长度无效");
    mac.verify_slice(&hex::decode(tag)?)
        .map_err(|_| anyhow::anyhow!("配对码不匹配或会话被修改"))
}

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum PairingMethod {#[default] Code,TemporaryCode,Password,PublicKey}
pub struct PairingChoice {
    pub method:PairingMethod,
    pub password:Zeroizing<String>,
}
#[derive(Clone)]
pub struct PairingPrompt {
    pub device:PublicDevice,
    pub preauthorized:bool,
}
#[derive(Default)]
pub struct PairingDisplay {
    code:Mutex<Option<Zeroizing<String>>>,
    interactive:std::sync::atomic::AtomicBool,
    prompt:Mutex<Option<(PairingPrompt,oneshot::Sender<PairingChoice>)>>,
    selected:Mutex<PairingMethod>,
}
impl PairingDisplay {
    /// Only the local UI reads this. Never add it to logs or diagnostics.
    pub fn code(&self) -> Option<String> {
        self.code.lock().unwrap().as_ref().map(|v| v.to_string())
    }
    fn show(&self, code: &str) {
        *self.code.lock().unwrap() = Some(Zeroizing::new(code.to_owned()));
    }
    pub fn clear(&self) {
        *self.code.lock().unwrap() = None;
        self.prompt.lock().unwrap().take();
    }
    pub fn enable_choices(&self) {self.interactive.store(true,std::sync::atomic::Ordering::Release);}
    pub fn prompt(&self)->Option<PairingPrompt> {self.prompt.lock().unwrap().as_ref().map(|p|p.0.clone())}
    pub fn selected_method(&self)->PairingMethod {*self.selected.lock().unwrap()}
    pub fn choose(&self,choice:PairingChoice)->Result<()> {
        self.prompt.lock().unwrap().take().context("配对请求已结束")?.1.send(choice)
            .map_err(|_|anyhow::anyhow!("配对请求已结束"))
    }
    async fn select(&self,remote:&PublicDevice,preauthorized:bool)->Result<PairingChoice> {
        if !self.interactive.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(PairingChoice{method:PairingMethod::Code,password:Zeroizing::new(String::new())});
        }
        let (send,receive)=oneshot::channel();
        *self.prompt.lock().unwrap()=Some((PairingPrompt{device:remote.clone(),preauthorized},send));
        tokio::time::timeout(Duration::from_secs(120),receive).await.context("等待选择配对方式超时")?
            .context("本机已取消配对")
    }
}
struct ClearDisplay<'a>(&'a PairingDisplay);
impl Drop for ClearDisplay<'_> {
    fn drop(&mut self) {
        self.0.clear();
    }
}

pub struct PairApproval {
    pub code: Zeroizing<String>,
    pub permissions: Permissions,
}
impl PairApproval {
    pub fn new(code: &str, permissions: Permissions) -> Result<Self> {
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        ensure!(
            code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit()),
            "请输入 8 位配对码"
        );
        permissions.validate()?;
        Ok(Self {
            code: Zeroizing::new(code),
            permissions,
        })
    }
}
pub struct PairRequest {
    pub id: String,
    pub device: PublicDevice,
    pub respond: oneshot::Sender<Option<PairApproval>>,
    pub status: watch::Receiver<PairStatus>,
    pub method:PairingMethod,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairStatus {
    Waiting,
    Verifying,
    Paired,
    Failed,
}
struct Completion {
    status: watch::Sender<PairStatus>,
    done: bool,
}
impl Completion {
    fn complete(&mut self) {
        self.done = true;
        self.status.send_replace(PairStatus::Paired);
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.done {
            self.status.send_replace(PairStatus::Failed);
        }
    }
}
struct Approved {
    approval: PairApproval,
    completion: Completion,
}
struct Window {
    until: Option<Instant>,
    continuous: bool,
    attempts: u8,
    since: Instant,
}
impl Default for Window {
    fn default()->Self {
        Self {until:None,continuous:true,attempts:0,since:Instant::now()}
    }
}
pub struct PairingControl {
    window: Mutex<Window>,
    slot: Arc<Semaphore>,
    requests: mpsc::UnboundedSender<PairRequest>,
    temporary:Mutex<HashMap<String,Weak<TemporaryRecord>>>,
}
pub(crate) struct TemporaryRecord {pub device:PublicDevice,pub permissions:Permissions}
pub(crate) struct TemporaryGrant(Arc<TemporaryRecord>);
impl TemporaryGrant {
    pub(crate) fn probe(&self)->Weak<TemporaryRecord> {Arc::downgrade(&self.0)}
}
impl PairingControl {
    pub fn new(requests: mpsc::UnboundedSender<PairRequest>) -> Self {
        Self {
            window: Mutex::new(Window::default()),
            slot: Arc::new(Semaphore::new(1)),
            requests,
            temporary:Default::default(),
        }
    }
    pub fn open(&self) {
        *self.window.lock().unwrap() = Window {
            until: Some(Instant::now() + Duration::from_secs(300)),
            continuous:false,
            attempts: 0,
            since:Instant::now(),
        };
    }
    pub fn close(&self) {
        let mut window=self.window.lock().unwrap();
        window.until=None;
        window.continuous=false;
    }
    pub fn is_open(&self) -> bool {
        let window=self.window.lock().unwrap();
        window.continuous || window.until.is_some_and(|t|Instant::now()<t)
    }
    fn reserve(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        let slot = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| Rejection::Busy)?;
        let mut w = self.window.lock().unwrap();
        if w.continuous && w.since.elapsed()>=Duration::from_secs(60) {
            w.attempts=0;w.since=Instant::now();
        }
        if !((w.continuous || w.until.is_some_and(|t| Instant::now() < t)) && w.attempts < 5) {
            anyhow::bail!(Rejection::PairingClosed);
        }
        w.attempts += 1;
        Ok(slot)
    }
    pub(crate) fn temporary_peer(&self,id:&str)->Option<(PublicDevice,Permissions)> {
        self.temporary.lock().unwrap().get(id).and_then(Weak::upgrade).map(|r|(r.device.clone(),r.permissions))
    }
    pub(crate) fn revoke_temporary(&self,id:&str)->bool {self.temporary.lock().unwrap().remove(id).is_some()}
    pub(crate) fn valid_temporary(&self,probe:&Weak<TemporaryRecord>)->bool {
        probe.upgrade().is_some_and(|r|self.temporary.lock().unwrap().get(&r.device.id()).is_some_and(|current|current.ptr_eq(probe)))
    }
    fn grant_temporary(&self,device:PublicDevice,permissions:Permissions)->TemporaryGrant {
        let record=Arc::new(TemporaryRecord{device,permissions});
        let mut map=self.temporary.lock().unwrap();
        map.retain(|_,r|r.strong_count()>0);
        map.insert(record.device.id(),Arc::downgrade(&record));
        TemporaryGrant(record)
    }
    async fn approve(&self, device: PublicDevice,method:PairingMethod) -> Result<Approved> {
        let (respond, receive) = oneshot::channel();
        let (status, updates) = watch::channel(PairStatus::Waiting);
        let completion = Completion {
            status,
            done: false,
        };
        self.requests
            .send(PairRequest {
                id: uuid::Uuid::new_v4().to_string(),
                device,
                respond,
                status: updates,
                method,
            })
            .map_err(|_| anyhow::anyhow!("被控端配对确认页面不可用"))?;
        tokio::pin!(receive);
        let approval = tokio::time::timeout(Duration::from_secs(120), async {
            let mut timer=tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    result=&mut receive=>return result.map_err(|_|Rejection::Declined)?.ok_or_else(||Rejection::Declined.into()),
                    _=timer.tick()=>if !self.is_open(){anyhow::bail!(Rejection::PairingClosed);},
                }
            }
        }).await.context("等待被控端批准超时")??;
        ensure!(self.is_open(), "被控端已停止接收配对");
        completion.status.send_replace(PairStatus::Verifying);
        Ok(Approved {
            approval,
            completion,
        })
    }
}

pub struct AuthenticatedPeer {
    pub device: PublicDevice,
    pub permissions: Permissions,
    pub(crate) temporary:Option<TemporaryGrant>,
}

/// `prior_pair` is allowed only after proving an old credential over its
/// pinned TLS channel, never from a provider's claimed account or device ID.
pub async fn client(
    io: &mut dyn AuthIo,
    store: &TrustStore,
    name: &str,
    binding: &[u8],
    expected: Option<&str>,
    prior_pair: bool,
    display: &PairingDisplay,
) -> Result<AuthenticatedPeer> {
    let _clear = ClearDisplay(display);
    let local = store.card(name);
    local.validate()?;
    let a = nonce();
    send(
        io,
        Message::Hello {
            version: 2,
            host: false,
            device: local.clone(),
            nonce: a.clone(),
            resumable: false,
        },
    )
    .await?;
    let (remote, b, resumable) = hello(receive(io).await?, true)?;
    ensure!(
        remote.id() != local.id(),
        "目标与本机使用了相同的原生设备身份"
    );
    if let Some(id) = expected {
        ensure!(id == remote.id(), "目标设备身份发生变化，请核实后重新配对");
    }
    let known = store.peer(&remote.id())?.is_some() || prior_pair;
    let mut pair = !(known && resumable);
    let choice=if pair {display.select(&remote,resumable).await?}else{PairingChoice{method:PairingMethod::Code,password:Zeroizing::new(String::new())}};
    let method=choice.method;
    *display.selected.lock().unwrap()=method;
    if method==PairingMethod::PublicKey {
        ensure!(resumable,"主机尚未授权本机公钥，请先在主机添加本机公钥");
        pair=false;
    }
    if matches!(method,PairingMethod::TemporaryCode|PairingMethod::Password) {
        send(io,Message::SelectMethod{method}).await?;
    }else{send(io, Message::Select { pair }).await?;}
    let salt=match receive(io).await? {
        Message::Ready{pair:p} if p==pair && matches!(method,PairingMethod::Code|PairingMethod::PublicKey)=>None,
        Message::ReadyMethod{method:m,salt} if m==method=>salt,
        _=>anyhow::bail!("被控端未接受本次配对方式，请核对设置并升级两端"),
    };
    let ctx=method_context(context(binding,&local,&remote,&a,&b)?,method,salt.as_deref())?;
    if pair {
        let password=if method==PairingMethod::Password {
            let salt: [u8;16]=hex::decode(salt.context("主机未提供密码配对参数")?)?.try_into()
                .map_err(|_|anyhow::anyhow!("密码配对参数无效"))?;
            let password=choice.password;
            tokio::task::spawn_blocking(move||crate::pairing_password::derive(&password,&salt)).await??
        }else{
            use rand::Rng;
            let pin=Zeroizing::new(format!("{:08}",rand::rngs::OsRng.gen_range(0..100_000_000u32)));
            display.show(&pin);
            Zeroizing::new(pin.as_bytes().to_vec())
        };
        let (pake, a_share) = Spake2::<Ed25519Group>::start_a(
            &Password::new(&password),
            &Identity::new(local.id().as_bytes()),
            &Identity::new(remote.id().as_bytes()),
        );
        send(
            io,
            Message::Pake {
                share: hex::encode(&a_share),
            },
        )
        .await?;
        let b_share = share(receive(io).await?)?;
        let key = Zeroizing::new(
            pake.finish(&b_share)
                .map_err(|_| anyhow::anyhow!("短码配对交换失败"))?,
        );
        let tag = confirmation(&key, &ctx, &a_share, &b_share, false)?
            .finalize()
            .into_bytes();
        send(
            io,
            Message::Confirm {
                tag: hex::encode(tag),
            },
        )
        .await?;
        check_tag(
            receive(io).await?,
            confirmation(&key, &ctx, &a_share, &b_share, true)?,
        )?;
    }
    send(
        io,
        Message::Proof {
            signature: store.sign(&proof(&ctx, false, None)?),
        },
    )
    .await?;
    let Message::Granted {
        signature,
        permissions,
    } = receive(io).await?
    else {
        anyhow::bail!("未收到设备授权确认");
    };
    permissions.validate()?;
    remote.verify(&proof(&ctx, true, Some(permissions))?, &signature)?;
    store.remember(remote.clone())?;
    send(io, Message::Finished).await?;
    Ok(AuthenticatedPeer {
        device: remote,
        permissions,
        temporary:None,
    })
}

pub async fn host(
    io: &mut dyn AuthIo,
    store: &TrustStore,
    name: &str,
    binding: &[u8],
    pairing: &PairingControl,
    legacy: Option<&str>,
) -> Result<AuthenticatedPeer> {
    let result = host_inner(io, store, name, binding, pairing, legacy).await;
    if let Err(error) = &result {
        // Only protocol-defined reasons cross the provisional transport;
        // local filesystem paths and secret material never enter rejection.
        let reason = error
            .downcast_ref::<Rejection>()
            .cloned()
            .unwrap_or(Rejection::AuthenticationFailed);
        let _ = send(io, Message::Rejected { reason }).await;
    }
    result
}
async fn host_inner(
    io: &mut dyn AuthIo,
    store: &TrustStore,
    name: &str,
    binding: &[u8],
    pairing: &PairingControl,
    legacy: Option<&str>,
) -> Result<AuthenticatedPeer> {
    let (remote, a, _) = hello(receive(io).await?, false)?;
    let local = store.card(name);
    local.validate()?;
    ensure!(
        remote.id() != local.id(),
        "控制端与本机使用了相同的原生设备身份"
    );
    let old = store.peer(&remote.id())?;
    if let Some(alias) = legacy {
        ensure!(
            store.legacy_migratable(alias, &remote.id())?,
            "旧配对授权已经撤销或迁移到其它身份"
        );
    }
    let permission = old
        .as_ref()
        .and_then(|p| p.inbound)
        .or(legacy.map(|_| Permissions::CONTROL));
    let b = nonce();
    send(
        io,
        Message::Hello {
            version: 2,
            host: true,
            device: local.clone(),
            nonce: b.clone(),
            resumable: permission.is_some(),
        },
    )
    .await?;
    let (pair,method)=match receive(io).await? {
        Message::Select{pair}=>(pair,PairingMethod::Code),
        Message::SelectMethod{method} if matches!(method,PairingMethod::TemporaryCode|PairingMethod::Password)=>(true,method),
        _=>anyhow::bail!("缺少有效设备认证方式"),
    };
    let _slot=if pair {Some(pairing.reserve()?)}else{
        ensure!(permission.is_some(),"该客户端尚未获得本机授权");None
    };
    let password_record=if method==PairingMethod::Password {
        Some(crate::pairing_password::read(store)?.ok_or(Rejection::MethodUnavailable)?)
    }else{None};
    let salt=password_record.as_ref().map(|r|hex::encode(r.salt));
    if matches!(method,PairingMethod::TemporaryCode|PairingMethod::Password) {
        send(io,Message::ReadyMethod{method,salt:salt.clone()}).await?;
    }else{send(io,Message::Ready{pair}).await?;}
    let ctx=method_context(context(binding,&remote,&local,&a,&b)?,method,salt.as_deref())?;
    let mut completion=None;
    let permission=if pair {
        let a_share=share(receive(io).await?)?;
        let (password,permission)=if let Some(record)=&password_record {
            (record.key(),record.permissions)
        }else{
            let approved=tokio::select! {
                result=pairing.approve(remote.clone(),method)=>result?,
                _=io.receive()=>anyhow::bail!("控制端已取消配对或发送了无效消息"),
            };
            let approval=approved.approval;
            completion=Some(approved.completion);
            (Zeroizing::new(approval.code.as_bytes().to_vec()),approval.permissions)
        };
        let (pake, b_share) = Spake2::<Ed25519Group>::start_b(
            &Password::new(&password),
            &Identity::new(remote.id().as_bytes()),
            &Identity::new(local.id().as_bytes()),
        );
        send(
            io,
            Message::Pake {
                share: hex::encode(&b_share),
            },
        )
        .await?;
        let key = Zeroizing::new(
            pake.finish(&a_share)
                .map_err(|_| anyhow::anyhow!("短码配对交换失败"))?,
        );
        check_tag(
            receive(io).await?,
            confirmation(&key, &ctx, &a_share, &b_share, false)?,
        )?;
        send(
            io,
            Message::Confirm {
                tag: hex::encode(
                    confirmation(&key, &ctx, &a_share, &b_share, true)?
                        .finalize()
                        .into_bytes(),
                ),
            },
        )
        .await?;
        permission
    } else {
        permission.unwrap()
    };
    let Message::Proof { signature } = receive(io).await? else {
        anyhow::bail!("缺少控制端身份验证");
    };
    remote.verify(&proof(&ctx, false, None)?, &signature)?;
    if pair {ensure!(pairing.is_open(),Rejection::PairingClosed);}
    if let Some(record)=&password_record {
        ensure!(crate::pairing_password::read(store)?.is_some_and(|current|current.salt==record.salt),"主机配对密码已更改，请重新配对");
    }
    let temporary=if method==PairingMethod::TemporaryCode {
        // Choosing a temporary session must not leave an older permanent
        // grant capable of bypassing the next pairing. This follows the
        // host's explicit approval and successful PAKE + identity proof.
        if old.as_ref().is_some_and(|p|p.inbound.is_some()) {store.revoke(&remote.id())?;}
        Some(pairing.grant_temporary(remote.clone(),permission))
    }else{
        if pair || legacy.is_some() {store.authorize(remote.clone(),permission,legacy)?;}
        ensure!(store.peer(&remote.id())?.and_then(|p|p.inbound)==Some(permission),"设备授权已被撤销或更改");
        None
    };
    send(
        io,
        Message::Granted {
            signature: store.sign(&proof(&ctx, true, Some(permission))?),
            permissions: permission,
        },
    )
    .await?;
    ensure!(
        matches!(receive(io).await?, Message::Finished),
        "控制端未完成设备配对保存"
    );
    if let Some(completion) = &mut completion {
        completion.complete();
    }
    Ok(AuthenticatedPeer {
        device: remote,
        permissions: permission,
        temporary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn choose(display:&PairingDisplay,method:PairingMethod,password:&str) {
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                if display.prompt().is_some() {
                    display.choose(PairingChoice{method,password:Zeroizing::new(password.into())}).unwrap();return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn changing_temporary_pairing_to_permanent_in_transit_fails_confirmation() {
        struct Downgrade(Memory);
        #[async_trait::async_trait]
        impl AuthIo for Downgrade {
            async fn send(&mut self,mut value:serde_json::Value)->Result<()> {
                if value["type"]=="select_method" {value=serde_json::json!({"type":"select","pair":true});}
                self.0.send(value).await
            }
            async fn receive(&mut self)->Result<serde_json::Value> {
                let value=self.0.receive().await?;
                if value["type"]=="ready" {Ok(serde_json::json!({"type":"ready_method","method":"temporary_code","salt":null}))}else{Ok(value)}
            }
        }
        let root=std::env::temp_dir().join(format!("eclipse-method-bind-{}",uuid::Uuid::new_v4()));
        let a=TrustStore::open(root.join("a")).unwrap();let b=TrustStore::open(root.join("b")).unwrap();
        let (events,mut requests)=mpsc::unbounded_channel();let pairing=PairingControl::new(events);
        let display=PairingDisplay::default();display.enable_choices();let (x,mut y)=wires();let mut x=Downgrade(x);
        let (left,right,_)=tokio::join!(
            client(&mut x,&a,"A",&[3;32],None,false,&display),host(&mut y,&b,"B",&[3;32],&pairing,None),
            async {
                choose(&display,PairingMethod::TemporaryCode,"").await;
                requests.recv().await.unwrap().respond.send(Some(PairApproval::new(&display.code().unwrap(),Permissions::CONTROL).unwrap())).ok().unwrap();
            },
        );
        assert!(left.is_err()&&right.is_err());assert!(b.peers().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn temporary_code_replaces_an_older_permanent_grant_after_host_approval() {
        let root=std::env::temp_dir().join(format!("eclipse-temporary-replace-{}",uuid::Uuid::new_v4()));
        let a=TrustStore::open(root.join("a")).unwrap();let b=TrustStore::open(root.join("b")).unwrap();
        let id=a.card("A").id();b.authorize(a.card("A"),Permissions::CONTROL,None).unwrap();
        let (events,mut requests)=mpsc::unbounded_channel();let pairing=PairingControl::new(events);
        let display=PairingDisplay::default();display.enable_choices();let (mut x,mut y)=wires();
        let (left,right,_)=tokio::join!(
            client(&mut x,&a,"A",&[3;32],None,false,&display),
            host(&mut y,&b,"B",&[3;32],&pairing,None),
            async {
                choose(&display,PairingMethod::TemporaryCode,"").await;
                let request=requests.recv().await.unwrap();
                assert_eq!(b.peer(&id).unwrap().unwrap().inbound,Some(Permissions::CONTROL));
                request.respond.send(Some(PairApproval::new(&display.code().unwrap(),Permissions::VIEW).unwrap())).ok().unwrap();
            },
        );
        left.unwrap();let authorized=right.unwrap();
        assert!(b.peer(&id).unwrap().unwrap().inbound.is_none());
        assert_eq!(pairing.temporary_peer(&id).unwrap().1,Permissions::VIEW);
        drop(authorized);assert!(pairing.temporary_peer(&id).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn password_pairs_without_host_prompt_but_wrong_password_never_grants_access() {
        let root=std::env::temp_dir().join(format!("eclipse-password-auth-{}",uuid::Uuid::new_v4()));
        let a=TrustStore::open(root.join("a")).unwrap();let b=TrustStore::open(root.join("b")).unwrap();
        b.set_pairing_password("generated fixture password",Permissions::VIEW).unwrap();
        let (events,mut requests)=mpsc::unbounded_channel();let pairing=PairingControl::new(events);
        for password in ["wrong generated password","generated fixture password"] {
            let display=PairingDisplay::default();display.enable_choices();
            let (mut x,mut y)=wires();
            let (left,right,_)=tokio::join!(
                client(&mut x,&a,"A",&[3;32],None,false,&display),
                host(&mut y,&b,"B",&[3;32],&pairing,None),
                choose(&display,PairingMethod::Password,password),
            );
            if password.starts_with("wrong") {
                assert!(left.is_err()&&right.is_err());assert!(b.peers().unwrap().is_empty());
            }else{
                assert_eq!(left.unwrap().permissions,Permissions::VIEW);assert_eq!(right.unwrap().permissions,Permissions::VIEW);
                assert_eq!(b.peer(&a.card("A").id()).unwrap().unwrap().inbound,Some(Permissions::VIEW));
            }
            assert!(requests.try_recv().is_err());assert!(display.prompt().is_none());assert!(display.code().is_none());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn temporary_pairing_is_owned_by_one_session_and_disconnect_requires_pairing_again() {
        let root=std::env::temp_dir().join(format!("eclipse-temporary-auth-{}",uuid::Uuid::new_v4()));
        let a=TrustStore::open(root.join("a")).unwrap();let b=TrustStore::open(root.join("b")).unwrap();
        let (events,mut requests)=mpsc::unbounded_channel();let pairing=Arc::new(PairingControl::new(events));
        let display=PairingDisplay::default();display.enable_choices();let (mut x,mut y)=wires();
        let (left,right,_)=tokio::join!(
            client(&mut x,&a,"A",&[3;32],None,false,&display),
            host(&mut y,&b,"B",&[3;32],&pairing,None),
            async {
                choose(&display,PairingMethod::TemporaryCode,"").await;
                let request=requests.recv().await.unwrap();assert_eq!(request.method,PairingMethod::TemporaryCode);
                request.respond.send(Some(PairApproval::new(&display.code().unwrap(),Permissions::CONTROL).unwrap())).ok().unwrap();
            },
        );
        left.unwrap();let mut right=right.unwrap();let id=right.device.id();
        assert!(b.peers().unwrap().is_empty());
        let access=crate::shared_host::Access{store:b.clone(),name:"B".into(),pairing:pairing.clone(),sessions:Default::default()};
        let lease=access.claim_authenticated(&mut right).unwrap();
        assert_eq!(access.peer_permissions(&id).unwrap().unwrap().1,Permissions::CONTROL);
        assert!(access.claim(&id).is_err(),"temporary grant cannot authorize a new session without fresh authentication");
        drop(lease);drop(right);assert!(access.peer_permissions(&id).unwrap().is_none());
        assert!(b.peers().unwrap().is_empty());
        let display=PairingDisplay::default();let (mut x,mut y)=wires();
        let (left,right,_)=tokio::join!(
            client(&mut x,&a,"A",&[4;32],None,false,&display),
            host(&mut y,&b,"B",&[4;32],&pairing,None),
            async {requests.recv().await.unwrap().respond.send(None).ok().unwrap();},
        );
        assert!(left.is_err()&&right.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn preauthorized_public_key_allows_explicit_first_trust_without_a_code() {
        let root=std::env::temp_dir().join(format!("eclipse-key-auth-{}",uuid::Uuid::new_v4()));
        let a=TrustStore::open(root.join("a")).unwrap();let b=TrustStore::open(root.join("b")).unwrap();
        let imported=PublicDevice::from_public_key(&a.card("A").export_public_key(),"Preauthorized client").unwrap();
        b.authorize(imported,Permissions::VIEW,None).unwrap();
        let (events,mut requests)=mpsc::unbounded_channel();let pairing=PairingControl::new(events);pairing.close();
        let display=PairingDisplay::default();display.enable_choices();let (mut x,mut y)=wires();
        let (left,right,_)=tokio::join!(
            client(&mut x,&a,"A",&[3;32],None,false,&display),
            host(&mut y,&b,"B",&[3;32],&pairing,None),
            choose(&display,PairingMethod::PublicKey,""),
        );
        assert_eq!(left.unwrap().permissions,Permissions::VIEW);assert_eq!(right.unwrap().permissions,Permissions::VIEW);
        assert!(requests.try_recv().is_err());assert!(display.code().is_none());
        assert!(a.peer(&b.card("B").id()).unwrap().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn startup_accepts_pairing_and_rate_limit_recovers_without_manual_reopening() {
        let (events,_)=mpsc::unbounded_channel();
        let pairing=PairingControl::new(events);
        assert!(pairing.is_open());
        for _ in 0..5 {drop(pairing.reserve().unwrap());}
        assert!(pairing.reserve().is_err());
        pairing.window.lock().unwrap().since-=Duration::from_secs(61);
        drop(pairing.reserve().unwrap());
        pairing.close();
        assert!(!pairing.is_open());
        assert!(pairing.reserve().is_err());
    }
    struct Memory {
        tx: mpsc::Sender<serde_json::Value>,
        rx: mpsc::Receiver<serde_json::Value>,
    }
    #[async_trait::async_trait]
    impl AuthIo for Memory {
        async fn send(&mut self, v: serde_json::Value) -> Result<()> {
            self.tx.send(v).await.map_err(Into::into)
        }
        async fn receive(&mut self) -> Result<serde_json::Value> {
            self.rx.recv().await.context("closed")
        }
    }
    fn wires() -> (Memory, Memory) {
        let (a, b) = mpsc::channel(8);
        let (c, d) = mpsc::channel(8);
        (Memory { tx: a, rx: d }, Memory { tx: c, rx: b })
    }
    #[tokio::test]
    async fn a_closed_pairing_window_returns_an_actionable_reason_to_the_client() {
        let root =
            std::env::temp_dir().join(format!("eclipse-pair-closed-{}", uuid::Uuid::new_v4()));
        let a = TrustStore::open(root.join("a")).unwrap();
        let b = TrustStore::open(root.join("b")).unwrap();
        let (events, _) = mpsc::unbounded_channel();
        let pairing = PairingControl::new(events);
        pairing.close();
        let (mut x, mut y) = wires();
        let display = PairingDisplay::default();
        let (left, right) = tokio::join!(
            client(&mut x, &a, "A", &[3; 32], None, false, &display),
            host(&mut y, &b, "B", &[3; 32], &pairing, None)
        );
        assert!(left
            .err()
            .unwrap()
            .to_string()
            .contains("被控端已停止接收配对"));
        assert!(right.is_err());
        assert!(display.code().is_none());
        assert!(b.peers().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cancelling_either_side_finishes_the_prompt_and_releases_the_pairing_slot() {
        for client_cancel in [true, false] {
            let root =
                std::env::temp_dir().join(format!("eclipse-cancel-pair-{}", uuid::Uuid::new_v4()));
            let a = TrustStore::open(root.join("a")).unwrap();
            let b = TrustStore::open(root.join("b")).unwrap();
            let (events, mut requests) = mpsc::unbounded_channel();
            let pairing = std::sync::Arc::new(PairingControl::new(events));
            pairing.open();
            let display = std::sync::Arc::new(PairingDisplay::default());
            let (mut x, mut y) = wires();
            let shown = display.clone();
            let control = pairing.clone();
            let left = tokio::spawn(async move {
                client(&mut x, &a, "A", &[3; 32], None, false, &shown).await
            });
            let right =
                tokio::spawn(async move { host(&mut y, &b, "B", &[3; 32], &control, None).await });
            let mut request = tokio::time::timeout(Duration::from_secs(2), requests.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*request.status.borrow(), PairStatus::Waiting);
            if client_cancel {
                left.abort();
            } else {
                pairing.close();
            }
            assert!(tokio::time::timeout(Duration::from_secs(2), right)
                .await
                .unwrap()
                .unwrap()
                .is_err());
            let _ = left.await;
            request
                .status
                .wait_for(|s| *s == PairStatus::Failed)
                .await
                .unwrap();
            assert!(display.code().is_none());
            assert!(request.respond.send(None).is_err());
            pairing.open();
            drop(pairing.reserve().unwrap());
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn short_code_pairing_binds_identity_and_permissions_then_resumes_without_code() {
        let root = std::env::temp_dir().join(format!("eclipse-auth-{}", uuid::Uuid::new_v4()));
        let a = TrustStore::open(root.join("a")).unwrap();
        let b = TrustStore::open(root.join("b")).unwrap();
        let (events, mut requests) = mpsc::unbounded_channel();
        let pairing = PairingControl::new(events);
        pairing.open();
        let display = PairingDisplay::default();
        let (mut x, mut y) = wires();
        let approval = async {
            let request = requests.recv().await.unwrap();
            let pin = display.code().unwrap();
            request
                .respond
                .send(Some(PairApproval::new(&pin, Permissions::VIEW).unwrap()))
                .ok();
        };
        let (left, right, _) = tokio::join!(
            client(&mut x, &a, "A", &[3; 32], None, false, &display),
            host(&mut y, &b, "B", &[3; 32], &pairing, None),
            approval
        );
        assert_eq!(left.unwrap().permissions, Permissions::VIEW);
        assert_eq!(right.unwrap().device.id(), a.card("A").id());
        assert!(display.code().is_none());
        assert!(a
            .peer(&b.card("B").id())
            .unwrap()
            .unwrap()
            .inbound
            .is_none());
        pairing.close();
        let (mut x, mut y) = wires();
        let expected = b.card("B").id();
        let (left, right) = tokio::join!(
            client(&mut x, &a, "A", &[4; 32], Some(&expected), false, &display),
            host(&mut y, &b, "B", &[4; 32], &pairing, None)
        );
        left.unwrap();
        right.unwrap();
        assert!(display.code().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn wrong_code_or_different_channel_never_creates_a_grant() {
        for mismatch in [false, true] {
            let root =
                std::env::temp_dir().join(format!("eclipse-reject-{}", uuid::Uuid::new_v4()));
            let a = TrustStore::open(root.join("a")).unwrap();
            let b = TrustStore::open(root.join("b")).unwrap();
            let (events, mut requests) = mpsc::unbounded_channel();
            let pairing = PairingControl::new(events);
            pairing.open();
            let display = PairingDisplay::default();
            let (mut x, mut y) = wires();
            let approval = async {
                let request = requests.recv().await.unwrap();
                let mut pin = display.code().unwrap();
                if !mismatch {
                    pin.replace_range(0..1, if pin.starts_with('0') { "1" } else { "0" });
                }
                request
                    .respond
                    .send(Some(PairApproval::new(&pin, Permissions::CONTROL).unwrap()))
                    .ok();
            };
            let binding = if mismatch { [9; 32] } else { [3; 32] };
            // Drop each wire immediately after an error so its peer observes cancellation.
            let left = async {
                let r = client(&mut x, &a, "A", &[3; 32], None, false, &display).await;
                drop(x);
                r
            };
            let right = async {
                let r = host(&mut y, &b, "B", &binding, &pairing, None).await;
                drop(y);
                r
            };
            let (left, right, _) = tokio::join!(left, right, approval);
            assert!(left.is_err() && right.is_err());
            assert!(a.peers().unwrap().is_empty());
            assert!(b.peers().unwrap().is_empty());
            assert!(display.code().is_none());
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
