//! Which client a request comes from, for the per-IP limits.
//!
//! By default only the TCP peer address counts: headers are written by the client and can say
//! anything. Behind Fly.io every connection comes from Fly's proxy (a private `172.16.x.x`
//! address), so all users would share one peer address; `CLIENT_IP_SOURCE=fly` then reads the
//! `Fly-Client-IP` header that the proxy always sets. Even in that mode the header is only
//! believed when the peer is a private address (the proxy, never a direct connection from the
//! internet), holds exactly one IP, and parses. Otherwise the peer address is used.
//! `X-Forwarded-For` is never read.
//!
//! IPv6 clients are keyed by their `/64`: one host usually gets a whole `/64` and could otherwise
//! rotate through addresses to escape its limit (and fill the limiter's table). A `/48` (one site,
//! 65,536 `/64`s) is cheap to get too, so the sign-in groups also limit each IPv6 `/48` as a whole
//! ([`ClientKey::ipv6_site`]).

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

use dioxus::server::axum::http::HeaderMap;

/// The header Fly's proxy sets to the client's address.
pub const FLY_CLIENT_IP: &str = "fly-client-ip";

/// Where the client's IP address comes from (`CLIENT_IP_SOURCE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientIpSource {
    /// The TCP peer address. Safe anywhere; behind a proxy every client shares one limit.
    #[default]
    Peer,
    /// `Fly-Client-IP`, when the peer is Fly's proxy (a private address). Only for Fly.io.
    Fly,
}

impl FromStr for ClientIpSource {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.to_ascii_lowercase().as_str() {
            "peer" => Ok(Self::Peer),
            "fly" => Ok(Self::Fly),
            _ => Err("must be `peer` (the default) or `fly` (behind Fly.io's proxy)".to_owned()),
        }
    }
}

impl fmt::Display for ClientIpSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Peer => "peer",
            Self::Fly => "fly",
        })
    }
}

/// The client's IP address, or `None` when neither the peer nor a trusted header gives one
/// (only in tests that call the router without a connection).
#[must_use]
pub fn client_ip(
    source: ClientIpSource,
    peer: Option<IpAddr>,
    headers: &HeaderMap,
) -> Option<IpAddr> {
    match source {
        ClientIpSource::Peer => peer,
        ClientIpSource::Fly => match peer {
            Some(proxy) if is_private(proxy) => fly_client_ip(headers).or(peer),
            _ => peer,
        },
    }
}

/// The single, valid `Fly-Client-IP` value.
fn fly_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let mut values = headers.get_all(FLY_CLIENT_IP).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()?.trim().parse().ok()
}

/// Not reachable from the internet: loopback, RFC 1918, carrier-grade NAT (`100.64.0.0/10`),
/// link-local, IPv6 unique-local (`fc00::/7`, Fly's private network) and link-local.
#[must_use]
pub fn is_private(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (a == 100 && (b & 0xc0) == 64)
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) as the IPv4 address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

/// The per-IP rate-limit key: an IPv4 address, an IPv6 `/64` or `/48`, or "unknown" (no
/// address).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKey {
    V4(Ipv4Addr),
    /// The first 64 bits.
    V6Net(u64),
    /// The first 48 bits, for the aggregate limit.
    V6Site(u64),
    Unknown,
}

impl ClientKey {
    #[must_use]
    pub fn of(ip: Option<IpAddr>) -> Self {
        match ip.map(canonical) {
            Some(IpAddr::V4(ip)) => Self::V4(ip),
            Some(IpAddr::V6(ip)) => Self::V6Net(v6_net(ip)),
            None => Self::Unknown,
        }
    }

    /// For an IPv6 `/64`, the `/48` it belongs to; `None` for anything else.
    #[must_use]
    pub fn ipv6_site(self) -> Option<Self> {
        match self {
            Self::V6Net(net) => Some(Self::V6Site(net >> 16)),
            Self::V4(_) | Self::V6Site(_) | Self::Unknown => None,
        }
    }
}

