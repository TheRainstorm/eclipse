//! RFC 4588 SSRC-multiplexed RTX bridge for webrtc-rs 0.11.
//! Its repair reader discards packets after interceptors read them. Tap that
//! boundary and deliver negotiated repairs to the same bounded video reorder
//! queue as primary RTP. No changes to the third-party crate are required.
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::atomic::Ordering,
    sync::{Arc, Mutex, RwLock},
};
use tokio::sync::mpsc;
use webrtc::{
    interceptor::{
        self, stream_info::StreamInfo, Attributes, Interceptor, InterceptorBuilder, RTCPReader,
        RTCPWriter, RTPReader, RTPWriter,
    },
    rtp::packet::Packet,
};

type Result<T> = std::result::Result<T, interceptor::Error>;

#[derive(Clone, Debug)]
struct Route {
    primary: u32,
    payloads: HashMap<u8, u8>,
}
#[derive(Default)]
pub struct RepairBus {
    routes: RwLock<HashMap<u32, Route>>,
    consumers: Mutex<HashMap<u32, mpsc::Sender<Packet>>>,
    stats: Option<Arc<crate::media::MediaStats>>,
}
impl RepairBus {
    pub fn new(stats: Option<Arc<crate::media::MediaStats>>) -> Arc<Self> {
        Arc::new(Self {
            stats,
            ..Self::default()
        })
    }
    pub fn configure(&self, sdp: &str) {
        *self.routes.write().unwrap() = routes(sdp);
    }
    pub fn subscribe(&self, ssrc: u32) -> mpsc::Receiver<Packet> {
        let (tx, rx) = mpsc::channel(512);
        self.consumers.lock().unwrap().insert(ssrc, tx);
        rx
    }
    pub fn unsubscribe(&self, ssrc: u32) {
        self.consumers.lock().unwrap().remove(&ssrc);
    }
    fn tap(&self, packet: &Packet) {
        let restored = {
            let routes = self.routes.read().unwrap();
            let Some(route) = routes.get(&packet.header.ssrc) else {
                return;
            };
            if let Some(s) = &self.stats {
                s.rtx_packets.fetch_add(1, Ordering::Relaxed);
            }
            restore(packet, route)
        };
        let Some(packet) = restored else {
            self.dropped();
            return;
        };
        let consumers = self.consumers.lock().unwrap();
        if consumers
            .get(&packet.header.ssrc)
            .map_or(true, |tx| tx.try_send(packet).is_err())
        {
            self.dropped();
        }
    }
    fn dropped(&self) {
        if let Some(s) = &self.stats {
            s.rtx_drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}
fn restore(packet: &Packet, route: &Route) -> Option<Packet> {
    let payload = *route.payloads.get(&packet.header.payload_type)?;
    if packet.payload.len() < 3 {
        return None;
    }
    let mut repaired = packet.clone();
    repaired.header.ssrc = route.primary;
    repaired.header.payload_type = payload;
    repaired.header.sequence_number = u16::from_be_bytes([packet.payload[0], packet.payload[1]]);
    repaired.header.padding = false;
    repaired.payload = packet.payload.slice(2..);
    Some(repaired)
}
fn routes(sdp: &str) -> HashMap<u32, Route> {
    let mut result = HashMap::new();
    // Resolve apt and FID within each negotiated video section, never by PT
    // guesses or by assuming repair SSRC == primary + 1.
    for section in sdp.split("m=").skip(1) {
        let mut lines = section.lines();
        let Some(media) = lines.next() else { continue };
        let words: Vec<_> = media.split_whitespace().collect();
        if words.first() != Some(&"video") || words.get(1) == Some(&"0") {
            continue;
        }
        let offered: Vec<u8> = words
            .iter()
            .skip(3)
            .filter_map(|w| w.parse().ok())
            .collect();
        let mut h264 = Vec::new();
        let mut rtx = Vec::new();
        let mut apt = HashMap::new();
        let mut fid = Vec::new();
        for line in lines.map(str::trim) {
            if let Some(value) = line.strip_prefix("a=rtpmap:") {
                if let Some((pt, codec)) = value.split_once(' ') {
                    if let Ok(pt) = pt.parse::<u8>() {
                        if codec.eq_ignore_ascii_case("H264/90000") {
                            h264.push(pt);
                        }
                        if codec.eq_ignore_ascii_case("rtx/90000") {
                            rtx.push(pt);
                        }
                    }
                }
            }
            if let Some(value) = line.strip_prefix("a=fmtp:") {
                if let Some((pt, params)) = value.split_once(' ') {
                    for parameter in params.split(';').map(str::trim) {
                        if let Some(original) = parameter.strip_prefix("apt=") {
                            if let (Ok(pt), Ok(original)) =
                                (pt.parse::<u8>(), original.parse::<u8>())
                            {
                                apt.insert(pt, original);
                            }
                        }
                    }
                }
            }
            if let Some(value) = line.strip_prefix("a=ssrc-group:FID ") {
                let pair: Vec<u32> = value
                    .split_whitespace()
                    .filter_map(|v| v.parse().ok())
                    .collect();
                if pair.len() == 2 && pair[0] != 0 && pair[1] != 0 && pair[0] != pair[1] {
                    fid.push((pair[0], pair[1]));
                }
            }
        }
        apt.retain(|pt, original| {
            rtx.contains(pt)
                && h264.contains(original)
                && offered.contains(pt)
                && offered.contains(original)
        });
        if !apt.is_empty() {
            for (primary, repair) in fid {
                result.insert(
                    repair,
                    Route {
                        primary,
                        payloads: apt.clone(),
                    },
                );
            }
        }
    }
    result
}
#[derive(Clone)]
pub struct RepairTap(pub Arc<RepairBus>);
impl InterceptorBuilder for RepairTap {
    fn build(&self, _id: &str) -> Result<Arc<dyn Interceptor + Send + Sync>> {
        Ok(Arc::new(self.clone()))
    }
}
struct Reader {
    parent: Arc<dyn RTPReader + Send + Sync>,
    bus: Arc<RepairBus>,
}
#[async_trait]
impl RTPReader for Reader {
    async fn read(&self, buf: &mut [u8], attributes: &Attributes) -> Result<(Packet, Attributes)> {
        let (packet, attributes) = self.parent.read(buf, attributes).await?;
        self.bus.tap(&packet);
        Ok((packet, attributes))
    }
}
#[async_trait]
impl Interceptor for RepairTap {
    async fn bind_remote_stream(
        &self,
        _: &StreamInfo,
        parent: Arc<dyn RTPReader + Send + Sync>,
    ) -> Arc<dyn RTPReader + Send + Sync> {
        Arc::new(Reader {
            parent,
            bus: self.0.clone(),
        })
    }
    async fn bind_rtcp_reader(
        &self,
        reader: Arc<dyn RTCPReader + Send + Sync>,
    ) -> Arc<dyn RTCPReader + Send + Sync> {
        reader
    }
    async fn bind_rtcp_writer(
        &self,
        writer: Arc<dyn RTCPWriter + Send + Sync>,
    ) -> Arc<dyn RTCPWriter + Send + Sync> {
        writer
    }
    async fn bind_local_stream(
        &self,
        _: &StreamInfo,
        writer: Arc<dyn RTPWriter + Send + Sync>,
    ) -> Arc<dyn RTPWriter + Send + Sync> {
        writer
    }
    async fn unbind_local_stream(&self, _: &StreamInfo) {}
    async fn unbind_remote_stream(&self, _: &StreamInfo) {}
    async fn close(&self) -> Result<()> {
        self.0.consumers.lock().unwrap().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SDP:&str="v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102 103\r\na=rtpmap:102 H264/90000\r\na=rtpmap:103 rtx/90000\r\na=fmtp:103 apt=102;rtx-time=100\r\na=ssrc-group:FID 42 99\r\n";
    fn repair() -> Packet {
        Packet {
            header: webrtc::rtp::header::Header {
                ssrc: 99,
                payload_type: 103,
                sequence_number: 5,
                timestamp: 90000,
                marker: true,
                ..Default::default()
            },
            payload: bytes::Bytes::from_static(&[255, 255, 0x7c, 0x45, 9]),
        }
    }
    #[test]
    fn negotiated_repairs_restore_osn_pt_ssrc_and_keep_picture_metadata() {
        let map = routes(SDP);
        let p = restore(&repair(), &map[&99]).unwrap();
        assert_eq!(
            (
                p.header.ssrc,
                p.header.payload_type,
                p.header.sequence_number
            ),
            (42, 102, 65535)
        );
        assert_eq!(p.header.timestamp, 90000);
        assert!(p.header.marker);
        assert_eq!(&p.payload[..], &[0x7c, 0x45, 9]);
        let mut p = repair();
        p.payload = bytes::Bytes::from_static(&[0]);
        assert!(restore(&p, &map[&99]).is_none());
        p = repair();
        p.header.payload_type = 104;
        assert!(restore(&p, &map[&99]).is_none());
        assert!(routes(&SDP.replace("apt=102", "apt=104")).is_empty());
        assert!(routes(&SDP.replace("m=video 9", "m=video 0")).is_empty());
    }
    #[test]
    fn repairs_reach_reorder_queue_and_duplicate_is_not_decoded_again() {
        use crate::video_receiver::VideoReceiver;
        use std::time::{Duration, Instant};
        let bus = RepairBus::new(None);
        bus.configure(SDP);
        let mut repairs = bus.subscribe(42);
        let mut rx = VideoReceiver::new(Duration::from_millis(60));
        let now = Instant::now();
        let mut start = repair();
        start.header.ssrc = 42;
        start.header.payload_type = 102;
        start.header.sequence_number = 65534;
        start.header.marker = false;
        start.payload = bytes::Bytes::from_static(&[0x7c, 0x85, 7]);
        assert!(rx.push(start, now).is_empty());
        bus.tap(&repair());
        let out = rx.push(repairs.try_recv().unwrap(), now);
        assert_eq!(out.len(), 1);
        assert_eq!(&out[0].data[..], &[0, 0, 0, 1, 0x65, 7, 9]);
        bus.tap(&repair());
        assert!(rx.push(repairs.try_recv().unwrap(), now).is_empty());
        assert_eq!(rx.stats.lost, 0);
    }
}
