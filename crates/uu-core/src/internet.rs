//! Provider-independent authenticated signaling for native direct sessions.
use crate::{direct_ice::InternetIce, native_signaling::SessionSignaling};
use anyhow::{ensure, Context, Result};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

pub mod self_hosted;
pub mod server;
pub mod stun;

pub const MAX_MESSAGE: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    SelfHosted,
    Uu,
}

/// Contains file locations, never secret contents. A shared access key is the
/// current out-of-band pairing method for both internet providers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub provider: Provider,
    pub server: String,
    pub device: String,
    pub access_key: std::path::PathBuf,
    pub server_token: std::path::PathBuf,
    pub server_ca: Option<std::path::PathBuf>,
    pub development_relay: bool,
    /// Old snapshots keep their explicit PSK behavior. New device routes use
    /// provider-independent public identity authentication instead.
    pub device_auth: bool,
    pub expected_peer: Option<String>,
    pub profile: Option<String>,
}
impl Settings {
    pub fn self_hosted(&self) -> Result<self_hosted::Config> {
        ensure!(
            !self.server.trim().is_empty(),
            "{}", uu_i18n::format!("请填写连接服务器的 IP 和端口"));
        let server = self.server.trim().trim_end_matches('/');
        let url = if server.starts_with("wss://") {
            server.to_owned()
        } else {
            format!("wss://{server}/v1/signal")
        };
        Ok(self_hosted::Config {
            url,
            token_file: self.server_token.clone(),
            ca_file: self.server_ca.clone(),
        })
    }
}

async fn cancelled(stop: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}
pub async fn host(
    settings: Settings,
    config: crate::Config,
    sharing: crate::native::SharingConfig,
    mut stop: tokio::sync::watch::Receiver<bool>,
    status: Option<tokio::sync::watch::Sender<crate::native::HostStatus>>,
) -> Result<crate::native::HostReport> {
    sharing.validate()?;
    let key = AccessKey::load(&settings.access_key)?;
    let mut startup_cancel=stop.clone();
    let accepting = async {
        match settings.provider {
            Provider::Uu => {
                let service=crate::plugins::registry().get("uu")?;
                let mut host=tokio::select! {
                    result=tokio::time::timeout(std::time::Duration::from_secs(20),service.host(&config,settings.development_relay))=>result.context("connection plugin startup timed out")??,
                    _=cancelled(&mut startup_cancel)=>anyhow::bail!("sharing cancelled"),
                };
                let result = tokio::select! {result=host.accept()=>result,_=cancelled(&mut stop)=>Err(anyhow::anyhow!("sharing cancelled"))};
                host.close().await;
                result
            }
            Provider::SelfHosted => {
                let endpoint=settings.self_hosted()?;
                let mut host=tokio::select! {
                    result=tokio::time::timeout(std::time::Duration::from_secs(20),self_hosted::Host::online(&endpoint,&settings.device))=>result.context("self-hosted startup timed out")??,
                    _=cancelled(&mut startup_cancel)=>anyhow::bail!("sharing cancelled"),
                };
                let result = tokio::select! {result=host.accept()=>result,_=cancelled(&mut stop)=>Err(anyhow::anyhow!("sharing cancelled"))};
                host.close().await;
                result
            }
        }
    };
    let connection = match accepting.await {
        Ok(connection)=>connection,
        Err(_) if *stop.borrow()=>return Ok(crate::native::HostReport::default()),
        Err(error)=>return Err(error),
    };
    let (mut signal, ice) = Authenticated::new(connection, key, Role::Host);
    let result = crate::native::serve_direct(&mut signal, &ice, sharing, stop, status).await;
    signal.close().await;
    result
}
pub async fn view(
    settings: Settings,
    config: crate::Config,
    frames: crate::media::FrameSender,
    stats: std::sync::Arc<crate::media::MediaStats>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    input: Option<crate::native_input::NativeInputReceiver>,
) -> Result<()> {
    let key = if settings.device_auth {None}else{Some(AccessKey::load(&settings.access_key)?)};
    let joining = async {
        match settings.provider {
            Provider::Uu => crate::plugins::registry().get("uu")?.join(&config, &settings.device, settings.development_relay).await,
            Provider::SelfHosted => {
                self_hosted::join(&settings.self_hosted()?, &settings.device).await
            }
        }
    };
    let connection = tokio::select! {result=joining=>result?,_=cancelled(&mut stop)=>return Ok(())};
    if settings.device_auth {
        return crate::device_internet::view(connection,&settings,&config,frames,stats,stop,input).await;
    }
    let (mut signal, ice) = Authenticated::new(connection, key.unwrap(), Role::Viewer);
    stats.connection.set_service(match settings.provider {Provider::Uu=>uu_i18n::text("UU 连接服务"),Provider::SelfHosted=>uu_i18n::text("自建连接服务")});
    stats.diagnostics.record(
        json!({"event":"native_provider","provider":settings.provider,
        "stun_services":ice.urls().len(),"development_relay":settings.development_relay}),
    );
    let result = crate::native::view_direct(&mut signal, &ice, frames, stats, stop, input).await;
    signal.close().await;
    result
}

