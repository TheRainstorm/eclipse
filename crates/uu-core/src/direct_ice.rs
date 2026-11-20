//! Native internet ICE: direct candidates have priority; authorized connection
//! services may supply per-session TURN credentials for automatic fallback.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use webrtc::{
    ice::url::{ProtoType, SchemeType, Url},
    ice_transport::ice_server::RTCIceServer,
    peer_connection::{
        configuration::RTCConfiguration, policy::ice_transport_policy::RTCIceTransportPolicy,
    },
};

/// Relay credentials stay in memory and require an authorized provider policy.
#[derive(Clone, Default)]
pub struct InternetIce {
    urls: Vec<String>,
    turn: Vec<RTCIceServer>,
    development_relay: bool,
    force_relay: bool,
    path: PathPolicy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathPolicy {
    #[default]
    Automatic,
    DirectOnly,
    RelayOnly,
    #[serde(skip)]
    PublicIpv4Direct,
}

impl PathPolicy {
    pub(crate) fn is_automatic(&self) -> bool {
        *self == Self::Automatic
    }
}

impl std::fmt::Debug for InternetIce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternetIce")
            .field("stun_count", &self.urls.len())
            .field("turn_count", &self.turn.len())
            .field("development_relay", &self.development_relay)
            .field("path", &self.path)
            .finish()
    }
}

impl InternetIce {
    pub(crate) fn with_path(mut self, path: PathPolicy) -> Result<Self> {
        match path {
            PathPolicy::Automatic => {}
            PathPolicy::DirectOnly | PathPolicy::PublicIpv4Direct => {
                ensure!(!self.force_relay, "{}", uu_i18n::text("此连接服务要求使用 TURN 中转。"));
                self.turn.clear();
                self.development_relay = false;
            }
            PathPolicy::RelayOnly => {
                ensure!(!self.turn.is_empty(), "{}", uu_i18n::text("此服务器未提供可用的 TURN 中转配置。"));
                self.force_relay = true;
            }
        }
        self.path = path;
        Ok(self)
    }

    pub(crate) fn path(&self) -> PathPolicy {
        self.path
    }

    pub(crate) fn automatic(stun: Vec<String>, turn: Vec<RTCIceServer>) -> Result<Self> {
        if turn.is_empty() { return Self::new(stun); }
        let mut entries: Vec<_> = stun.into_iter().map(|url| RTCIceServer { urls: vec![url], ..Default::default() }).collect();
        entries.extend(turn);
        Self::from_servers(entries, true, false)
    }
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

pub(crate) fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0 || a >= 224
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..=19).contains(&b))
        || ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_documentation())
}

pub(crate) fn public_ipv4_sdp(sdp: &str) -> Result<String> {
    ensure!(sdp.len() <= 256 * 1024, "direct SDP exceeds size limit");
    let mut filtered = String::with_capacity(sdp.len());
    let mut candidates = 0;
    for line in sdp.split_inclusive('\n') {
        if line.starts_with("a=candidate:") {
            let fields: Vec<_> = line.split_whitespace().take(8).collect();
            let allowed = fields.len() == 8 && fields[2].eq_ignore_ascii_case("udp")
                && fields[6] == "typ" && matches!(fields[7], "host" | "srflx" | "prflx")
                && fields[4].parse::<Ipv4Addr>().is_ok_and(is_public_ipv4)
                && fields[5].parse::<u16>().is_ok_and(|port| port != 0);
            if !allowed { continue; }
            candidates += 1;
        }
        filtered.push_str(line);
    }
    ensure!(candidates != 0, "no public IPv4 UDP candidate; check STUN reachability");
    Ok(filtered)
}

const PUBLIC_IPV4_ATTRIBUTE: &str = "a=x-eclipse-ice-path:public-ipv4-direct-v1";

pub(crate) fn advertise_public_ipv4(sdp: &str) -> Result<String> {
    let mut filtered = public_ipv4_sdp(sdp)?;
    let at = filtered.find("m=").unwrap_or(filtered.len());
    filtered.insert_str(at, &format!("{PUBLIC_IPV4_ATTRIBUTE}\r\n"));
    Ok(filtered)
}

pub(crate) fn accept_public_ipv4(sdp: &str) -> Result<String> {
    ensure!(sdp.lines().filter(|line| *line == PUBLIC_IPV4_ATTRIBUTE).count() == 1,
        "both peers must enable public IPv4 direct mode");
    validate_sdp(sdp, false)?;
    public_ipv4_sdp(sdp)
}

