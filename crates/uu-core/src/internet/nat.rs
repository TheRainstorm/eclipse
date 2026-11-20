//! Observe UDP mapping and filtering from one local socket (RFC 5780).
use crate::direct_ice::InternetIce;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{net::SocketAddr, time::Duration};
use tokio::{net::UdpSocket, time::timeout};
use webrtc::{
    ice::url::Url,
    stun::{
        addr::MappedAddress,
        agent::TransactionId,
        attributes::{AttrType, ATTR_CHANGE_REQUEST, ATTR_OTHER_ADDRESS, ATTR_RESPONSE_ORIGIN},
        message::{Getter, Message, BINDING_REQUEST, BINDING_SUCCESS},
        xoraddr::XorMappedAddress,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Behavior {
    Unknown,
    EndpointIndependent,
    EndpointDependent,
    AddressDependent,
    AddressAndPortDependent,
    MultipleExternalAddresses,
    Unstable,
}

#[derive(Serialize)]
pub struct Observation {
    pub destination: SocketAddr,
    pub mapped: Option<SocketAddr>,
}

#[derive(Serialize)]
pub struct Report {
    pub local: SocketAddr,
    pub observations: Vec<Observation>,
    pub rfc5780_other_address: Option<SocketAddr>,
    pub address_translation_observed: Option<bool>,
    pub mapping: Behavior,
    pub filtering: Behavior,
}

#[derive(Debug, PartialEq, Eq)]
struct Binding {
    mapped: SocketAddr,
    other: Option<SocketAddr>,
}

enum Reply {
    Bound(Binding),
    NoReply,
    Unexpected,
}

impl Reply {
    fn mapped(&self) -> Option<SocketAddr> {
        match self {
            Self::Bound(binding) => Some(binding.mapped),
            _ => None,
        }
    }
}

fn address(message: &Message, attribute: AttrType) -> Option<SocketAddr> {
    let mut address = MappedAddress::default();
    address.get_from_as(message, attribute).ok()?;
    Some(SocketAddr::new(address.ip, address.port))
}

fn parse_binding(message: &Message, sender: SocketAddr) -> Option<Binding> {
    if message.typ != BINDING_SUCCESS {
        return None;
    }
    let mut mapped = XorMappedAddress::default();
    mapped.get_from(message).ok()?;
    let mapped = SocketAddr::new(mapped.ip, mapped.port);
    if mapped.port() == 0 || mapped.ip().is_unspecified() || mapped.ip().is_multicast() {
        return None;
    }
    let other = address(message, ATTR_OTHER_ADDRESS).filter(|other| {
        // A behavior-discovery server needs both a different IP and port.
        address(message, ATTR_RESPONSE_ORIGIN) == Some(sender)
            && other.is_ipv4() == sender.is_ipv4()
            && other.ip() != sender.ip()
            && !other.ip().is_unspecified()
            && !other.ip().is_multicast()
            && other.port() != 0
            && other.port() != sender.port()
    });
    Some(Binding { mapped, other })
}

async fn binding(
    socket: &UdpSocket,
    destination: SocketAddr,
    expected_sender: SocketAddr,
    change: u32,
) -> Result<Reply> {
    let mut request = Message::new();
    request.build(&[Box::<TransactionId>::default(), Box::new(BINDING_REQUEST)])?;
    if change != 0 {
        request.add(ATTR_CHANGE_REQUEST, &change.to_be_bytes());
    }
    let mut bytes = [0; 1500];
    for wait in [500, 1000, 1500] {
        // Some OSes report ICMP refusal on the UDP socket. Treat it like an
        // unanswered destination while preserving other socket errors.
        match socket.send_to(&request.raw, destination).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                return Ok(Reply::NoReply)
            }
            Err(e) => return Err(e.into()),
        }
        let reply = timeout(Duration::from_millis(wait), async {
            loop {
                let (length, source) = match socket.recv_from(&mut bytes).await {
                    Ok(value) => value,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionRefused
                                | std::io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        return Ok::<_, std::io::Error>(Reply::NoReply);
                    }
                    Err(e) => return Err(e),
                };
                if source != expected_sender && source != destination {
                    continue;
                }
                let mut response = Message::new();
                if response.write(&bytes[..length]).is_err()
                    || response.transaction_id != request.transaction_id
                {
                    continue;
                }
                return Ok(if source == expected_sender {
                    parse_binding(&response, source).map_or(Reply::Unexpected, Reply::Bound)
                } else {
                    Reply::Unexpected
                });
            }
        })
        .await;
        if let Ok(reply) = reply {
            return Ok(reply?);
        }
    }
    Ok(Reply::NoReply)
}

async fn gather(socket: &UdpSocket, destination: SocketAddr) -> Result<Observation> {
    let mapped = binding(socket, destination, destination, 0).await?.mapped();
    Ok(Observation {
        destination,
        mapped,
    })
}

