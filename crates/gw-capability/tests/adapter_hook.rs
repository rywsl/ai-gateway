use gw_capability::adapter_hook::{
    AdapterCapabilityHook, CapabilityDetectResult, DetectCtx, EntityVendorKeyView,
};
use gw_capability::types::{Capability, CapabilityOrigin, RequiredCapabilities};
use smallvec::smallvec;

struct StaticHook;

impl AdapterCapabilityHook for StaticHook {
    fn detect(&self, ctx: &DetectCtx<'_>) -> CapabilityDetectResult {
        for cap in ctx.required.set.iter() {
            if !ctx.vendor_key.detected_capabilities.contains(cap) {
                return CapabilityDetectResult::Missing(smallvec![*cap]);
            }
        }
        CapabilityDetectResult::Match
    }
}

#[test]
fn match_when_all_caps_present() {
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    let vk = EntityVendorKeyView {
        detected_capabilities: vec![Capability::Vision, Capability::Thinking],
    };
    let r = StaticHook.detect(&DetectCtx {
        required: &req,
        vendor_key: &vk,
    });
    assert_eq!(r, CapabilityDetectResult::Match);
}

#[test]
fn missing_when_cap_absent() {
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Audio, CapabilityOrigin::HeaderRequire);
    let vk = EntityVendorKeyView {
        detected_capabilities: vec![Capability::Vision],
    };
    let r = StaticHook.detect(&DetectCtx {
        required: &req,
        vendor_key: &vk,
    });
    match r {
        CapabilityDetectResult::Missing(v) => assert_eq!(v[..], [Capability::Audio]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn match_when_required_is_empty() {
    let req = RequiredCapabilities::default();
    let vk = EntityVendorKeyView {
        detected_capabilities: vec![],
    };
    let r = StaticHook.detect(&DetectCtx {
        required: &req,
        vendor_key: &vk,
    });
    assert_eq!(r, CapabilityDetectResult::Match);
}