/// Older hosts can still advertise TURN. Omit their relay candidates from
/// connectivity checks, retaining the endpoints to recognize peer mappings.
pub(crate) fn without_relay(sdp: &str) -> Result<(String, Vec<SocketAddr>)> {
    validate_sdp(sdp, true)?;
    let mut direct = String::with_capacity(sdp.len());
    let mut relays = Vec::new();
    for line in sdp.split_inclusive('\n') {
        if line.starts_with("a=candidate:") {
            let fields: Vec<_> = line.split_whitespace().skip(4).take(4).collect();
            if let [address, port, "typ", "relay"] = fields.as_slice() {
                relays.push(SocketAddr::new(address.parse()?, port.parse()?));
                continue;
            }
        }
        direct.push_str(line);
    }
    validate_sdp(&direct, false).context(uu_i18n::text("对端未提供直连候选。"))?;
    Ok((direct, relays))
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
        let ip: IpAddr = fields[4]
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

    #[test]
    fn turn_path_choices_apply_to_both_gathering_and_remote_candidates() {
        let entries = serde_json::json!({"iceServers":[{
            "urls":["turn:relay.example:3478", "stun:stun.example:3478"],
            "username":"fixture-user","credential":"fixture-secret"}]});
        let automatic = room(&entries, true).unwrap();
        assert_eq!(automatic.configuration().ice_transport_policy, RTCIceTransportPolicy::All);
        assert_eq!(automatic.configuration().ice_servers.len(), 2);
        let direct = automatic.clone().with_path(PathPolicy::DirectOnly).unwrap();
        assert_eq!(direct.configuration().ice_transport_policy, RTCIceTransportPolicy::All);
        assert_eq!(direct.configuration().ice_servers.len(), 1);
        assert!(!direct.development_relay());
        let relay = automatic.with_path(PathPolicy::RelayOnly).unwrap();
        assert_eq!(relay.configuration().ice_transport_policy, RTCIceTransportPolicy::Relay);
        assert!(InternetIce::default().with_path(PathPolicy::RelayOnly).is_err());

        let ipv4 = "a=candidate:1 1 udp 2 192.0.2.1 49005 typ host\r\n";
        let ipv6 = "a=candidate:2 1 udp 2 2001:db8::1 49005 typ host\r\n";
        let turn = "a=candidate:3 1 udp 1 192.0.2.1 49153 typ relay\r\n";
        assert_eq!(
            without_relay(&[ipv4, turn, ipv6].concat()).unwrap(),
            ([ipv4, ipv6].concat(), vec!["192.0.2.1:49153".parse().unwrap()]),
        );
        assert!(without_relay(turn).is_err());
    }

    #[test]
    fn public_ipv4_trial_keeps_stun_and_rejects_required_relay() {
        let entries = serde_json::json!({"iceServers":[{
            "urls":["turn:relay.example:3478", "stun:stun.example:3478"],
            "username":"fixture-user","credential":"fixture-secret"}]});
        let normal=room(&entries,true).unwrap();
        let trial=normal.clone().with_path(PathPolicy::PublicIpv4Direct).unwrap();
        assert_eq!(normal.configuration().ice_servers.len(),2);
        assert_eq!(trial.configuration().ice_servers.len(),1);
        assert!(!trial.development_relay());
        assert_eq!(trial.urls(),["stun:stun.example:3478"]);
        let mut forced = entries;
        forced["force_relay"] = true.into();
        assert!(room(&forced,true).unwrap().with_path(PathPolicy::PublicIpv4Direct).is_err());
    }

    #[test]
    fn public_ipv4_trial_keeps_peer_mapping_and_removes_private_and_relay_paths() {
        let public="a=candidate:1 1 udp 1 8.8.4.4 1234 typ srflx raddr 10.0.0.2 rport 49005\r\n";
        let alternate="a=candidate:2 1 udp 1 1.1.1.1 1235 typ prflx\r\n";
        let private="a=candidate:3 1 udp 2 10.0.0.2 49005 typ host\r\n";
        let carrier="a=candidate:4 1 udp 2 100.72.0.2 49005 typ srflx\r\n";
        let relay="a=candidate:5 1 udp 1 1.1.1.1 45766 typ relay\r\n";
        let ipv6="a=candidate:6 1 udp 2 2001:db8::2 49005 typ host\r\n";
        let credentials="a=ice-ufrag:fixture\r\na=ice-pwd:fixture\r\n";
        let input=[credentials,public,private,carrier,ipv6,relay,alternate].concat();
        let filtered=public_ipv4_sdp(&input).unwrap();
        assert_eq!(filtered,[credentials,public,alternate].concat());
        validate_sdp(&filtered,false).unwrap();
        assert!(public_ipv4_sdp(&[private,carrier,relay,ipv6].concat()).is_err());
        assert!(accept_public_ipv4(&filtered).is_err());
        let offer=advertise_public_ipv4(&input).unwrap();
        assert_eq!(accept_public_ipv4(&offer).unwrap(),offer);
        assert!(accept_public_ipv4(&[offer.as_str(),relay].concat()).is_err());
    }
}
