//! Error-passthrough allow/deny rules (audit 2026-05-30 §3.8).
//!
//! The global `[error.passthrough].enabled` switch is all-or-nothing. These
//! rules give operators per-pattern control over WHICH upstream error messages
//! are exposed to clients vs. masked — e.g. allow quota/billing messages through
//! but mask anything mentioning internal hostnames or stack frames.
//!
//! Substring matching only (case-insensitive): no regex, hence no ReDoS surface
//! and no compile step that can fail at boot. Matching is bounded to the first
//! 8 KiB of the message so a pathologically large upstream body cannot turn the
//! scan into a DoS vector.

use serde::Deserialize;

/// What to do with an upstream error message that matches a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    /// Pass the upstream message through to the client.
    #[default]
    Allow,
    /// Mask the upstream message (client sees a generic "upstream error").
    Deny,
}

/// One substring allow/deny rule for upstream error-message passthrough.
#[derive(Debug, Clone)]
pub struct PassthroughRule {
    /// Substring matched (case-insensitively) against the extracted message.
    pub needle: String,
    pub effect: RuleEffect,
}

/// Max message bytes scanned for rule matching (DoS guard for large bodies).
const MAX_SCAN_BYTES: usize = 8 * 1024;

/// Decide whether the extracted upstream error `message` may be passed through.
///
/// First rule whose needle is contained (case-insensitive) in the message wins:
/// `Allow` → `true`, `Deny` → `false`. No rule matches (or no rules) → `true`,
/// preserving the prior passthrough-when-enabled behavior.
pub fn message_allowed(message: &str, rules: &[PassthroughRule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    // Bounded, lowercase haystack (char-safe truncation, never splits a char).
    let hay: String = message
        .chars()
        .take(MAX_SCAN_BYTES)
        .flat_map(|c| c.to_lowercase())
        .collect();
    for rule in rules {
        let needle = rule.needle.to_lowercase();
        if !needle.is_empty() && hay.contains(&needle) {
            return matches!(rule.effect, RuleEffect::Allow);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(needle: &str, effect: RuleEffect) -> PassthroughRule {
        PassthroughRule {
            needle: needle.to_string(),
            effect,
        }
    }

    #[test]
    fn empty_rules_allow_all() {
        assert!(message_allowed("anything at all", &[]));
    }

    #[test]
    fn deny_rule_masks_matching_message() {
        let rules = [rule("internal", RuleEffect::Deny)];
        assert!(!message_allowed("Internal server stack at host x", &rules));
        assert!(message_allowed("quota exceeded", &rules)); // no match → allow
    }

    #[test]
    fn first_match_wins() {
        // Allow quota even though a later Deny would also match "error".
        let rules = [
            rule("quota", RuleEffect::Allow),
            rule("error", RuleEffect::Deny),
        ];
        assert!(message_allowed("quota error: limit reached", &rules));
        // A message that only hits the Deny rule is masked.
        assert!(!message_allowed("unexpected error", &rules));
    }

    #[test]
    fn case_insensitive() {
        let rules = [rule("SECRET", RuleEffect::Deny)];
        assert!(!message_allowed("contains a secret token", &rules));
    }
}
