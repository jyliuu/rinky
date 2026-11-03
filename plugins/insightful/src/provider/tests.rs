use super::*;
use crate::testutil::{run_sse, CannedClient};
use ink_net::testserver::TestServer;
use ink_net::{HttpClient, UreqClient};

fn req() -> ChatRequest {
    ChatRequest {
        system: "SYS".into(),
        messages: vec![
            Msg::User("hello".into()),
            Msg::Assistant {
                text: "".into(),
                tool_calls: vec![ToolCall { id: "c1".into(), name: "toc".into(), arguments: json!({}) }],
                state: Some(json!({"reasoning_content": "think", "other": 1}).as_object().unwrap().clone()),
            },
            Msg::Tool { call_id: "c1".into(), name: "toc".into(), content: json!({"ok": true}) },
        ],
        tools: vec![json!({"name": "toc", "description": "d", "parameters": {"type": "object", "properties": {}}})],
    }
}

#[test]
fn compatible_body_and_headers() {
    let mut p = Profile::new(Kind::DeepSeek, "https://api.deepseek.com/chat/completions", "m", "sk-fake-test");
    p.max_tokens = Some(100);
    p.parameters.insert("top_p".into(), json!(0.5));
    let b = request_body(&p, &req(), true);
    assert_eq!(b["messages"][0], json!({"role": "system", "content": "SYS"}));
    assert_eq!(b["messages"][2]["tool_calls"][0]["function"]["arguments"], "{}");
    assert_eq!(b["messages"][2]["reasoning_content"], "think");
    assert!(b["messages"][2].get("other").is_none());
    assert_eq!(b["messages"][3], json!({"role": "tool", "tool_call_id": "c1", "content": "{\"ok\":true}"}));
    assert_eq!(b["max_tokens"], 100, "DeepSeek uses max_tokens");
    assert_eq!(b["stream_options"]["include_usage"], true);
    assert_eq!(b["top_p"], 0.5);
    assert_eq!(b["tools"][0]["type"], "function");
    p.kind = Kind::OpenAi;
    let b = request_body(&p, &req(), false);
    assert_eq!(b["max_completion_tokens"], 100);
    assert!(b.get("stream").is_none() && b.get("stream_options").is_none());
    assert!(b["messages"][2].get("reasoning_content").is_none(), "OpenAI has no state fields");
    let r = build_request(&p, &req(), true).unwrap();
    assert_eq!(r.header_value("authorization"), Some("Bearer sk-fake-test"));
    assert_eq!(r.header_value("accept"), Some("text/event-stream"));
    p.api_key = " ".into();
    assert_eq!(build_request(&p, &req(), true).unwrap_err(), "Insightful is not configured: API key is missing.");
}

#[test]
fn anthropic_body_and_headers() {
    let p = Profile::new(Kind::Anthropic, "https://api.anthropic.com/v1/messages", "claude-x", "sk-ant-fake");
    let b = request_body(&p, &req(), true);
    assert_eq!(b["system"], "SYS");
    assert_eq!(b["max_tokens"], 8192);
    assert_eq!(b["messages"][0], json!({"role": "user", "content": "hello"}));
    assert_eq!(b["messages"][1]["content"][0], json!({"type": "tool_use", "id": "c1", "name": "toc", "input": {}}));
    assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    assert!(b.get("stream_options").is_none());
    let r = build_request(&p, &req(), false).unwrap();
    assert_eq!(r.header_value("x-api-key"), Some("sk-ant-fake"));
    assert_eq!(r.header_value("anthropic-version"), Some("2023-06-01"));
    assert!(r.header_value("authorization").is_none());
}

#[test]
fn infer_and_ids() {
    assert_eq!(Kind::infer("https://api.anthropic.com/v1/messages"), Kind::Anthropic);
    assert_eq!(Kind::infer("https://openrouter.ai/api/v1/chat/completions"), Kind::OpenRouter);
    assert_eq!(Kind::infer("https://api.deepseek.com/chat/completions"), Kind::DeepSeek);
    assert_eq!(Kind::infer("http://localhost:8080/v1"), Kind::OpenAi);
    for id in PROVIDER_ORDER {
        assert_eq!(Kind::from_id(id).unwrap().id(), id);
    }
}

const OPENAI_SSE: &str = concat!(
    ": keep-alive\n\n",
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"lo, w\\u00f6rld\"}}]}\r\n\r\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search_\",\"arguments\":\"{\\\"que\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"book\",\"arguments\":\"ry\\\":\\\"Verona\\\"}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15,\"cost\":0.25}}\n\n",
    "data: [DONE]\n\n",
);

#[test]
fn openai_stream_through_ink_net_sse() {
    let client = CannedClient::new(200, OPENAI_SSE);
    let p = Profile::new(Kind::OpenRouter, "https://openrouter.ai/api/v1/chat/completions", "m", "k");
    let (acc, deltas) = run_sse(&client, &p, &req());
    assert_eq!(deltas.concat(), "Hello, wörld");
    assert!(deltas.len() >= 2);
    let r = acc.finish().unwrap();
    assert_eq!(r.text.as_deref(), Some("Hello, wörld"));
    assert_eq!(r.tool_calls, vec![ToolCall { id: "call_1".into(), name: "search_book".into(), arguments: json!({"query": "Verona"}) }]);
    assert_eq!(r.usage, Some(Usage { input_tokens: 10, output_tokens: 5, total_tokens: 15, cost_usd: Some(0.25) }));
    // Request went out with the streaming body.
    let sent = client.last_body();
    assert_eq!(sent["stream"], true);
}

