//! Management 通道：/admin/* 路径独立鉴权。
//!
//! 与业务 API key 完全独立：
//! - 路由：仅 `/admin/*` 路径接受
//! - 校验：独立 entity `entity:management_token`
//! - header 形态：`Authorization: Mgmt <token>`
//!
//! 真正的管理 API 端点设计不在 v1 范围；此模块只负责早分流 + header 解析。

#[derive(Debug, PartialEq, Eq)]
pub struct MgmtAuth {
    pub token: String,
}

/// True iff the path is a management endpoint.
///
/// Strict requirement: must start with `/admin/` (note trailing slash). This
/// prevents `/administer/...` and similar false positives.
pub fn is_management_path(path: &str) -> bool {
    path.starts_with("/admin/")
}

/// Parse `Authorization: Mgmt <token>` header value into `MgmtAuth`.
///
/// Returns `None` if the value does not start with `Mgmt ` (case-sensitive),
/// or if the token portion is empty after trimming.
pub fn parse_mgmt_authorization(header: &str) -> Option<MgmtAuth> {
    let trimmed = header.trim();
    let rest = trimmed.strip_prefix("Mgmt ")?;
    let token = rest.trim();
    if token.is_empty() {
        return None;
    }
    Some(MgmtAuth {
        token: token.to_string(),
    })
}

use gw_core::entity::{KeyStatus, ManagementTokenEntry, MgmtScope};
use gw_core::versioned::Versioned;
use sha2::{Digest, Sha256};

/// Outcome of [`verify_mgmt_token_secret`]. Verification is fail-closed: any
/// non-`Ok` variant means the request must be rejected. Variants exist as
/// distinct enum values (rather than collapsing to a single `Err`) so the
/// caller can map each to its own error code / metric without re-deriving
/// the cause.
#[derive(Debug, PartialEq, Eq)]
pub enum MgmtVerifyOutcome {
    /// All checks passed; the bearer is authorised under `required_scope`.
    Ok,
    /// The raw bearer payload is not a well-formed `mt_<id>.<secret>`.
    /// Reserved for future use by the higher-level entrypoint.
    InvalidFormat,
    /// The presented secret's SHA-256 does not match the stored hash.
    Invalid,
    /// `expires_at` is in the past.
    Expired,
    /// `status` is not `KeyStatus::Active`.
    Disabled,
    /// The token's scope does not cover the route-required scope.
    ScopeDenied,
}

pub type MgmtPublicError = (u16, &'static str);

pub fn public_error_for_outcome(outcome: &MgmtVerifyOutcome) -> Option<MgmtPublicError> {
    match outcome {
        MgmtVerifyOutcome::Ok => None,
        _ => Some((401, gw_core::error::codes::GW_AUTH_FAILED)),
    }
}

/// Split a raw management bearer of the form `mt_<token_id>.<secret>` into
/// its `token_id` and `secret` halves. Returns `None` when the `mt_` prefix
/// is missing, no `.` is present, or the secret half is empty.
///
/// The `token_id` slice always includes the `mt_` prefix so the caller can
/// use it as-is when querying `EntityStore::mgmt_token`.
#[derive(Debug, PartialEq, Eq)]
pub struct MgmtTokenParts<'a> {
    pub token_id: &'a str,
    pub secret: &'a str,
}

pub fn split_mgmt_token_payload(raw: &str) -> Option<MgmtTokenParts<'_>> {
    if !raw.starts_with("mt_") {
        return None;
    }
    let dot_pos = raw.find('.')?;
    let token_id = &raw[..dot_pos];
    let secret = &raw[dot_pos + 1..];
    if secret.is_empty() {
        return None;
    }
    Some(MgmtTokenParts { token_id, secret })
}

