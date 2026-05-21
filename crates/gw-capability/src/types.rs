use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Vision,
    Audio,
    Video,
    Thinking,
    ToolUse,
    StreamingOnly,
    OpenAiResponsesShape,
    VertexAiServiceAccount,
    Tagged(&'static str),
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
            self.set.push(c);
        }
        self.provenance.push((c, origin));
    }
}