/// Share out of band with the intended device; never send this key to a provider.
pub struct AccessKey([u8; 32]);
impl Drop for AccessKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl AccessKey {
    pub fn create(path: &Path) -> Result<()> {
        use std::io::Write;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .context("cannot create new private access key")?;
        let result = file
            .write_all(hex::encode(bytes).as_bytes())
            .and_then(|_| file.sync_all());
        bytes.zeroize();
        result?;
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            std::fs::metadata(path)?.len() <= 128,
            "access key file too large"
        );
        let mut text = std::fs::read_to_string(path)?;
        let mut key = [0u8; 32];
        let result = hex::decode_to_slice(text.trim(), &mut key);
        text.zeroize();
        result.map_err(|_| {
            anyhow::anyhow!("access key must contain 32 random bytes encoded as hex")
        })?;
        Ok(Self(key))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Host,
    Viewer,
}
impl Role {
    fn opposite(self) -> Self {
        match self {
            Self::Host => Self::Viewer,
            Self::Viewer => Self::Host,
        }
    }
}

#[async_trait::async_trait]
pub trait Transport: Send {
    async fn send(&mut self, message: Value) -> Result<()>;
    async fn receive(&mut self) -> Result<Value>;
    async fn close(&mut self);
}

pub struct Connection {
    pub transport: Box<dyn Transport>,
    pub session: String,
    pub ice: InternetIce,
}