/// The first 64 bits of an IPv6 address.
fn v6_net(ip: Ipv6Addr) -> u64 {
    let bits = u128::from(ip) >> 64;
    u64::try_from(bits).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::server::axum::http::HeaderValue;

    const FLY_PROXY: &str = "172.16.3.4";
    const PUBLIC: &str = "203.0.113.9";
    const SPOOFED: &str = "198.51.100.7";

    fn ip(raw: &str) -> IpAddr {
        raw.parse().unwrap()
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(FLY_CLIENT_IP, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn source_parses_case_insensitively_and_rejects_anything_else() {
        assert_eq!("peer".parse(), Ok(ClientIpSource::Peer));
        assert_eq!("FLY".parse(), Ok(ClientIpSource::Fly));
        assert!("x-forwarded-for".parse::<ClientIpSource>().is_err());
        assert!("".parse::<ClientIpSource>().is_err());
        assert_eq!(ClientIpSource::default(), ClientIpSource::Peer);
        assert_eq!(ClientIpSource::Fly.to_string(), "fly");
        assert_eq!(ClientIpSource::Peer.to_string(), "peer");
    }

    #[test]
    fn peer_mode_ignores_a_spoofed_fly_client_ip() {
        let spoofed = headers(&[SPOOFED]);
        for peer in [PUBLIC, FLY_PROXY, "127.0.0.1"] {
            assert_eq!(
                client_ip(ClientIpSource::Peer, Some(ip(peer)), &spoofed),
                Some(ip(peer))
            );
        }
        assert_eq!(client_ip(ClientIpSource::Peer, None, &spoofed), None);
    }

    #[test]
    fn fly_mode_reads_the_header_from_the_proxy() {
        let proxied = headers(&[PUBLIC]);
        assert_eq!(
            client_ip(ClientIpSource::Fly, Some(ip(FLY_PROXY)), &proxied),
            Some(ip(PUBLIC))
        );
        let v6 = headers(&[" 2001:db8::1 "]);
        assert_eq!(
            client_ip(ClientIpSource::Fly, Some(ip("fdaa:0:1::3")), &v6),
            Some(ip("2001:db8::1"))
        );
    }

    #[test]
    fn fly_mode_ignores_the_header_from_a_direct_internet_connection() {
        let spoofed = headers(&[SPOOFED]);
        assert_eq!(
            client_ip(ClientIpSource::Fly, Some(ip(PUBLIC)), &spoofed),
            Some(ip(PUBLIC))
        );
        assert_eq!(client_ip(ClientIpSource::Fly, None, &spoofed), None);
    }

    #[test]
    fn fly_mode_falls_back_to_the_peer_for_a_missing_or_bad_header() {
        let proxy = Some(ip(FLY_PROXY));
        for bad in [
            headers(&[]),
            headers(&["not-an-ip"]),
            headers(&[&format!("{PUBLIC}, {SPOOFED}")]),
            headers(&[PUBLIC, SPOOFED]),
            headers(&[""]),
        ] {
            assert_eq!(
                client_ip(ClientIpSource::Fly, proxy, &bad),
                proxy,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn private_addresses() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.127.255.255",
            "169.254.1.1",
            "::1",
            "fdaa:0:1::3",
            "fc00::1",
            "fe80::1",
            "::ffff:172.16.0.1",
        ] {
            assert!(is_private(ip(private)), "{private}");
        }
        for public in [
            PUBLIC,
            "8.8.8.8",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "2001:db8::1",
            "2606:4700::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private(ip(public)), "{public}");
        }
    }

    #[test]
    fn ipv6_clients_are_keyed_by_their_64() {
        let a = ClientKey::of(Some(ip("2001:db8:1:2::1")));
        let b = ClientKey::of(Some(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")));
        let other = ClientKey::of(Some(ip("2001:db8:1:3::1")));
        assert_eq!(a, b);
        assert_ne!(a, other);
        assert_eq!(a, ClientKey::V6Net(0x2001_0db8_0001_0002));
    }

    #[test]
    fn ipv6_sites_are_the_48_and_only_for_ipv6() {
        let a = ClientKey::of(Some(ip("2001:db8:1:2::1")));
        let same_site = ClientKey::of(Some(ip("2001:db8:1:ffff::1")));
        let other_site = ClientKey::of(Some(ip("2001:db8:2:2::1")));
        assert_eq!(a.ipv6_site(), Some(ClientKey::V6Site(0x2001_0db8_0001)));
        assert_eq!(a.ipv6_site(), same_site.ipv6_site());
        assert_ne!(a.ipv6_site(), other_site.ipv6_site());
        assert_eq!(ClientKey::of(Some(ip(PUBLIC))).ipv6_site(), None);
        assert_eq!(
            ClientKey::of(Some(ip("::ffff:203.0.113.9"))).ipv6_site(),
            None
        );
        assert_eq!(ClientKey::Unknown.ipv6_site(), None);
        assert_eq!(ClientKey::V6Site(1).ipv6_site(), None);
    }

    #[test]
    fn ipv4_mapped_addresses_are_keyed_as_ipv4() {
        assert_eq!(
            ClientKey::of(Some(ip("::ffff:203.0.113.9"))),
            ClientKey::of(Some(ip(PUBLIC)))
        );
        assert_ne!(
            ClientKey::of(Some(ip(PUBLIC))),
            ClientKey::of(Some(ip("203.0.113.10")))
        );
        assert_eq!(ClientKey::of(None), ClientKey::Unknown);
    }
}
