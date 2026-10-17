//! Minimal STUN/ICE-TCP client used to open the peer's PCK port.
//!
//! The UU streamer listens on a passive TCP host candidate.  Before TLS (and
//! before any PCK frame) the controller must perform an ICE connectivity
//! check on that same connection:
//!
//!   u16 BE length || STUN Binding Request
//!
//! The peer answers with `u16 BE length || STUN Binding Success Response`.
//! Only after that exchange does the port accept a TLS ClientHello (ICE-TCP
//! multiplexing, RFC 6544).  The request needs the peer's ICE ufrag/pwd from
//! the SDP answer; MESSAGE-INTEGRITY is HMAC-SHA1 keyed with the *peer*
//! password.

use anyhow::{bail, Result};
use hmac::{Hmac, Mac};
use sha1::Sha1;

pub const MAGIC_COOKIE: u32 = 0x2112_A442;
pub const BINDING_REQUEST: u16 = 0x0001;
pub const BINDING_SUCCESS: u16 = 0x0101;
pub const BINDING_ERROR: u16 = 0x0111;

#[derive(Debug, Clone)]
pub struct IceCredentials {
    /// ufrag advertised by the controlled peer (SDP answer).
    pub remote_ufrag: String,
    /// password advertised by the controlled peer (SDP answer).
    pub remote_pwd: String,
    /// our own ufrag (SDP offer).
    pub local_ufrag: String,
    /// our own password (SDP offer); used to authenticate the peer's check.
    pub local_pwd: String,
}

#[derive(Debug, Default, Clone)]
pub struct BindingResponse {
    pub success: bool,
    pub error_code: Option<u16>,
    pub error_reason: Option<String>,
    pub mapped: Option<std::net::SocketAddr>,
}

fn attr(kind: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + value.len() + 3);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

fn set_len(buf: &mut [u8], attrs_len: usize) {
    buf[2..4].copy_from_slice(&(attrs_len as u16).to_be_bytes());
}

/// Build `u16 BE length || STUN Binding Request`.
pub fn binding_request(
    creds: &IceCredentials,
    priority: u32,
    controlling: bool,
    tiebreaker: u64,
    txid: [u8; 12],
) -> Vec<u8> {
    let mut attrs = Vec::new();
    let username = format!("{}:{}", creds.remote_ufrag, creds.local_ufrag);
    attrs.extend(attr(0x0006, username.as_bytes()));
    // NetEase/ICE custom attributes observed on the wire.
    attrs.extend(attr(0xC057, &999u32.to_be_bytes()));
    let ice_attr = if controlling { 0x8029u16 } else { 0x802A };
    attrs.extend(attr(ice_attr, &tiebreaker.to_be_bytes()));
    attrs.extend(attr(0x0024, &priority.to_be_bytes()));
    attrs.extend(attr(0xC070, &0u32.to_be_bytes()));

    let mut msg = Vec::with_capacity(20 + attrs.len() + 32);
    msg.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&txid);
    msg.extend_from_slice(&attrs);

    // MESSAGE-INTEGRITY covers the header (with a length that includes the
    // 24-byte MI attribute) plus all attributes before MI.
    set_len(&mut msg, attrs.len() + 24);
    let mut mac = Hmac::<Sha1>::new_from_slice(creds.remote_pwd.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(&msg);
    let digest = mac.finalize().into_bytes();
    msg.extend(attr(0x0008, &digest));

    // FINGERPRINT covers everything before it, with a length that includes
    // the 8-byte FINGERPRINT attribute.
    set_len(&mut msg, attrs.len() + 24 + 8);
    let crc = crc32fast::hash(&msg) ^ 0x5354_554E;
    msg.extend(attr(0x8028, &crc.to_be_bytes()));

    let mut framed = Vec::with_capacity(2 + msg.len());
    framed.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    framed.extend_from_slice(&msg);
    framed
}


