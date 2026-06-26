use bytes::{Bytes, BytesMut};
use gw_core::entity::{
    SensitiveFilterMode, SensitiveFilterPolicy, SensitiveFilterRule, SensitiveFilterTarget,
};
use rand::RngCore;
use regex::{Captures, Regex};

const MAX_TRANSIT_MAP_ENTRIES: usize = 512;
const MAX_TRANSIT_SURROGATE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENABLED_GROUPS: usize = 100;
const MAX_ENABLED_RULES: usize = 300;
const MAX_PATTERN_LEN: usize = 2048;
const MAX_REPLACEMENT_LEN: usize = 256;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitReplacementError {
    #[error("transit replacement budget exhausted: entries={entries}, surrogate_bytes={surrogate_bytes}")]
    BudgetExhausted {
        entries: usize,
        surrogate_bytes: usize,
    },
}

#[derive(Debug)]
pub struct CompiledSensitiveFilterPolicy {
    rules: Vec<CompiledRule>,
}

#[derive(Debug)]
struct CompiledRule {
    rule: SensitiveFilterRule,
    re: Regex,
}

#[derive(Debug, Clone, Default)]
pub struct TransitMap {
    entries: Vec<(Bytes, Bytes)>,
}

impl TransitMap {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[(Bytes, Bytes)] {
        &self.entries
    }

    fn push(&mut self, surrogate: Bytes, original: Bytes) {
        self.entries.push((surrogate, original));
    }

    fn sort_by_surrogate_len_desc(&mut self) {
        self.entries
            .sort_by(|left, right| right.0.len().cmp(&left.0.len()));
    }
}

#[derive(Debug, Clone)]
pub struct ReverseStream {
    entries: Vec<(Bytes, Bytes)>,
    tail: Vec<u8>,
    trie: Vec<ReverseTrieNode>,
}

