//! Bounded RTP reordering followed by strict H.264/HEVC access-unit assembly.
//! Healthy frames have no added delay. Only gaps wait for NACK/retransmission;
//! an expired gap invalidates the AU instead of feeding corrupt FU-A data.
use bytes::Bytes;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use webrtc::rtp::packet::Packet;
use uu_proto::media::VideoCodec;

pub(crate) const MAX_PACKETS: usize = 2048;
pub(crate) const MAX_AU: usize = 16 * 1024 * 1024;

pub struct AccessUnit {
    pub data: Bytes,
    pub timestamp: u32,
    pub epoch: u64,
}
#[derive(Default)]
pub struct ReceiverStats {
    pub received: u64,
    pub lost: u64,
    pub reordered: u64,
    pub late: u64,
    pub damaged: u64,
}
#[derive(Default)]
struct Picture {
    timestamp: u32,
    av1:crate::av1_rtp::Assembler,
    pyrowave:crate::pyrowave_rtp::Assembler,
    data: Vec<u8>,
    fragment: Option<(u16, Vec<u8>)>,
    damaged: bool,
    vcl: bool,
}
impl Picture {
    fn nal(&mut self, nal: &[u8]) -> Result<(), ()> {
        if nal.is_empty() || nal[0] & 0x80 != 0 || !(1..=23).contains(&(nal[0] & 31)) {
            return Err(());
        }
        if self.data.len() + nal.len() + 4 > MAX_AU {
            return Err(());
        }
        self.vcl |= (1..=5).contains(&(nal[0] & 31));
        self.data.extend_from_slice(&[0, 0, 0, 1]);
        self.data.extend_from_slice(nal);
        Ok(())
    }
    fn payload(&mut self, payload: &[u8]) -> Result<(), ()> {
        if payload.is_empty() {
            return Ok(());
        } // RTP padding still occupies a sequence number.
        match payload[0] & 31 {
            1..=23 => {
                if self.fragment.is_some() {
                    return Err(());
                }
                self.nal(payload)
            }
            24 => {
                if self.fragment.is_some() {
                    return Err(());
                }
                let mut offset = 1;
                while offset < payload.len() {
                    let pair = payload.get(offset..offset + 2).ok_or(())?;
                    let length = u16::from_be_bytes([pair[0], pair[1]]) as usize;
                    offset += 2;
                    self.nal(payload.get(offset..offset + length).ok_or(())?)?;
                    offset += length;
                }
                Ok(())
            }
            28 => {
                if payload.len() < 3 || payload[1] & 0x20 != 0 {
                    return Err(());
                }
                let start = payload[1] & 0x80 != 0;
                let end = payload[1] & 0x40 != 0;
                let header = (payload[0] & 0xe0) | (payload[1] & 31);
                if start {
                    if end || self.fragment.is_some() {
                        return Err(());
                    }
                    self.fragment = Some((u16::from(header), vec![header]));
                }
                let (expected, data) = self.fragment.as_mut().ok_or(())?;
                if *expected != u16::from(header) || data.len() + payload.len() - 2 > MAX_AU {
                    return Err(());
                }
                data.extend_from_slice(&payload[2..]);
                if end {
                    let (_, nal) = self.fragment.take().ok_or(())?;
                    self.nal(&nal)?;
                }
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn hevc_nal(&mut self, nal: &[u8]) -> Result<(), ()> {
        if nal.len() < 2 || nal[0] & 0x80 != 0 || nal[1] & 7 == 0
            || (nal[0] >> 1) & 63 >= 48 || self.data.len() + nal.len() + 4 > MAX_AU {
            return Err(());
        }
        self.vcl |= (nal[0] >> 1) & 63 <= 31;
        self.data.extend_from_slice(&[0, 0, 0, 1]);
        self.data.extend_from_slice(nal);
        Ok(())
    }

    /// RFC 7798 single-layer, non-interleaved RTP (sprop-max-don-diff=0).
    /// The common reorder/loss/size limits apply before either codec parser.
    fn hevc_payload(&mut self, payload: &[u8]) -> Result<(), ()> {
        if payload.is_empty() { return Ok(()); }
        if payload.len() < 2 || payload[0] & 0x80 != 0 || payload[1] & 7 == 0 {
            return Err(());
        }
        match (payload[0] >> 1) & 63 {
            0..=47 => {
                if self.fragment.is_some() { return Err(()); }
                self.hevc_nal(payload)
            }
            48 => {
                if self.fragment.is_some() { return Err(()); }
                let mut offset = 2;
                let mut count = 0;
                while offset < payload.len() {
                    let size = payload.get(offset..offset + 2).ok_or(())?;
                    let size = u16::from_be_bytes([size[0], size[1]]) as usize;
                    offset += 2;
                    self.hevc_nal(payload.get(offset..offset + size).ok_or(())?)?;
                    offset += size;
                    count += 1;
                }
                if count < 2 { return Err(()); }
                Ok(())
            }
            49 => {
                if payload.len() < 4 || payload[2] & 63 >= 48 { return Err(()); }
                let start = payload[2] & 0x80 != 0;
                let end = payload[2] & 0x40 != 0;
                let header = [(payload[0] & 0x81) | ((payload[2] & 63) << 1), payload[1]];
                let id = u16::from_be_bytes(header);
                if start {
                    if end || self.fragment.is_some() { return Err(()); }
                    self.fragment = Some((id, header.to_vec()));
                }
                let (expected, data) = self.fragment.as_mut().ok_or(())?;
                if *expected != id || data.len() + payload.len() - 3 > MAX_AU { return Err(()); }
                data.extend_from_slice(&payload[3..]);
                if end {
                    let (_, nal) = self.fragment.take().ok_or(())?;
                    self.hevc_nal(&nal)?;
                }
                Ok(())
            }
            _ => Err(()), // PACI and DON-interleaved streams are not negotiated.
        }
    }
}

pub struct VideoReceiver {
    codec: VideoCodec,
    packets: HashMap<u16, (Packet, Instant)>,
    next: Option<u16>,
    picture: Option<Picture>,
    completed_ts: Option<u32>,
    wait: Duration,
    last_packet: Option<Instant>,
    gap_since: Option<Instant>,
    pub stats: ReceiverStats,
    refresh: bool,
    pub epoch: u64,
}
impl VideoReceiver {
    #[cfg(test)]
    pub fn new(wait: Duration) -> Self {
        Self::with_codec(wait, VideoCodec::H264)
    }
    pub fn with_codec(wait: Duration, codec: VideoCodec) -> Self {
        Self {
            codec,
            packets: HashMap::new(),
            next: None,
            picture: None,
            completed_ts: None,
            wait,
            last_packet: None,
            gap_since: None,
            stats: ReceiverStats::default(),
            refresh: false,
            epoch: 0,
        }
    }
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh)
    }
    /// A negotiated payload format change starts a new reference generation.
    /// Never concatenate fragments across codecs; keep cumulative transport
    /// counters and the repair deadline while retiring the old reorder state.
    pub fn change_codec(&mut self, codec: VideoCodec, first_sequence: u16) {
        self.codec = codec;
        self.packets.clear();
        self.picture = None;
        self.completed_ts = None;
        self.next = Some(first_sequence);
        self.gap_since = None;
        self.last_packet = None;
        self.epoch += 1;
        self.refresh = true;
    }
    pub fn push(&mut self, packet: Packet, now: Instant) -> Vec<AccessUnit> {
        let seq = packet.header.sequence_number;
        let next = *self.next.get_or_insert(seq);
        let distance = seq.wrapping_sub(next) as i16;
        if distance < 0 || self.packets.contains_key(&seq) {
            self.stats.late += 1;
            return self.poll(now);
        }
        if distance > 0 {
            self.stats.reordered += 1;
        }
        if !packet.payload.is_empty() {
            self.last_packet = Some(now);
        }
        self.packets.insert(seq, (packet, now));
        self.poll(now)
    }
    /// Request only active holes, after a short reorder grace, and stop at
    /// the same deadline as AU assembly. Bounded even on a sequence jump.
    pub fn missing(&self, now: Instant) -> Vec<u16> {
        let Some(next) = self.next else {
            return Vec::new();
        };
        let Some(oldest) = self.gap_since else {
            return Vec::new();
        };
        let age = now.duration_since(oldest);
        if age < Duration::from_millis(5) || age >= self.wait {
            return Vec::new();
        }
        let end = self
            .packets
            .keys()
            .map(|seq| seq.wrapping_sub(next) as usize)
            .max()
            .unwrap_or(0)
            .min(MAX_PACKETS);
        (0..end)
            .map(|offset| next.wrapping_add(offset as u16))
            .filter(|seq| !self.packets.contains_key(seq))
            .take(512)
            .collect()
    }
    pub fn poll(&mut self, now: Instant) -> Vec<AccessUnit> {
        let mut ready = Vec::new();
        while let Some(next) = self.next {
            if let Some((packet, _)) = self.packets.remove(&next) {
                self.stats.received += 1;
                self.gap_since = None;
                self.next = Some(next.wrapping_add(1));
                self.assemble(packet, &mut ready);
                continue;
            }
            if self.packets.is_empty() {
                self.gap_since = None;
                break;
            }
            // Cache the hole's deadline. Scanning a growing HashMap twice on
            // every subsequent packet made a loss burst quadratic in packets.
            let oldest = *self.gap_since.get_or_insert_with(|| self.packets.values().map(|(_, at)| *at).min().unwrap());
            if self.packets.len() < MAX_PACKETS && now.duration_since(oldest) < self.wait {
                break;
            }
            let (&seq, (packet, _)) = self.packets.iter().min_by_key(|(seq, _)| seq.wrapping_sub(next)).unwrap();
            let timestamp = packet.header.timestamp;
            self.stats.lost += seq.wrapping_sub(next) as u64;
            self.epoch += 1;
            self.refresh = true;
            // Conservatively invalidate the resumed timestamp too: the gap
            // might contain that picture's first slice or SPS/PPS.
            self.discard_picture();
            self.picture = Some(Picture {
                timestamp,
                damaged: true,
                ..Default::default()
            });
            self.next = Some(seq);
        }
        if self.packets.is_empty()
            && self.last_packet.is_some_and(|at| {
                now.duration_since(at) >= self.wait.max(Duration::from_millis(200))
            })
        {
            self.discard_picture();
        }
        ready
    }
    /// Only holes wait; complete in-order frames still pass immediately.
    pub fn set_repair_wait(&mut self, wait: Duration) { self.wait = wait; }
    fn discard_picture(&mut self) {
        if let Some(picture) = self.picture.take() {
            if picture.vcl
                || picture.fragment.is_some()
                || picture.damaged
                || !picture.data.is_empty()
            {
                self.stats.damaged += 1;
                self.epoch += 1;
                self.refresh = true;
            }
        }
    }
    fn assemble(&mut self, packet: Packet, out: &mut Vec<AccessUnit>) {
        if packet.payload.is_empty() {
            return;
        }
        let ts = packet.header.timestamp;
        // Do not let a late retransmission reopen an already completed AU.
        if self
            .completed_ts
            .is_some_and(|old| ts.wrapping_sub(old) as i32 <= 0)
        {
            return;
        }
        if self.picture.as_ref().is_some_and(|p| p.timestamp != ts) {
            self.discard_picture();
        }
        let picture = self.picture.get_or_insert_with(|| Picture {
            timestamp: ts,
            ..Default::default()
        });
        if !picture.damaged {
            let result = match self.codec {
                VideoCodec::H264 => picture.payload(&packet.payload),
                VideoCodec::Hevc => picture.hevc_payload(&packet.payload),
                VideoCodec::Av1 => picture.av1.push(&packet.payload),
                VideoCodec::Pyrowave=>picture.pyrowave.push(&packet.payload),
            };
            picture.damaged = result.is_err();
        }
        if packet.header.marker {
            let mut picture = self.picture.take().unwrap();
            if self.codec==VideoCodec::Av1 {
                match picture.av1.finish(){Ok((data,frame))=>{picture.data=data;picture.vcl=frame;},Err(())=>picture.damaged=true}
            }
            if self.codec==VideoCodec::Pyrowave {
                match picture.pyrowave.finish(){Ok(data)=>{picture.data=data;picture.vcl=true;},Err(())=>picture.damaged=true}
            }
            self.completed_ts = Some(ts);
            if picture.damaged || picture.fragment.is_some() {
                self.stats.damaged += 1;
                self.epoch += 1;
                self.refresh = true;
            } else if picture.vcl && !picture.data.is_empty() {
                out.push(AccessUnit {
                    data: Bytes::from(picture.data),
                    timestamp: ts,
                    epoch: self.epoch,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Packet {
        Packet {
            header: webrtc::rtp::header::Header {
                sequence_number: seq,
                timestamp: ts,
                marker,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }
    }
    fn generated_av1_packets(sequence: u16, timestamp: u32) -> Vec<Packet> {
        use webrtc::rtp::packetizer::Payloader;
        let mut payloader = webrtc::rtp::codecs::av1::Av1Payloader::default();
        let payloads = payloader
            .payload(5, &Bytes::from_static(uu_media::av1::PROBE_UNITS[0]))
            .unwrap();
        assert!(payloads.len() > 3);
        let count = payloads.len();
        payloads
            .into_iter()
            .enumerate()
            .map(|(index, payload)| {
                packet(
                    sequence.wrapping_add(index as u16),
                    timestamp,
                    index + 1 == count,
                    &payload,
                )
            })
            .collect()
    }
    fn assert_generated_av1_unchanged(data: &[u8]) {
        let expected: Vec<_> = uu_media::av1::obus(uu_media::av1::PROBE_UNITS[0])
            .unwrap()
            .into_iter()
            .filter(|obu| !matches!((obu[0] >> 3) & 15, 2 | 15))
            .collect();
        assert_eq!(uu_media::av1::obus(data).unwrap(), expected);
    }
    #[test]
    fn av1_wrapped_fragment_repair_preserves_the_complete_obus() {
        let now = Instant::now();
        let mut rx = VideoReceiver::with_codec(Duration::from_millis(60), VideoCodec::Av1);
        let packets = generated_av1_packets(65_534, u32::MAX - 10);
        for (index, packet) in packets.iter().enumerate() {
            if index != 1 {
                assert!(rx.push(packet.clone(), now).is_empty());
            }
        }
        assert_eq!(rx.missing(now + Duration::from_millis(10)), vec![65_535]);
        let repaired = rx.push(packets[1].clone(), now + Duration::from_millis(12));
        assert_eq!(repaired.len(), 1);
        assert_generated_av1_unchanged(&repaired[0].data);
        assert_eq!(rx.stats.lost, 0);
        assert!(rx.missing(now + Duration::from_millis(20)).is_empty());
        assert!(rx
            .push(packets[1].clone(), now + Duration::from_millis(21))
            .is_empty());
        assert!(!rx.take_refresh());
        let mut next = Vec::new();
        for packet in generated_av1_packets(65_534u16.wrapping_add(packets.len() as u16), 40) {
            next.extend(rx.push(packet, now + Duration::from_millis(22)));
        }
        assert_eq!(next.len(), 1);
        assert_generated_av1_unchanged(&next[0].data);
    }
    #[test]
    fn av1_missing_fragment_requests_refresh_without_poisoning_next_picture() {
        let now = Instant::now();
        let mut rx = VideoReceiver::with_codec(Duration::from_millis(60), VideoCodec::Av1);
        let packets = generated_av1_packets(10, 100);
        for (index, packet) in packets.iter().enumerate() {
            if index != 1 {
                assert!(rx.push(packet.clone(), now).is_empty());
            }
        }
        assert!(rx.poll(now + Duration::from_millis(61)).is_empty());
        assert!(rx.take_refresh());
        let mut next = Vec::new();
        for packet in generated_av1_packets(10 + packets.len() as u16, 200) {
            next.extend(rx.push(packet, now + Duration::from_millis(62)));
        }
        assert_eq!(next.len(), 1);
        assert!(next[0].epoch > 0); // Both loss and the discarded partial picture invalidate references.
        assert_generated_av1_unchanged(&next[0].data);
        assert!(rx
            .push(packets[1].clone(), now + Duration::from_millis(63))
            .is_empty());
        assert!(!rx.take_refresh());
    }
    #[test]
    fn hevc_aggregation_fragment_repair_wrap_and_format_change_are_isolated() {
        let now = Instant::now();
        let mut rx = VideoReceiver::with_codec(Duration::from_millis(60), VideoCodec::Hevc);
        // VPS + IDR in one AP, no DON fields in the negotiated format.
        let au = rx.push(packet(65_533, 100, true, &[96,1,0,3,64,1,0x80,0,3,38,1,0x80]), now);
        assert_eq!(au[0].data.as_ref(), &[0,0,0,1,64,1,0x80,0,0,0,1,38,1,0x80]);
        assert!(rx.push(packet(65_534, 200, false, &[98,1,0x93,1]), now).is_empty());
        assert!(rx.push(packet(0, 200, true, &[98,1,0x53,3]), now).is_empty());
        let au = rx.push(packet(65_535, 200, false, &[98,1,0x13,2]), now);
        assert_eq!(au[0].data.as_ref(), &[0,0,0,1,38,1,1,2,3]);
        rx.push(packet(1,300,false,&[98,1,0x93,4]),now);
        rx.change_codec(VideoCodec::H264,2);
        assert!(rx.push(packet(1,300,true,&[98,1,0x53,9]),now).is_empty());
        let au = rx.push(packet(2,400,true,&[0x65,0x80]),now);
        assert_eq!(au[0].data.as_ref(),&[0,0,0,1,0x65,0x80]);
        assert_eq!(au[0].epoch,1);
        rx.change_codec(VideoCodec::Hevc,3);
        let au=rx.push(packet(3,500,true,&[38,1,0x80]),now);
        assert_eq!(au[0].data.as_ref(),&[0,0,0,1,38,1,0x80]);
        assert_eq!(au[0].epoch,2);
    }
    #[test]
    fn hevc_incomplete_malformed_and_mixed_fragments_never_reach_decoder() {
        let now = Instant::now();
        for payloads in [
            vec![vec![98,1,0x93,1],vec![98,1,0x54,2]], // changed NAL type
            vec![vec![98,1,0xd3,1]], // start + end forbidden
            vec![vec![98,1,0x93,1]], // unfinished FU at marker
            vec![vec![96,1,0,8,38,1,0x80]], // AP length exceeds payload
            vec![vec![96,1,0,3,38,1,0x80]], // AP needs at least two NALs
            vec![vec![38,0,0x80]], // invalid temporal id
            vec![vec![0xa6,1,0x80]], // forbidden bit
            vec![vec![98,1,0x93,1],vec![98,2,0x53,2]], // changed temporal id
        ] {
            let mut rx=VideoReceiver::with_codec(Duration::from_millis(60),VideoCodec::Hevc);
            for (i,payload) in payloads.iter().enumerate() {
                assert!(rx.push(packet(i as u16,100,i+1==payloads.len(),payload),now).is_empty());
            }
            assert!(rx.take_refresh());
            let next=rx.push(packet(payloads.len() as u16,200,true,&[38,1,0x80]),now);
            assert_eq!(next.len(),1);
            assert_eq!(next[0].epoch,1);
        }
        let mut rx=VideoReceiver::with_codec(Duration::from_millis(60),VideoCodec::Hevc);
        rx.push(packet(10,300,false,&[98,1,0x93,1]),now);
        rx.push(packet(12,300,true,&[98,1,0x53,3]),now);
        assert!(rx.poll(now+Duration::from_millis(70)).is_empty());
        assert!(rx.take_refresh());
    }
    #[test]
    fn nack_holes_expire_and_stop_after_repair_and_missing_tail_times_out() {
        let now = Instant::now();
        let mut rx = VideoReceiver::new(Duration::from_millis(60));
        rx.push(packet(1, 1, false, &[0x7c, 0x85, 1]), now);
        rx.push(packet(3, 1, true, &[0x7c, 0x45, 3]), now);
        assert!(rx.missing(now).is_empty());
        assert_eq!(rx.missing(now + Duration::from_millis(10)), vec![2]);
        assert_eq!(
            rx.push(
                packet(2, 1, false, &[0x7c, 0x05, 2]),
                now + Duration::from_millis(12)
            )
            .len(),
            1
        );
        assert!(rx.missing(now + Duration::from_millis(20)).is_empty());
        rx.push(packet(4, 2, false, &[0x7c, 0x85, 4]), now);
        rx.push(packet(6, 2, true, &[0x7c, 0x45, 6]), now);
        assert!(rx.missing(now + Duration::from_millis(60)).is_empty());
        rx.poll(now + Duration::from_millis(61));
        assert!(rx.take_refresh());
        let mut rx = VideoReceiver::new(Duration::from_millis(60));
        rx.push(packet(1, 1, false, &[0x7c, 0x85, 1]), now);
        rx.poll(now + Duration::from_millis(201));
        assert!(rx.take_refresh());
        assert_eq!(rx.stats.damaged, 1);
    }
    #[test]
    fn out_of_order_fu_a_is_repaired_before_deadline_including_sequence_wrap() {
        let now = Instant::now();
        let mut rx = VideoReceiver::new(Duration::from_millis(60));
        assert!(rx
            .push(packet(65535, 0, false, &[0x7c, 0x85, 1]), now)
            .is_empty());
        assert!(rx
            .push(packet(1, 0, true, &[0x7c, 0x45, 3]), now)
            .is_empty());
        let ready = rx.push(
            packet(0, 0, false, &[0x7c, 0x05, 2]),
            now + Duration::from_millis(20),
        );
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].data.as_ref(), [0, 0, 0, 1, 0x65, 1, 2, 3]);
        assert_eq!(rx.stats.lost, 0);
        assert!(!rx.take_refresh());
    }
    #[test]
    fn lost_fragment_never_reaches_decoder_or_contaminates_next_picture() {
        let now = Instant::now();
        let mut rx = VideoReceiver::new(Duration::from_millis(60));
        rx.push(packet(10, 1, false, &[0x7c, 0x85, 1]), now);
        rx.push(packet(12, 1, true, &[0x7c, 0x45, 3]), now);
        assert!(rx.poll(now + Duration::from_millis(61)).is_empty());
        assert!(rx.take_refresh());
        assert_eq!(rx.stats.lost, 1);
        let ready = rx.push(
            packet(13, 2, true, &[0x65, 9]),
            now + Duration::from_millis(62),
        );
        assert_eq!(ready[0].data.as_ref(), [0, 0, 0, 1, 0x65, 9]);
        assert!(rx
            .push(
                packet(11, 1, false, &[0x7c, 0x05, 2]),
                now + Duration::from_millis(63)
            )
            .is_empty());
        assert_eq!(rx.stats.late, 1);
    }
    #[test]
    fn incomplete_marker_or_malformed_stap_is_discarded() {
        let now = Instant::now();
        let mut rx = VideoReceiver::new(Duration::from_millis(60));
        rx.push(packet(1, 1, false, &[0x61, 1]), now);
        let ready = rx.push(packet(2, 2, true, &[0x65, 2]), now);
        assert_eq!(ready.len(), 1);
        assert_eq!(rx.stats.damaged, 1);
        assert!(rx.take_refresh());
        assert!(rx.push(packet(3, 3, true, &[24, 0]), now).is_empty());
        assert!(rx
            .push(packet(4, 4, true, &[0x7c, 0x41, 1]), now)
            .is_empty());
        assert_eq!(rx.stats.damaged, 3);
    }
    #[test]
    fn duplicates_are_not_decoded_twice_and_healthy_frames_have_no_buffer_delay() {
        let now = Instant::now();
        let mut rx = VideoReceiver::new(Duration::from_secs(1));
        let p = packet(1, u32::MAX - 10, true, &[0x65, 9]);
        assert_eq!(rx.push(p.clone(), now).len(), 1);
        assert!(rx.push(p, now).is_empty());
        assert_eq!(rx.push(packet(2, 10, true, &[0x61, 10]), now).len(), 1);
        assert_eq!(rx.stats.lost, 0);
    }
}
