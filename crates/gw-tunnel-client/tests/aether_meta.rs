use gw_tunnel_client::aether::meta::{RequestMeta, ResponseMeta};
use std::collections::HashMap;

#[test]
fn request_meta_defaults_timeout_when_missing() {
    let raw = r#"{"method":"GET","url":"http://1.1.1.1/","headers":{"accept":"*/*"}}"#;
    let meta: RequestMeta = serde_json::from_str(raw).unwrap();
    assert_eq!(meta.timeout, 60);
}

#[test]
fn request_meta_accepts_integer_like_float_timeout() {
    let raw = r#"{"method":"GET","url":"http://1.1.1.1/","headers":{},"timeout":15.0}"#;
    let meta: RequestMeta = serde_json::from_str(raw).unwrap();
    assert_eq!(meta.timeout, 15);
}

#[test]
fn request_meta_rejects_fractional_timeout() {
    let raw = r#"{"method":"GET","url":"http://1.1.1.1/","headers":{},"timeout":15.5}"#;
    assert!(serde_json::from_str::<RequestMeta>(raw).is_err());
}

#[test]
fn request_meta_serializes_timeout_as_non_optional_integer() {
    let meta = RequestMeta {
        method: "GET".to_string(),
        url: "http://1.1.1.1/".to_string(),
        headers: HashMap::from([("accept".to_string(), "*/*".to_string())]),
        timeout: 60,
    };
    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains(r#""timeout":60"#), "got {json}");
}

#[test]
fn response_meta_preserves_duplicate_headers() {
    let meta = ResponseMeta {
        status: 203,
        headers: vec![
            ("set-cookie".to_string(), "a=1".to_string()),
            ("set-cookie".to_string(), "b=2".to_string()),
        ],
    };
    let json = serde_json::to_string(&meta).unwrap();
    let decoded: ResponseMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.headers.len(), 2);
    assert_eq!(decoded.headers[0].1, "a=1");
    assert_eq!(decoded.headers[1].1, "b=2");
}