async fn behavior_discovery(
    socket: &UdpSocket,
    primary: SocketAddr,
    first: &Binding,
    other: SocketAddr,
    observations: &mut Vec<Observation>,
) -> Result<(Behavior, Behavior)> {
    use Behavior::*;
    let alternate_port = SocketAddr::new(primary.ip(), other.port());
    let alternate_ip = SocketAddr::new(other.ip(), primary.port());
    // Filtering checks must precede sending this socket to an alternate.
    let mut filtering = match binding(socket, primary, other, 0x06).await? {
        Reply::Bound(_) => EndpointIndependent,
        Reply::NoReply => match binding(socket, primary, alternate_port, 0x02).await? {
            Reply::Bound(_) => AddressDependent,
            Reply::NoReply => AddressAndPortDependent,
            Reply::Unexpected => Unknown,
        },
        Reply::Unexpected => Unknown,
    };
    let by_ip = gather(socket, alternate_ip).await?;
    let by_both = gather(socket, other).await?;
    let by_port = gather(socket, alternate_port).await?;
    let mapping = match (by_ip.mapped, by_both.mapped, by_port.mapped) {
        (_, _, Some(port)) if port != first.mapped => AddressAndPortDependent,
        (Some(ip), Some(both), _) if ip != both => AddressAndPortDependent,
        (Some(ip), Some(both), Some(port)) if ip == both && port == first.mapped => {
            if ip == first.mapped {
                EndpointIndependent
            } else {
                AddressDependent
            }
        }
        (Some(ip), _, _) if ip != first.mapped => EndpointDependent,
        _ => Unknown,
    };
    if (filtering != EndpointIndependent && by_both.mapped.is_none())
        || (filtering == AddressAndPortDependent && by_port.mapped.is_none())
    {
        filtering = Unknown;
    }
    observations.extend([by_ip, by_both, by_port]);
    Ok((mapping, filtering))
}

fn sampled_mapping(observations: &[Observation]) -> Behavior {
    use Behavior::*;
    let mut addresses = observations
        .iter()
        .filter_map(|observation| observation.mapped);
    if addresses
        .next()
        .is_some_and(|first| addresses.any(|address| address.ip() != first.ip()))
    {
        return MultipleExternalAddresses;
    }
    let mut across_ips = false;
    let mut depends_on_destination = false;
    for (i, a) in observations.iter().enumerate() {
        for b in &observations[..i] {
            if let (Some(a_mapped), Some(b_mapped)) = (a.mapped, b.mapped) {
                if a.destination == b.destination {
                    continue;
                }
                if a.destination.ip() == b.destination.ip() {
                    if a_mapped != b_mapped {
                        return AddressAndPortDependent;
                    }
                } else {
                    across_ips = true;
                    depends_on_destination |= a_mapped != b_mapped;
                }
            }
        }
    }
    if depends_on_destination {
        EndpointDependent
    } else if across_ips {
        EndpointIndependent
    } else {
        Unknown
    }
}

