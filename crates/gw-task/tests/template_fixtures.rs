use std::{fs, path::Path};

use gw_task::template::{render_fixture, TemplateError, BUILTIN_MODEL_ID_ONLY};
use serde_json::Value;

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../async-task-template-routing-go/docs/contracts/async-task-template/v1/fixtures"
);

fn load_fixture(name: &str) -> Value {
    let path = Path::new(FIXTURE_ROOT).join(name);
    let body = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read fixture {}: {err}", path.display());
    });
    serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("decode fixture {}: {err}", path.display());
    })
}

#[test]
fn renders_shared_success_fixtures_like_go() {
    for fixture in [
        "openai-image-default.json",
        "openai-embedding-default.json",
        "openai-video-default.json",
        "custom-lro-google-veo.json",
        "builtin-model-id-only.json",
    ] {
        let fx = load_fixture(fixture);
        let rendered = render_fixture(&fx).unwrap_or_else(|err| {
            panic!("render {fixture}: {err}");
        });
        let expected = fx.get("expected").expect("fixture expected");

        assert_eq!(
            rendered.rendered_upstream_request.body, expected["rendered_upstream_request"]["body"],
            "{fixture} upstream body"
        );
        assert_eq!(
            rendered.client_sync_response, expected["client_sync_response"],
            "{fixture} client response"
        );
        assert!(
            rendered.effective_template_hash.starts_with("sha256:"),
            "{fixture} effective template hash"
        );
    }
}

#[test]
fn builtin_model_id_only_matches_go_fixture_byte_for_byte() {
    let path = Path::new(FIXTURE_ROOT).join("builtin-model-id-only.json");
    let fixture = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read fixture {}: {err}", path.display());
    });

    assert_eq!(BUILTIN_MODEL_ID_ONLY, fixture);
}

#[test]
fn error_fixtures_return_stable_codes() {
    let cases = [
        ("errors/forbidden-header.json", "GW_TASK_TEMPLATE_INVALID"),
        (
            "errors/missing-required-client-field.json",
            "GW_TASK_TEMPLATE_RENDER_FAILED",
        ),
    ];

    for (fixture, expected_code) in cases {
        let fx = load_fixture(fixture);
        let err = render_fixture(&fx).expect_err("fixture should fail");
        assert_eq!(err.code(), expected_code, "{fixture}");
    }
}

#[test]
fn map_limits_are_enforced_inside_rendering() {
    let ctx = serde_json::json!({
        "items": [1, 2, 3]
    });
    let tmpl = serde_json::json!({
        "$map": {
            "source": "{{items}}",
            "as": "item",
            "template": {"value": "{{item}}"}
        }
    });

    let err = gw_task::template::render(
        ctx.as_object().expect("object context"),
        &tmpl,
        gw_task::template::RenderLimits {
            max_map_items: 2,
            ..Default::default()
        },
    )
    .expect_err("map should exceed limit");

    assert!(matches!(err, TemplateError::RenderFailed(_)));
    assert_eq!(err.code(), "GW_TASK_TEMPLATE_RENDER_FAILED");
}
