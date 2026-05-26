use gw_core::entity::{IpBlockMode, IpBlockScope};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{ApiKeyId, UserId};
use gw_store::EntityStore;
use ipnet::IpNet;
use std::borrow::Cow;
use std::net::IpAddr;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpAllowlistMode {
    Off,
    Enforce,
}

#[derive(Clone, Copy, Debug)]
pub struct IpAllowlistGuard {
    mode: IpAllowlistMode,
}

impl IpAllowlistGuard {
    pub fn new(mode: IpAllowlistMode) -> Self {
        Self { mode }
    }

    pub async fn check(
        &self,
        ip: IpAddr,
        user_id: Option<&UserId>,
        api_key_id: Option<&ApiKeyId>,
        store: &EntityStore,
    ) -> Result<(), ErrorIR<'static>> {
        metrics::counter!("gw_ip_allowlist_check_total").increment(1);
        let mut has_applicable_allow = false;
        let mut matched_allow = false;
        let snapshot = store.ipblock_snapshot();
        metrics::gauge!("gw_ip_allowlist_entries").set(snapshot.len() as f64);

        for (cidr_key, entry) in snapshot {
            if !scope_applies(&entry.value.scope, user_id, api_key_id) {
                continue;
            }

            let Some(net) = parse_cidr(&entry.value.cidr) else {
                tracing::warn!(
                    cidr_key = %cidr_key,
                    cidr = %entry.value.cidr,
                    "ipblock entry has malformed CIDR; skipping"
                );
                continue;
            };

            match entry.value.mode {
                IpBlockMode::Deny => {
                    if net.contains(&ip) {
                        return Err(blocked());
                    }
                }
                IpBlockMode::Allow => {
                    if self.mode == IpAllowlistMode::Enforce {
                        has_applicable_allow = true;
                        matched_allow |= net.contains(&ip);
                    }
                }
            }
        }

        if self.mode == IpAllowlistMode::Enforce && has_applicable_allow && !matched_allow {
            return Err(not_in_allowlist());
        }

        Ok(())
    }
}

fn scope_applies(
    scope: &IpBlockScope,
    user_id: Option<&UserId>,
    api_key_id: Option<&ApiKeyId>,
) -> bool {
    match scope {
        IpBlockScope::Global => true,
        IpBlockScope::User(uid) => Some(uid) == user_id,
        IpBlockScope::ApiKey(aid) => Some(aid) == api_key_id,
    }
}

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

fn not_in_allowlist() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth,
        status: 403,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_AUTH_IP_NOT_IN_ALLOWLIST),
        message: Cow::Borrowed("ip not in allowlist"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}
