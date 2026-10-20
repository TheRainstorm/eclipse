//! Read-only interface discovery for explicit native sharing address selection.
use std::{collections::HashSet, io, net::IpAddr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Address {
    pub name: String,
    pub ip: IpAddr,
}

pub(super) fn enumerate() -> io::Result<Vec<Address>> {
    let interfaces = if_addrs::get_if_addrs()?;
    Ok(normalize(
        interfaces
            .into_iter()
            .filter_map(|interface| {
                use if_addrs::IfOperStatus;
                if matches!(
                    interface.oper_status,
                    IfOperStatus::Down | IfOperStatus::NotPresent | IfOperStatus::LowerLayerDown
                ) {
                    return None;
                }
                let ip = interface.ip();
                Some(Address {
                    name: interface.name,
                    ip,
                })
            })
            .collect(),
    ))
}

fn normalize(mut addresses: Vec<Address>) -> Vec<Address> {
    addresses.retain(|address| {
        !address.ip.is_loopback() && !address.ip.is_unspecified() && !address.ip.is_multicast()
            // The current invitation/listener has no interface scope ID.
            && !matches!(address.ip, IpAddr::V6(ip) if ip.is_unicast_link_local())
            && !matches!(address.ip, IpAddr::V4(ip) if ip.is_broadcast())
    });
    addresses.sort_by(|a, b| {
        a.ip.is_ipv6()
            .cmp(&b.ip.is_ipv6())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.ip.cmp(&b.ip))
    });
    let mut seen = HashSet::new();
    addresses.retain(|address| seen.insert(address.ip));
    addresses
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choices_exclude_non_bindable_addresses_and_unscoped_ipv6() {
        let input = [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            "fe80::1234",
            "192.0.2.5",
            "192.0.2.5",
            "2001:db8::5",
        ];
        let choices = normalize(
            input
                .into_iter()
                .map(|ip| Address {
                    name: "generated".into(),
                    ip: ip.parse().unwrap(),
                })
                .collect(),
        );
        assert_eq!(
            choices.iter().map(|a| a.ip.to_string()).collect::<Vec<_>>(),
            ["192.0.2.5", "2001:db8::5"]
        );
        assert!(normalize(vec![]).is_empty());
    }
}
