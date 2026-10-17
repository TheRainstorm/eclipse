//! PCK control frame encoding (recovered from the Windows client).
//!
//! Header (28 bytes, LE unless noted):
//!   "PCK", 0x02, "V3", 0x0200, type=0x51,
//!   seq (u32 BE), timestamp (u32 LE), channel (u16 LE), reserved (u8),
//!   kind (u32 LE), payload_len (u32 LE)
//! Payload is JSON followed by `03 00 01 00`.

pub const TYPE_DATA: u8 = 0x51;

pub fn encode_control(payload: &[u8], kind: u32, seq: u32, ts: u32) -> Vec<u8> {
    encode_control_ch(payload, kind, seq, ts, 35)
}

pub fn encode_control_ch(
    payload: &[u8],
    kind: u32,
    seq: u32,
    ts: u32,
    channel: u16,
) -> Vec<u8> {
    encode_control_full(payload, kind, seq, ts, channel, TYPE_DATA)
}

pub fn encode_control_full(
    payload: &[u8],
    kind: u32,
    seq: u32,
    ts: u32,
    channel: u16,
    ptype: u8,
) -> Vec<u8> {
    let plen = payload.len() as u32 + 4;
    let mut out = Vec::with_capacity(28 + plen as usize);
    out.extend_from_slice(b"PCK");
    out.push(0x02);
    out.extend_from_slice(b"V3");
    out.extend_from_slice(&[0x02, 0x00]);
    out.push(ptype);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ts.to_le_bytes());
    out.extend_from_slice(&channel.to_le_bytes());
    out.push(0);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&plen.to_le_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&[0x03, 0x00, 0x01, 0x00]);
    out
}
