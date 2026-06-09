use gw_codec::openai::responses::{
    wire_response::ResponseOutputItem, OpenAiResponsesCodec, OpenAiResponsesRequest,
    OpenAiResponsesResponse, OpenAiResponsesStreamChunk,
};
use gw_codec::ProtocolCodec;
use gw_core::ir::{ContentIR, MessageIR, RequestIR, ResponseIR, StreamEventIR};
use std::borrow::Cow;

#[test]
fn deserialize_minimal_request() {
    let s = r#"{"model":"gpt-5","input":[{"type":"input_text","text":"hi"}],"stream":true}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert_eq!(r.model, "gpt-5");
    assert!(r.stream);
    assert_eq!(r.input.as_array().unwrap().len(), 1);
}

#[test]
fn deserialize_request_preserves_extras() {
    let s = r#"{"model":"gpt-5","input":[],"temperature":0.7,"top_p":0.9}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert!(r.extras.contains_key("temperature"));
    assert!(r.extras.contains_key("top_p"));
}

#[test]
fn deserialize_response_object() {
    let s = r#"{"id":"resp_1","object":"response","model":"gpt-5","output":[]}"#;
    let r: OpenAiResponsesResponse = serde_json::from_str(s).unwrap();
    assert_eq!(r.id, "resp_1");
    assert_eq!(r.model, "gpt-5");
    assert!(r.output.is_empty());
}