#[test]
fn openai_cost_only_for_openrouter_and_errors() {
    let p = Profile::new(Kind::OpenAi, "https://api.openai.com/v1/chat/completions", "m", "k");
    let (acc, _) = run_sse(&CannedClient::new(200, OPENAI_SSE), &p, &req());
    assert_eq!(acc.finish().unwrap().usage.unwrap().cost_usd, None);

    let mut a = StreamAccumulator::new(Kind::OpenAi);
    a.feed(r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"length"}]}"#).unwrap();
    assert_eq!(a.finish().unwrap_err().message, "The AI service stopped because its output limit was reached.");
    let mut a = StreamAccumulator::new(Kind::OpenAi);
    assert_eq!(a.feed(r#"{"error":{"message":"bad key"}}"#).unwrap_err(), "AI provider error: bad key");
    let mut a = StreamAccumulator::new(Kind::OpenAi);
    assert!(a.feed("not json").is_err());
    assert_eq!(StreamAccumulator::new(Kind::OpenAi).finish().unwrap_err().message, "The AI service returned an invalid streaming response.");
    let mut a = StreamAccumulator::new(Kind::OpenAi);
    a.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"toc","arguments":"{bad"}}]}}]}"#).unwrap();
    assert_eq!(a.finish().unwrap_err().message, "The AI service returned malformed tool arguments.");
}

#[test]
fn deepseek_reasoning_state() {
    let mut a = StreamAccumulator::new(Kind::DeepSeek);
    a.feed(r#"{"choices":[{"delta":{"reasoning_content":"th"}}]}"#).unwrap();
    a.feed(r#"{"choices":[{"delta":{"reasoning_content":"ink"}}]}"#).unwrap();
    assert!(a.reasoning());
    a.feed(r#"{"choices":[{"delta":{"content":"A"},"finish_reason":"stop"}]}"#).unwrap();
    let r = a.finish().unwrap();
    assert_eq!(r.provider_state.unwrap()["reasoning_content"], "think");
}

const ANTHROPIC_SSE: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"cache_read_input_tokens\":3,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: ping\ndata: {\"type\":\"ping\"}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Two house\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"holds\"}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_around\",\"input\":{}}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"page\\\":\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"3}\"}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

#[test]
fn anthropic_stream_through_ink_net_sse() {
    let p = Profile::new(Kind::Anthropic, "https://api.anthropic.com/v1/messages", "m", "k");
    let (acc, deltas) = run_sse(&CannedClient::new(200, ANTHROPIC_SSE), &p, &req());
    assert_eq!(deltas.concat(), "Two households");
    assert_eq!(acc.tool_names(), ["read_around"]);
    let r = acc.finish().unwrap();
    assert_eq!(r.tool_calls, vec![ToolCall { id: "toolu_1".into(), name: "read_around".into(), arguments: json!({"page": 3}) }]);
    assert_eq!(r.usage, Some(Usage { input_tokens: 10, output_tokens: 20, total_tokens: 30, cost_usd: None }));

    let mut a = StreamAccumulator::new(Kind::Anthropic);
    assert_eq!(a.feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#).unwrap_err(), "AI provider error: Overloaded");
    let mut a = StreamAccumulator::new(Kind::Anthropic);
    a.feed(r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#).unwrap();
    assert_eq!(a.finish().unwrap_err().message, "The AI service stopped because the request was refused.");
}

#[test]
fn whole_responses_and_http_errors() {
    let r = parse_whole(
        Kind::OpenAi,
        br#"{"choices":[{"message":{"content":"Hi","tool_calls":[{"id":"a","function":{"name":"toc","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#,
    )
    .unwrap();
    assert_eq!(r.text.as_deref(), Some("Hi"));
    assert_eq!(r.tool_calls[0].name, "toc");
    assert_eq!(r.usage.unwrap().total_tokens, 3);
    let r = parse_whole(Kind::Anthropic, br#"{"content":[{"type":"text","text":"A"},{"type":"tool_use","id":"t","name":"toc","input":{"x":1}}],"stop_reason":"tool_use","usage":{"input_tokens":4,"output_tokens":6}}"#).unwrap();
    assert_eq!((r.text.as_deref(), r.tool_calls[0].arguments.clone(), r.usage.unwrap().total_tokens), (Some("A"), json!({"x": 1}), 10));
    assert_eq!(parse_whole(Kind::OpenAi, b"<html>").unwrap_err().message, "The AI service returned invalid JSON.");
    assert_eq!(http_error(Kind::OpenAi, 401, br#"{"error":{"message":"Incorrect API key"}}"#), "AI service HTTP 401: Incorrect API key");
    assert_eq!(http_error(Kind::Anthropic, 529, b"oops"), "AI service HTTP 529: request failed");
}

/// The real HTTP client against ink-net's mock server: headers and JSON body as sent.
#[test]
fn real_http_request_to_mock_echo() {
    let server = TestServer::start().unwrap();
    let p = Profile::new(Kind::Anthropic, &server.url("/echo"), "claude-test", "sk-ant-fake-key");
    let r = build_request(&p, &req(), false).unwrap();
    let resp = UreqClient::new().send(r).unwrap();
    assert_eq!(resp.status, 200);
    let echo: Value = resp.json(1 << 20).unwrap();
    let body: Value = serde_json::from_str(echo["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["model"], "claude-test");
    assert_eq!(echo["method"], "POST");
    let rec = server.state.requests_to("/echo");
    assert_eq!(rec[0].header("x-api-key"), Some("sk-ant-fake-key"));
    assert_eq!(rec[0].header("anthropic-version"), Some("2023-06-01"));
    assert!(rec[0].header("content-type").unwrap().starts_with("application/json"));
}