/// HMAC binds the complete offer/answer (including DTLS fingerprints), provider
/// session, direction and fresh offer challenge. Provider authentication alone
/// never grants screen or input access. One negotiation per connection.
pub struct Authenticated {
    transport: Box<dyn Transport>,
    key: AccessKey,
    session: String,
    role: Role,
    challenge: Option<String>,
    sent: bool,
    received: bool,
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn signature(key: &AccessKey, value: &Value) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(&key.0)?;
    mac.update(b"eclipse-native-internet-v1\0");
    mac.update(&serde_json::to_vec(value)?);
    Ok(mac.finalize().into_bytes().to_vec())
}
impl Authenticated {
    pub fn new(connection: Connection, key: AccessKey, role: Role) -> (Self, InternetIce) {
        let ice = connection.ice;
        (
            Self {
                transport: connection.transport,
                key,
                session: connection.session,
                role,
                challenge: None,
                sent: false,
                received: false,
            },
            ice,
        )
    }
    pub async fn close(&mut self) {
        self.transport.close().await;
    }
}
#[async_trait::async_trait]
impl SessionSignaling for Authenticated {
    async fn send(&mut self, payload: Value) -> Result<()> {
        ensure!(!self.sent, "native internet renegotiation is unsupported");
        let challenge = match self.role {
            Role::Viewer => uuid::Uuid::new_v4().to_string(),
            Role::Host => self
                .challenge
                .clone()
                .context("answer requires an authenticated offer")?,
        };
        let mut message = json!({"v":1,"session":self.session,"role":self.role,
            "challenge":challenge,"at":now()?,"payload":payload});
        message["mac"] = hex::encode(signature(&self.key, &message)?).into();
        ensure!(
            serde_json::to_vec(&message)?.len() <= MAX_MESSAGE,
            "native internet message too large"
        );
        self.transport.send(message).await?;
        self.challenge = Some(challenge);
        self.sent = true;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Value> {
        ensure!(!self.received, "unexpected native internet message");
        let mut message = self.transport.receive().await?;
        ensure!(
            serde_json::to_vec(&message)?.len() <= MAX_MESSAGE,
            "native internet message too large"
        );
        let encoded = message
            .as_object_mut()
            .and_then(|v| v.remove("mac"))
            .and_then(|v| v.as_str().map(str::to_owned))
            .context("missing native authentication")?;
        let tag =
            hex::decode(encoded).map_err(|_| anyhow::anyhow!("invalid native authentication"))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key.0)?;
        mac.update(b"eclipse-native-internet-v1\0");
        mac.update(&serde_json::to_vec(&message)?);
        mac.verify_slice(&tag)
            .map_err(|_| anyhow::anyhow!("native access key mismatch or modified SDP"))?;
        ensure!(
            message["v"] == 1
                && message["session"].as_str() == Some(&self.session)
                && serde_json::from_value::<Role>(message["role"].clone()).ok()
                    == Some(self.role.opposite()),
            "native message belongs to a different session or direction"
        );
        let timestamp = message["at"].as_u64().context("missing native timestamp")?;
        ensure!(
            now()?.abs_diff(timestamp) <= 120,
            "native offer expired; check device clocks"
        );
        let challenge = message["challenge"]
            .as_str()
            .context("missing native challenge")?
            .to_owned();
        ensure!(
            uuid::Uuid::parse_str(&challenge).is_ok(),
            "invalid native challenge"
        );
        if self.role == Role::Viewer {
            ensure!(
                self.challenge.as_deref() == Some(&challenge),
                "native answer challenge mismatch"
            );
        }
        self.challenge = Some(challenge);
        self.received = true;
        Ok(message["payload"].take())
    }
    async fn disconnected(&mut self) -> Result<()> {
        // Keep driving provider heartbeat and release messages during streaming.
        let _ = self.transport.receive().await?;
        anyhow::bail!("unexpected native internet renegotiation")
    }
}

