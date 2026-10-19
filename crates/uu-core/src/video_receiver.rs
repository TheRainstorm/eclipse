//! Bounded RTP reordering followed by strict H.264 access-unit assembly.
//! Healthy frames have no added delay. Only gaps wait for NACK/retransmission;
//! an expired gap invalidates the AU instead of feeding corrupt FU-A data.
use bytes::Bytes;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use webrtc::rtp::packet::Packet;

const MAX_PACKETS: usize = 2048;
const MAX_AU: usize = 16 * 1024 * 1024;

pub struct AccessUnit {
    pub data: Bytes,
    pub timestamp: u32,
    pub epoch: u64,
}
#[derive(Default)]
pub struct ReceiverStats {
    pub lost: u64,
    pub reordered: u64,
    pub late: u64,
    pub damaged: u64,
}
#[derive(Default)]
struct Picture {
    timestamp: u32,
    data: Vec<u8>,
    fragment: Option<(u8, Vec<u8>)>,
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
                    self.fragment = Some((header, vec![header]));
                }
                let (expected, data) = self.fragment.as_mut().ok_or(())?;
                if *expected != header || data.len() + payload.len() - 2 > MAX_AU {
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
}

pub struct VideoReceiver {
    packets: HashMap<u16, (Packet, Instant)>,
    next: Option<u16>,
    picture: Option<Picture>,
    completed_ts: Option<u32>,
    wait: Duration,
    pub stats: ReceiverStats,
    refresh: bool,
    pub epoch: u64,
}
impl VideoReceiver {
    pub fn new(wait: Duration) -> Self {
        Self {
            packets: HashMap::new(),
            next: None,
            picture: None,
            completed_ts: None,
            wait,
            stats: ReceiverStats::default(),
            refresh: false,
            epoch: 0,
        }
    }
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh)
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
        self.packets.insert(seq, (packet, now));
        self.poll(now)
    }
    pub fn poll(&mut self, now: Instant) -> Vec<AccessUnit> {
        let mut ready = Vec::new();
        while let Some(next) = self.next {
            if let Some((packet, _)) = self.packets.remove(&next) {
                self.next = Some(next.wrapping_add(1));
                self.assemble(packet, &mut ready);
                continue;
            }
            let nearest = self
                .packets
                .iter()
                .min_by_key(|(seq, _)| seq.wrapping_sub(next));
            let Some((&seq, (packet, _))) = nearest else {
                break;
            };
            let oldest = self.packets.values().map(|(_, at)| *at).min().unwrap();
            if self.packets.len() < MAX_PACKETS && now.duration_since(oldest) < self.wait {
                break;
            }
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
        ready
    }
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
        if !picture.damaged && picture.payload(&packet.payload).is_err() {
            picture.damaged = true;
        }
        if packet.header.marker {
            let picture = self.picture.take().unwrap();
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
