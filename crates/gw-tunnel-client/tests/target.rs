use gw_tunnel_client::aether::meta::ResponseMeta;
use gw_tunnel_client::StreamTarget;

#[test]
fn stream_target_http_variant_constructs() {
    let (body_tx, body_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
    let (resp_body_tx, _) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
    let (meta_tx, _) = tokio::sync::oneshot::channel::<ResponseMeta>();

    let target = StreamTarget::Http {
        method: http::Method::POST,
        url: url::Url::parse("https://example.test/v1/x").unwrap(),
        headers: vec![("x".into(), "y".into())],
        body_rx,
        body_tx_response: resp_body_tx,
        meta_tx_response: meta_tx,
    };

    match target {
        StreamTarget::Http { method, .. } => assert_eq!(method, http::Method::POST),
        StreamTarget::Tcp { .. } => panic!("wrong variant"),
        _ => panic!("unknown variant"),
    }
    drop(body_tx);
}
