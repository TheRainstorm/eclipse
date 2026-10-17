//! Small SDP helpers for the UU transport.
//!
//! We only need the ICE credentials and the peer's passive TCP candidates:
//! the keyboard channel is a TLS session opened on that TCP port after an
//! ICE-TCP STUN binding exchange.

use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct IceTransport {
    pub remote_ufrag: String,
    pub remote_pwd: String,
    pub local_ufrag: String,
    pub local_pwd: String,
    pub tcp_candidates: Vec<SocketAddr>,
    pub relay_candidates: Vec<SocketAddr>,
    pub udp_candidates: Vec<SocketAddr>,
}

fn ice_value(sdp: &str, key: &str) -> Option<String> {
    sdp.lines()
        .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
}

/// Parse one `a=candidate:` / candidate value into a passive TCP address.
pub fn parse_tcp_candidate(line: &str) -> Option<SocketAddr> {
    let s = line.trim();
    let s = s.strip_prefix("a=").unwrap_or(s);
    let s = s.strip_prefix("candidate:").unwrap_or(s);
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 8 {
        return None;
    }
    // foundation component transport priority ip port typ ...
    if !parts[2].eq_ignore_ascii_case("tcp") {
        return None;
    }
    if !parts
        .get(7)
        .map(|v| v.eq_ignore_ascii_case("host") || v.eq_ignore_ascii_case("srflx"))
        .unwrap_or(false)
    {
        return None;
    }
    if let Some(tc) = parts.iter().position(|p| p.eq_ignore_ascii_case("tcptype")) {
        if let Some(v) = parts.get(tc + 1) {
            if !v.eq_ignore_ascii_case("passive") && !v.eq_ignore_ascii_case("active") {
                return None;
            }
        }
    }
    let ip = parts[4].parse().ok()?;
    let port = parts[5].parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

/// Parse a relay candidate advertised by the controlled endpoint.  TURN
/// permissions must target this address rather than the endpoint's host
/// candidate.  `relay-protocol tls` is preferred by the caller when several
/// relay candidates are present.
pub fn parse_relay_candidate(line: &str) -> Option<SocketAddr> {
    let s = line.trim().strip_prefix("a=").unwrap_or(line.trim());
    let s = s.strip_prefix("candidate:").unwrap_or(s);
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 8
        || !parts[2].eq_ignore_ascii_case("udp") && !parts[2].eq_ignore_ascii_case("tcp")
    {
        return None;
    }
    let typ = parts.iter().position(|p| p.eq_ignore_ascii_case("typ"))?;
    if parts
        .get(typ + 1)
        .map(|v| !v.eq_ignore_ascii_case("relay"))
        .unwrap_or(true)
    {
        return None;
    }
    // The PCK tunnel is TURN-over-TLS.  Ignore UDP relay allocations when a
    // relay-protocol extension is present; those are WebRTC media candidates.
    if let Some(i) = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("relay-protocol"))
    {
        if !parts
            .get(i + 1)
            .map(|v| v.eq_ignore_ascii_case("tls"))
            .unwrap_or(false)
        {
            return None;
        }
    }
    let ip = parts[4].parse().ok()?;
    let port = parts[5].parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

/// Parse a public UDP candidate suitable as a TURN permission target.
pub fn parse_udp_candidate(line: &str) -> Option<SocketAddr> {
    let s = line.trim().strip_prefix("a=").unwrap_or(line.trim());
    let s = s.strip_prefix("candidate:").unwrap_or(s);
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 8 || !parts[2].eq_ignore_ascii_case("udp") {
        return None;
    }
    let typ = parts.iter().position(|p| p.eq_ignore_ascii_case("typ"))?;
    let kind = parts.get(typ + 1)?;
    if !kind.eq_ignore_ascii_case("srflx") && !kind.eq_ignore_ascii_case("relay") {
        return None;
    }
    let ip = parts[4].parse().ok()?;
    let port = parts[5].parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

pub fn parse_answer(offer: &str, answer: &str) -> Option<IceTransport> {
    let remote_ufrag = ice_value(answer, "a=ice-ufrag:")?;
    let remote_pwd = ice_value(answer, "a=ice-pwd:")?;
    let local_ufrag =
        ice_value(offer, "a=ice-ufrag:").or_else(|| ice_value(answer, "a=ice-ufrag:"))?;
    let local_pwd = ice_value(offer, "a=ice-pwd:").or_else(|| ice_value(answer, "a=ice-pwd:"))?;
    let mut tcp_candidates = Vec::new();
    let mut relay_candidates = Vec::new();
    let mut udp_candidates = Vec::new();
    for line in answer.lines() {
        if let Some(a) = parse_tcp_candidate(line) {
            if !tcp_candidates.contains(&a) {
                tcp_candidates.push(a);
            }
        }
        if let Some(a) = parse_relay_candidate(line) {
            if !relay_candidates.contains(&a) {
                relay_candidates.push(a);
            }
        }
        if let Some(a) = parse_udp_candidate(line) {
            if !udp_candidates.contains(&a) {
                udp_candidates.push(a);
            }
        }
    }
    Some(IceTransport {
        remote_ufrag,
        remote_pwd,
        local_ufrag,
        local_pwd,
        tcp_candidates,
        relay_candidates,
        udp_candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passive_tcp() {
        let a = parse_tcp_candidate(
            "a=candidate:1 1 tcp 1518280447 192.168.35.6 63347 typ host tcptype passive generation 0",
        )
        .unwrap();
        assert_eq!(a.to_string(), "192.168.35.6:63347");
        assert!(parse_tcp_candidate("candidate:2 1 udp 1 1.2.3.4 5000 typ host").is_none());
        assert_eq!(parse_relay_candidate("candidate:3 1 udp 16777215 112.28.240.180 54234 typ relay raddr 10.0.0.1 rport 4000 relay-protocol tls").unwrap().to_string(), "112.28.240.180:54234");
        assert_eq!(parse_udp_candidate("candidate:4 1 udp 100 36.159.243.209 8566 typ srflx raddr 192.168.35.6 rport 55923").unwrap().to_string(), "36.159.243.209:8566");
        assert!(parse_udp_candidate("candidate:5 1 udp 100 192.168.35.6 55923 typ host").is_none());
    }
}
