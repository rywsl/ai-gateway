use gw_adapter::AdapterCtx;

#[test]
fn adapter_ctx_exposes_optional_oauth_runtime_slot() {
    fn assert_has_field(ctx: &AdapterCtx<'_>) -> bool {
        ctx.oauth_runtime.is_none()
    }

    let _field_reader: fn(&AdapterCtx<'_>) -> bool = assert_has_field;
}
