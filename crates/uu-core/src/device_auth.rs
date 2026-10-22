//! Versioned device authentication over a bounded, provisional transport.
//! Only a successful result may be used to start capture or desktop input.
use crate::device_trust::{Permissions, PublicDevice, TrustStore};
use anyhow::{ensure, Context, Result};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Semaphore};
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
}
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
    serde_json::from_value(value).map_err(|_| anyhow::anyhow!("设备认证协议无效"))
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

#[derive(Default)]
pub struct PairingDisplay(Mutex<Option<Zeroizing<String>>>);
impl PairingDisplay {
    /// Only the local UI reads this. Never add it to logs or diagnostics.
    pub fn code(&self) -> Option<String> {
        self.0.lock().unwrap().as_ref().map(|v| v.to_string())
    }
    fn show(&self, code: &str) {
        *self.0.lock().unwrap() = Some(Zeroizing::new(code.to_owned()));
    }
    pub fn clear(&self) {
        *self.0.lock().unwrap() = None;
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
}
#[derive(Default)]
struct Window {
    until: Option<Instant>,
    attempts: u8,
}
pub struct PairingControl {
    window: Mutex<Window>,
    slot: Arc<Semaphore>,
    requests: mpsc::UnboundedSender<PairRequest>,
}
impl PairingControl {
    pub fn new(requests: mpsc::UnboundedSender<PairRequest>) -> Self {
        Self {
            window: Mutex::new(Window::default()),
            slot: Arc::new(Semaphore::new(1)),
            requests,
        }
    }
    pub fn open(&self) {
        *self.window.lock().unwrap() = Window {
            until: Some(Instant::now() + Duration::from_secs(300)),
            attempts: 0,
        };
    }
    pub fn close(&self) {
        self.window.lock().unwrap().until = None;
    }
    pub fn is_open(&self) -> bool {
        self.window
            .lock()
            .unwrap()
            .until
            .is_some_and(|t| Instant::now() < t)
    }
    fn reserve(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        let slot = self
            .slot
            .clone()
            .try_acquire_owned()
            .context("正在确认另一台设备，请稍后重试")?;
        let mut w = self.window.lock().unwrap();
        ensure!(
            w.until.is_some_and(|t| Instant::now() < t) && w.attempts < 5,
            "请先在被控端允许新设备配对；配对许可已关闭、过期或达到尝试上限"
        );
        w.attempts += 1;
        Ok(slot)
    }
    async fn approve(&self, device: PublicDevice) -> Result<PairApproval> {
        let (respond, receive) = oneshot::channel();
        self.requests
            .send(PairRequest {
                id: uuid::Uuid::new_v4().to_string(),
                device,
                respond,
            })
            .map_err(|_| anyhow::anyhow!("被控端配对确认页面不可用"))?;
        tokio::pin!(receive);
        let approval = tokio::time::timeout(Duration::from_secs(120), async {
            let mut timer=tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    result=&mut receive=>return result.context("被控端已关闭配对请求")?.context("被控端拒绝配对"),
                    _=timer.tick()=>ensure!(self.is_open(),"被控端已停止接收配对"),
                }
            }
        }).await.context("等待被控端批准超时")??;
        ensure!(self.is_open(), "被控端已停止接收配对");
        Ok(approval)
    }
}

pub struct AuthenticatedPeer {
    pub device: PublicDevice,
    pub permissions: Permissions,
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
    let pair = !(known && resumable);
    let ctx = context(binding, &local, &remote, &a, &b)?;
    send(io, Message::Select { pair }).await?;
    ensure!(
        matches!(receive(io).await?,Message::Ready{pair:p} if p==pair),
        "被控端未接受本次配对方式"
    );
    if pair {
        use rand::Rng;
        let pin = Zeroizing::new(format!(
            "{:08}",
            rand::rngs::OsRng.gen_range(0..100_000_000u32)
        ));
        display.show(&pin);
        let (pake, a_share) = Spake2::<Ed25519Group>::start_a(
            &Password::new(pin.as_bytes()),
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
    let ctx = context(binding, &remote, &local, &a, &b)?;
    let Message::Select { pair } = receive(io).await? else {
        anyhow::bail!("缺少设备认证方式");
    };
    let _slot = if pair {
        Some(pairing.reserve()?)
    } else {
        ensure!(permission.is_some(), "该客户端尚未获得本机授权");
        None
    };
    send(io, Message::Ready { pair }).await?;
    let permission = if pair {
        let a_share = share(receive(io).await?)?;
        let approval = tokio::select! {
            result=pairing.approve(remote.clone())=>result?,
            _=io.receive()=>anyhow::bail!("控制端已取消配对或发送了无效消息"),
        };
        let (pake, b_share) = Spake2::<Ed25519Group>::start_b(
            &Password::new(approval.code.as_bytes()),
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
        approval.permissions
    } else {
        permission.unwrap()
    };
    let Message::Proof { signature } = receive(io).await? else {
        anyhow::bail!("缺少控制端身份验证");
    };
    remote.verify(&proof(&ctx, false, None)?, &signature)?;
    if pair || legacy.is_some() {
        store.authorize(remote.clone(), permission, legacy)?;
    }
    ensure!(
        store.peer(&remote.id())?.and_then(|p| p.inbound) == Some(permission),
        "设备授权已被撤销或更改"
    );
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
    Ok(AuthenticatedPeer {
        device: remote,
        permissions: permission,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