/// Build a Binding Success Response for a peer's connectivity check.
///
/// The peer keys its MESSAGE-INTEGRITY with *our* password (the remote
/// password from its point of view), so `pwd` must be our local ICE password.
pub fn binding_response(request: &[u8], pwd: &str, mapped: std::net::SocketAddr) -> Vec<u8> {
    if request.len() < 20 {
        return Vec::new();
    }
    let txid: [u8; 12] = request[8..20].try_into().unwrap_or([0u8; 12]);
    let mut attrs = Vec::new();
    match mapped {
        std::net::SocketAddr::V4(v4) => {
            let port = v4.port() ^ (MAGIC_COOKIE >> 16) as u16;
            let ip = v4.ip().octets();
            let cookie = MAGIC_COOKIE.to_be_bytes();
            let mut value = Vec::with_capacity(8);
            value.push(0);
            value.push(1);
            value.extend_from_slice(&port.to_be_bytes());
            for i in 0..4 {
                value.push(ip[i] ^ cookie[i]);
            }
            attrs.extend(attr(0x0020, &value));
        }
        std::net::SocketAddr::V6(_) => {}
    }

    let mut msg = Vec::with_capacity(20 + attrs.len() + 32);
    msg.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&txid);
    msg.extend_from_slice(&attrs);

    set_len(&mut msg, attrs.len() + 24);
    let mut mac = Hmac::<Sha1>::new_from_slice(pwd.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(&msg);
    let digest = mac.finalize().into_bytes();
    msg.extend(attr(0x0008, &digest));

    set_len(&mut msg, attrs.len() + 24 + 8);
    let crc = crc32fast::hash(&msg) ^ 0x5354_554E;
    msg.extend(attr(0x8028, &crc.to_be_bytes()));

    let mut framed = Vec::with_capacity(2 + msg.len());
    framed.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    framed.extend_from_slice(&msg);
    framed
}

fn parse_error(v: &[u8]) -> (u16, String) {
    if v.len() < 4 {
        return (0, String::new());
    }
    let class = v[2] & 0x07;
    let number = v[3];
    let code = (class as u16) * 100 + number as u16;
    let reason = String::from_utf8_lossy(&v[4..]).to_string();
    (code, reason)
}

pub fn parse_response(msg: &[u8]) -> Result<BindingResponse> {
    if msg.len() < 20 {
        bail!("short STUN response ({} bytes)", msg.len());
    }
    let ty = u16::from_be_bytes([msg[0], msg[1]]);
    if u32::from_be_bytes([msg[4], msg[5], msg[6], msg[7]]) != MAGIC_COOKIE {
        bail!("bad STUN magic cookie");
    }
    let attrs_len = u16::from_be_bytes([msg[2], msg[3]]) as usize;
    let end = (20 + attrs_len).min(msg.len());
    let mut out = BindingResponse::default();
    out.success = ty == BINDING_SUCCESS;
    let mut p = 20;
    while p + 4 <= end {
        let kind = u16::from_be_bytes([msg[p], msg[p + 1]]);
        let len = u16::from_be_bytes([msg[p + 2], msg[p + 3]]) as usize;
        p += 4;
        if p + len > end {
            break;
        }
        let v = &msg[p..p + len];
        match kind {
            0x0020 => {
                if len >= 8 {
                    let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC_COOKIE >> 16) as u16;
                    let mut ip = [0u8; 4];
                    for i in 0..4 {
                        ip[i] = v[4 + i] ^ ((MAGIC_COOKIE >> (24 - i * 8)) & 0xff) as u8;
                    }
                    out.mapped = Some(std::net::SocketAddr::from((ip, port)));
                }
            }
            0x0009 => {
                let (code, reason) = parse_error(v);
                out.error_code = Some(code);
                out.error_reason = Some(reason);
            }
            _ => {}
        }
        p += len;
        while p % 4 != 0 && p < end {
            p += 1;
        }
    }
    if ty == BINDING_ERROR && out.error_code.is_none() {
        out.error_code = Some(0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_well_formed() {
        let creds = IceCredentials {
            remote_ufrag: "abcd".into(),
            remote_pwd: "secret".into(),
            local_ufrag: "wxyz".into(),
            local_pwd: "secret2".into(),
        };
        let b = binding_request(&creds, 0x6e00_20ff, true, 0x1122_3344_5566_7788, [7; 12]);
        assert_eq!(u16::from_be_bytes([b[0], b[1]]) as usize, b.len() - 2);
        assert_eq!(u16::from_be_bytes([b[2], b[3]]), BINDING_REQUEST);
        assert_eq!(u32::from_be_bytes([b[6], b[7], b[8], b[9]]), MAGIC_COOKIE);
        let text = String::from_utf8_lossy(&b);
        assert!(text.contains("abcd:wxyz"));
    }
}
