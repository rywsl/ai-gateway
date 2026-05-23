use gw_codec::stream_policy::{decide, UpstreamStreamCapability, UpstreamStreamPolicy};

#[test]
fn passthrough_sync_when_client_sync_and_upstream_supports_both() {
    let caps = UpstreamStreamCapability {
        supports_sync: true,
        supports_stream: true,
    };
    assert_eq!(decide(false, caps), UpstreamStreamPolicy::PassThroughSync);
}

#[test]
fn passthrough_stream_when_client_stream_and_upstream_supports_both() {
    let caps = UpstreamStreamCapability {
        supports_sync: true,
        supports_stream: true,
    };
    assert_eq!(decide(true, caps), UpstreamStreamPolicy::PassThroughStream);
}

#[test]
fn accumulate_when_client_sync_but_upstream_only_streams() {
    let caps = UpstreamStreamCapability {
        supports_sync: false,
        supports_stream: true,
    };
    assert_eq!(
        decide(false, caps),
        UpstreamStreamPolicy::AccumulateUpstreamStream
    );
}

#[test]
fn simulate_when_client_stream_but_upstream_only_sync() {
    let caps = UpstreamStreamCapability {
        supports_sync: true,
        supports_stream: false,
    };
    assert_eq!(
        decide(true, caps),
        UpstreamStreamPolicy::SimulateStreamFromSync
    );
}

#[test]
fn passthrough_stream_when_client_stream_and_upstream_only_streams() {
    let caps = UpstreamStreamCapability {
        supports_sync: false,
        supports_stream: true,
    };
    assert_eq!(decide(true, caps), UpstreamStreamPolicy::PassThroughStream);
}

#[test]
fn upstream_with_no_capability_falls_through_to_sync() {
    // misconfiguration; let upper layer surface the error
    let caps = UpstreamStreamCapability {
        supports_sync: false,
        supports_stream: false,
    };
    assert_eq!(decide(false, caps), UpstreamStreamPolicy::PassThroughSync);
    assert_eq!(decide(true, caps), UpstreamStreamPolicy::PassThroughSync);
}
