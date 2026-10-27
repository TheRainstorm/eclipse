//! Bounded pointer metadata on its own SCTP stream. Video surfaces stay on GPU.
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
pub use uu_capture::cursor::{Description, Shape, Snapshot, Stream};
use webrtc::{
    data_channel::{data_channel_state::RTCDataChannelState, RTCDataChannel},
    peer_connection::RTCPeerConnection,
};

const ATTRIBUTE: &str = "x-eclipse-cursor-v1";
const LABEL: &str = "eclipse-cursor-v1";
const CHUNK: usize = 8192;
const MAX_MESSAGE: usize = 16 * 1024;
const MAX_COMPRESSED: usize = uu_capture::cursor::MAX_BYTES + 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    revision: u64,
    shape_id: u64,
    position: Option<[i32; 2]>,
    desktop: [u32; 2],
}
impl Position {
    fn valid(&self) -> bool {
        self.revision > 0
            && self.desktop.iter().all(|v| (1..=32768).contains(v))
            && self
                .position
                .is_none_or(|p| p.iter().all(|v| (-32768..=32768).contains(v)))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Packet {
    Position {
        state: Position,
    },
    Shape {
        id: u64,
        description: Description,
        total: usize,
        offset: usize,
        data: String,
    },
}
pub(crate) fn advertise(
    sdp: &mut webrtc::peer_connection::sdp::session_description::RTCSessionDescription,
) -> Result<()> {
    crate::native_quality::put(sdp, ATTRIBUTE, &true)
}
pub(crate) fn offered(
    sdp: &webrtc::peer_connection::sdp::session_description::RTCSessionDescription,
) -> Result<bool> {
    Ok(crate::native_quality::get::<bool>(sdp, ATTRIBUTE)?.unwrap_or(false))
}
pub(crate) fn accepts(channel: &RTCDataChannel) -> bool {
    channel.label() == LABEL
        && channel.ordered()
        && channel.max_retransmits() == 0
        && channel.max_packet_lifetime() == 0
}
struct Partial {
    id: u64,
    description: Description,
    total: usize,
    data: Vec<u8>,
}
#[derive(Default)]
struct Received {
    position: Option<Position>,
    shape: Option<(u64, Arc<Shape>)>,
    partial: Option<Partial>,
}
impl Received {
    fn receive(&mut self, packet: Packet) -> Result<()> {
        match packet {
            Packet::Position { state } => {
                ensure!(state.valid(), "invalid cursor position");
                if self
                    .position
                    .as_ref()
                    .is_none_or(|old| state.revision > old.revision)
                {
                    self.position = Some(state);
                }
            }
            Packet::Shape {
                id,
                description,
                total,
                offset,
                data,
            } => {
                ensure!(
                    id > 0
                        && description.bytes().is_some()
                        && (1..=MAX_COMPRESSED).contains(&total)
                        && data.len() <= CHUNK.div_ceil(3) * 4,
                    "invalid cursor shape header"
                );
                let data = STANDARD.decode(data)?;
                ensure!(
                    !data.is_empty()
                        && data.len() <= CHUNK
                        && offset <= total
                        && data.len() <= total - offset,
                    "invalid cursor shape chunk"
                );
                if self.shape.as_ref().is_some_and(|(old, _)| id <= *old) {
                    return Ok(());
                }
                if offset == 0 {
                    self.partial = Some(Partial {
                        id,
                        description,
                        total,
                        data: Vec::with_capacity(total),
                    });
                }
                let part = self
                    .partial
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("missing cursor shape start"))?;
                ensure!(
                    part.id == id
                        && part.description == description
                        && part.total == total
                        && part.data.len() == offset,
                    "out-of-order cursor shape"
                );
                part.data.extend(data);
                if part.data.len() == total {
                    let part = self.partial.take().unwrap();
                    let expected = description.bytes().unwrap();
                    let mut decoded = Vec::new();
                    flate2::read::ZlibDecoder::new(part.data.as_slice())
                        .take(expected as u64 + 1)
                        .read_to_end(&mut decoded)?;
                    ensure!(decoded.len() == expected, "invalid inflated cursor size");
                    self.shape = Some((
                        id,
                        Arc::new(Shape {
                            description,
                            data: decoded,
                        }),
                    ));
                }
            }
        }
        Ok(())
    }
    fn snapshot(&self) -> Option<Snapshot> {
        let p = self.position.as_ref()?;
        let shape = self
            .shape
            .as_ref()
            .filter(|(id, _)| *id == p.shape_id)
            .map(|(_, s)| s.clone());
        Some(Snapshot {
            revision: p.revision,
            shape_id: p.shape_id,
            position: p.position,
            desktop: p.desktop,
            shape,
        })
    }
}
/// One connection's latest state. No cursor events accumulate in the GUI.
#[derive(Default)]
pub struct Client {
    supported: AtomicBool,
    received: Mutex<Received>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl Client {
    pub fn supported(&self) -> bool {
        self.supported.load(Ordering::Acquire)
    }
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().unwrap() = Some(Arc::new(wake));
    }
    fn notify(&self) {
        let wake = self.wake.lock().unwrap().clone();
        if let Some(wake) = wake {
            wake();
        }
    }
    pub(crate) fn reset(&self, supported: bool) {
        *self.received.lock().unwrap() = Received::default();
        self.supported.store(supported, Ordering::Release);
        self.notify();
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        if self.supported() {
            self.received.lock().unwrap().snapshot()
        } else {
            None
        }
    }
}
async fn send(channel: &RTCDataChannel, packet: &Packet) -> Result<()> {
    let data = serde_json::to_vec(packet)?;
    ensure!(data.len() <= MAX_MESSAGE, "cursor packet too large");
    tokio::time::timeout(Duration::from_millis(200), channel.send(&data.into())).await??;
    Ok(())
}
struct Encoded {
    id: u64,
    description: Description,
    data: Vec<u8>,
    offset: usize,
}
impl Encoded {
    fn new(id: u64, shape: &Shape) -> Result<Self> {
        ensure!(shape.valid(), "invalid source cursor");
        let mut compressed =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        compressed.write_all(&shape.data)?;
        let data = compressed.finish()?;
        ensure!(data.len() <= MAX_COMPRESSED, "compressed cursor too large");
        Ok(Self {
            id,
            description: shape.description,
            data,
            offset: 0,
        })
    }
    fn packet(&self) -> Packet {
        Packet::Shape {
            id: self.id,
            description: self.description,
            total: self.data.len(),
            offset: self.offset,
            data: STANDARD
                .encode(&self.data[self.offset..(self.offset + CHUNK).min(self.data.len())]),
        }
    }
}
pub(crate) fn host_channel(channel: Arc<RTCDataChannel>, source: Stream) {
    let weak = Arc::downgrade(&channel);
    let notify = Arc::new(tokio::sync::Notify::new());
    let wake = notify.clone();
    source.set_waker(Some(Arc::new(move || wake.notify_one())));
    channel.on_open(Box::new(move || {
        let weak = weak.clone();
        let source = source.clone();
        let notify = notify.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                struct ClearWake(Stream);
                impl Drop for ClearWake {
                    fn drop(&mut self) {
                        self.0.set_waker(None);
                    }
                }
                let _wake = ClearWake(source.clone());
                // Updates wake immediately; the timer only drains bounded SCTP
                // backpressure and notices close, never paces cursor motion.
                let mut timer = tokio::time::interval(Duration::from_millis(50));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let (mut revision, mut shape_id) = (0, 0);
                let mut encoded: Option<Encoded> = None;
                loop {
                    tokio::select! {_=timer.tick()=>{},_=notify.notified()=>{}}
                    let Some(channel) = weak.upgrade() else {
                        break;
                    };
                    if channel.ready_state() != RTCDataChannelState::Open {
                        break;
                    }
                    if channel.buffered_amount().await > 32 * 1024 {
                        continue;
                    }
                    let latest = source.snapshot();
                    if latest.desktop.contains(&0) {
                        continue;
                    }
                    if latest.revision != revision {
                        let state = Position {
                            revision: latest.revision,
                            shape_id: latest.shape_id,
                            position: latest.position,
                            desktop: latest.desktop,
                        };
                        if send(&channel, &Packet::Position { state }).await.is_err() {
                            break;
                        }
                        revision = latest.revision;
                    }
                    if latest.shape_id != shape_id {
                        shape_id = latest.shape_id;
                        encoded = None;
                        if let Some(shape) = latest.shape {
                            encoded =
                                tokio::task::spawn_blocking(move || Encoded::new(shape_id, &shape))
                                    .await
                                    .ok()
                                    .and_then(Result::ok);
                        }
                    }
                    if let Some(pending) = encoded.as_mut() {
                        while pending.offset < pending.data.len()
                            && channel.buffered_amount().await <= 32 * 1024
                        {
                            if send(&channel, &pending.packet()).await.is_err() {
                                return;
                            }
                            pending.offset = (pending.offset + CHUNK).min(pending.data.len());
                        }
                        if pending.offset == pending.data.len() {
                            encoded = None;
                        }
                    }
                }
            });
        })
    }));
}
pub(crate) struct ViewerChannel {
    _channel: Arc<RTCDataChannel>,
    stats: Arc<crate::media::MediaStats>,
    epoch: u64,
}
impl Drop for ViewerChannel {
    fn drop(&mut self) {
        if self.stats.native_media_epoch.load(Ordering::Acquire) == self.epoch {
            self.stats.native_cursor.reset(false);
        }
    }
}
pub(crate) async fn viewer_channel(
    pc: &RTCPeerConnection,
    stats: Arc<crate::media::MediaStats>,
) -> Result<ViewerChannel> {
    let epoch = stats.native_media_epoch.load(Ordering::Acquire);
    let channel = pc.create_data_channel(LABEL, None).await?;
    let incoming = stats.clone();
    channel.on_message(Box::new(move |message| {
        if incoming.native_media_epoch.load(Ordering::Acquire) != epoch {
            return Box::pin(async {});
        }
        if message.data.len() <= MAX_MESSAGE {
            if let Ok(packet) = serde_json::from_slice(&message.data) {
                let mut reader = incoming.native_cursor.received.lock().unwrap();
                if reader.receive(packet).is_err() {
                    reader.partial = None;
                }
                drop(reader);
                incoming.native_cursor.notify();
            }
        }
        Box::pin(async {})
    }));
    let closed = stats.clone();
    channel.on_close(Box::new(move || {
        if closed.native_media_epoch.load(Ordering::Acquire) == epoch {
            closed.native_cursor.reset(false);
        }
        Box::pin(async {})
    }));
    Ok(ViewerChannel {
        _channel: channel,
        stats,
        epoch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn encrypted_channel_delivers_latest_position_shape_hide_and_clears_on_close() {
        let ip = "127.0.0.1".parse().unwrap();
        let mut host = crate::native_media::NativePeer::new(ip, None, true)
            .await
            .unwrap();
        let stats = Arc::new(crate::media::MediaStats::default());
        stats.native_cursor.reset(true);
        let mut viewer = crate::native_media::NativePeer::new(ip, Some(stats.clone()), false)
            .await
            .unwrap();
        let source = Stream::default();
        let controls = crate::native_control::HostControl::with_injector(
            false,
            None,
            None,
            None,
            Arc::default(),
        )
        .await;
        controls.set_cursor(Some(source.clone()));
        controls.attach(&host.pc);
        let channel = viewer_channel(&viewer.pc, stats.clone()).await.unwrap();
        let result = async {
            let mut offer = viewer.description(true).await?;
            advertise(&mut offer)?;
            ensure!(offered(&offer)?, "cursor request not advertised");
            host.remote(offer, ip).await?;
            let answer = host.description(false).await?;
            viewer.remote(answer, ip).await?;
            tokio::try_join!(host.connected(), viewer.connected())?;
            let shape = Shape {
                description: Description {
                    kind: 2,
                    width: 2,
                    height: 1,
                    pitch: 8,
                    hotspot: [1, 0],
                },
                data: vec![10, 20, 30, 255, 40, 50, 60, 128],
            };
            source.update([3840, 2160], Some([-1, 100]), Some(shape.clone()));
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    if stats
                        .native_cursor
                        .snapshot()
                        .is_some_and(|s| s.position == Some([-1, 100]) && s.shape.is_some())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            for x in 0..100 {
                source.update([3840, 2160], Some([x, 200]), None);
            }
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    if stats
                        .native_cursor
                        .snapshot()
                        .is_some_and(|s| s.position == Some([99, 200]))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            ensure!(
                stats.native_cursor.snapshot().unwrap().shape.unwrap().data == shape.data,
                "cursor bytes changed in transit"
            );
            let large = random_shape();
            source.update([3840, 2160], Some([99, 200]), Some(large.clone()));
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    if stats
                        .native_cursor
                        .snapshot()
                        .is_some_and(|s| s.shape.is_some_and(|s| s.data == large.data))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            source.clear();
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    if stats
                        .native_cursor
                        .snapshot()
                        .is_some_and(|s| s.position.is_none() && s.shape.is_none())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            source.update([1920, 1080], Some([30, 40]), Some(shape));
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    if stats
                        .native_cursor
                        .snapshot()
                        .is_some_and(|s| s.desktop == [1920, 1080] && s.shape.is_some())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            channel._channel.close().await?;
            drop(channel);
            ensure!(
                !stats.native_cursor.supported() && stats.native_cursor.snapshot().is_none(),
                "cursor retained after closing its owner"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        controls.stop().await;
        host.close().await;
        viewer.close().await;
        result.unwrap();
    }
    #[test]
    fn chunks_are_bounded_and_visibility_shape_replacement_and_replay_are_scoped() {
        let shape = random_shape();
        let mut encoded = Encoded::new(4, &shape).unwrap();
        assert!(
            encoded.data.len() > CHUNK,
            "fixture must exercise multiple chunks"
        );
        let mut reader = Received::default();
        reader
            .receive(Packet::Position {
                state: Position {
                    revision: 1,
                    shape_id: 4,
                    position: Some([-2, 100]),
                    desktop: [3840, 2160],
                },
            })
            .unwrap();
        assert!(reader.snapshot().unwrap().shape.is_none());
        while encoded.offset < encoded.data.len() {
            let packet = encoded.packet();
            assert!(serde_json::to_vec(&packet).unwrap().len() <= MAX_MESSAGE);
            reader.receive(packet).unwrap();
            encoded.offset = (encoded.offset + CHUNK).min(encoded.data.len());
        }
        assert_eq!(reader.snapshot().unwrap().shape.unwrap().data, shape.data);
        reader
            .receive(Packet::Position {
                state: Position {
                    revision: 2,
                    shape_id: 4,
                    position: None,
                    desktop: [3840, 2160],
                },
            })
            .unwrap();
        reader
            .receive(Packet::Position {
                state: Position {
                    revision: 1,
                    shape_id: 4,
                    position: Some([20, 30]),
                    desktop: [3840, 2160],
                },
            })
            .unwrap();
        assert!(reader.snapshot().unwrap().position.is_none());
        assert!(reader
            .receive(Packet::Shape {
                id: 5,
                description: shape.description,
                total: MAX_COMPRESSED + 1,
                offset: 0,
                data: STANDARD.encode([1])
            })
            .is_err());
        let mut bomb = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        bomb.write_all(&vec![0; 65537]).unwrap();
        let data = bomb.finish().unwrap();
        assert!(reader
            .receive(Packet::Shape {
                id: 5,
                description: shape.description,
                total: data.len(),
                offset: 0,
                data: STANDARD.encode(data)
            })
            .is_err());
    }
    fn random_shape() -> Shape {
        let mut seed = 0x12345678u32;
        Shape {
            description: Description {
                kind: 2,
                width: 128,
                height: 128,
                pitch: 512,
                hotspot: [2, 3],
            },
            data: (0..65536)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect(),
        }
    }
}
