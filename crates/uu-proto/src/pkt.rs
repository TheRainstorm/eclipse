//! Media packetization.
//!
//! Encoded frames are far larger than a datagram, so each access unit is
//! split into fixed-size chunks carrying a small header. This is the minimal
//! equivalent of what RTP does for us once the real WebRTC transport lands;
//! keeping it explicit now means the self-loop exercises the same
//! fragmentation/reassembly logic the production path needs.

use bytes::{BufMut, Bytes, BytesMut};

/// Fixed packet header size in bytes.
pub const HEADER_LEN: usize = 20;

/// Conservative payload budget: comfortably under a 1500-byte Ethernet MTU
/// once IPv4/UDP headers are accounted for.
pub const DEFAULT_MTU: usize = 1200;

const FLAG_KEYFRAME: u8 = 0b0000_0001;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketHeader {
    pub frame_id: u32,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub pts: u64,
    pub keyframe: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketError {
    #[error("packet is shorter than the {HEADER_LEN} byte header")]
    Truncated,
    #[error("mtu {mtu} leaves no room for payload")]
    MtuTooSmall { mtu: usize },
    #[error("chunk_count is zero")]
    EmptyChunkCount,
    #[error("chunk index {index} >= chunk_count {count}")]
    IndexOutOfRange { index: u16, count: u16 },
}

impl PacketHeader {
    pub fn encode(&self, dst: &mut BytesMut) {
        dst.put_u32(self.frame_id);
        dst.put_u16(self.chunk_index);
        dst.put_u16(self.chunk_count);
        dst.put_u64(self.pts);
        dst.put_u8(if self.keyframe { FLAG_KEYFRAME } else { 0 });
        dst.put_slice(&[0u8; 3]); // reserved, keeps 8-byte alignment
    }

    pub fn decode(buf: &[u8]) -> Result<Self, PacketError> {
        if buf.len() < HEADER_LEN {
            return Err(PacketError::Truncated);
        }
        let frame_id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let chunk_index = u16::from_be_bytes([buf[4], buf[5]]);
        let chunk_count = u16::from_be_bytes([buf[6], buf[7]]);
        let pts = u64::from_be_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
        ]);
        let flags = buf[16];
        if chunk_count == 0 {
            return Err(PacketError::EmptyChunkCount);
        }
        if chunk_index >= chunk_count {
            return Err(PacketError::IndexOutOfRange { index: chunk_index, count: chunk_count });
        }
        Ok(Self {
            frame_id,
            chunk_index,
            chunk_count,
            pts,
            keyframe: flags & FLAG_KEYFRAME != 0,
        })
    }
}

/// Split an access unit into wire-ready datagrams.
pub fn packetize(
    frame_id: u32,
    pts: u64,
    keyframe: bool,
    payload: &[u8],
    mtu: usize,
) -> Result<Vec<Bytes>, PacketError> {
    let budget = mtu.saturating_sub(HEADER_LEN);
    if budget == 0 {
        return Err(PacketError::MtuTooSmall { mtu });
    }
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![&[]]
    } else {
        payload.chunks(budget).collect()
    };
    let count = chunks.len() as u16;
    let mut out = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let header = PacketHeader {
            frame_id,
            chunk_index: i as u16,
            chunk_count: count,
            pts,
            keyframe,
        };
        let mut buf = BytesMut::with_capacity(HEADER_LEN + chunk.len());
        header.encode(&mut buf);
        buf.put_slice(chunk);
        out.push(buf.freeze());
    }
    Ok(out)
}

/// Reassembles chunked frames, tolerating reordering and duplicates.
#[derive(Default)]
pub struct Reassembler {
    pending: std::collections::HashMap<u32, Partial>,
    /// Highest frame id handed to the caller; older ones are considered
    /// already delivered and their late chunks are dropped.
    last_completed: Option<u32>,
}

struct Partial {
    chunks: Vec<Option<Bytes>>,
    received: usize,
    expected: u16,
    pts: u64,
    keyframe: bool,
}

