use crate::ctx::ReqCtx;
use bytes::Bytes;
use gw_codec::cross_render::WireFormat;
use gw_core::entity::SensitiveFilterTarget;
use gw_store::sensitive_filter::CompiledSensitiveFilterPolicy;

const MAX_TRANSIT_RESPONSE_DECODED_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn apply_transit_request_body(
    ctx: &mut ReqCtx,
    compiled: &CompiledSensitiveFilterPolicy,
    body: Bytes,
) -> Result<Bytes, gw_store::sensitive_filter::TransitReplacementError> {
    let (out, map) = compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, &body)?;
    if map.is_empty() {
        return Ok(body);
    }
    ctx.sensitive_filter_transit = Some(map);
    ctx.sensitive_filter_reverse_stream = None;
    Ok(out)
}

pub(crate) fn apply_transit_request_body_if_configured(
    ctx: &mut ReqCtx,
    body: Bytes,
) -> Result<Bytes, gw_store::sensitive_filter::TransitReplacementError> {
    let Some(compiled) = ctx.sensitive_filter_policy.clone() else {
        return Ok(body);
    };
    apply_transit_request_body(ctx, &compiled, body)
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
        .get_or_insert_with(|| gw_store::sensitive_filter::ReverseStream::new(map));
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

pub(crate) fn reverse_non_stream_response_body_option(
    ctx: &mut ReqCtx,
    body: &mut Option<Bytes>,
    end_of_stream: bool,
) -> pingora::Result<()> {
    if should_reverse_compressed_non_stream_response(ctx, body, end_of_stream) {
        if let Some(chunk) = body.take() {
            match reverse_compressed_response_body(ctx, chunk) {
                Ok(reversed) => {
                    *body = Some(reversed);
                    return Ok(());
                }
                Err(reason) => {
                    metrics::counter!(
                        "gw_sensitive_filter_transit_reverse_decode_failure_total",
                        "reason" => reason,
                    )
                    .increment(1);
                    return Err(pingora::Error::new(pingora::ErrorType::Custom(
                        "gw_sensitive_filter_transit_reverse_failed",
                    )));
                }
            }
        }
    }
    reverse_body_option(ctx, body, end_of_stream);
    Ok(())
}

pub(crate) fn has_compressed_transit_response(ctx: &ReqCtx) -> bool {
    ctx.sensitive_filter_transit
        .as_ref()
        .is_some_and(|map| !map.is_empty())
        && ctx.resp_content_encoding != gw_codec::compression::Encoding::Identity
}

pub(crate) fn should_buffer_compressed_non_stream_response_for_reverse(ctx: &ReqCtx) -> bool {
    !ctx.is_stream && has_compressed_transit_response(ctx) && !is_cross_protocol(ctx)
}

fn should_reverse_compressed_non_stream_response(
    ctx: &ReqCtx,
    body: &Option<Bytes>,
    end_of_stream: bool,
) -> bool {
    end_of_stream
        && !ctx.is_stream
        && body.is_some()
        && has_compressed_transit_response(ctx)
        && !is_cross_protocol(ctx)
}

fn reverse_compressed_response_body(ctx: &mut ReqCtx, chunk: Bytes) -> Result<Bytes, &'static str> {
    let decoded = gw_codec::compression::decode(
        ctx.resp_content_encoding,
        &chunk,
        MAX_TRANSIT_RESPONSE_DECODED_BYTES,
    )
    .map_err(|err| match err {
        gw_codec::compression::CompressionError::TooLarge { .. } => "too_large",
        gw_codec::compression::CompressionError::Io(_) => "io",
        gw_codec::compression::CompressionError::Brotli(_) => "brotli",
    })?;
    let reversed = reverse_transit_response_body(ctx, decoded, true);
    gw_codec::compression::encode(ctx.resp_content_encoding, &reversed).map_err(|err| match err {
        gw_codec::compression::CompressionError::TooLarge { .. } => "too_large",
        gw_codec::compression::CompressionError::Io(_) => "io",
        gw_codec::compression::CompressionError::Brotli(_) => "brotli",
    })
}

fn is_cross_protocol(ctx: &ReqCtx) -> bool {
    matches!(
        (ctx.client_codec.as_ref(), ctx.upstream_codec.as_ref()),
        (Some(client), Some(upstream))
            if WireFormat::from_any_codec(client) != WireFormat::from_any_codec(upstream)
    )
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
            apply_transit_request_body(&mut ctx, &compiled, Bytes::from_static(b"ak-live"))
                .unwrap();
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
            apply_transit_request_body(&mut ctx, &compiled, Bytes::from_static(b"ak-live"))
                .unwrap();
        let split = outbound.len() - 2;

        let first = reverse_transit_response_body(&mut ctx, outbound.slice(..split), false);
        let second = reverse_transit_response_body(&mut ctx, outbound.slice(split..), true);

        let mut joined = Vec::new();
        joined.extend_from_slice(&first);
        joined.extend_from_slice(&second);
        assert_eq!(joined, b"ak-live");
    }

    #[test]
    fn transit_overflow_returns_error_without_setting_map() {
        let compiled = test_support::transit_policy();
        let mut ctx = ReqCtx::default();
        let input = (0..514)
            .map(|idx| format!("ak-{idx}"))
            .collect::<Vec<_>>()
            .join(" ");

        let err = apply_transit_request_body(&mut ctx, &compiled, Bytes::from(input)).unwrap_err();

        assert!(matches!(
            err,
            gw_store::sensitive_filter::TransitReplacementError::BudgetExhausted { .. }
        ));
        assert!(ctx.sensitive_filter_transit.is_none());
    }

    #[test]
    fn compressed_transit_response_detection_requires_map_and_encoding() {
        let compiled = test_support::transit_policy();
        let mut ctx = ReqCtx {
            resp_content_encoding: gw_codec::compression::Encoding::Gzip,
            ..ReqCtx::default()
        };
        assert!(!has_compressed_transit_response(&ctx));

        let _ = apply_transit_request_body(&mut ctx, &compiled, Bytes::from_static(b"ak-live"))
            .unwrap();
        assert!(has_compressed_transit_response(&ctx));

        ctx.resp_content_encoding = gw_codec::compression::Encoding::Identity;
        assert!(!has_compressed_transit_response(&ctx));
    }
}
