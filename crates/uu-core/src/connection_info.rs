//! On-demand, presentation-only connection details. No addresses enter the HUD
//! or automatic diagnostics, and this task never runs on the media packet path.
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use webrtc::{
    ice_transport::{ice_candidate::RTCIceCandidate, ice_candidate_type::RTCIceCandidateType},
    peer_connection::{peer_connection_state::RTCPeerConnectionState, RTCPeerConnection},
    stats::StatsReportType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Interface,
    StunMapping,
    PeerMapping,
    Relay,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Endpoint {
    pub address: SocketAddr,
    pub protocol: String,
    pub kind: Kind,
    /// Related/base endpoint is reported only when ICE actually supplies it.
    pub base: Option<SocketAddr>,
}
fn kind(candidate: RTCIceCandidateType) -> Option<Kind> {
    Some(match candidate {
        RTCIceCandidateType::Host => Kind::Interface,
        RTCIceCandidateType::Srflx => Kind::StunMapping,
        RTCIceCandidateType::Prflx => Kind::PeerMapping,
        RTCIceCandidateType::Relay => Kind::Relay,
        _ => return None,
    })
}
fn endpoint(candidate: &RTCIceCandidate) -> Option<Endpoint> {
    let ip: IpAddr = candidate.address.parse().ok()?;
    Some(Endpoint {
        address: SocketAddr::new(ip, candidate.port),
        protocol: candidate.protocol.to_string(),
        kind: kind(candidate.typ)?,
        base: candidate
            .related_address
            .parse::<IpAddr>()
            .ok()
            .filter(|_| candidate.related_port > 0)
            .map(|ip| SocketAddr::new(ip, candidate.related_port)),
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedPath {
    pub local: Endpoint,
    pub remote: Endpoint,
}
impl SelectedPath {
    pub fn relayed(&self) -> bool {
        self.local.kind == Kind::Relay || self.remote.kind == Kind::Relay
    }
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub service: String,
    pub peer_state: String,
    pub ice_state: String,
    pub local: Vec<Endpoint>,
    pub remote: Vec<Endpoint>,
    pub selected: Option<SelectedPath>,
    pub updated_ms: u64,
}
impl Snapshot {
    /// Explicit copy action only; never automatically write address information.
    pub fn copy_text(&self) -> String {
        let mut text = uu_i18n::format!(
            "连接服务：{}\n连接状态：{} / ICE {}\n",
            self.service, self.peer_state, self.ice_state
        );
        if let Some(path) = &self.selected {
            text += &uu_i18n::format!(
                "实际媒体路径：{}\n本机：{} ({:?}, {})\n远端：{} ({:?}, {})\n",
                if path.relayed() { uu_i18n::text("中继") } else { uu_i18n::text("直连") },
                path.local.address,
                path.local.kind,
                path.local.protocol,
                path.remote.address,
                path.remote.kind,
                path.remote.protocol
            );
        }
        for (side, candidates) in [(uu_i18n::text("本机"), &self.local), (uu_i18n::text("远端"), &self.remote)] {
            text += &uu_i18n::format!("{side}地址：\n");
            for candidate in candidates {
                text += &format!(
                    "  {:?} {} {}",
                    candidate.kind, candidate.protocol, candidate.address
                );
                if let Some(base) = candidate.base {
                    text += &uu_i18n::format!(" → 网卡 {base}");
                }
                text.push('\n');
            }
        }
        text
    }
}
#[derive(Default)]
pub struct Details {
    snapshot: RwLock<Snapshot>,
    requested: AtomicBool,
}
impl Details {
    pub fn set_service(&self, label: &str) {
        self.snapshot.write().unwrap().service = label.to_owned();
    }
    pub fn request(&self) -> Snapshot {
        self.requested.store(true, Ordering::Release);
        self.snapshot.read().unwrap().clone()
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().unwrap().clone()
    }
}

fn merge(candidates: &mut Vec<Endpoint>, value: Endpoint) {
    if let Some(existing) = candidates.iter_mut().find(|v| {
        v.address == value.address && v.protocol == value.protocol && v.kind == value.kind
    }) {
        if value.base.is_some() {
            existing.base = value.base;
        }
    } else if candidates.len() < 128 {
        candidates.push(value);
    }
}
fn from_sdp(sdp: &str) -> Vec<Endpoint> {
    use webrtc::ice::candidate::candidate_base::unmarshal_candidate;
    let mut result = Vec::new();
    for line in sdp.lines().filter_map(|l| l.strip_prefix("a=candidate:")) {
        if let Ok(candidate) = unmarshal_candidate(line) {
            let candidate: Arc<dyn webrtc::ice::candidate::Candidate + Send + Sync> =
                Arc::new(candidate);
            if let Some(value) = endpoint(&RTCIceCandidate::from(&candidate)) {
                merge(&mut result, value);
            }
        }
    }
    result
}
pub(crate) async fn refresh(pc: &RTCPeerConnection, details: &Details) {
    let mut local = pc
        .local_description()
        .await
        .map(|s| from_sdp(&s.sdp))
        .unwrap_or_default();
    let mut remote = pc
        .remote_description()
        .await
        .map(|s| from_sdp(&s.sdp))
        .unwrap_or_default();
    let reports = pc.get_stats().await;
    for report in reports.reports.values() {
        let (candidate, side) = match report {
            StatsReportType::LocalCandidate(c) => (c, &mut local),
            StatsReportType::RemoteCandidate(c) => (c, &mut remote),
            _ => continue,
        };
        let Ok(ip) = candidate.ip.parse() else {
            continue;
        };
        let candidate_type = candidate.candidate_type.to_string();
        let candidate_kind = match candidate_type.as_str() {
            "host" => Kind::Interface,
            "srflx" => Kind::StunMapping,
            "prflx" => Kind::PeerMapping,
            "relay" => Kind::Relay,
            _ => continue,
        };
        let protocol = if candidate.network_type.is_tcp() {
            "tcp"
        } else {
            "udp"
        };
        merge(
            side,
            Endpoint {
                address: SocketAddr::new(ip, candidate.port),
                protocol: protocol.into(),
                kind: candidate_kind,
                base: None,
            },
        );
    }
    let selected = pc
        .sctp()
        .transport()
        .ice_transport()
        .get_selected_candidate_pair()
        .await
        .and_then(|pair| {
            let local = endpoint(pair.local())?;
            let remote = endpoint(pair.remote())?;
            Some(SelectedPath { local, remote })
        });
    if let Some(path) = &selected {
        merge(&mut local, path.local.clone());
        merge(&mut remote, path.remote.clone());
    }
    local.sort();
    remote.sort();
    let mut snapshot = details.snapshot.write().unwrap();
    *snapshot = Snapshot {
        service: snapshot.service.clone(),
        peer_state: pc.connection_state().to_string(),
        ice_state: pc.ice_connection_state().to_string(),
        local,
        remote,
        selected,
        updated_ms: crate::control_wire::now_ms(),
    };
}
pub(crate) struct Monitor(tokio::task::JoinHandle<()>);
impl Monitor {
    pub fn start(pc: &Arc<RTCPeerConnection>, stats: &Arc<crate::media::MediaStats>) -> Self {
        let pc = Arc::downgrade(pc);
        let stats = Arc::downgrade(stats);
        Self(tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut connected_snapshot = false;
            loop {
                timer.tick().await;
                let (Some(pc), Some(stats)) = (pc.upgrade(), stats.upgrade()) else {
                    break;
                };
                let state = pc.connection_state();
                let closed = matches!(
                    state,
                    RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed
                );
                if stats.connection.requested.swap(false, Ordering::AcqRel)
                    || (!connected_snapshot && state == RTCPeerConnectionState::Connected)
                    || closed
                {
                    refresh(&pc, &stats.connection).await;
                    connected_snapshot |= state == RTCPeerConnectionState::Connected;
                }
                if closed {
                    break;
                }
            }
        }))
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdp_preserves_mapping_and_base_without_confusing_relay_with_public_address() {
        let candidates=from_sdp("v=0\r\na=candidate:1 1 udp 100 192.0.2.10 4444 typ host\r\na=candidate:2 1 udp 99 198.51.100.2 5555 typ srflx raddr 192.0.2.10 rport 4444\r\na=candidate:3 1 udp 98 203.0.113.3 6666 typ relay raddr 198.51.100.2 rport 5555\r\n");
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[1].kind, Kind::StunMapping);
        assert_eq!(candidates[1].base, Some("192.0.2.10:4444".parse().unwrap()));
        assert_eq!(candidates[2].kind, Kind::Relay);
        let ipv6 = Endpoint {
            address: "[2001:db8::1]:1234".parse().unwrap(),
            protocol: "udp".into(),
            kind: Kind::Interface,
            base: None,
        };
        assert_eq!(ipv6.address.to_string(), "[2001:db8::1]:1234");
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn details_report_the_exact_selected_pair_and_close_without_retaining_the_session() {
        let ip = "127.0.0.1".parse().unwrap();
        let stats = Arc::new(crate::media::MediaStats::default());
        let mut viewer = crate::native_media::NativePeer::new(ip, Some(stats.clone()), false)
            .await
            .unwrap();
        let mut host = crate::native_media::NativePeer::new(ip, None, true)
            .await
            .unwrap();
        let (frames, _rx) = crate::media::frame_channel();
        viewer.receiver(frames, stats.clone()).await.unwrap();
        host.sender_for_color(Arc::new(AtomicBool::new(false)),uu_proto::media::VideoCodec::H264,false).await.unwrap();
        let offer = viewer.description(true).await.unwrap();
        let local = from_sdp(&offer.sdp)[0].address;
        host.remote(offer, ip).await.unwrap();
        let answer = host.description(false).await.unwrap();
        let remote = from_sdp(&answer.sdp)[0].address;
        viewer.remote(answer, ip).await.unwrap();
        let (a, b) = tokio::join!(viewer.connected(), host.connected());
        a.unwrap();
        b.unwrap();
        refresh(&viewer.pc, &stats.connection).await;
        let details = stats.connection.snapshot();
        let selected = details.selected.as_ref().unwrap();
        assert_eq!(selected.local.address, local);
        assert_eq!(selected.remote.address, remote);
        assert!(!selected.relayed());
        assert!(details.local.iter().all(|v| v.kind == Kind::Interface));
        assert!(!details.copy_text().contains("credential"));
        viewer.close().await;
        host.close().await;
        drop(viewer);
        drop(host);
    }
}
