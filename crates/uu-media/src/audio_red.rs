//! One-packet Opus redundancy, independently implemented from RFC 2198.
//! https://www.rfc-editor.org/rfc/rfc2198.html#section-3
use anyhow::{ensure, Result};
use bytes::Bytes;
pub const MAX_PAYLOAD: usize = 1100;
pub struct Parts<'a> {
    pub primary: &'a [u8],
    pub previous: Option<(u16, &'a [u8])>,
}
pub fn pack(payload_type: u8, current: &[u8], previous: Option<(u16, &[u8])>) -> Result<Bytes> {
    ensure!(
        payload_type < 128 && !current.is_empty() && current.len() < MAX_PAYLOAD,
        "invalid primary audio block"
    );
    let previous = previous.filter(|(offset, data)| {
        *offset > 0
            && *offset < 16384
            && !data.is_empty()
            && data.len() < 1024
            && data.len() + current.len() + 5 <= MAX_PAYLOAD
    });
    let mut out = Vec::with_capacity(MAX_PAYLOAD);
    if let Some((offset, data)) = previous {
        out.extend([
            payload_type | 0x80,
            (offset >> 6) as u8,
            ((offset & 63) << 2) as u8 | ((data.len() >> 8) & 3) as u8,
            data.len() as u8,
            payload_type,
        ]);
        out.extend_from_slice(data);
    } else {
        out.push(payload_type);
    }
    out.extend_from_slice(current);
    Ok(out.into())
}
pub fn unpack(payload_type: u8, payload: &[u8]) -> Result<Parts<'_>> {
    ensure!(
        payload_type < 128 && !payload.is_empty() && payload.len() <= MAX_PAYLOAD,
        "invalid redundant audio payload"
    );
    ensure!(
        payload[0] & 127 == payload_type,
        "unsupported redundant audio codec"
    );
    if payload[0] & 128 == 0 {
        ensure!(payload.len() > 1, "empty primary audio block");
        return Ok(Parts {
            primary: &payload[1..],
            previous: None,
        });
    }
    ensure!(
        payload.len() >= 6 && payload[4] == payload_type,
        "invalid redundant audio headers"
    );
    let offset = (u16::from(payload[1]) << 6) | u16::from(payload[2] >> 2);
    let length = (usize::from(payload[2] & 3) << 8) | usize::from(payload[3]);
    ensure!(
        offset > 0 && length > 0 && 5 + length < payload.len(),
        "truncated redundant audio block"
    );
    Ok(Parts {
        previous: Some((offset, &payload[5..5 + length])),
        primary: &payload[5 + length..],
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carries_exact_previous_payload_and_timestamp_across_all_header_bits() {
        for offset in [1, 480, 4095, 16383] {
            for length in [1, 255, 256, 800] {
                let old = vec![17; length];
                let packet = pack(111, b"current", Some((offset, &old))).unwrap();
                let parts = unpack(111, &packet).unwrap();
                assert_eq!(parts.primary, b"current");
                assert_eq!(parts.previous, Some((offset, old.as_slice())));
            }
        }
        let big = vec![0; 1024];
        let packet = pack(111, b"current", Some((480, &big))).unwrap();
        assert!(unpack(111, &packet).unwrap().previous.is_none());
    }
    #[test]
    fn rejects_truncation_unknown_codecs_and_extra_header_chains() {
        let good = pack(111, b"current", Some((480, b"previous"))).unwrap();
        for length in 0..=5 + 8 {
            assert!(unpack(111, &good[..length]).is_err());
        }
        let mut bad = good.to_vec();
        bad[4] |= 128;
        assert!(unpack(111, &bad).is_err());
        assert!(unpack(110, &good).is_err());
        assert!(pack(128, b"x", None).is_err());
    }
}
