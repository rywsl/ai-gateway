//! Scope-aware IP blocklist enforcement.
//!
//! Iterates the entire `ipblock` cache snapshot and matches each entry's CIDR,
//! *gated by its [`IpBlockScope`]*: `Global` entries apply to every request,
//! `User(uid)` entries only when the call carries that user id, and
//! `ApiKey(aid)` entries only when the call carries that api-key id.
//!
//! Malformed CIDR entries are logged at `warn` level and skipped so a single
//! bad config row cannot mask the rest of the list.
//!
//! A future revision may use a Patricia trie for O(log n) lookups.

use gw_core::entity::IpBlockScope;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{ApiKeyId, UserId};
use gw_store::EntityStore;
use ipnet::IpNet;
use std::borrow::Cow;
use std::net::IpAddr;
use std::str::FromStr;

pub async fn check_blocklist(
    ip: IpAddr,
    user_id: Option<&UserId>,
    api_key_id: Option<&ApiKeyId>,
    store: &EntityStore,
) -> Result<(), ErrorIR<'static>> {
    for (cidr_key, entry) in store.ipblock_snapshot() {
        match &entry.value.scope {
            IpBlockScope::Global => { /* always check */ }
            IpBlockScope::User(uid) => {
                if Some(uid) != user_id {
                    continue;
                }
            }
            IpBlockScope::ApiKey(aid) => {
                if Some(aid) != api_key_id {
                    continue;
                }
            }
        }

        match parse_cidr(&entry.value.cidr) {
            Some(net) => {
                if net.contains(&ip) {
                    return Err(blocked());
                }
            }
            None => {
                tracing::warn!(
                    cidr_key = %cidr_key,
                    cidr = %entry.value.cidr,
                    "ipblock entry has malformed CIDR; skipping"
                );
            }
        }
    }
    Ok(())
}

/// G2 — return `true` iff `ip` is contained in any CIDR in `allowlist`.
/// Malformed CIDR entries are logged at `warn` and skipped (fail closed:
/// they never auto-match). Reuses the file-local `parse_cidr` helper used
/// by `check_blocklist`.
pub fn match_any_cidr(ip: IpAddr, allowlist: &[String]) -> bool {
    for cidr in allowlist {
        match parse_cidr(cidr) {
            Some(net) => {
                if net.contains(&ip) {
                    return true;
                }
            }
            None => {
                tracing::warn!(
                    cidr = %cidr,
                    "ip_allowlist entry has malformed CIDR; skipping"
                );
            }
        }
    }
    false
}

/// Parse `cidr` as an IPv4/IPv6 [`IpNet`]. Returns `None` on malformed input
/// so the caller can log and skip that entry.
fn parse_cidr(cidr: &str) -> Option<IpNet> {
    IpNet::from_str(cidr).ok()
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
