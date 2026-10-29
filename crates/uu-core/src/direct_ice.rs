//! Native internet ICE: direct by default, optional explicit development relay.
use anyhow::{ensure, Context, Result};
use webrtc::{
    ice::url::{ProtoType, SchemeType, Url},
    ice_transport::ice_server::RTCIceServer,
    peer_connection::{
        configuration::RTCConfiguration, policy::ice_transport_policy::RTCIceTransportPolicy,
    },
};

/// Relay credentials exist only when the caller explicitly enables development relay.
#[derive(Clone, Default)]
pub struct InternetIce {
    urls: Vec<String>,
    turn: Vec<RTCIceServer>,
    development_relay: bool,
    force_relay: bool,
}
impl std::fmt::Debug for InternetIce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternetIce")
            .field("stun_count", &self.urls.len())
            .field("turn_count", &self.turn.len())
            .field("development_relay", &self.development_relay)
            .finish()
    }
}

impl InternetIce {
    pub fn new(urls: Vec<String>) -> Result<Self> {
        ensure!(urls.len() <= 16, "too many STUN services");
        for url in &urls {
            ensure!(url.len() <= 2048, "STUN URL too long");
            let parsed = Url::parse_url(url).map_err(|_| anyhow::anyhow!("invalid STUN URL"))?;
            ensure!(
                parsed.scheme == SchemeType::Stun && parsed.proto == ProtoType::Udp,
                "direct sessions accept stun: UDP services only; TURN is disabled"
            );
        }
        Ok(Self {
            urls,
            ..Default::default()
        })
    }
    /// Enforce common ICE policy after a provider parses its own message.
    pub fn from_servers(entries: Vec<RTCIceServer>, development_relay: bool, force_relay: bool) -> Result<Self> {
        ensure!(development_relay || !force_relay,"connection requires relay but direct-only policy rejects it");
        let mut urls = Vec::new();
        let mut turn = Vec::new();
        {
            let entries=&entries;
            ensure!(entries.len() <= 32, "too many session ICE services");
            for entry in entries {
                let values:Vec<&str>=entry.urls.iter().map(String::as_str).collect();
                ensure!(values.len() <= 32, "too many session ICE URLs");
                for url in values {
                    if development_relay
                        && url.len() <= 2048
                        && Url::parse_url(url).is_ok_and(|u| {
                            u.scheme == SchemeType::Turn && u.proto == ProtoType::Udp
                        })
                    {
                        let username = entry.username.as_str();
                        let credential = entry.credential.as_str();
                        ensure!(
                            username.len() <= 4096 && credential.len() <= 4096,
                            "ICE credential too long"
                        );
                        if !username.is_empty() && !credential.is_empty() {
                            turn.push(RTCIceServer {
                                urls: vec![url.to_owned()],
                                username: username.into(),
                                credential: credential.into(),
                                credential_type:webrtc::ice_transport::ice_credential_type::RTCIceCredentialType::Password,
                                ..Default::default()
                            });
                        }
                    }
                    if url.len() <= 2048
                        && Url::parse_url(url).is_ok_and(|u| {
                            u.scheme == SchemeType::Stun && u.proto == ProtoType::Udp
                        })
                        && !urls.iter().any(|u| u == url)
                    {
                        urls.push(url.to_owned());
                    }
                }
            }
        }
        let mut result = Self::new(urls)?;
        ensure!(
            !force_relay || !turn.is_empty(),
            "relay required without supported TURN/UDP configuration"
        );
        result.turn = turn;
        result.development_relay = development_relay;
        result.force_relay = force_relay;
        Ok(result)
    }
    pub fn development_relay(&self) -> bool {
        self.development_relay
    }
    pub fn urls(&self) -> &[String] {
        &self.urls
    }
    pub fn configuration(&self) -> RTCConfiguration {
        let mut servers: Vec<_> = self
            .urls
            .iter()
            .map(|url| RTCIceServer {
                urls: vec![url.clone()],
                ..Default::default()
            })
            .collect();
        servers.extend(self.turn.clone());
        RTCConfiguration {
            ice_servers: servers,
            ice_transport_policy: if self.force_relay {
                RTCIceTransportPolicy::Relay
            } else {
                RTCIceTransportPolicy::All
            },
            ..Default::default()
        }
    }
}

