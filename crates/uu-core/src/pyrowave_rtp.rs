//! Eclipse PyroWave RTP v1: big-endian byte offset + complete AU length.
//! The existing receiver owns reordering, repair deadlines and loss epochs.
use bytes::Bytes;
use webrtc::rtp::packetizer::Payloader;
#[derive(Clone, Default, Debug)]
pub(crate) struct Fragmenter;
impl Payloader for Fragmenter {
    fn payload(&mut self, mtu: usize, data: &Bytes) -> Result<Vec<Bytes>, webrtc::rtp::Error> {
        if mtu <= 8 || data.len() < 8 || data.len() > crate::video_receiver::MAX_AU {
            return Ok(vec![]);
        }
        let size = mtu - 8;
        Ok(data
            .chunks(size)
            .enumerate()
            .map(|(index, chunk)| {
                let mut fragment = Vec::with_capacity(8 + chunk.len());
                fragment.extend_from_slice(&((index * size) as u32).to_be_bytes());
                fragment.extend_from_slice(&(data.len() as u32).to_be_bytes());
                fragment.extend_from_slice(chunk);
                Bytes::from(fragment)
            })
            .collect())
    }
    fn clone_to(&self) -> Box<dyn Payloader + Send + Sync> {
        Box::new(self.clone())
    }
}
#[derive(Default)]
pub(crate) struct Assembler {
    data: Vec<u8>,
    length: Option<usize>,
}
impl Assembler {
    pub fn push(&mut self, payload: &[u8]) -> Result<(), ()> {
        if payload.len() <= 8 {
            return Err(());
        }
        let offset = u32::from_be_bytes(payload[..4].try_into().unwrap()) as usize;
        let length = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize;
        if !(8..=crate::video_receiver::MAX_AU).contains(&length)
            || offset != self.data.len()
            || self.length.is_some_and(|expected| expected != length)
            || payload.len() - 8 > length.saturating_sub(offset)
        {
            return Err(());
        }
        self.length = Some(length);
        self.data.extend_from_slice(&payload[8..]);
        Ok(())
    }
    pub fn finish(self) -> Result<Vec<u8>, ()> {
        if self.length != Some(self.data.len()) {
            return Err(());
        }
        Ok(self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_reassemble_and_reject_holes_or_conflicting_lengths() {
        let data = Bytes::from((0..128_000).map(|n| n as u8).collect::<Vec<_>>());
        let fragments = Fragmenter.payload(1172, &data).unwrap();
        let mut assembler = Assembler::default();
        for part in &fragments {
            assembler.push(part).unwrap();
        }
        assert_eq!(assembler.finish().unwrap(), data);
        assert!(Assembler::default().push(&fragments[1]).is_err());
        let mut assembler = Assembler::default();
        assembler.push(&fragments[0]).unwrap();
        assert!(assembler.push(&fragments[0]).is_err());
        let mut wrong = fragments[1].to_vec();
        wrong[4..8].copy_from_slice(&1_000u32.to_be_bytes());
        assert!(assembler.push(&wrong).is_err());
        assert!(assembler.finish().is_err());
        assert!(Fragmenter.payload(8, &data).unwrap().is_empty());
    }
    #[test]
    fn receiver_drops_incomplete_frame_and_recovers_next_independent_frame() {
        use std::time::{Duration, Instant};
        use webrtc::rtp::{header::Header, packet::Packet};
        let data = Bytes::from(vec![0x81; 16_000]);
        let fragments = Fragmenter.payload(1000, &data).unwrap();
        let mut receiver = crate::video_receiver::VideoReceiver::with_codec(
            Duration::from_millis(10),
            uu_proto::media::VideoCodec::Pyrowave,
        );
        let at = Instant::now();
        let mut seq = 65530u16;
        let mut units = vec![];
        for frame in 0..3 {
            for (index, payload) in fragments.iter().enumerate() {
                let packet = Packet {
                    header: Header {
                        sequence_number: seq,
                        timestamp: 9000 + frame * 1500,
                        marker: index + 1 == fragments.len(),
                        ..Default::default()
                    },
                    payload: payload.clone(),
                };
                seq = seq.wrapping_add(1);
                if frame == 1 && index == 4 {
                    continue;
                }
                units.extend(
                    receiver.push(packet, at + Duration::from_millis(u64::from(frame) * 30)),
                );
            }
        }
        units.extend(receiver.poll(at + Duration::from_millis(100)));
        assert_eq!(units.len(), 2);
        assert_eq!((units[0].timestamp, units[1].timestamp), (9000, 12000));
        assert!(units.iter().all(|u| u.data == data));
        assert_eq!(receiver.stats.lost, 1);
    }
}
