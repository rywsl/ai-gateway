use std::collections::BTreeMap;

use smallvec::SmallVec;

const MAX_RULES: usize = 5_000;
const MAX_VALUES_PER_DIMENSION: usize = 50;
const MAX_CONTAINS_FRAGMENTS: usize = 10;
const MAX_CONTAINS_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 1_000;
const MAX_EXPANDED_BRANCHES: usize = 100_000;
const MAX_UPSTREAM_MESSAGE_BYTES: usize = 8 * 1024;
const DIMENSIONS: usize = 8;
const FORBIDDEN_MARKERS: &[&str] = &[
    "(?", "(?i)", ".*", "[", "]", "=>", "function", "while", "for(", "eval", "lua", "wasm", "cel:",
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("too many enabled error message rules: {0}")]
    TooManyRules(usize),
    #[error("too many values for dimension {dimension}: {count}")]
    TooManyDimensionValues {
        dimension: &'static str,
        count: usize,
    },
    #[error("empty value for dimension {0}")]
    EmptyDimensionValue(&'static str),
    #[error("dimension {0} must use literal text")]
    NonLiteralDimensionValue(&'static str),
    #[error("too many contains fragments: {0}")]
    TooManyContains(usize),
    #[error("empty contains fragment")]
    EmptyContains,
    #[error("contains fragment too long")]
    ContainsTooLong,
    #[error("contains fragment must be literal text")]
    NonLiteralContains,
    #[error("message is empty")]
    EmptyMessage,
    #[error("message too long")]
    MessageTooLong,
    #[error("http status out of range: {0}")]
    InvalidHttpStatus(u16),
    #[error("expanded branches too large: {0}")]
    ExpandedBranchesTooLarge(usize),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorMessageRule {
    pub id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub priority: u32,
    #[serde(rename = "match")]
    pub matcher: ErrorMessageMatcher,
    pub message: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ErrorMessageMatcher {
    #[serde(default)]
    pub provider_ids: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub endpoint_ids: Vec<String>,
    #[serde(default)]
    pub api_formats: Vec<String>,
    #[serde(default)]
    pub request_paths: Vec<String>,
    #[serde(default)]
    pub client_protocols: Vec<String>,
    #[serde(default)]
    pub http_statuses: Vec<u16>,
    #[serde(default)]
    pub gateway_codes: Vec<String>,
    #[serde(default)]
    pub upstream_message_contains: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ErrorMessageMatchCtx<'a> {
    pub provider_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub endpoint_id: Option<&'a str>,
    pub api_format: Option<&'a str>,
    pub request_path: Option<&'a str>,
    pub client_protocol: Option<&'a str>,
    pub http_status: Option<u16>,
    pub gateway_code: Option<&'a str>,
}

#[derive(Debug, Default)]
struct RuleNode {
    exact: BTreeMap<String, RuleNode>,
    exact_status: BTreeMap<u16, RuleNode>,
    wildcard: Option<Box<RuleNode>>,
    leaves: Vec<usize>,
}

#[derive(Debug)]
struct CompiledRule {
    id: String,
    priority: u32,
    message: String,
    contains: Vec<String>,
}

#[derive(Debug, Default)]
pub struct CompiledErrorMessageRules {
    root: RuleNode,
    rules: Vec<CompiledRule>,
    node_count: usize,
}

impl CompiledErrorMessageRules {
    pub fn compile(rules: Vec<ErrorMessageRule>) -> Result<Self, CompileError> {
        let mut enabled: Vec<_> = rules.into_iter().filter(|rule| rule.enabled).collect();
        if enabled.len() > MAX_RULES {
            return Err(CompileError::TooManyRules(enabled.len()));
        }
        enabled.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));

        let mut compiled = Self {
            root: RuleNode::default(),
            rules: Vec::with_capacity(enabled.len()),
            node_count: 1,
        };

        for rule in enabled {
            validate_rule(&rule)?;
            let contains = rule
                .matcher
                .upstream_message_contains
                .iter()
                .map(|fragment| fragment.trim().to_ascii_lowercase())
                .collect();
            let idx = compiled.rules.len();
            compiled.rules.push(CompiledRule {
                id: rule.id,
                priority: rule.priority,
                message: rule.message.trim().to_string(),
                contains,
            });
            let paths = dimension_paths(&compiled.rules[idx].id, &rule.matcher);
            insert_rule(&mut compiled.root, &mut compiled.node_count, &paths, idx);
        }

        Ok(compiled)
    }

    pub fn match_message<'a>(
        &'a self,
        ctx: &ErrorMessageMatchCtx<'_>,
        upstream_message: &str,
    ) -> Option<&'a str> {
        self.match_rule(ctx, upstream_message)
            .map(|matched| matched.message)
    }

    pub fn match_rule<'a>(
        &'a self,
        ctx: &ErrorMessageMatchCtx<'_>,
        upstream_message: &str,
    ) -> Option<MatchedErrorMessageRule<'a>> {
        if self.rules.is_empty() {
            return None;
        }

        let mut current: SmallVec<[&RuleNode; 256]> = SmallVec::new();
        current.push(&self.root);
        for depth in 0..DIMENSIONS {
            let mut next: SmallVec<[&RuleNode; 256]> = SmallVec::new();
            for node in current {
                match branch_value(ctx, depth) {
                    BranchValue::Text(Some(value)) => {
                        if let Some(exact) = node.exact.get(value) {
                            next.push(exact);
                        }
                    }
                    BranchValue::Status(Some(value)) => {
                        if let Some(exact) = node.exact_status.get(&value) {
                            next.push(exact);
                        }
                    }
                    BranchValue::Text(None) | BranchValue::Status(None) => {}
                }
                if let Some(wildcard) = node.wildcard.as_deref() {
                    next.push(wildcard);
                }
            }
            if next.is_empty() {
                return None;
            }
            current = next;
        }

        let hay = upstream_message
            .get(..MAX_UPSTREAM_MESSAGE_BYTES)
            .unwrap_or(upstream_message);
        let mut best: Option<&CompiledRule> = None;
        for node in current {
            for &idx in &node.leaves {
                let rule = &self.rules[idx];
                if contains_all_ascii_case_insensitive(hay, &rule.contains)
                    && best.is_none_or(|candidate| {
                        rule.priority < candidate.priority
                            || (rule.priority == candidate.priority && rule.id < candidate.id)
                    })
                {
                    best = Some(rule);
                }
            }
        }
        best.map(|rule| MatchedErrorMessageRule {
            rule_id: rule.id.as_str(),
            message: rule.message.as_str(),
        })
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn node_count(&self) -> usize {
        self.node_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedErrorMessageRule<'a> {
    pub rule_id: &'a str,
    pub message: &'a str,
}

