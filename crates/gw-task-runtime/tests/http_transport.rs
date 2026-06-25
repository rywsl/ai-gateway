use std::collections::BTreeMap;

use gw_task_runtime::http_transport::HttpTemplateTransport;
use gw_task_runtime::template_executor::{TransportError, UpstreamRequest, UpstreamTransport};
use wiremock::matchers::{body_json, body_string_contains, header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn http_transport_exposes_default_timeout_constructor() {
    let _transport =
        HttpTemplateTransport::with_default_timeout(Some("https://example.com".into()))
            .expect("default timeout client should build");
}

#[tokio::test]
async fn http_transport_posts_json_to_relative_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/images/generations"))
        .and(header("x-test", "yes"))
        .and(body_json(serde_json::json!({
            "model": "gpt-image-2",
            "prompt": "glass sphere"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"b64_json": "Zmlyc3Q="}]
        })))
        .mount(&server)
        .await;

    let transport = HttpTemplateTransport::new(reqwest::Client::new(), Some(server.uri()));
    let response = transport
        .submit(UpstreamRequest {
            method: "POST".to_string(),
            path: "/v1/images/generations".to_string(),
            headers: BTreeMap::from([("x-test".to_string(), "yes".to_string())]),
            body: serde_json::json!({
                "model": "gpt-image-2",
                "prompt": "glass sphere"
            }),
        })
        .await
        .expect("submit");

    assert_eq!(response.status, 200);
    assert_eq!(response.body["data"][0]["b64_json"], "Zmlyc3Q=");
}

#[tokio::test]
async fn http_transport_preserves_non_success_response_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bad"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": {"message": "rate limited"}
        })))
        .mount(&server)
        .await;

    let transport = HttpTemplateTransport::new(reqwest::Client::new(), Some(server.uri()));
    let err = transport
        .submit(UpstreamRequest {
            method: "POST".to_string(),
            path: "/bad".to_string(),
            headers: BTreeMap::new(),
            body: serde_json::json!({}),
        })
        .await
        .expect_err("non-success");

    assert!(
        matches!(err, TransportError::Failed(message) if message.contains("429") && message.contains("rate limited"))
    );
}

#[tokio::test]
async fn http_transport_posts_multipart_body_from_template_description() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/images/edits"))
        .and(header_regex("content-type", "^multipart/form-data;"))
        .and(body_string_contains("name=\"model\""))
        .and(body_string_contains("gpt-image-2"))
        .and(body_string_contains(
            "name=\"image\"; filename=\"input.png\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"b64_json": "Zmlyc3Q="}]
        })))
        .mount(&server)
        .await;

    let transport = HttpTemplateTransport::new(reqwest::Client::new(), Some(server.uri()));
    let response = transport
        .submit(UpstreamRequest {
            method: "POST".to_string(),
            path: "/v1/images/edits".to_string(),
            headers: BTreeMap::new(),
            body: serde_json::json!({
                "multipart": {
                    "fields": {
                        "model": "gpt-image-2",
                        "prompt": "make it brighter"
                    },
                    "files": {
                        "image": {
                            "filename": "input.png",
                            "content_type": "image/png",
                            "bytes_b64": "UE5HREFUQQ=="
                        }
                    }
                }
            }),
        })
        .await
        .expect("submit");

    assert_eq!(response.status, 200);
    assert_eq!(response.body["data"][0]["b64_json"], "Zmlyc3Q=");
}