#[test]
fn deserialize_stream_chunk_with_type() {
    let s = r#"{"type":"response.output_text.delta","delta":"He"}"#;
    let c: OpenAiResponsesStreamChunk = serde_json::from_str(s).unwrap();
    assert!(matches!(c.r#type, gw_codec::openai::responses::wire_stream_chunk::OpenAiResponsesStreamEventType::ResponseOutputTextDelta));
    assert!(c.fields.contains_key("delta"));
}

#[test]
fn serialize_request_round_trip() {
    let s = r#"{"model":"gpt-5","input":[{"type":"input_text","text":"hi"}],"stream":false}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    let back = serde_json::to_string(&r).unwrap();
    let r2: OpenAiResponsesRequest = serde_json::from_str(&back).unwrap();
    assert_eq!(r2.model, "gpt-5");
}

#[test]
fn deserialize_optional_fields() {
    let s = r#"{"model":"gpt-5","input":[],"instructions":"be helpful","previous_response_id":"resp_0"}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert_eq!(r.instructions.as_deref(), Some("be helpful"));
    assert_eq!(r.previous_response_id.as_deref(), Some("resp_0"));
}

#[test]
fn responses_request_parse_render_round_trips_task_body() {
    let body = br#"{"model":"gpt-5","background":true,"input":"hi","instructions":"be direct","stream":false}"#;
    let ir = OpenAiResponsesCodec.parse_request(body).unwrap();

    assert_eq!(ir.model, "gpt-5");
    assert!(!ir.stream);
    assert!(matches!(ir.messages[0], MessageIR::System { .. }));
    assert!(matches!(ir.messages[1], MessageIR::User { .. }));

    let stored = serde_json::to_value(&ir).expect("serialize request ir");
    let restored: RequestIR<'static> = serde_json::from_value(stored).expect("restore request ir");

    let mut rendered = bytes::BytesMut::new();
    OpenAiResponsesCodec
        .render_request(&restored, &mut rendered)
        .expect("render restored responses request");
    let rendered_json: serde_json::Value =
        serde_json::from_slice(&rendered).expect("rendered request json");

    assert_eq!(rendered_json["model"], "gpt-5");
    assert_eq!(rendered_json["background"], true);
    assert_eq!(rendered_json["instructions"], "be direct");
    assert_eq!(rendered_json["input"], "hi");
    assert_eq!(rendered_json["stream"], false);
}

#[test]
fn responses_request_render_reconstructed_ir_uses_item_array_input() {
    let ir = RequestIR {
        model: Cow::Borrowed("gpt-5"),
        messages: vec![MessageIR::User {
            content: ContentIR::Text(Cow::Borrowed("hi")),
        }],
        ..RequestIR::default()
    };

    let mut rendered = bytes::BytesMut::new();
    OpenAiResponsesCodec
        .render_request(&ir, &mut rendered)
        .expect("render reconstructed responses request");
    let rendered_json: serde_json::Value =
        serde_json::from_slice(&rendered).expect("rendered request json");

    assert_eq!(rendered_json["model"], "gpt-5");
    let input = rendered_json["input"].as_array().expect("input item array");
    assert_eq!(input[0]["type"], "input_text");
    assert_eq!(input[0]["text"], "hi");
}

#[test]
fn deserialize_new_output_item_types() {
    let s = r#"{
        "id":"resp_tools",
        "object":"response",
        "model":"gpt-5",
        "output":[
            {"type":"web_search_call","id":"ws_1","status":"completed","query":"rust"},
            {"type":"code_interpreter_call","id":"ci_1","name":"python","status":"completed","input":{"code":"print(1)"}},
            {"type":"mcp_call","id":"mcp_1","name":"lookup","call_id":"call_1","server_label":"srv"},
            {"type":"compaction","id":"cmp_1","status":"completed","summary":"trimmed"}
        ]
    }"#;
    let r: OpenAiResponsesResponse = serde_json::from_str(s).unwrap();
    assert_eq!(r.output.len(), 4);
    match &r.output[0] {
        ResponseOutputItem::WebSearchCall(item) => {
            assert_eq!(item.id, "ws_1");
            assert_eq!(item.extra["query"], "rust");
        }
        other => panic!("unexpected item: {other:?}"),
    }
    match &r.output[1] {
        ResponseOutputItem::CodeInterpreterCall(item) => {
            assert_eq!(item.name, "python");
            assert_eq!(item.extra["input"]["code"], "print(1)");
        }
        other => panic!("unexpected item: {other:?}"),
    }
    match &r.output[2] {
        ResponseOutputItem::McpCall(item) => {
            assert_eq!(item.name, "lookup");
            assert_eq!(item.call_id, "call_1");
        }
        other => panic!("unexpected item: {other:?}"),
    }
    assert!(matches!(r.output[3], ResponseOutputItem::Compaction(_)));
}

#[test]
fn parse_response_maps_new_items_to_server_tool_blocks() {
    let s = br#"{
        "id":"resp_tools",
        "object":"response",
        "model":"gpt-5",
        "output":[
            {"type":"web_search_call","id":"ws_1","status":"completed","query":"rust"},
            {"type":"code_interpreter_call","id":"ci_1","name":"python","status":"completed","input":{"code":"print(1)"}},
            {"type":"mcp_call","id":"mcp_1","name":"lookup","call_id":"call_1","server_label":"srv"},
            {"type":"compaction","id":"cmp_1","status":"completed","summary":"trimmed"}
        ]
    }"#;
    let ir = OpenAiResponsesCodec.parse_response(s).unwrap();
    let blocks = ir.server_tool_blocks.as_ref().unwrap();
    assert_eq!(blocks.len(), 4);
    assert_eq!(blocks[0].kind, "web_search_call");
    assert_eq!(blocks[0].extra["query"], "rust");
    assert_eq!(blocks[1].kind, "code_interpreter_call");
    assert_eq!(blocks[1].input.get(), r#"{"code":"print(1)"}"#);
    assert_eq!(blocks[2].kind, "mcp_call");
    assert_eq!(blocks[2].name, "lookup");
    assert_eq!(blocks[2].extra["call_id"], "call_1");
    assert_eq!(blocks[3].kind, "compaction");
}

#[test]
fn existing_message_function_call_and_reasoning_behavior_survives() {
    let s = br#"{
        "id":"resp_existing",
        "object":"response",
        "model":"gpt-5",
        "output":[
            {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thought"}]},
            {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hello"}]},
            {"type":"function_call","id":"fc_1","name":"lookup","arguments":"{\"x\":1}","call_id":"call_1","status":"completed"}
        ]
    }"#;
    let ir = OpenAiResponsesCodec.parse_response(s).unwrap();
    assert_eq!(ir.reasoning.as_ref().unwrap().summary, "thought");
    let MessageIR::Assistant {
        content,
        tool_calls,
    } = &ir.messages[0]
    else {
        panic!("expected assistant message");
    };
    let ContentIR::Text(text) = content else {
        panic!("expected text content");
    };
    assert_eq!(text, "hello");
    let tool_call = &tool_calls.as_ref().unwrap()[0];
    assert_eq!(tool_call.id, "call_1");
    assert_eq!(tool_call.name, "lookup");
    assert_eq!(tool_call.arguments_json.get(), r#"{"x":1}"#);
}

#[test]
fn render_response_maps_server_tool_blocks_to_output_items() {
    use gw_core::ir::ServerToolBlockIR;
    use serde_json::value::RawValue;
    use std::borrow::Cow;

    let input = RawValue::from_string(r#"{"code":"print(1)"}"#.to_owned()).unwrap();
    let mut extra = serde_json::Map::new();
    extra.insert("call_id".to_owned(), serde_json::json!("call_1"));
    extra.insert("server_label".to_owned(), serde_json::json!("srv"));
    let ir = ResponseIR {
        id: Cow::Borrowed("resp_render"),
        model: Cow::Borrowed("gpt-5"),
        server_tool_blocks: Some(vec![
            ServerToolBlockIR {
                kind: Cow::Borrowed("code_interpreter_call"),
                id: Cow::Borrowed("ci_1"),
                name: Cow::Borrowed("python"),
                input,
                extra,
            },
            ServerToolBlockIR {
                kind: Cow::Borrowed("compaction"),
                id: Cow::Borrowed("cmp_1"),
                name: Cow::Borrowed(""),
                input: RawValue::from_string("null".to_owned()).unwrap(),
                extra: serde_json::Map::new(),
            },
        ]),
        ..ResponseIR::default()
    };
    let mut buf = bytes::BytesMut::new();
    OpenAiResponsesCodec.render_response(&ir, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(v["output"][0]["type"], "code_interpreter_call");
    assert_eq!(v["output"][0]["name"], "python");
    assert_eq!(v["output"][0]["call_id"], "call_1");
    assert_eq!(v["output"][0]["input"]["code"], "print(1)");
    assert_eq!(v["output"][1]["type"], "compaction");
}

#[test]
fn responses_new_item_types_roundtrip_through_ir() {
    let body = br#"{
        "id":"resp_full","object":"response","model":"gpt-5",
        "output":[
            {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"think"}]},
            {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"no"},{"type":"output_text","text":"fallback"}]},
            {"type":"custom_tool_call","id":"ct_1","name":"custom","call_id":"call_custom","input":{"x":1}},
            {"type":"mcp_list_tools","id":"mcp_list","name":"list","call_id":"call_mcp","server_label":"srv"},
            {"type":"tool_search_call","id":"ts_1","name":"search","call_id":"call_ts","query":"docs"}
        ],
        "usage":{"input_tokens":11,"output_tokens":7,"output_tokens_details":{"reasoning_tokens":3}}
    }"#;

    let ir = OpenAiResponsesCodec.parse_response(body).unwrap();
    assert_eq!(ir.reasoning.as_ref().unwrap().summary.as_ref(), "think");
    assert_eq!(ir.refusal.as_deref(), Some("no"));
    let blocks = ir.server_tool_blocks.as_ref().unwrap();
    assert!(blocks.iter().any(|b| b.kind.as_ref() == "custom_tool_call"));
    assert!(blocks.iter().any(|b| b.kind.as_ref() == "mcp_list_tools"));
    assert!(blocks.iter().any(|b| b.kind.as_ref() == "tool_search_call"));

    let mut buf = bytes::BytesMut::new();
    OpenAiResponsesCodec.render_response(&ir, &mut buf).unwrap();
    let reparsed = OpenAiResponsesCodec.parse_response(&buf).unwrap();
    assert_eq!(
        reparsed.reasoning.as_ref().unwrap().summary.as_ref(),
        "think"
    );
    assert!(reparsed
        .server_tool_blocks
        .as_ref()
        .unwrap()
        .iter()
        .any(|b| b.kind.as_ref() == "mcp_list_tools"));
}

#[test]
fn responses_stream_gap_events_parse_to_ir() {
    let codec = OpenAiResponsesCodec;
    let mut state = codec.new_stream_state();
    let chunk = br#"data: {"type":"response.refusal.delta","delta":"no"}

data: {"type":"response.reasoning_text.delta","delta":"thinking"}

data: {"type":"response.output_item.added","item":{"type":"mcp_call","id":"mcp_1","name":"lookup","call_id":"call_1","server_label":"srv"}}

data: {"type":"response.custom_tool_call_input.delta","item_id":"ct_1","delta":"{\"x\""}

"#;

    let events = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEventIR::RefusalDelta { text } if text.as_ref() == "no")));
    assert!(events.iter().any(
        |e| matches!(e, StreamEventIR::ThinkingDelta { text, .. } if text.as_ref() == "thinking")
    ));
    assert!(events.iter().any(
        |e| matches!(e, StreamEventIR::ServerToolStart { kind, .. } if kind.as_ref() == "mcp_call")
    ));
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEventIR::ServerToolDelta { .. })));
}
