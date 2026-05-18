use smallvec::SmallVec;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default)]
pub struct PhaseRecord {
    pub name: &'static str,
    pub start_ns: u64,
    pub end_ns: u64,
    pub alloc_at_start: u64,
    pub alloc_at_end: u64,
}

#[derive(Clone, Debug)]
pub struct ChunkRecord {
    pub idx: u32,
    pub size: u32,
    pub parse_ns: u64,
    pub render_ns: u64,
    pub at_ns: u64,
}

#[derive(Clone, Debug)]
pub struct MutationRecord {
    pub field: &'static str,
    pub reason: &'static str,
    pub at_ns: u64,
}

pub struct TraceBuffer {
    pub phases: [PhaseRecord; 32],
    pub phase_count: u8,
    pub stream_chunks: Vec<ChunkRecord>,
    pub ir_mutations: Vec<MutationRecord>,
    pub filter_drop_reasons: SmallVec<[(String, &'static str); 8]>,
    base: Instant,
}

impl Default for TraceBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceBuffer {
    pub fn new() -> Self {
        Self {
            phases: [PhaseRecord::default(); 32],
            phase_count: 0,
            stream_chunks: Vec::new(),
            ir_mutations: Vec::new(),
            filter_drop_reasons: SmallVec::new(),
            base: Instant::now(),
        }
    }

    #[inline(always)]
    pub fn now_ns(&self) -> u64 {
        self.base.elapsed().as_nanos() as u64
    }

    #[inline(always)]
    pub fn phase_start(&mut self, name: &'static str, alloc: u64) {
        let idx = self.phase_count as usize;
        if idx >= self.phases.len() {
            return;
        }
        self.phases[idx] = PhaseRecord {
            name,
            start_ns: self.now_ns(),
            end_ns: 0,
            alloc_at_start: alloc,
            alloc_at_end: 0,
        };
        self.phase_count += 1;
    }

    #[inline(always)]
    pub fn phase_end(&mut self, alloc: u64) {
        let idx = (self.phase_count as usize).saturating_sub(1);
        if idx >= self.phases.len() {
            return;
        }
        let now = self.now_ns();
        let p = &mut self.phases[idx];
        p.end_ns = now;
        p.alloc_at_end = alloc;
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DebugTrigger {
    None,
    Header,
    UserFlag,
    Sampling,
    TraceSuffix,
}

#[derive(Clone, Copy, Debug)]
pub struct DebugMode {
    pub enabled: bool,
    pub trigger: DebugTrigger,
    pub include_request_body: bool,
    pub include_response_body: bool,
}

impl DebugMode {
    pub const DISABLED: Self = Self {
        enabled: false,
        trigger: DebugTrigger::None,
        include_request_body: false,
        include_response_body: false,
    };
    #[inline]
    pub fn upgrade_to_enabled(&mut self, trigger: DebugTrigger) {
        if self.enabled {
            return;
        }
        self.enabled = true;
        self.trigger = trigger;
        self.include_request_body = true;
        self.include_response_body = true;
    }
}

pub trait DebugSink: Send + Sync {
    fn emit(&self, trace: &TraceBuffer);
}
pub struct NoopDebugSink;
impl DebugSink for NoopDebugSink {
    fn emit(&self, _: &TraceBuffer) {}
}