fn default_enabled() -> bool {
    true
}

fn validate_rule(rule: &ErrorMessageRule) -> Result<(), CompileError> {
    let message = rule.message.trim();
    if message.is_empty() {
        return Err(CompileError::EmptyMessage);
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(CompileError::MessageTooLong);
    }
    validate_string_dimension("provider_ids", &rule.matcher.provider_ids)?;
    validate_string_dimension("models", &rule.matcher.models)?;
    validate_string_dimension("endpoint_ids", &rule.matcher.endpoint_ids)?;
    validate_string_dimension("api_formats", &rule.matcher.api_formats)?;
    validate_string_dimension("request_paths", &rule.matcher.request_paths)?;
    validate_string_dimension("client_protocols", &rule.matcher.client_protocols)?;
    validate_string_dimension("gateway_codes", &rule.matcher.gateway_codes)?;

    if rule.matcher.http_statuses.len() > MAX_VALUES_PER_DIMENSION {
        return Err(CompileError::TooManyDimensionValues {
            dimension: "http_statuses",
            count: rule.matcher.http_statuses.len(),
        });
    }
    for status in &rule.matcher.http_statuses {
        if !(100..=599).contains(status) {
            return Err(CompileError::InvalidHttpStatus(*status));
        }
    }

    if rule.matcher.upstream_message_contains.len() > MAX_CONTAINS_FRAGMENTS {
        return Err(CompileError::TooManyContains(
            rule.matcher.upstream_message_contains.len(),
        ));
    }
    for fragment in &rule.matcher.upstream_message_contains {
        let trimmed = fragment.trim();
        if trimmed.is_empty() {
            return Err(CompileError::EmptyContains);
        }
        if trimmed.len() > MAX_CONTAINS_BYTES {
            return Err(CompileError::ContainsTooLong);
        }
        if contains_forbidden_marker(trimmed) {
            return Err(CompileError::NonLiteralContains);
        }
    }

    let expanded = expanded_branches(&rule.matcher);
    if expanded > MAX_EXPANDED_BRANCHES {
        return Err(CompileError::ExpandedBranchesTooLarge(expanded));
    }
    Ok(())
}

fn validate_string_dimension(
    dimension: &'static str,
    values: &[String],
) -> Result<(), CompileError> {
    if values.len() > MAX_VALUES_PER_DIMENSION {
        return Err(CompileError::TooManyDimensionValues {
            dimension,
            count: values.len(),
        });
    }
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CompileError::EmptyDimensionValue(dimension));
        }
        if contains_forbidden_marker(trimmed) {
            return Err(CompileError::NonLiteralDimensionValue(dimension));
        }
    }
    Ok(())
}

