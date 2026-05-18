use crate::error::ErrorIR;
use crate::ir::*;

pub trait ContentFilter: Send + Sync {
    fn check_request(&self, ir: &RequestIR<'_>) -> ContentVerdict;
    fn check_stream_chunk(&self, chunk: &StreamEventIR<'_>) -> ContentVerdict;
    fn check_response(&self, ir: &ResponseIR<'_>) -> ContentVerdict;
}

pub enum ContentVerdict {
    Pass,
    Tag(String),
    Reject(ErrorIR<'static>),
}

pub struct NoopContentFilter;
impl ContentFilter for NoopContentFilter {
    fn check_request(&self, _: &RequestIR<'_>) -> ContentVerdict {
        ContentVerdict::Pass
    }
    fn check_stream_chunk(&self, _: &StreamEventIR<'_>) -> ContentVerdict {
        ContentVerdict::Pass
    }
    fn check_response(&self, _: &ResponseIR<'_>) -> ContentVerdict {
        ContentVerdict::Pass
    }
}

pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, text: &str, model: &str) -> u32;
}

pub struct NoopTokenEstimator;
impl TokenEstimator for NoopTokenEstimator {
    fn estimate(&self, text: &str, _: &str) -> u32 {
        // ADR-004 保守上限：char/4 经验系数
        (text.chars().count() as u32 / 4).max(1)
    }
}

pub trait Redactor: Send + Sync {
    fn redact(&self, s: &mut String);
}