pub(crate) fn validate_sdp(sdp: &str, development_relay: bool) -> Result<()> {
    ensure!(sdp.len() <= 256 * 1024, "direct SDP exceeds size limit");
    let mut candidates = 0;
    for line in sdp.lines().filter(|l| l.starts_with("a=candidate:")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(
            fields.len() >= 8
                && fields[2].eq_ignore_ascii_case("udp")
                && fields[6] == "typ"
                && (matches!(fields[7], "host" | "srflx" | "prflx")
                    || (development_relay && fields[7] == "relay")),
            "direct session rejects relay and non-UDP candidates"
        );
        let ip: std::net::IpAddr = fields[4]
            .parse()
            .context("direct candidate needs a numeric IP")?;
        ensure!(
            !ip.is_unspecified() && !ip.is_multicast(),
            "invalid direct candidate IP"
        );
        ensure!(
            fields[5].parse::<u16>().is_ok_and(|port| port > 0),
            "invalid direct candidate port"
        );
        candidates += 1;
    }
    ensure!(
        candidates > 0,
        "direct session has no completed media candidates"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn room(value:&serde_json::Value,relay:bool)->Result<InternetIce>{
        let entries=value["iceServers"].as_array().map(|v|v.iter().map(|entry|RTCIceServer {
            urls:match &entry["urls"] {serde_json::Value::String(s)=>vec![s.clone()],serde_json::Value::Array(v)=>v.iter().filter_map(|v|v.as_str().map(str::to_owned)).collect(),_=>vec![]},
            username:entry["username"].as_str().unwrap_or_default().into(),credential:entry["credential"].as_str().unwrap_or_default().into(),..Default::default()
        }).collect()).unwrap_or_default();
        InternetIce::from_servers(entries,relay,value["force_relay"].as_bool().unwrap_or(false))
    }
    #[test]
    fn session_turn_is_never_allocated_and_force_relay_never_falls_back() {
        let config = room(&serde_json::json!({"iceServers":[
            {"urls":["turn:relay.example:3478", "turns:relay.example:443", "stun:stun.example:3478"], "credential":"secret"},
            {"urls":"stun:other.example:2580"}]}),false).unwrap().configuration();
        assert_eq!(config.ice_servers.len(), 2);
        assert!(config.ice_servers.iter().all(|s| s.username.is_empty()
            && s.credential.is_empty()
            && s.urls.iter().all(|u| u.starts_with("stun:"))));
        assert!(room(&serde_json::json!({"force_relay":true}),false).is_err());
        assert!(InternetIce::new(vec!["turn:relay.example:3478".into()]).is_err());
        assert!(InternetIce::default()
            .configuration()
            .ice_servers
            .is_empty());
    }
    #[test]
    fn remote_relay_is_rejected_even_when_we_did_not_allocate_turn() {
        for kind in ["host", "srflx", "prflx", "relay"] {
            let sdp = format!("a=candidate:1 1 udp 1 192.0.2.1 1234 typ {kind}\r\n");
            assert_eq!(validate_sdp(&sdp, false).is_ok(), kind != "relay");
        }
        assert!(validate_sdp("a=candidate:1 1 tcp 1 192.0.2.1 1234 typ host\r\n", false).is_err());
    }
    #[test]
    fn development_relay_is_explicit_session_scoped_and_redacted() {
        let header = serde_json::json!({"force_relay":true,"iceServers":[{
            "urls":["turn:relay.example:3478?transport=udp","turns:relay.example:443"],
            "username":"fixture-user","credential":"fixture-secret"}]});
        let ice = room(&header,true).unwrap();
        assert_eq!(
            ice.configuration().ice_transport_policy,
            RTCIceTransportPolicy::Relay
        );
        assert_eq!(ice.configuration().ice_servers.len(), 1);
        assert!(!format!("{ice:?}").contains("fixture-secret"));
        assert!(validate_sdp("a=candidate:1 1 udp 1 192.0.2.1 1234 typ relay\r\n", true).is_ok());
        assert!(room(&header,false).is_err());
    }
}