pub async fn probe(stun: Vec<String>, mut bind: SocketAddr) -> Result<Report> {
    use Behavior::*;
    ensure!(
        (1..=4).contains(&stun.len()),
        "provide one to four UDP STUN URLs"
    );
    let servers = InternetIce::new(stun)?;
    let mut destinations = Vec::new();
    for server in servers.urls() {
        let url = Url::parse_url(server).map_err(|_| anyhow::anyhow!("invalid STUN URL"))?;
        ensure!(url.port != 0, "invalid STUN port");
        let destination = timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((url.host.as_str(), url.port)),
        )
        .await
        .context("STUN DNS timed out")??
        .find(|addr| addr.is_ipv4() == bind.is_ipv4())
        .with_context(|| format!("no matching address family for {server}"))?;
        if !destinations.contains(&destination) {
            destinations.push(destination);
        }
    }
    let primary = destinations[0];
    if bind.ip().is_unspecified() {
        // UDP connect here only asks the OS for a source address. The actual
        // probe socket must accept responses from the alternate endpoint too.
        let route = crate::listen_socket::udp(SocketAddr::new(bind.ip(), 0))?;
        route.connect(primary).await?;
        bind.set_ip(route.local_addr()?.ip());
    }
    let socket = crate::listen_socket::udp(bind).context("binding NAT probe socket")?;
    let local = socket.local_addr()?;
    let primary_reply = binding(&socket, primary, primary, 0).await?;
    let mut observations = vec![Observation {
        destination: primary,
        mapped: primary_reply.mapped(),
    }];
    let mut mapping = Unknown;
    let mut filtering = Unknown;
    let mut rfc5780_other_address = None;
    if let Reply::Bound(first) = &primary_reply {
        rfc5780_other_address = first.other;
        if let Some(other) = first.other {
            (mapping, filtering) =
                behavior_discovery(&socket, primary, first, other, &mut observations).await?;
        }
    }
    for destination in destinations.into_iter().skip(1) {
        if !observations.iter().any(|o| o.destination == destination) {
            observations.push(gather(&socket, destination).await?);
        }
    }
    let sampled = sampled_mapping(&observations);
    if mapping == Unknown
        || sampled == AddressAndPortDependent
        || sampled == MultipleExternalAddresses
        || (mapping == EndpointIndependent && sampled == EndpointDependent)
    {
        mapping = sampled;
    }
    if mapping != Unknown || filtering != Unknown {
        if let Some(first) = observations.iter().find(|o| o.mapped.is_some()) {
            let repeats = gather(&socket, first.destination).await?;
            if repeats.mapped.is_none() {
                mapping = Unknown;
                filtering = Unknown;
            } else if repeats.mapped != first.mapped {
                mapping = Unstable;
                filtering = Unknown;
            }
            observations.push(repeats);
        }
    }
    let mut mapped = observations
        .iter()
        .filter_map(|observation| observation.mapped);
    let address_translation_observed = mapped
        .next()
        .map(|first| first != local || mapped.any(|value| value != local));
    Ok(Report {
        local,
        observations,
        rfc5780_other_address,
        address_translation_observed,
        mapping,
        filtering,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::stun::message::Setter;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn two_rfc5780_endpoints_measure_both_axes_from_one_probe_socket() {
        let primary = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let by_port = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let secondary = UdpSocket::bind(SocketAddr::new(
            "127.0.0.2".parse().unwrap(),
            by_port.local_addr().unwrap().port(),
        ))
        .await
        .unwrap();
        let a = primary.local_addr().unwrap();
        let b = secondary.local_addr().unwrap();
        let by_ip = UdpSocket::bind(SocketAddr::new(b.ip(), a.port()))
            .await
            .unwrap();
        let ports = [primary, secondary, by_ip, by_port];
        let task = tokio::spawn(async move {
            let mut packet = [0; 128];
            for (recv, send) in [(0, 0), (0, 1), (2, 2), (1, 1), (3, 3), (0, 0)] {
                let (length, remote) = ports[recv].recv_from(&mut packet).await.unwrap();
                let mut request = Message::new();
                request.write(&packet[..length]).unwrap();
                assert_eq!(request.contains(ATTR_CHANGE_REQUEST), recv != send);
                let from = ports[send].local_addr().unwrap();
                let mut response = Message::new();
                response
                    .build(&[Box::new(request.transaction_id), Box::new(BINDING_SUCCESS)])
                    .unwrap();
                XorMappedAddress {
                    ip: remote.ip(),
                    port: remote.port(),
                }
                .add_to(&mut response)
                .unwrap();
                MappedAddress {
                    ip: from.ip(),
                    port: from.port(),
                }
                .add_to_as(&mut response, ATTR_RESPONSE_ORIGIN)
                .unwrap();
                MappedAddress {
                    ip: b.ip(),
                    port: b.port(),
                }
                .add_to_as(&mut response, ATTR_OTHER_ADDRESS)
                .unwrap();
                ports[send].send_to(&response.raw, remote).await.unwrap();
            }
        });
        let report = probe(vec![format!("stun:{a}")], "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(report.rfc5780_other_address, Some(b));
        assert_eq!(report.address_translation_observed, Some(false));
        assert_eq!(report.mapping, Behavior::EndpointIndependent);
        assert_eq!(report.filtering, Behavior::EndpointIndependent);
        task.await.unwrap();
    }

    #[test]
    fn one_destination_cannot_identify_mapping_behavior() {
        let primary = Observation {
            destination: "192.0.2.2:3478".parse().unwrap(),
            mapped: Some("203.0.113.1:1234".parse().unwrap()),
        };
        assert_eq!(sampled_mapping(&[primary]), Behavior::Unknown);
    }

    #[test]
    fn single_listener_coturn_can_report_a_binding_with_placeholder_origin() {
        let sender: SocketAddr = "192.0.2.2:3478".parse().unwrap();
        let mapped: SocketAddr = "203.0.113.1:1234".parse().unwrap();
        let mut response = Message::new();
        response
            .build(&[Box::<TransactionId>::default(), Box::new(BINDING_SUCCESS)])
            .unwrap();
        XorMappedAddress {
            ip: mapped.ip(),
            port: mapped.port(),
        }
        .add_to(&mut response)
        .unwrap();
        MappedAddress {
            ip: "0.0.0.0".parse().unwrap(),
            port: 3478,
        }
        .add_to_as(&mut response, ATTR_RESPONSE_ORIGIN)
        .unwrap();
        assert_eq!(
            parse_binding(&response, sender),
            Some(Binding {
                mapped,
                other: None
            })
        );
    }
}
