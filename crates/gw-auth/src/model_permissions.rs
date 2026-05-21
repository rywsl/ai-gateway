//! Regex-based ApiKey model permissions with ReDoS-resistant guardrails.
//!
//! v1 implements **compile-time** guardrails only (max pattern length + DFA
//! size limit). Runtime per-call budget enforcement (100ms) and per-ApiKey
//! disable-after-N-timeouts tracker land in T5-B as `RedosTracker`.

use regex::{Regex, RegexBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct ModelPermission {
    pub regex: Regex,
    pub raw_pattern: String,
    pub effect: PermissionEffect,
}

impl ModelPermission {
    pub fn new(pattern: &str, effect: PermissionEffect) -> Result<Self, RegexError> {
        let regex = compile_pattern(pattern)?;
        Ok(Self {
            regex,
            raw_pattern: pattern.to_string(),
            effect,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegexError {
    #[error("pattern too large (max {0} bytes)")]
    TooLarge(usize),
    #[error("regex compile: {0}")]
    Compile(String),
}

pub const MAX_PATTERN_LEN: usize = 1024;
pub const NFA_SIZE_LIMIT: usize = 256 * 1024;
pub const DFA_SIZE_LIMIT: usize = 8 * 1024 * 1024;

pub fn compile_pattern(pattern: &str) -> Result<Regex, RegexError> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(RegexError::TooLarge(MAX_PATTERN_LEN));
    }
    RegexBuilder::new(pattern)
        .size_limit(NFA_SIZE_LIMIT)
        .dfa_size_limit(DFA_SIZE_LIMIT)
        .build()
        .map_err(|e| RegexError::Compile(e.to_string()))
}

/// Walk the permission list in order. Each match flips `allow`. Final value
/// of `allow` decides. Default is deny (no perms match → false).
pub fn check_model_against_permissions(model: &str, perms: &[ModelPermission]) -> bool {
    let mut allow = false;
    for p in perms {
        if p.regex.is_match(model) {
            allow = matches!(p.effect, PermissionEffect::Allow);
        }
    }
    allow
}
