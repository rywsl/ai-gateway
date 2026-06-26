use bytes::{Bytes, BytesMut};
use gw_core::entity::{
    SensitiveFilterMode, SensitiveFilterPolicy, SensitiveFilterRule, SensitiveFilterTarget,
};
use regex::{Captures, Regex};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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
        self.entries
            .sort_by(|left, right| right.0.len().cmp(&left.0.len()));
    }
}

#[derive(Debug, Clone)]
pub struct ReverseStream {
    entries: Vec<(Bytes, Bytes)>,
    tail: Vec<u8>,
    max_surrogate_len: usize,
}

impl ReverseStream {
    pub fn new(map: &TransitMap) -> Self {
        let entries = map.entries.clone();
        let max_surrogate_len = entries
            .iter()
            .map(|(surrogate, _)| surrogate.len())
            .max()
            .unwrap_or(0);
        Self {
            entries,
            tail: Vec::new(),
            max_surrogate_len,
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
            if !eos && input.len() - pos < self.max_surrogate_len {
                self.tail.extend_from_slice(&input[pos..]);
                break;
            }
            if let Some((surrogate, original)) = self
                .entries
                .iter()
                .find(|(surrogate, _)| input[pos..].starts_with(surrogate.as_ref()))
            {
                out.extend_from_slice(original.as_ref());
                pos += surrogate.len();
            } else {
                out.extend_from_slice(&input[pos..pos + 1]);
                pos += 1;
            }
        }
        Bytes::from(out.freeze())
    }
}

impl CompiledSensitiveFilterPolicy {
    pub fn compile(policy: &SensitiveFilterPolicy) -> anyhow::Result<Self> {
        let mut groups = policy
            .groups
            .iter()
            .filter(|group| group.enabled)
            .cloned()
            .collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut rules = Vec::new();
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
            for rule in group_rules {
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
    ) -> (Bytes, TransitMap) {
        let Some(text) = std::str::from_utf8(input).ok() else {
            return (Bytes::copy_from_slice(input), TransitMap::default());
        };
        let mut out = text.to_string();
        let mut map = TransitMap::default();
        let mut seq = 0u64;
        for rule in self.transit_rules(target) {
            let hash = rule_id_hash(&rule.rule.id);
            let replaced = rule.re.replace_all(&out, |caps: &Captures<'_>| {
                let matched = caps.get(0).expect("regex capture 0").as_str();
                let surrogate = format!("__sf_{hash}_{seq}__");
                seq += 1;
                map.push(
                    Bytes::from(surrogate.clone()),
                    Bytes::copy_from_slice(matched.as_bytes()),
                );
                surrogate
            });
            out = replaced.into_owned();
        }
        (Bytes::from(out), map)
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

fn rule_id_hash(rule_id: &str) -> String {
    let mut hasher = DefaultHasher::new();
    rule_id.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
        let (out, map) =
            compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one sk-two");
        assert_ne!(out.as_ref(), b"sk-one sk-two");
        assert_eq!(map.len(), 2);
        let restored = compiled.reverse_transit_response(&map, out.as_ref(), true);
        assert_eq!(restored.as_ref(), b"sk-one sk-two");
    }

    #[test]
    fn streaming_reverse_handles_split_surrogate() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) =
            compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one");
        let split = out.len() - 2;
        let mut stream = ReverseStream::new(&map);
        let a = stream.push(&out[..split], false);
        let b = stream.push(&out[split..], true);
        let mut joined = Vec::new();
        joined.extend_from_slice(&a);
        joined.extend_from_slice(&b);
        assert_eq!(joined, b"sk-one");
    }
}