fn expanded_branches(matcher: &ErrorMessageMatcher) -> usize {
    [
        matcher.provider_ids.len(),
        matcher.models.len(),
        matcher.endpoint_ids.len(),
        matcher.api_formats.len(),
        matcher.request_paths.len(),
        matcher.client_protocols.len(),
        matcher.gateway_codes.len(),
        matcher.http_statuses.len(),
    ]
    .into_iter()
    .map(|count| count.max(1))
    .try_fold(1usize, |acc, count| acc.checked_mul(count))
    .unwrap_or(usize::MAX)
}

fn contains_forbidden_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    FORBIDDEN_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

enum DimensionValues {
    Text(Vec<String>),
    Status(Vec<u16>),
}

enum BranchValue<'a> {
    Text(Option<&'a str>),
    Status(Option<u16>),
}

fn branch_value<'a>(ctx: &'a ErrorMessageMatchCtx<'_>, depth: usize) -> BranchValue<'a> {
    match depth {
        0 => BranchValue::Text(ctx.provider_id),
        1 => BranchValue::Text(ctx.model),
        2 => BranchValue::Text(ctx.endpoint_id),
        3 => BranchValue::Text(ctx.api_format),
        4 => BranchValue::Text(ctx.request_path),
        5 => BranchValue::Text(ctx.client_protocol),
        6 => BranchValue::Text(ctx.gateway_code),
        7 => BranchValue::Status(ctx.http_status),
        _ => BranchValue::Text(None),
    }
}

fn dimension_paths(rule_id: &str, matcher: &ErrorMessageMatcher) -> Vec<DimensionValues> {
    vec![
        DimensionValues::Text(normalized_string_dimension(rule_id, &matcher.provider_ids)),
        DimensionValues::Text(normalized_string_dimension(rule_id, &matcher.models)),
        DimensionValues::Text(normalized_string_dimension(rule_id, &matcher.endpoint_ids)),
        DimensionValues::Text(normalized_string_dimension(rule_id, &matcher.api_formats)),
        DimensionValues::Text(normalized_string_dimension(rule_id, &matcher.request_paths)),
        DimensionValues::Text(normalized_string_dimension(
            rule_id,
            &matcher.client_protocols,
        )),
        DimensionValues::Text(normalized_string_dimension(rule_id, &matcher.gateway_codes)),
        DimensionValues::Status(normalized_http_dimension(&matcher.http_statuses)),
    ]
}

fn normalized_string_dimension(rule_id: &str, values: &[String]) -> Vec<String> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<_> = values
        .iter()
        .map(|value| value.trim().to_string())
        .collect();
    out.sort();
    out.dedup();
    if out.len() != values.len() {
        tracing::debug!(
            target: "gw_core::error::message_rules",
            rule_id,
            "deduplicated error message rule dimension values"
        );
    }
    out
}

fn normalized_http_dimension(values: &[u16]) -> Vec<u16> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<_> = values.to_vec();
    out.sort();
    out.dedup();
    out
}

fn insert_rule(
    root: &mut RuleNode,
    node_count: &mut usize,
    dimensions: &[DimensionValues],
    rule_idx: usize,
) {
    insert_rule_at(root, node_count, dimensions, 0, rule_idx);
}

fn insert_rule_at(
    node: &mut RuleNode,
    node_count: &mut usize,
    dimensions: &[DimensionValues],
    depth: usize,
    rule_idx: usize,
) {
    if depth == DIMENSIONS {
        node.leaves.push(rule_idx);
        return;
    }
    match &dimensions[depth] {
        DimensionValues::Text(values) if values.is_empty() => {
            let child = wildcard_child(node, node_count);
            insert_rule_at(child, node_count, dimensions, depth + 1, rule_idx);
        }
        DimensionValues::Status(values) if values.is_empty() => {
            let child = wildcard_child(node, node_count);
            insert_rule_at(child, node_count, dimensions, depth + 1, rule_idx);
        }
        DimensionValues::Text(values) => {
            for value in values {
                let child = node.exact.entry(value.clone()).or_insert_with(|| {
                    *node_count += 1;
                    RuleNode::default()
                });
                insert_rule_at(child, node_count, dimensions, depth + 1, rule_idx);
            }
        }
        DimensionValues::Status(values) => {
            for value in values {
                let child = node.exact_status.entry(*value).or_insert_with(|| {
                    *node_count += 1;
                    RuleNode::default()
                });
                insert_rule_at(child, node_count, dimensions, depth + 1, rule_idx);
            }
        }
    }
}

