use crate::ctx::ReqCtx;
use bytes::Bytes;
use gw_core::entity::SensitiveFilterTarget;
use gw_runtime::CompiledSensitiveFilterPolicy;

pub(crate) fn apply_transit_request_body(
    ctx: &mut ReqCtx,
    compiled: &CompiledSensitiveFilterPolicy,
    body: Bytes,
) -> Bytes {
    let (out, map) = compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, &body);
    if map.is_empty() {
        return body;
    }
    ctx.sensitive_filter_transit = Some(map);
    ctx.sensitive_filter_reverse_stream = None;
    out
}

pub(crate) fn reverse_transit_response_body(
    ctx: &mut ReqCtx,
    body: Bytes,
    end_of_stream: bool,
) -> Bytes {
    let Some(map) = ctx.sensitive_filter_transit.as_ref() else {
        return body;
    };
    if map.is_empty() {
        return body;
    }
    let stream = ctx
        .sensitive_filter_reverse_stream
        .get_or_insert_with(|| gw_runtime::ReverseStream::new(map));
    stream.push(&body, end_of_stream)
}

pub(crate) fn reverse_body_option(ctx: &mut ReqCtx, body: &mut Option<Bytes>, end_of_stream: bool) {
    let should_flush_tail = end_of_stream
        && ctx
            .sensitive_filter_transit
            .as_ref()
            .is_some_and(|map| !map.is_empty());
    let had_body = body.is_some();
    let Some(chunk) = body.take().or_else(|| should_flush_tail.then(Bytes::new)) else {
        return;
    };
    let reversed = reverse_transit_response_body(ctx, chunk, end_of_stream);
    if had_body || !reversed.is_empty() {
        *body = Some(reversed);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use gw_core::entity::{
        SensitiveFilterGroup, SensitiveFilterMode, SensitiveFilterPolicy, SensitiveFilterRule,
        SensitiveFilterTarget,
    };

    pub(crate) fn transit_policy() -> CompiledSensitiveFilterPolicy {
        CompiledSensitiveFilterPolicy::compile(&SensitiveFilterPolicy {
            id: "global".to_owned(),
            version: 1,
            groups: vec![SensitiveFilterGroup {
                id: "g1".to_owned(),
                name: "Default".to_owned(),
                description: String::new(),
                enabled: true,
                priority: 1,
                rules: vec![SensitiveFilterRule {
                    id: "transit".to_owned(),
                    name: "Transit".to_owned(),
                    enabled: true,
                    priority: 1,
                    mode: SensitiveFilterMode::TransitReversible,
                    pattern: "ak-[A-Za-z0-9_-]+".to_owned(),
                    replacement: "ak-placeholder".to_owned(),
                    targets: vec![
                        SensitiveFilterTarget::RequestBody,
                        SensitiveFilterTarget::ResponseBody,
                    ],
                }],
            }],
        })
        .expect("compile policy")
    }

    pub(crate) fn archive_policy() -> CompiledSensitiveFilterPolicy {
        CompiledSensitiveFilterPolicy::compile(&SensitiveFilterPolicy {
            id: "global".to_owned(),
            version: 1,
            groups: vec![SensitiveFilterGroup {
                id: "g1".to_owned(),
                name: "Default".to_owned(),
                description: String::new(),
                enabled: true,
                priority: 1,
                rules: vec![SensitiveFilterRule {
                    id: "archive".to_owned(),
                    name: "Archive".to_owned(),
                    enabled: true,
                    priority: 1,
                    mode: SensitiveFilterMode::ArchiveMask,
                    pattern: "ak-[A-Za-z0-9_-]+".to_owned(),
                    replacement: "<AK>".to_owned(),
                    targets: vec![SensitiveFilterTarget::Archive],
                }],
            }],
        })
        .expect("compile policy")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transit_request_sets_map_and_reverses_response() {
        let compiled = test_support::transit_policy();
        let mut ctx = ReqCtx::default();
        let outbound =
            apply_transit_request_body(&mut ctx, &compiled, Bytes::from_static(b"ak-live"));
        assert_ne!(outbound.as_ref(), b"ak-live");
        assert!(std::str::from_utf8(&outbound)
            .expect("utf8 surrogate")
            .starts_with("__sf_"));

        let restored = reverse_transit_response_body(&mut ctx, outbound, true);
        assert_eq!(restored.as_ref(), b"ak-live");
    }

    #[test]
    fn reverse_response_handles_surrogate_split_across_chunks() {
        let compiled = test_support::transit_policy();
        let mut ctx = ReqCtx::default();
        let outbound =
            apply_transit_request_body(&mut ctx, &compiled, Bytes::from_static(b"ak-live"));
        let split = outbound.len() - 2;

        let first = reverse_transit_response_body(&mut ctx, outbound.slice(..split), false);
        let second = reverse_transit_response_body(&mut ctx, outbound.slice(split..), true);

        let mut joined = Vec::new();
        joined.extend_from_slice(&first);
        joined.extend_from_slice(&second);
        assert_eq!(joined, b"ak-live");
    }
}
