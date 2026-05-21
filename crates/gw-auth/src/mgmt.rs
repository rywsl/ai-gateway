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