#[derive(Debug, Clone, Default)]
struct ReverseTrieNode {
    terminal: Option<usize>,
    children: Vec<(u8, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReverseMatch {
    Full { entry_idx: usize, len: usize },
    NeedMore,
    None,
}

impl ReverseTrieNode {
    fn child(&self, byte: u8) -> Option<usize> {
        self.children
            .iter()
            .find_map(|(candidate, child)| (*candidate == byte).then_some(*child))
    }
}

impl ReverseStream {
    pub fn new(map: &TransitMap) -> Self {
        let entries = map.entries.clone();
        let trie = build_reverse_trie(&entries);
        Self {
            entries,
            tail: Vec::new(),
            trie,
        }
    }

    pub fn push(&mut self, chunk: &[u8], eos: bool) -> Bytes {
        if self.entries.is_empty() {
            return Bytes::copy_from_slice(chunk);
        }

        let mut input = Vec::with_capacity(self.tail.len() + chunk.len());
        input.extend_from_slice(&self.tail);
        input.extend_from_slice(chunk);
        self.tail.clear();

        let mut out = BytesMut::with_capacity(input.len());
        let mut pos = 0usize;
        while pos < input.len() {
            match self.match_at(&input, pos, eos) {
                ReverseMatch::Full { entry_idx, len } => {
                    out.extend_from_slice(self.entries[entry_idx].1.as_ref());
                    pos += len;
                }
                ReverseMatch::NeedMore => {
                    self.tail.extend_from_slice(&input[pos..]);
                    break;
                }
                ReverseMatch::None => {
                    out.extend_from_slice(&input[pos..pos + 1]);
                    pos += 1;
                }
            }
        }
        Bytes::from(out.freeze())
    }

    fn match_at(&self, input: &[u8], pos: usize, eos: bool) -> ReverseMatch {
        let mut node_idx = 0usize;
        let mut idx = pos;
        let mut best = None;
        while idx < input.len() {
            let Some(child_idx) = self.trie[node_idx].child(input[idx]) else {
                return best
                    .map(|(entry_idx, len)| ReverseMatch::Full { entry_idx, len })
                    .unwrap_or(ReverseMatch::None);
            };
            node_idx = child_idx;
            idx += 1;
            if let Some(entry_idx) = self.trie[node_idx].terminal {
                best = Some((entry_idx, idx - pos));
            }
        }
        if !eos && !self.trie[node_idx].children.is_empty() {
            ReverseMatch::NeedMore
        } else {
            best.map(|(entry_idx, len)| ReverseMatch::Full { entry_idx, len })
                .unwrap_or(ReverseMatch::None)
        }
    }
}

fn build_reverse_trie(entries: &[(Bytes, Bytes)]) -> Vec<ReverseTrieNode> {
    let mut trie = vec![ReverseTrieNode::default()];
    for (entry_idx, (surrogate, _original)) in entries.iter().enumerate() {
        if surrogate.is_empty() {
            continue;
        }
        let mut node_idx = 0usize;
        for byte in surrogate.as_ref() {
            let child_idx = if let Some(child_idx) = trie[node_idx].child(*byte) {
                child_idx
            } else {
                trie.push(ReverseTrieNode::default());
                let child_idx = trie.len() - 1;
                trie[node_idx].children.push((*byte, child_idx));
                child_idx
            };
            node_idx = child_idx;
        }
        trie[node_idx].terminal = Some(entry_idx);
    }
    trie
}

impl CompiledSensitiveFilterPolicy {
    pub fn compile(policy: &SensitiveFilterPolicy) -> anyhow::Result<Self> {
        let mut groups = policy
            .groups
            .iter()
            .filter(|group| group.enabled)
            .cloned()
            .collect::<Vec<_>>();
        if groups.len() > MAX_ENABLED_GROUPS {
            anyhow::bail!(
                "enabled group count {} exceeds {}",
                groups.len(),
                MAX_ENABLED_GROUPS
            );
        }
        groups.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut rules = Vec::new();
        let mut enabled_rules = 0usize;
        for group in groups {
            let mut group_rules = group
                .rules
                .into_iter()
                .filter(|rule| rule.enabled)
                .collect::<Vec<_>>();
            group_rules.sort_by(|left, right| {
                left.priority
                    .cmp(&right.priority)
                    .then_with(|| left.id.cmp(&right.id))
            });
            for mut rule in group_rules {
                rule.replacement = rule.replacement.trim().to_owned();
                if rule.pattern.trim().is_empty() || rule.pattern.len() > MAX_PATTERN_LEN {
                    anyhow::bail!(
                        "rule {} pattern length must be 1..{}",
                        rule.id,
                        MAX_PATTERN_LEN
                    );
                }
                if rule.replacement.is_empty() || rule.replacement.len() > MAX_REPLACEMENT_LEN {
                    anyhow::bail!(
                        "rule {} replacement length must be 1..{}",
                        rule.id,
                        MAX_REPLACEMENT_LEN
                    );
                }
                enabled_rules += 1;
                if enabled_rules > MAX_ENABLED_RULES {
                    anyhow::bail!(
                        "enabled rule count {} exceeds {}",
                        enabled_rules,
                        MAX_ENABLED_RULES
                    );
                }
                let re = Regex::new(&rule.pattern)?;
                rules.push(CompiledRule { rule, re });
            }
        }
        Ok(Self { rules })
    }

    pub fn mask_archive_bytes(&self, target: SensitiveFilterTarget, input: &[u8]) -> Bytes {
        let Ok(mut out) = std::str::from_utf8(input).map(ToOwned::to_owned) else {
            return Bytes::copy_from_slice(input);
        };
        for rule in self.archive_rules(target) {
            out = rule
                .re
                .replace_all(&out, rule.rule.replacement.as_str())
                .into_owned();
        }
        Bytes::from(out)
    }

    pub fn replace_transit_request(
        &self,
        target: SensitiveFilterTarget,
        input: &[u8],
    ) -> Result<(Bytes, TransitMap), TransitReplacementError> {
        let Some(text) = std::str::from_utf8(input).ok() else {
            return Ok((Bytes::copy_from_slice(input), TransitMap::default()));
        };
        let mut out = text.to_string();
        let mut map = TransitMap::default();
        let mut seq = 0u64;
        let mut surrogate_bytes = 0usize;
        let nonce = request_nonce_hex();
        for rule in self.transit_rules(target) {
            let mut overflow = None;
            let replaced = rule.re.replace_all(&out, |caps: &Captures<'_>| {
                let matched = caps.get(0).expect("regex capture 0").as_str();
                let candidate = format!("{}__sf_{seq}_{nonce}__", rule.rule.replacement);
                let reversible = map.len() < MAX_TRANSIT_MAP_ENTRIES
                    && surrogate_bytes.saturating_add(candidate.len())
                        <= MAX_TRANSIT_SURROGATE_BYTES;
                if !reversible {
                    tracing::warn!(
                            target: "gw_store::sensitive_filter",
                        map_entries = map.len(),
                        surrogate_bytes,
                        "transit replacement budget exhausted; rejecting request"
                    );
                    metrics::counter!("gw_sensitive_filter_transit_overflow_total").increment(1);
                    overflow = Some(TransitReplacementError::BudgetExhausted {
                        entries: map.len(),
                        surrogate_bytes,
                    });
                    return matched.to_owned();
                }
                seq += 1;
                surrogate_bytes = surrogate_bytes.saturating_add(candidate.len());
                map.push(
                    Bytes::from(candidate.clone()),
                    Bytes::copy_from_slice(matched.as_bytes()),
                );
                candidate
            });
            if let Some(err) = overflow {
                return Err(err);
            }
            out = replaced.into_owned();
        }
        map.sort_by_surrogate_len_desc();
        Ok((Bytes::from(out), map))
    }

    pub fn reverse_transit_response(&self, map: &TransitMap, input: &[u8], eos: bool) -> Bytes {
        let mut stream = ReverseStream::new(map);
        stream.push(input, eos)
    }

    fn archive_rules(&self, target: SensitiveFilterTarget) -> impl Iterator<Item = &CompiledRule> {
        self.rules.iter().filter(move |rule| {
            rule.rule.mode == SensitiveFilterMode::ArchiveMask
                && rule.rule.targets.iter().any(|item| *item == target)
        })
    }

    fn transit_rules(&self, target: SensitiveFilterTarget) -> impl Iterator<Item = &CompiledRule> {
        self.rules.iter().filter(move |rule| {
            rule.rule.mode == SensitiveFilterMode::TransitReversible
                && rule.rule.targets.iter().any(|item| *item == target)
        })
    }
}

fn request_nonce_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::*;

