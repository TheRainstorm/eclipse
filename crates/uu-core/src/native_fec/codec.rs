//! Private XOR format, independent of codec and of Sunshine's RS wire format.
use bytes::Bytes;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};
use webrtc::rtp::{header::Header, packet::Packet};
pub(super) const PAYLOAD: usize = 1120;
pub(super) const GROUP: usize = 8;
pub(super) const MAX_WIRE: usize = 12 + GROUP * 2 + PAYLOAD;
const CACHE: usize = 1024;
const PENDING: usize = 128;
const AGE: Duration = Duration::from_millis(200);

#[derive(Clone, Debug)]
pub(super) struct Parity {
    base: u16,
    timestamp: u32,
    lengths: Vec<u16>,
    markers: u8,
    xor: Bytes,
}
impl Parity {
    fn encode(&self) -> Bytes {
        let mut bytes = Vec::with_capacity(MAX_WIRE);
        bytes.extend_from_slice(b"EFP1");
        bytes.extend_from_slice(&self.base.to_be_bytes());
        bytes.extend_from_slice(&self.timestamp.to_be_bytes());
        bytes.push(self.lengths.len() as u8);
        bytes.push(self.markers);
        for length in &self.lengths {
            bytes.extend_from_slice(&length.to_be_bytes());
        }
        bytes.extend_from_slice(&self.xor);
        bytes.into()
    }
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_WIRE || bytes.get(..4) != Some(b"EFP1") {
            return None;
        }
        let base = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?);
        let timestamp = u32::from_be_bytes(bytes.get(6..10)?.try_into().ok()?);
        let count = *bytes.get(10)? as usize;
        let markers = *bytes.get(11)?;
        if !(2..=GROUP).contains(&count) || (u16::from(markers) >> count) != 0 {
            return None;
        }
        // A group never crosses an access unit; only its last packet may end it.
        if markers != 0 && markers != 1 << (count - 1) {
            return None;
        }
        let lengths: Vec<u16> = bytes
            .get(12..12 + count * 2)?
            .chunks_exact(2)
            .map(|v| u16::from_be_bytes([v[0], v[1]]))
            .collect();
        if lengths.iter().any(|n| *n == 0 || usize::from(*n) > PAYLOAD) {
            return None;
        }
        let xor = bytes.get(12 + count * 2..)?;
        if xor.len() != usize::from(*lengths.iter().max()?) {
            return None;
        }
        Some(Self {
            base,
            timestamp,
            lengths,
            markers,
            xor: Bytes::copy_from_slice(xor),
        })
    }
}
#[derive(Default)]
pub(super) struct Encoder {
    packets: Vec<Packet>,
    credit: usize,
}
impl Encoder {
    pub fn push(&mut self, packet: &Packet) -> Option<Bytes> {
        if packet.payload.is_empty() || packet.payload.len() > PAYLOAD {
            self.packets.clear();
            return None;
        }
        if self.packets.last().is_some_and(|last| {
            last.header.timestamp != packet.header.timestamp
                || last.header.sequence_number.wrapping_add(1) != packet.header.sequence_number
        }) {
            self.packets.clear();
        }
        // Credit is in units of source bytes. Charging five times the estimated
        // parity cost bounds protection to 20%, including a conservative fixed
        // encapsulation allowance; never wait for credit or hold source video.
        self.credit = self
            .credit
            .saturating_add(packet.payload.len())
            .min(2 * (MAX_WIRE + 128) * 5);
        self.packets.push(packet.clone());
        if self.packets.len() < GROUP && !packet.header.marker {
            return None;
        }
        let packets = std::mem::take(&mut self.packets);
        if packets.len() < 2 {
            return None;
        }
        let max = packets.iter().map(|p| p.payload.len()).max().unwrap();
        let cost = (12 + packets.len() * 2 + max + 128) * 5;
        if self.credit < cost {
            return None;
        }
        self.credit -= cost;
        let mut xor = vec![0; max];
        let mut markers = 0;
        for (index, p) in packets.iter().enumerate() {
            for (dst, src) in xor.iter_mut().zip(&p.payload) {
                *dst ^= src;
            }
            if p.header.marker {
                markers |= 1 << index;
            }
        }
        Some(
            Parity {
                base: packets[0].header.sequence_number,
                timestamp: packets[0].header.timestamp,
                lengths: packets.iter().map(|p| p.payload.len() as u16).collect(),
                markers,
                xor: xor.into(),
            }
            .encode(),
        )
    }
}
#[derive(Default)]
pub(super) struct Decoder {
    source: Option<(u32, u8)>,
    packets: HashMap<(u16, u32), (Packet, Instant)>,
    order: VecDeque<((u16, u32), Instant)>,
    groups: VecDeque<(Parity, Instant)>,
    seen: HashMap<(u16, u32), Instant>,
    seen_order: VecDeque<((u16, u32), Instant)>,
}
impl Decoder {
    pub fn source(&mut self, ssrc: u32, payload: u8) {
        if self.source != Some((ssrc, payload)) {
            if self.source.is_some() {
                self.groups.clear();
                self.seen.clear();
                self.seen_order.clear();
            }
            self.packets.clear();
            self.order.clear();
            self.source = Some((ssrc, payload));
        }
    }
    fn expire(&mut self, now: Instant) {
        while self
            .order
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) > AGE || self.order.len() > CACHE)
        {
            let (key, at) = self.order.pop_front().unwrap();
            if self
                .packets
                .get(&key)
                .is_some_and(|(_, stored)| *stored == at)
            {
                self.packets.remove(&key);
            }
        }
        while self
            .seen_order
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) > AGE || self.seen_order.len() > CACHE)
        {
            let (key, at) = self.seen_order.pop_front().unwrap();
            if self.seen.get(&key) == Some(&at) {
                self.seen.remove(&key);
            }
        }
        self.groups.retain(|(_, at)| now.duration_since(*at) <= AGE);
    }
    pub fn packet(&mut self, packet: &Packet, now: Instant) -> Vec<Packet> {
        if self.source != Some((packet.header.ssrc, packet.header.payload_type))
            || packet.payload.is_empty()
            || packet.payload.len() > PAYLOAD
        {
            return Vec::new();
        }
        self.expire(now);
        self.cache(packet.clone(), now);
        self.recover(now)
    }
    fn cache(&mut self, packet: Packet, now: Instant) {
        let key = (packet.header.sequence_number, packet.header.timestamp);
        if self.packets.contains_key(&key) {
            return;
        }
        self.packets.insert(key, (packet, now));
        self.order.push_back((key, now));
        self.expire(now);
    }
    pub fn parity(&mut self, parity: Parity, now: Instant) -> Vec<Packet> {
        self.expire(now);
        let key = (parity.base, parity.timestamp);
        if self.seen.contains_key(&key) {
            return Vec::new();
        }
        self.seen.insert(key, now);
        self.seen_order.push_back((key, now));
        if self.groups.len() >= PENDING {
            self.groups.pop_front();
        }
        self.groups.push_back((parity, now));
        self.recover(now)
    }
    fn recover(&mut self, now: Instant) -> Vec<Packet> {
        let Some((ssrc, payload_type)) = self.source else {
            return Vec::new();
        };
        let mut ready = Vec::new();
        let mut keep = VecDeque::new();
        while let Some((parity, at)) = self.groups.pop_front() {
            let mut missing = None;
            let mut count = 0;
            let mut valid = true;
            for (index, len) in parity.lengths.iter().enumerate() {
                let seq = parity.base.wrapping_add(index as u16);
                if let Some((packet, _)) = self.packets.get(&(seq, parity.timestamp)) {
                    if packet.payload.len() != usize::from(*len)
                        || packet.header.marker != (parity.markers & (1 << index) != 0)
                    {
                        valid = false;
                        break;
                    }
                } else {
                    missing = Some(index);
                    count += 1;
                }
            }
            if !valid || count == 0 {
                continue;
            }
            if count != 1 {
                keep.push_back((parity, at));
                continue;
            }
            let index = missing.unwrap();
            let mut data = parity.xor.to_vec();
            for i in 0..parity.lengths.len() {
                if i != index {
                    let (packet, _) =
                        &self.packets[&(parity.base.wrapping_add(i as u16), parity.timestamp)];
                    for (dst, src) in data.iter_mut().zip(&packet.payload) {
                        *dst ^= src;
                    }
                }
            }
            // Padding above the original size must also match zero. Reject
            // inconsistent parity instead of feeding corrupted data downstream.
            let length = usize::from(parity.lengths[index]);
            if data[length..].iter().any(|v| *v != 0) {
                continue;
            }
            data.truncate(length);
            let packet = Packet {
                header: Header {
                    version: 2,
                    ssrc,
                    payload_type,
                    sequence_number: parity.base.wrapping_add(index as u16),
                    timestamp: parity.timestamp,
                    marker: parity.markers & (1 << index) != 0,
                    ..Default::default()
                },
                payload: data.into(),
            };
            self.cache(packet.clone(), now);
            ready.push(packet);
        }
        self.groups = keep;
        ready
    }
}
