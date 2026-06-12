use smallvec::SmallVec;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    Vision,
    Audio,
    Video,
    Thinking,
    ToolUse,
    StreamingOnly,
    OpenAiResponsesShape,
    VertexAiServiceAccount,
    Tagged(Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    Exclusive,
    #[default]
    Compatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityOrigin {
    Model,
    KeyForced,
    HeaderRequire,
    AdapterDetected,
    RetryUpgrade,
}

#[derive(Debug, Clone, Default)]
pub struct RequiredCapabilities {
    pub set: SmallVec<[Capability; 6]>,
    pub mode: MatchMode,
    pub provenance: SmallVec<[(Capability, CapabilityOrigin); 8]>,
}

impl RequiredCapabilities {
    pub fn add(&mut self, c: Capability, origin: CapabilityOrigin) {
        if !self.set.contains(&c) {
            self.set.push(c.clone());
        }
        self.provenance.push((c, origin));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn tagged_arc_strings_equal_when_content_matches() {
        // Two Tagged values constructed from distinct Arcs but with
        // the same content must compare equal — this is what makes
        // intern eviction observably benign.
        let a = Capability::Tagged(Arc::<str>::from("custom-foo"));
        let b = Capability::Tagged(Arc::<str>::from("custom-foo"));
        assert_eq!(a, b);

        let c = Capability::Tagged(Arc::<str>::from("custom-bar"));
        assert_ne!(a, c);
    }

    #[test]
    fn tagged_hash_consistent_with_eq() {
        use std::collections::HashSet;
        let mut set: HashSet<Capability> = HashSet::new();
        set.insert(Capability::Tagged(Arc::<str>::from("custom-foo")));
        assert!(set.contains(&Capability::Tagged(Arc::<str>::from("custom-foo"))));
    }
}