/// A fully reassembled access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reassembled {
    pub frame_id: u32,
    pub pts: u64,
    pub keyframe: bool,
    pub data: Bytes,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Frames currently awaiting more chunks. A non-zero value after the
    /// producer stops means chunks were lost or never delivered.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Feed one datagram. Returns a frame when it completes one.
    pub fn push(&mut self, packet: &[u8]) -> Result<Option<Reassembled>, PacketError> {
        let header = PacketHeader::decode(packet)?;
        if let Some(last) = self.last_completed {
            if header.frame_id <= last {
                // Duplicate or stale re-transmission: ignore rather than
                // reporting an error, since UDP reordering is normal.
                return Ok(None);
            }
        }
        let payload = Bytes::copy_from_slice(&packet[HEADER_LEN..]);
        let entry = self.pending.entry(header.frame_id).or_insert_with(|| Partial {
            chunks: vec![None; header.chunk_count as usize],
            received: 0,
            expected: header.chunk_count,
            pts: header.pts,
            keyframe: header.keyframe,
        });
        if entry.expected != header.chunk_count {
            // Inconsistent chunk count for the same frame: drop the frame.
            self.pending.remove(&header.frame_id);
            return Ok(None);
        }
        let slot = &mut entry.chunks[header.chunk_index as usize];
        if slot.is_none() {
            *slot = Some(payload);
            entry.received += 1;
        }
        if entry.received < entry.chunks.len() {
            return Ok(None);
        }

        let partial = self.pending.remove(&header.frame_id).expect("just inserted");
        let mut data = BytesMut::with_capacity(partial.chunks.iter().flatten().map(|c| c.len()).sum());
        for chunk in partial.chunks.into_iter() {
            data.put_slice(&chunk.expect("all chunks present when complete"));
        }
        self.last_completed = Some(header.frame_id);
        Ok(Some(Reassembled {
            frame_id: header.frame_id,
            pts: partial.pts,
            keyframe: partial.keyframe,
            data: data.freeze(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = PacketHeader {
            frame_id: 42,
            chunk_index: 3,
            chunk_count: 9,
            pts: 1_234_567,
            keyframe: true,
        };
        let mut buf = BytesMut::new();
        h.encode(&mut buf);
        assert_eq!(buf.len(), HEADER_LEN);
        assert_eq!(PacketHeader::decode(&buf).unwrap(), h);
    }

    #[test]
    fn single_chunk_roundtrip() {
        let payload = vec![7u8; 100];
        let packets = packetize(1, 0, true, &payload, DEFAULT_MTU).unwrap();
        assert_eq!(packets.len(), 1);
        let mut r = Reassembler::new();
        let frame = r.push(&packets[0]).unwrap().expect("complete in one chunk");
        assert_eq!(frame.data.as_ref(), &payload[..]);
        assert!(frame.keyframe);
    }

    #[test]
    fn large_frame_splits_and_reassembles_out_of_order() {
        let payload: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let packets = packetize(9, 111, false, &payload, DEFAULT_MTU).unwrap();
        assert!(packets.len() >= 4, "expected several chunks, got {}", packets.len());

        let mut r = Reassembler::new();
        // Deliver in reverse: reassembly must not depend on ordering.
        let mut done = None;
        for p in packets.iter().rev() {
            if let Some(f) = r.push(p).unwrap() {
                done = Some(f);
            }
        }
        let frame = done.expect("frame completes");
        assert_eq!(frame.frame_id, 9);
        assert_eq!(frame.pts, 111);
        assert_eq!(frame.data.as_ref(), &payload[..]);
    }

    #[test]
    fn duplicate_chunks_are_ignored() {
        let payload = vec![1u8; 3000];
        let packets = packetize(5, 0, true, &payload, DEFAULT_MTU).unwrap();
        let mut r = Reassembler::new();
        let mut completions = 0;
        for p in packets.iter() {
            if r.push(p).unwrap().is_some() {
                completions += 1;
            }
            // Same chunk twice: must not complete or error.
            assert!(r.push(p).unwrap().is_none());
        }
        assert_eq!(completions, 1);
    }

    #[test]
    fn incomplete_frame_yields_nothing() {
        let payload = vec![2u8; 3000];
        let packets = packetize(1, 0, false, &payload, DEFAULT_MTU).unwrap();
        let mut r = Reassembler::new();
        for p in &packets[..packets.len() - 1] {
            assert!(r.push(p).unwrap().is_none());
        }
    }

    #[test]
    fn stale_frames_are_dropped_after_a_newer_one_completes() {
        let small = packetize(1, 0, true, &[0u8; 32], DEFAULT_MTU).unwrap();
        let later = packetize(2, 0, true, &[1u8; 32], DEFAULT_MTU).unwrap();
        let mut r = Reassembler::new();
        assert!(r.push(&small[0]).unwrap().is_some());
        assert!(r.push(&later[0]).unwrap().is_some());
        // Frame 1 arriving late must not complete again.
        assert!(r.push(&small[0]).unwrap().is_none());
    }

    #[test]
    fn empty_payload_is_one_packet() {
        let packets = packetize(3, 5, false, &[], DEFAULT_MTU).unwrap();
        assert_eq!(packets.len(), 1);
        let mut r = Reassembler::new();
        let f = r.push(&packets[0]).unwrap().unwrap();
        assert!(f.data.is_empty());
    }

    #[test]
    fn tiny_mtu_is_rejected() {
        assert_eq!(
            packetize(1, 0, false, &[0u8; 10], HEADER_LEN),
            Err(PacketError::MtuTooSmall { mtu: HEADER_LEN })
        );
    }

    #[test]
    fn truncated_packet_is_rejected() {
        assert_eq!(Reassembler::new().push(&[0u8; 5]), Err(PacketError::Truncated));
    }
}