    fn policy() -> SensitiveFilterPolicy {
        SensitiveFilterPolicy {
            id: "global".to_string(),
            version: 1,
            groups: vec![SensitiveFilterGroup {
                id: "g1".to_string(),
                name: "Default".to_string(),
                description: String::new(),
                enabled: true,
                priority: 1,
                rules: vec![
                    SensitiveFilterRule {
                        id: "mask".to_string(),
                        name: "Mask".to_string(),
                        enabled: true,
                        priority: 1,
                        mode: SensitiveFilterMode::ArchiveMask,
                        pattern: "ak-[A-Za-z0-9_-]+".to_string(),
                        replacement: "<AK>".to_string(),
                        targets: vec![SensitiveFilterTarget::Archive],
                    },
                    SensitiveFilterRule {
                        id: "transit".to_string(),
                        name: "Transit".to_string(),
                        enabled: true,
                        priority: 2,
                        mode: SensitiveFilterMode::TransitReversible,
                        pattern: "sk-[A-Za-z0-9_-]+".to_string(),
                        replacement: "sk-safe".to_string(),
                        targets: vec![
                            SensitiveFilterTarget::RequestBody,
                            SensitiveFilterTarget::ResponseBody,
                        ],
                    },
                ],
            }],
        }
    }

    fn policy_with_rule(
        mode: SensitiveFilterMode,
        pattern: &str,
        replacement: &str,
        targets: Vec<SensitiveFilterTarget>,
    ) -> SensitiveFilterPolicy {
        SensitiveFilterPolicy {
            id: "global".to_string(),
            version: 1,
            groups: vec![SensitiveFilterGroup {
                id: "g1".to_string(),
                name: "Default".to_string(),
                description: String::new(),
                enabled: true,
                priority: 1,
                rules: vec![SensitiveFilterRule {
                    id: "rule-one".to_string(),
                    name: "Rule".to_string(),
                    enabled: true,
                    priority: 1,
                    mode,
                    pattern: pattern.to_string(),
                    replacement: replacement.to_string(),
                    targets,
                }],
            }],
        }
    }