/// Verify a presented secret against an already-loaded
/// [`ManagementTokenEntry`] and the route-required scope.
///
/// Pure: no I/O. The constant-time SHA-256 compare runs *first* so a hash
/// mismatch cannot be distinguished from a scope / status / expiry failure
/// by timing. The caller is responsible for:
/// * looking up the entry by `token_id`,
/// * checking the mgmt-token blacklist namespace (T2-D),
/// * and any deadline tightening beyond the persisted `expires_at`.
pub fn verify_mgmt_token_secret(
    entry: &Versioned<ManagementTokenEntry>,
    secret: &str,
    required_scope: MgmtScope,
) -> MgmtVerifyOutcome {
    use subtle::ConstantTimeEq;

    // 1. Constant-time SHA-256 compare — must run before any other check so a
    //    hash mismatch is timing-indistinguishable from later failures.
    let secret_hash = Sha256::digest(secret.as_bytes());
    let stored: &[u8] = entry.value.hash.as_slice();
    let eq: subtle::Choice = stored.ct_eq(secret_hash.as_slice());
    if !bool::from(eq) {
        return MgmtVerifyOutcome::Invalid;
    }

    // 2. Status — only `Active` is acceptable; everything else (Disabled /
    //    Expired / Depleted / RateLimited / Rotated / Deleted) collapses to
    //    `Disabled` from the verifier's point of view.
    if !matches!(entry.value.status, KeyStatus::Active) {
        return MgmtVerifyOutcome::Disabled;
    }

    // 3. Expiry — `expires_at == None` means "no expiry".
    if let Some(exp) = entry.value.expires_at {
        if time::OffsetDateTime::now_utc() >= exp {
            return MgmtVerifyOutcome::Expired;
        }
    }

    // 4. Scope — `covers` defines the privilege lattice
    //    (AdminFull ⊇ AdminRead ⊇ UserSelf).
    if !entry.value.scope.covers(required_scope) {
        return MgmtVerifyOutcome::ScopeDenied;
    }

    MgmtVerifyOutcome::Ok
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use gw_core::entity::{KeyStatus, ManagementTokenEntry, MgmtScope};
    use gw_core::versioned::Versioned;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use time::OffsetDateTime;

    fn build_entry(
        secret: &str,
        scope: MgmtScope,
        expires_in_secs: i64,
        status: KeyStatus,
    ) -> Versioned<ManagementTokenEntry> {
        let hash = Sha256::digest(secret.as_bytes()).to_vec();
        let entry = ManagementTokenEntry {
            token_id: "mt_demo".to_string(),
            user_id: "u_demo".into(),
            scope,
            hash,
            expires_at: Some(OffsetDateTime::now_utc() + time::Duration::seconds(expires_in_secs)),
            status,
            display_name: "test".into(),
        };
        Versioned {
            version: 1,
            value: Arc::new(entry),
        }
    }

    #[test]
    fn parse_token_splits_id_and_secret() {
        let parsed = split_mgmt_token_payload("mt_abc.s3cret").expect("split ok");
        assert_eq!(parsed.token_id, "mt_abc");
        assert_eq!(parsed.secret, "s3cret");
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        assert!(split_mgmt_token_payload("xx_abc.s").is_none());
    }

    #[test]
    fn parse_rejects_missing_dot() {
        assert!(split_mgmt_token_payload("mt_abc").is_none());
    }

    #[test]
    fn parse_rejects_empty_secret() {
        assert!(split_mgmt_token_payload("mt_abc.").is_none());
    }

    #[test]
    fn verify_constant_time_succeeds_on_match() {
        let entry = build_entry("s3cret", MgmtScope::AdminRead, 3600, KeyStatus::Active);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Ok));
    }

    #[test]
    fn verify_fails_on_mismatch() {
        let entry = build_entry("s3cret", MgmtScope::AdminRead, 3600, KeyStatus::Active);
        let outcome = verify_mgmt_token_secret(&entry, "wrong", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Invalid));
    }

    #[test]
    fn verify_scope_denied() {
        let entry = build_entry("s3cret", MgmtScope::UserSelf, 3600, KeyStatus::Active);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::AdminRead);
        assert!(matches!(outcome, MgmtVerifyOutcome::ScopeDenied));
    }

    #[test]
    fn verify_expired() {
        let entry = build_entry("s3cret", MgmtScope::AdminFull, -1, KeyStatus::Active);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Expired));
    }

    #[test]
    fn verify_disabled() {
        let entry = build_entry("s3cret", MgmtScope::AdminFull, 3600, KeyStatus::Disabled);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Disabled));
    }

    #[test]
    fn non_ok_outcomes_share_public_error() {
        let outcomes = [
            MgmtVerifyOutcome::InvalidFormat,
            MgmtVerifyOutcome::Invalid,
            MgmtVerifyOutcome::Expired,
            MgmtVerifyOutcome::Disabled,
            MgmtVerifyOutcome::ScopeDenied,
        ];
        let first = public_error_for_outcome(&outcomes[0]).expect("non-ok maps to error");
        for outcome in outcomes.iter().skip(1) {
            assert_eq!(public_error_for_outcome(outcome), Some(first));
        }
    }
}