/// Carry native metadata in a session-level SDP extension, preserving UU's
/// standard soac shape. Authentication covers the original unmodified SDP.
pub(crate) fn pack_sdp(mut envelope: Value) -> Result<String> {
    let sdp = envelope["payload"]["sdp"]["sdp"]
        .take()
        .as_str()
        .context("missing native SDP")?
        .to_owned();
    ensure!(
        !sdp.contains("a=x-eclipse-native:"),
        "duplicate native SDP extension"
    );
    let metadata = hex::encode(serde_json::to_vec(&envelope)?);
    ensure!(metadata.len() <= 16 * 1024, "native SDP metadata too large");
    let index = sdp.find("m=").context("native SDP has no media")?;
    Ok(format!(
        "{}a=x-eclipse-native:{metadata}\r\n{}",
        &sdp[..index],
        &sdp[index..]
    ))
}
pub(crate) fn unpack_sdp(sdp: &str) -> Result<Value> {
    ensure!(sdp.len() <= MAX_MESSAGE, "native SDP too large");
    let mut envelope = None;
    let mut original = String::new();
    for line in sdp.split_inclusive('\n') {
        if let Some(metadata) = line
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("a=x-eclipse-native:")
        {
            ensure!(
                envelope.is_none() && metadata.len() <= 16 * 1024,
                "invalid native SDP extension"
            );
            envelope = Some(
                serde_json::from_slice::<Value>(
                    &hex::decode(metadata)
                        .map_err(|_| anyhow::anyhow!("invalid native SDP metadata"))?,
                )
                .map_err(|_| anyhow::anyhow!("invalid native SDP metadata"))?,
            );
        } else {
            original.push_str(line);
        }
    }
    let mut envelope =
        envelope.context("peer does not support Eclipse native internet sessions")?;
    ensure!(
        envelope["payload"]["sdp"].is_object(),
        "invalid native SDP envelope"
    );
    envelope["payload"]["sdp"]["sdp"] = original.into();
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{atomic::Ordering, Arc},
        time::Duration,
    };
    use tokio::sync::{mpsc, watch};
    struct Memory {
        tx: mpsc::Sender<Value>,
        rx: mpsc::Receiver<Value>,
        tamper: bool,
    }
    #[async_trait::async_trait]
    impl Transport for Memory {
        async fn send(&mut self, mut message: Value) -> Result<()> {
            if self.tamper {
                let sdp = message["payload"]["sdp"]["sdp"]
                    .as_str()
                    .unwrap()
                    .replace("original", "modified");
                message["payload"]["sdp"]["sdp"] = sdp.into();
            }
            let message = unpack_sdp(&pack_sdp(message)?)?;
            self.tx.send(message).await?;
            Ok(())
        }
        async fn receive(&mut self) -> Result<Value> {
            self.rx.recv().await.context("closed")
        }
        async fn close(&mut self) {}
    }
    fn pair(
        wrong_key: bool,
        tamper: bool,
    ) -> (Authenticated, Authenticated, crate::direct_ice::InternetIce) {
        let (a_tx, a_rx) = mpsc::channel(4);
        let (b_tx, b_rx) = mpsc::channel(4);
        let session = uuid::Uuid::new_v4().to_string();
        let ice = crate::direct_ice::InternetIce::default();
        let (a, _) = Authenticated::new(
            Connection {
                transport: Box::new(Memory {
                    tx: a_tx,
                    rx: b_rx,
                    tamper,
                }),
                session: session.clone(),
                ice: ice.clone(),
            },
            AccessKey([7; 32]),
            Role::Viewer,
        );
        let (b, _) = Authenticated::new(
            Connection {
                transport: Box::new(Memory {
                    tx: b_tx,
                    rx: a_rx,
                    tamper: false,
                }),
                session,
                ice: ice.clone(),
            },
            AccessKey([if wrong_key { 8 } else { 7 }; 32]),
            Role::Host,
        );
        (a, b, ice)
    }
    #[tokio::test]
    async fn provider_sdp_roundtrip_authenticates_fingerprint_and_rejects_modified_or_wrong_key() {
        let offer = json!({"version":1,"sdp":{"type":"offer","sdp":"v=0\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=fingerprint:sha-256 original\r\n"}});
        for (wrong, tamper) in [(false, false), (true, false), (false, true)] {
            let (mut viewer, mut host, _) = pair(wrong, tamper);
            viewer.send(offer.clone()).await.unwrap();
            let received = host.receive().await;
            if wrong || tamper {
                assert!(received.is_err());
            } else {
                assert_eq!(received.unwrap(), offer);
                host.send(offer.clone()).await.unwrap();
                assert_eq!(viewer.receive().await.unwrap(), offer);
                assert!(host.receive().await.is_err());
            }
        }
    }
    async fn generated_session(
        mut viewer: Authenticated,
        mut host: Authenticated,
        ice: crate::direct_ice::InternetIce,
    ) {
        let (host_stop, host_rx) = watch::channel(false);
        let (viewer_stop, viewer_rx) = watch::channel(false);
        let host_ice = ice.clone();
        let sharing = crate::native::SharingConfig {encoder_tuning:Default::default(),session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),
            source: crate::native::CaptureSource::Synthetic {
                width: 320,
                height: 180,
                fps: 30,
            },
            fps: 30,
            bitrate_kbps: 1200,
            encoder: crate::native::EncoderPreference::Software,
            session_limit: Some(Duration::from_secs(12)),
            allow_input: false,
            audio: Some(crate::native::AudioSource::Synthetic),
            clipboard: None,
        };
        let host_task = tokio::spawn(async move {
            let result =
                crate::native::serve_direct(&mut host, &host_ice, sharing, host_rx, None).await;
            host.close().await;
            result
        });
        let stats = Arc::new(crate::media::MediaStats::default());
        let measured = stats.clone();
        let (frames, mut rx) = crate::media::frame_channel();
        let viewer_task = tokio::spawn(async move {
            let result =
                crate::native::view_direct(&mut viewer, &ice, frames, measured, viewer_rx, None)
                    .await;
            viewer.close().await;
            result
        });
        let observation = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                while rx.try_recv().is_ok() {}
                if stats.decoded_total.load(Ordering::Relaxed) >= 24
                    && stats.audio.packets.load(Ordering::Relaxed) > 0
                {
                    break;
                }
                if viewer_task.is_finished() || host_task.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        viewer_stop.send_replace(true);
        host_stop.send_replace(true);
        let viewed = viewer_task.await.unwrap();
        let hosted = host_task.await.unwrap();
        observation.unwrap();
        viewed.unwrap();
        let report = hosted.unwrap();
        assert!(report.sent >= 24);
        assert!(stats.decoded_total.load(Ordering::Relaxed) >= 24);
        assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
        assert!(stats.audio.packets.load(Ordering::Relaxed) > 0);
        assert!(!stats.native_input_available.load(Ordering::Relaxed));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn native_protocol_streams_generated_video_and_audio_over_provider_independent_authenticated_udp(
    ) {
        let (viewer, host, ice) = pair(false, false);
        generated_session(viewer, host, ice).await;
    }

    #[tokio::test]
    async fn stopping_during_provider_tls_startup_does_not_wait_for_network_timeout() {
        let root=Fixture(std::env::temp_dir().join(format!("eclipse-cancel-{}",uuid::Uuid::new_v4())));
        std::fs::create_dir(&root.0).unwrap();let key=root.0.join("key");AccessKey::create(&key).unwrap();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let settings=Settings{server:listener.local_addr().unwrap().to_string(),device:"generated".into(),access_key:key.clone(),server_token:key,..Default::default()};
        let config=crate::Config{data_dir:root.0.clone(),state_dir:root.0.clone(),signaling_host:"unused.invalid".into()};
        let sharing=crate::native::SharingConfig{encoder_tuning:Default::default(),session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),source:crate::native::CaptureSource::Synthetic{width:320,height:180,fps:30},fps:30,bitrate_kbps:1200,encoder:crate::native::EncoderPreference::Software,session_limit:None,allow_input:false,audio:None,clipboard:None};
        let (stop,rx)=watch::channel(false);let task=tokio::spawn(host(settings,config,sharing,rx,None));
        let (_held,_)=tokio::time::timeout(Duration::from_secs(2),listener.accept()).await.unwrap().unwrap();
        stop.send_replace(true);
        assert_eq!(tokio::time::timeout(Duration::from_millis(500),task).await.unwrap().unwrap().unwrap().sent,0);
    }
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn self_hosted_wss_service_runs_the_same_native_video_and_audio_without_uu() {
        let root = Fixture(
            std::env::temp_dir().join(format!("eclipse-signal-test-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&root.0).unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = root.0.join("certificate.pem");
        let key = root.0.join("private.pem");
        let token = root.0.join("token");
        std::fs::write(&cert, certificate.cert.pem()).unwrap();
        std::fs::write(&key, certificate.key_pair.serialize_pem()).unwrap();
        AccessKey::create(&token).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let (stop, stop_rx) = watch::channel(false);
        let service = tokio::spawn(server::serve(
            server::Config {
                bind: address,
                certificate: cert.clone(),
                private_key: key,
                token_file: token.clone(),
                stun: vec![],
            },
            stop_rx,
        ));
        let config = self_hosted::Config {
            url: format!("wss://localhost:{}/v1/signal", address.port()),
            token_file: token,
            ca_file: Some(cert),
        };
        let mut online = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match self_hosted::Host::online(&config, "generated").await {
                    Ok(host) => break host,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(self_hosted::presence(&config,&["generated".into(),"absent".into()]).await.unwrap(),vec!["generated"]);
        let viewer = self_hosted::join(&config, "generated").await.unwrap();
        let host = online.accept().await.unwrap();
        assert_eq!(viewer.session, host.session);
        // A busy legacy host remains online and receives no extra connection.
        assert_eq!(self_hosted::presence(&config,&["generated".into()]).await.unwrap(),vec!["generated"]);
        let (viewer, ice) = Authenticated::new(viewer, AccessKey([9; 32]), Role::Viewer);
        let (host, _) = Authenticated::new(host, AccessKey([9; 32]), Role::Host);
        generated_session(viewer, host, ice).await;
        online.close().await;
        stop.send_replace(true);
        service.await.unwrap().unwrap();
    }
}