fn wildcard_child<'a>(node: &'a mut RuleNode, node_count: &mut usize) -> &'a mut RuleNode {
    if node.wildcard.is_none() {
        node.wildcard = Some(Box::default());
        *node_count += 1;
    }
    node.wildcard
        .as_deref_mut()
        .expect("wildcard child just inserted")
}

fn contains_all_ascii_case_insensitive(hay: &str, fragments: &[String]) -> bool {
    fragments
        .iter()
        .all(|fragment| contains_ascii_case_insensitive(hay, fragment))
}

fn contains_ascii_case_insensitive(hay: &str, needle_lower: &str) -> bool {
    let needle = needle_lower.as_bytes();
    if needle.is_empty() {
        return true;
    }
    hay.as_bytes().windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_branch_wins_by_priority_and_id() {
        let rules = vec![
            rule("b", 200, matcher().model("gpt-image-2-max"), "wild"),
            rule(
                "a",
                100,
                matcher()
                    .provider("v_openai")
                    .model("gpt-image-2-max")
                    .status(400)
                    .contains("unsupported"),
                "exact",
            ),
        ];
        let compiled = CompiledErrorMessageRules::compile(rules).unwrap();
        let ctx = ErrorMessageMatchCtx {
            provider_id: Some("v_openai"),
            model: Some("gpt-image-2-max"),
            http_status: Some(400),
            ..Default::default()
        };
        assert_eq!(
            compiled.match_message(&ctx, "image unsupported").unwrap(),
            "exact"
        );
    }

    #[test]
    fn miss_short_circuits_without_leaf_scan() {
        let compiled = CompiledErrorMessageRules::compile(vec![rule(
            "r1",
            1,
            matcher().provider("v_other"),
            "no",
        )])
        .unwrap();
        let ctx = ErrorMessageMatchCtx {
            provider_id: Some("v_openai"),
            ..Default::default()
        };
        assert!(compiled.match_message(&ctx, "anything").is_none());
    }

    #[test]
    fn matches_ascii_case_insensitive_contains_over_prefix() {
        let compiled = CompiledErrorMessageRules::compile(vec![rule(
            "r1",
            1,
            matcher().contains("Unsupported Endpoint"),
            "rewrite",
        )])
        .unwrap();
        let ctx = ErrorMessageMatchCtx::default();
        assert_eq!(
            compiled.match_message(&ctx, "the endpoint is unsupported endpoint"),
            Some("rewrite")
        );
    }

    #[test]
    fn rejects_expanded_branch_explosion() {
        let mut m = matcher();
        m.inner.provider_ids = many_values("p", 50);
        m.inner.models = many_values("m", 50);
        m.inner.endpoint_ids = many_values("e", 41);
        let err = CompiledErrorMessageRules::compile(vec![rule("r", 1, m, "msg")]).unwrap_err();
        assert!(matches!(err, CompileError::ExpandedBranchesTooLarge(_)));
    }

    #[test]
    fn rejects_non_literal_contains() {
        let err = CompiledErrorMessageRules::compile(vec![rule(
            "r",
            1,
            matcher().contains("foo.*bar"),
            "msg",
        )])
        .unwrap_err();
        assert_eq!(err, CompileError::NonLiteralContains);
    }

    fn rule(id: &str, priority: u32, matcher: MatcherBuilder, message: &str) -> ErrorMessageRule {
        ErrorMessageRule {
            id: id.to_string(),
            enabled: true,
            priority,
            matcher: matcher.inner,
            message: message.to_string(),
        }
    }

    fn matcher() -> MatcherBuilder {
        MatcherBuilder {
            inner: ErrorMessageMatcher::default(),
        }
    }

    fn many_values(prefix: &str, count: usize) -> Vec<String> {
        (0..count).map(|idx| format!("{prefix}_{idx}")).collect()
    }

    struct MatcherBuilder {
        inner: ErrorMessageMatcher,
    }

    impl MatcherBuilder {
        fn provider(mut self, value: &str) -> Self {
            self.inner.provider_ids.push(value.to_string());
            self
        }

        fn model(mut self, value: &str) -> Self {
            self.inner.models.push(value.to_string());
            self
        }

        fn status(mut self, value: u16) -> Self {
            self.inner.http_statuses.push(value);
            self
        }

        fn contains(mut self, value: &str) -> Self {
            self.inner.upstream_message_contains.push(value.to_string());
            self
        }
    }
}
