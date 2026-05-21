//! Safe DNS — IP blocklist for SSRF protection.
//!
//! T3-D lands the IP classifier and the BlockedIpError type. T3-H builds on
//! top of this by adding a `SafeDnsResolver` that consults DoH then runs each
//! result through `is_ip_blocked` before returning. The classifier alone is
//! valuable even without DoH (direct reqwest paths can pre-filter resolved IPs).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, thiserror::Error)]
#[error("ip {0} is blocked")]
pub struct BlockedIpError(pub IpAddr);

pub fn is_ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_blocked(v4),
        IpAddr::V6(v6) => is_v6_blocked(v6),
    }
}

fn is_v4_blocked(ip: Ipv4Addr) -> bool {
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
    {
        return true;
    }
    let o = ip.octets();
    if o[0] == 100 && (o[1] & 0xC0) == 0x40 {
        return true;
    }
    false
}

fn is_v6_blocked(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    let s = ip.segments()[0];
    if (s & 0xFE00) == 0xFC00 {
        return true;
    }
    if (s & 0xFFC0) == 0xFE80 {
        return true;
    }
    false
}