    #[test]
    fn archive_mask_replaces_enabled_archive_rules() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let out = compiled.mask_archive_bytes(SensitiveFilterTarget::Archive, b"ak-secret keep");
        assert_eq!(out.as_ref(), b"<AK> keep");
    }

    #[test]
    fn archive_mask_leaves_non_utf8_unchanged() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let input = b"ak-secret\xff";
        let out = compiled.mask_archive_bytes(SensitiveFilterTarget::Archive, input);
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn transit_replacement_is_unique_and_reversible() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) = compiled
            .replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one sk-two")
            .unwrap();
        assert_ne!(out.as_ref(), b"sk-one sk-two");
        assert!(std::str::from_utf8(&out).unwrap().contains("sk-safe__sf_"));
        assert_eq!(map.len(), 2);
        let restored = compiled.reverse_transit_response(&map, out.as_ref(), true);
        assert_eq!(restored.as_ref(), b"sk-one sk-two");
    }

    #[test]
    fn compile_rejects_enabled_policy_over_caps() {
        let mut policy = policy_with_rule(
            SensitiveFilterMode::ArchiveMask,
            "ak-[A-Za-z0-9]+",
            "<AK>",
            vec![SensitiveFilterTarget::Archive],
        );
        let rule = policy.groups[0].rules[0].clone();
        policy.groups[0].rules = vec![rule; MAX_ENABLED_RULES + 1];

        assert!(CompiledSensitiveFilterPolicy::compile(&policy).is_err());
    }

    #[test]
    fn compile_rejects_empty_runtime_replacement() {
        let policy = policy_with_rule(
            SensitiveFilterMode::ArchiveMask,
            "ak-[A-Za-z0-9]+",
            "   ",
            vec![SensitiveFilterTarget::Archive],
        );

        assert!(CompiledSensitiveFilterPolicy::compile(&policy).is_err());
    }

    #[test]
    fn compile_rejects_empty_runtime_pattern() {
        let policy = policy_with_rule(
            SensitiveFilterMode::ArchiveMask,
            "   ",
            "<AK>",
            vec![SensitiveFilterTarget::Archive],
        );

        assert!(CompiledSensitiveFilterPolicy::compile(&policy).is_err());
    }

    #[test]
    fn compile_preserves_regex_pattern_whitespace() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy_with_rule(
            SensitiveFilterMode::ArchiveMask,
            " ak-[A-Za-z0-9]+ ",
            "<AK>",
            vec![SensitiveFilterTarget::Archive],
        ))
        .unwrap();

        let out = compiled.mask_archive_bytes(SensitiveFilterTarget::Archive, b"ak-secret");
        assert_eq!(out.as_ref(), b"ak-secret");

        let out = compiled.mask_archive_bytes(SensitiveFilterTarget::Archive, b" ak-secret ");
        assert_eq!(out.as_ref(), b"<AK>");
    }

    #[test]
    fn transit_surrogate_is_not_plain_rule_hash_and_sequence() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy_with_rule(
            SensitiveFilterMode::TransitReversible,
            "ak-[A-Za-z0-9_-]+",
            "ak-safe",
            vec![SensitiveFilterTarget::RequestBody],
        ))
        .unwrap();

        let (out, _map) = compiled
            .replace_transit_request(SensitiveFilterTarget::RequestBody, b"ak-live")
            .unwrap();
        let rendered = std::str::from_utf8(&out).unwrap();

        assert!(rendered.starts_with("ak-safe__sf_"));
        assert!(!rendered.ends_with("_0__"));
    }

    #[test]
    fn transit_replacement_rejects_wide_patterns_at_budget() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy_with_rule(
            SensitiveFilterMode::TransitReversible,
            ".",
            "safe",
            vec![SensitiveFilterTarget::RequestBody],
        ))
        .unwrap();
        let input = vec![b'a'; 4096];

        let err = compiled
            .replace_transit_request(SensitiveFilterTarget::RequestBody, &input)
            .unwrap_err();

        assert!(matches!(
            err,
            TransitReplacementError::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn transit_overflow_rejects_request_instead_of_leaking_plaintext() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy_with_rule(
            SensitiveFilterMode::TransitReversible,
            "ak-[0-9]+",
            "safe",
            vec![SensitiveFilterTarget::RequestBody],
        ))
        .unwrap();
        let input = (0..(MAX_TRANSIT_MAP_ENTRIES + 2))
            .map(|idx| format!("ak-{idx}"))
            .collect::<Vec<_>>()
            .join(" ");

        let err = compiled
            .replace_transit_request(SensitiveFilterTarget::RequestBody, input.as_bytes())
            .unwrap_err();

        let TransitReplacementError::BudgetExhausted {
            entries,
            surrogate_bytes,
        } = err;
        assert_eq!(entries, MAX_TRANSIT_MAP_ENTRIES);
        assert!(surrogate_bytes > 0);
    }

    #[test]
    fn streaming_reverse_handles_split_surrogate() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) = compiled
            .replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one")
            .unwrap();
        let split = out.len() - 2;
        let mut stream = ReverseStream::new(&map);
        let a = stream.push(&out[..split], false);
        let b = stream.push(&out[split..], true);
        let mut joined = Vec::new();
        joined.extend_from_slice(&a);
        joined.extend_from_slice(&b);
        assert_eq!(joined, b"sk-one");
    }

    #[test]
    fn streaming_reverse_does_not_buffer_unrelated_short_chunk() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (_out, map) = compiled
            .replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one")
            .unwrap();
        let mut stream = ReverseStream::new(&map);

        let out = stream.push(b"hello", false);

        assert_eq!(out.as_ref(), b"hello");
    }
}
