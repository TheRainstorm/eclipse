//! Minimal protobuf wire helpers for the UU CONTROL data channel.
//!
//! The official controller and host exchange `Message { 1: seq, 2: timestamp,
//! <kind>: payload }` protobufs on the `CONTROL_DATA_CHANNEL` data channel.
//! The handshake is a SimpleAction echo request/response (kind 3) that the host
//! repeats until answered; only after that does it accept input JSON.
//!
//! Recovered from the interoperable `uu-remote-linux` project's viewer wire
//! definitions (viewer/wire.rs).

#[derive(Debug, PartialEq, Eq)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

pub fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
}

pub fn int(field: u32, value: u64, out: &mut Vec<u8>) {
    varint(u64::from(field) << 3, out);
    varint(value, out);
}

pub fn bytes(field: u32, value: &[u8], out: &mut Vec<u8>) {
    varint((u64::from(field) << 3) | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

/// `Message { 1: seq, <kind>: payload }`.
pub fn envelope(seq: u64, kind: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    int(1, seq, &mut out);
    bytes(kind, payload, &mut out);
    out
}

/// Viewer FeatureFlag pairs; clipboard (field 8) is advertised separately.
pub const VIEWER_FEATURES: [[u32; 2]; 8] =
    [[1, 6], [2, 2], [3, 1], [4, 2], [6, 2], [7, 2], [8, 3], [9, 1]];

fn feature_flag(flags: &[[u32; 2]]) -> Vec<u8> {
    let mut out = Vec::new();
    for [f, v] in flags {
        int(*f, u64::from(*v), &mut out);
    }
    out
}

fn session_features(clipboard: bool) -> Vec<u8> {
    let mut flags: Vec<[u32; 2]> = VIEWER_FEATURES.iter().copied().filter(|p| p[0] != 8).collect();
    if clipboard {
        flags.push([8, 3]);
    }
    feature_flag(&flags)
}

/// `Message { 1: seq, 3: SimpleAction { 1: 0, 4: features } }`.
pub fn echo_request(seq: u64) -> Vec<u8> {
    let mut action = Vec::new();
    int(1, 0, &mut action);
    bytes(4, &session_features(false), &mut action);
    envelope(seq, 3, &action)
}

/// `Message { 1: seq, 3: SimpleAction { 1: 1, 2: {"seq": request_seq}, 4: features } }`.
pub fn echo_response(seq: u64, request_seq: u64) -> Vec<u8> {
    let mut action = Vec::new();
    int(1, 1, &mut action);
    bytes(2, format!("{{\"seq\":{request_seq}}}").as_bytes(), &mut action);
    bytes(4, &session_features(false), &mut action);
    envelope(seq, 3, &action)
}

/// Returns the request seq when `data` is a host echo request (SimpleAction
/// with action 0, omitted field 1 included).
pub fn echo_request_seq(data: &[u8]) -> Option<u64> {
    let fields = decode_wire(data).ok()?;
    let seq = fields.iter().find_map(|(n, v)| match (n, v) {
        (1, WireValue::Varint(v)) => Some(*v),
        _ => None,
    })?;
    fields.iter().find_map(|(n, v)| match (n, v) {
        (3, WireValue::Bytes(action)) => {
            let af = decode_wire(action).ok()?;
            let a = af
                .iter()
                .find_map(|(n, v)| match (n, v) {
                    (1, WireValue::Varint(v)) => Some(*v),
                    _ => None,
                })
                .unwrap_or(0);
            (a == 0).then_some(seq)
        }
        _ => None,
    })
}

/// True when `data` is an echo response acknowledging the request `seq`.
pub fn echo_matches(data: &[u8], seq: u64) -> bool {
    let Ok(fields) = decode_wire(data) else {
        return false;
    };
    fields.iter().any(|(n, v)| match (n, v) {
        (3, WireValue::Bytes(action)) => {
            let Ok(af) = decode_wire(action) else {
                return false;
            };
            let response = af.iter().any(|(n, v)| *n == 1 && *v == WireValue::Varint(1));
            response
                && af.iter().any(|(n, v)| match (n, v) {
                    (2, WireValue::Bytes(args)) => serde_json::from_slice::<serde_json::Value>(args)
                        .ok()
                        .and_then(|j| j.get("seq").and_then(|s| s.as_u64()))
                        == Some(seq),
                    _ => false,
                })
        }
        _ => false,
    })
}

/// Decode a protobuf message into `(field number, value)` pairs. Nested byte
/// payloads are left opaque.
pub fn decode_wire(input: &[u8]) -> Result<Vec<(u32, WireValue<'_>)>, ()> {
    fn take<'a>(input: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], ()> {
        let end = offset.checked_add(len).ok_or(())?;
        let v = input.get(*offset..end).ok_or(())?;
        *offset = end;
        Ok(v)
    }
    fn read_varint(input: &[u8], offset: &mut usize) -> Result<u64, ()> {
        let mut result = 0u64;
        for shift in (0..70).step_by(7) {
            let b = *input.get(*offset).ok_or(())?;
            *offset += 1;
            if shift == 63 && b > 1 {
                return Err(());
            }
            result |= u64::from(b & 127) << shift;
            if b & 128 == 0 {
                return Ok(result);
            }
        }
        Err(())
    }
    let mut offset = 0usize;
    let mut fields = Vec::new();
    while offset < input.len() {
        let key = read_varint(input, &mut offset)?;
        let number = (key >> 3) as u32;
        if number == 0 {
            return Err(());
        }
        let value = match (key & 7) as u8 {
            0 => WireValue::Varint(read_varint(input, &mut offset)?),
            1 => WireValue::Fixed64(take(input, &mut offset, 8)?.try_into().map_err(|_| ())?),
            2 => {
                let len = usize::try_from(read_varint(input, &mut offset)?).map_err(|_| ())?;
                WireValue::Bytes(take(input, &mut offset, len)?)
            }
            5 => WireValue::Fixed32(take(input, &mut offset, 4)?.try_into().map_err(|_| ())?),
            _ => return Err(()),
        };
        fields.push((number, value));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_request_is_detected() {
        let req = echo_request(7);
        assert_eq!(echo_request_seq(&req), Some(7));
        assert!(!echo_matches(&req, 7));
    }

    #[test]
    fn echo_response_acknowledges_request_seq() {
        let resp = echo_response(99, 7);
        assert!(echo_matches(&resp, 7));
        assert!(!echo_matches(&resp, 99));
        assert_eq!(echo_request_seq(&resp), None);
    }

    #[test]
    fn host_echo_with_omitted_action_is_detected() {
        // Message{1:893,3:{4:<features>}} — action 0 elided by proto3.
        let mut action = Vec::new();
        bytes(4, &[8, 6, 16, 3], &mut action);
        let msg = envelope(893, 3, &action);
        assert_eq!(echo_request_seq(&msg), Some(893));
    }
}
