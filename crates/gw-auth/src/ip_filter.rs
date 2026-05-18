//! Global IP blocklist enforcement.
//!
//! v1 simply iterates the entire `ipblock` cache snapshot and matches each CIDR;
//! a future revision may use a Patricia trie for O(log n) lookups.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_store::EntityStore;
use ipnet::IpNet;
use std::borrow::Cow;
use std::net::IpAddr;
use std::str::FromStr;

pub async fn check_blocklist(
    ip: IpAddr,
    store: &EntityStore,
) -> Result<(), ErrorIR<'static>> {
    for (_, entry) in store.ipblock_snapshot() {
        if cidr_contains(&entry.value.cidr, ip) {
            return Err(blocked());
        }
    }
    Ok(())
}

/// Parse `cidr` as an IPv4/IPv6 [`IpNet`] and test membership.
/// Returns `false` if the CIDR is malformed (fail-open for the matching step;
/// the surrounding policy decides what to do with malformed entries).
fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    match IpNet::from_str(cidr) {
        Ok(net) => net.contains(&ip),
        Err(_) => false,
    }
}

fn blocked() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth,
        status: 403,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_AUTH_IP_BLOCKED),
        message: Cow::Borrowed("ip blocked"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}
