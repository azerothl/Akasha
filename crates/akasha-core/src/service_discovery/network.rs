//! Local subnet detection and /24 host enumeration for network scans.

use std::net::IpAddr;

/// Returns the local IPv4 address and a /24 prefix length via UDP trick.
pub fn local_subnet() -> Option<(IpAddr, u8)> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    let ip = addr.ip();
    match ip {
        IpAddr::V4(_) => Some((ip, 24)),
        IpAddr::V6(_) => None,
    }
}

/// Hosts to scan on a /24 (1..=254 excluding our own last octet).
pub fn subnet_hosts(ip: IpAddr, _prefix_len: u8) -> Vec<IpAddr> {
    match ip {
        IpAddr::V4(a) => {
            let oct = a.octets();
            (1..=254)
                .filter(|&i| i != oct[3])
                .map(|i| IpAddr::V4(std::net::Ipv4Addr::new(oct[0], oct[1], oct[2], i)))
                .collect()
        }
        IpAddr::V6(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_hosts_excludes_self() {
        let ip = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 10));
        let hosts = subnet_hosts(ip, 24);
        assert_eq!(hosts.len(), 253);
        assert!(!hosts.contains(&ip));
    }
}
