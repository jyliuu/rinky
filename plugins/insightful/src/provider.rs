//! AI providers: OpenAI-compatible Chat Completions (OpenAI, DeepSeek, OpenRouter) and
//! Anthropic Messages, with streaming (server-sent events).
//!
//! Port of `providers/openai_compatible.lua`, `providers/anthropic.lua` and the variant
//! files of insightful.koplugin v0.7.0. The error strings are the same as there.
//!
//! The module is pure: [`build_request`] makes an [`ink_net::Request`], the plugin starts
//! it with `StreamMode::Sse`, feeds each event's `data` to a [`StreamAccumulator`] and
//! calls [`StreamAccumulator::finish`] on `Done`. Without streaming, [`parse_whole`]
//! reads the body.

use ink_net::Request;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

/// Which wire protocol a provider speaks, with its variant fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// OpenAI Chat Completions.
    OpenAi,
    /// DeepSeek (Chat Completions, `reasoning_content`).
    DeepSeek,
    /// OpenRouter (Chat Completions, `reasoning`, `reasoning_details`, cost in usage).
    OpenRouter,
    /// Anthropic Messages.
    Anthropic,
}

/// Provider ids in menu order.
pub const PROVIDER_ORDER: [&str; 4] = ["openai", "deepseek", "openrouter", "anthropic"];

impl Kind {
    /// From a provider id (`openai`, `deepseek`, `openrouter`, `anthropic`).
    pub fn from_id(id: &str) -> Option<Kind> {
        match id.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Kind::OpenAi),
            "deepseek" => Some(Kind::DeepSeek),
            "openrouter" => Some(Kind::OpenRouter),
            "anthropic" => Some(Kind::Anthropic),
            _ => None,
        }
    }

    /// Guess from a base URL (same rules as `providers/registry.lua`).
    pub fn infer(base_url: &str) -> Kind {
        let u = base_url.trim().to_ascii_lowercase();
        if u.contains("api.anthropic.com") {
            Kind::Anthropic
        } else if u.contains("openrouter.ai") {
            Kind::OpenRouter
        } else if u.contains("api.deepseek.com") {
            Kind::DeepSeek
        } else {
            Kind::OpenAi
        }
    }

    /// The provider id.
    pub fn id(self) -> &'static str {
        match self {
            Kind::OpenAi => "openai",
            Kind::DeepSeek => "deepseek",
            Kind::OpenRouter => "openrouter",
            Kind::Anthropic => "anthropic",
        }
    }

    fn max_tokens_field(self) -> &'static str {
        match self {
            Kind::DeepSeek | Kind::Anthropic => "max_tokens",
            Kind::OpenAi | Kind::OpenRouter => "max_completion_tokens",
        }
    }

    fn state_fields(self) -> &'static [&'static str] {
        match self {
            Kind::DeepSeek => &["reasoning_content"],
            Kind::OpenRouter => &["reasoning", "reasoning_details"],
            Kind::OpenAi | Kind::Anthropic => &[],
        }
    }
}

/// One resolved provider profile (common settings merged in).
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Wire protocol.
    pub kind: Kind,
    /// Endpoint URL (full path, for example `.../v1/chat/completions`).
    pub base_url: String,
    /// Model name.
    pub model: String,
    /// Secret key.
    pub api_key: String,
    /// Request timeout in seconds.
    pub timeout: u64,
    /// Stream answers.
    pub stream: bool,
    /// Output token limit (`max_completion_tokens` / `max_tokens`).
    pub max_tokens: Option<u64>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Extra headers.
    pub headers: BTreeMap<String, String>,
    /// Extra body fields.
    pub parameters: Map<String, Value>,
    /// `anthropic-version` header (default `2023-06-01`).
    pub anthropic_version: Option<String>,
}

impl Profile {
    /// A profile with defaults (timeout 60, streaming on).
    pub fn new(kind: Kind, base_url: &str, model: &str, api_key: &str) -> Profile {
        Profile {
            kind,
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            timeout: 60,
            stream: true,
            max_tokens: None,
            temperature: None,
            headers: BTreeMap::new(),
            parameters: Map::new(),
            anthropic_version: None,
        }
    }
}

/// A tool call made by the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// Call id (generated when the provider gave none).
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments (a JSON object).
    pub arguments: Value,
}

/// One message of the request (provider-neutral).
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// User text (already rendered with the prompt templates).
    User(String),
    /// Assistant text, with tool calls when the model asked for tools.
    Assistant {
        /// Text.
        text: String,
        /// Tool calls.
        tool_calls: Vec<ToolCall>,
        /// Provider state (reasoning fields) to send back.
        state: Option<Map<String, Value>>,
    },
    /// Result of one tool call.
    Tool {
        /// Call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Result (sent as JSON text).
        content: Value,
    },
}

/// A request to a provider.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    /// System prompt.
    pub system: String,
    /// Conversation.
    pub messages: Vec<Msg>,
    /// Tool schemas: objects with `name`, `description`, `parameters`.
    pub tools: Vec<Value>,
}

/// Token use (and OpenRouter's cost) of one response.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Total tokens.
    pub total_tokens: u64,
    /// Cost in USD when the provider reports it.
    pub cost_usd: Option<f64>,
}

/// A complete model turn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    /// Text, when any.
    pub text: Option<String>,
    /// Tool calls (in index order).
    pub tool_calls: Vec<ToolCall>,
    /// Provider state to send back with the tool calls.
    pub provider_state: Option<Map<String, Value>>,
    /// Usage, when reported.
    pub usage: Option<Usage>,
}

/// A failed turn: the message and the usage seen before the failure.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// User-facing message.
    pub message: String,
    /// Usage, when reported.
    pub usage: Option<Usage>,
}

impl Failure {
    fn new(message: impl Into<String>, usage: Option<Usage>) -> Failure {
        Failure { message: message.into(), usage }
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn tok(v: Option<f64>) -> u64 {
    v.map(|x| x.max(0.0).floor() as u64).unwrap_or(0)
}

fn compatible_usage(raw: &Value, with_cost: bool) -> Option<Usage> {
    let raw = raw.as_object()?;
    let get = |k: &str| raw.get(k).and_then(num);
    let (i, o, t) = (get("prompt_tokens"), get("completion_tokens"), get("total_tokens"));
    let cost = if with_cost { get("cost") } else { None };
    if i.is_none() && o.is_none() && t.is_none() && cost.is_none() {
        return None;
    }
    let (input, output) = (tok(i), tok(o));
    let total = t.map(|_| tok(t)).unwrap_or(input + output);
    Some(Usage { input_tokens: input, output_tokens: output, total_tokens: total, cost_usd: cost.map(|c| c.max(0.0)) })
}

fn anthropic_usage(raw: &Value) -> Option<Usage> {
    let raw = raw.as_object()?;
    let get = |k: &str| raw.get(k).and_then(num);
    let (u, cc, cr, o) = (get("input_tokens"), get("cache_creation_input_tokens"), get("cache_read_input_tokens"), get("output_tokens"));
    if u.is_none() && cc.is_none() && cr.is_none() && o.is_none() {
        return None;
    }
    let input = tok(u) + tok(cc) + tok(cr);
    let output = tok(o);
    Some(Usage { input_tokens: input, output_tokens: output, total_tokens: input + output, cost_usd: None })
}

fn finish_error(reason: &str) -> Option<&'static str> {
    match reason {
        "length" => Some("The AI service stopped because its output limit was reached."),
        "content_filter" => Some("The AI service stopped because content was filtered."),
        "insufficient_system_resource" => Some("The AI service stopped because inference resources were unavailable."),
        "error" => Some("The AI service stopped because the upstream provider failed."),
        _ => None,
    }
}

fn stop_error(reason: &str) -> Option<&'static str> {
    match reason {
        "max_tokens" => Some("The AI service stopped because its output limit was reached."),
        "model_context_window_exceeded" => Some("The AI service stopped because its context window was exceeded."),
        "refusal" => Some("The AI service stopped because the request was refused."),
        _ => None,
    }
}

/// The `error` object's message of a provider response, when present.
fn provider_error(decoded: &Value, anthropic: bool) -> Option<String> {
    let err = decoded.get("error")?.as_object()?;
    let pick = |k: &str| err.get(k).filter(|v| !v.is_null()).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
    let m = if anthropic { pick("message").or_else(|| pick("type")) } else { pick("message").or_else(|| pick("type")).or_else(|| pick("code")) };
    Some(m.unwrap_or_else(|| "provider error".into()))
}

fn json_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn decode_arguments(raw: &str) -> Result<Value, String> {
    if raw.is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => Ok(v),
        _ => Err("The AI service returned malformed tool arguments.".into()),
    }
}

fn wire_compatible(kind: Kind, req: &ChatRequest) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": req.system})];
    for m in &req.messages {
        match m {
            Msg::User(t) => out.push(json!({"role": "user", "content": t})),
            Msg::Assistant { text, tool_calls, state } if !tool_calls.is_empty() => {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments.to_string()}}))
                    .collect();
                let mut a = json!({"role": "assistant", "content": text, "tool_calls": calls});
                if let Some(st) = state {
                    for f in kind.state_fields() {
                        if let Some(v) = st.get(*f) {
                            a[*f] = v.clone();
                        }
                    }
                }
                out.push(a);
            }
            Msg::Assistant { text, .. } => out.push(json!({"role": "assistant", "content": text})),
            Msg::Tool { call_id, content, .. } => out.push(json!({"role": "tool", "tool_call_id": call_id, "content": json_text(content)})),
        }
    }
    out
}

fn wire_anthropic(req: &ChatRequest) -> Vec<Value> {
    let mut out = Vec::new();
    for m in &req.messages {
        match m {
            Msg::User(t) => out.push(json!({"role": "user", "content": t})),
            Msg::Assistant { text, tool_calls, .. } if !tool_calls.is_empty() => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                for c in tool_calls {
                    content.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments}));
                }
                out.push(json!({"role": "assistant", "content": content}));
            }
            Msg::Assistant { text, .. } => out.push(json!({"role": "assistant", "content": text})),
            Msg::Tool { call_id, content, .. } => {
                out.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call_id, "content": json_text(content)}]}))
            }
        }
    }
    out
}

/// The JSON body for `req` (with `stream` set when streaming).
pub fn request_body(p: &Profile, req: &ChatRequest, stream: bool) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(p.model));
    if p.kind == Kind::Anthropic {
        body.insert("system".into(), json!(req.system));
        body.insert("messages".into(), Value::Array(wire_anthropic(req)));
        let tools: Vec<Value> = req.tools.iter().map(|t| json!({"name": t["name"], "description": t["description"], "input_schema": t["parameters"]})).collect();
        body.insert("tools".into(), Value::Array(tools));
        // Anthropic's Messages API requires this field; it has no omitted default.
        body.insert("max_tokens".into(), json!(p.max_tokens.unwrap_or(8192)));
    } else {
        body.insert("messages".into(), Value::Array(wire_compatible(p.kind, req)));
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t["name"], "description": t["description"], "parameters": t["parameters"]}}))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if let Some(n) = p.max_tokens {
            body.insert(p.kind.max_tokens_field().into(), json!(n));
        }
    }
    if let Some(t) = p.temperature {
        body.insert("temperature".into(), json!(t));
    }
    for (k, v) in &p.parameters {
        body.insert(k.clone(), v.clone());
    }
    if stream {
        body.insert("stream".into(), json!(true));
        if p.kind != Kind::Anthropic {
            let so = body.entry("stream_options").or_insert_with(|| json!({}));
            if !so.is_object() {
                *so = json!({});
            }
            so["include_usage"] = json!(true);
        }
    }
    Value::Object(body)
}

/// Checks the profile and builds the HTTP request.
pub fn build_request(p: &Profile, req: &ChatRequest, stream: bool) -> Result<Request, String> {
    if p.api_key.trim().is_empty() {
        return Err("Insightful is not configured: API key is missing.".into());
    }
    if p.base_url.trim().is_empty() || p.model.trim().is_empty() {
        return Err("Insightful is not configured: base_url or model is missing.".into());
    }
    let body = request_body(p, req, stream);
    let mut r = Request::post_json(p.base_url.trim(), &body)
        .set_header("Accept", if stream { "text/event-stream" } else { "application/json" })
        .timeout(Duration::from_secs(p.timeout.max(1)));
    if p.kind == Kind::Anthropic {
        r = r.set_header("x-api-key", &p.api_key).set_header("anthropic-version", p.anthropic_version.as_deref().unwrap_or("2023-06-01"));
    } else {
        r = r.set_header("Authorization", &format!("Bearer {}", p.api_key));
    }
    for (k, v) in &p.headers {
        r = r.set_header(k, v);
    }
    Ok(r)
}

/// Message for a non-2xx status (`AI service HTTP 401: ...`).
pub fn http_error(kind: Kind, status: u16, body: &[u8]) -> String {
    let msg = serde_json::from_slice::<Value>(body).ok().and_then(|v| provider_error(&v, kind == Kind::Anthropic));
    format!("AI service HTTP {status}: {}", msg.unwrap_or_else(|| "request failed".into()))
}

#[derive(Default, Debug)]
struct PartialCall {
    id: String,
    name: String,
    input: Option<Value>,
    args: String,
}

/// Collects a streamed answer from the `data` of each server-sent event.
#[derive(Debug)]
pub struct StreamAccumulator {
    kind: Kind,
    text: String,
    state: Map<String, Value>,
    calls: BTreeMap<i64, PartialCall>,
    events: usize,
    finish: Option<String>,
    usage: Option<Usage>,
    error: Option<String>,
    reasoning: bool,
}

impl StreamAccumulator {
    /// A new accumulator for `kind`.
    pub fn new(kind: Kind) -> StreamAccumulator {
        StreamAccumulator {
            kind,
            text: String::new(),
            state: Map::new(),
            calls: BTreeMap::new(),
            events: 0,
            finish: None,
            usage: None,
            error: None,
            reasoning: false,
        }
    }

    /// Text received so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True once reasoning (thinking) activity was seen.
    pub fn reasoning(&self) -> bool {
        self.reasoning
    }

    /// Names of the tool calls seen so far.
    pub fn tool_names(&self) -> Vec<String> {
        self.calls.values().map(|c| c.name.clone()).collect()
    }

    /// Feeds one event's data. Returns the new text (possibly empty), or the error that
    /// ends the stream.
    pub fn feed(&mut self, data: &str) -> Result<String, String> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let before = self.text.len();
        if self.kind == Kind::Anthropic {
            self.feed_anthropic(data);
        } else {
            self.feed_compatible(data);
        }
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(self.text[before..].to_string()),
        }
    }

    fn feed_compatible(&mut self, data: &str) {
        if data == "[DONE]" {
            return;
        }
        let Ok(d) = serde_json::from_str::<Value>(data) else {
            self.error = Some("The AI service returned an invalid streaming response.".into());
            return;
        };
        if !d.is_object() {
            self.error = Some("The AI service returned an invalid streaming response.".into());
            return;
        }
        if let Some(e) = provider_error(&d, false) {
            self.error = Some(format!("AI provider error: {e}"));
            return;
        }
        self.events += 1;
        if let Some(u) = d.get("usage").and_then(|u| compatible_usage(u, self.kind == Kind::OpenRouter)) {
            self.usage = Some(u);
        }
        let Some(choice) = d.get("choices").and_then(|c| c.get(0)).filter(|c| c.is_object()) else { return };
        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(r.into());
        }
        let Some(delta) = choice.get("delta").and_then(Value::as_object) else { return };
        for f in self.kind.state_fields() {
            match delta.get(*f) {
                Some(Value::String(s)) => {
                    let e = self.state.entry(f.to_string()).or_insert_with(|| json!(""));
                    if let Value::String(prev) = e {
                        prev.push_str(s);
                    } else {
                        *e = json!(s);
                    }
                    self.reasoning = true;
                }
                Some(Value::Array(items)) => {
                    let e = self.state.entry(f.to_string()).or_insert_with(|| json!([]));
                    if !e.is_array() {
                        *e = json!([]);
                    }
                    e.as_array_mut().expect("array").extend(items.iter().cloned());
                    self.reasoning = true;
                }
                Some(Value::Null) | None => {}
                Some(_) => self.reasoning = true,
            }
        }
        if let Some(c) = delta.get("content").and_then(Value::as_str) {
            self.text.push_str(c);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (seq, raw) in calls.iter().enumerate() {
                if !raw.is_object() {
                    continue;
                }
                let index = raw.get("index").and_then(num).map(|x| x as i64).unwrap_or(seq as i64);
                let call = self.calls.entry(index).or_default();
                if let Some(id) = raw.get("id").and_then(Value::as_str) {
                    call.id.push_str(id);
                }
                if let Some(f) = raw.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        call.name.push_str(n);
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        call.args.push_str(a);
                    }
                }
            }
        }
    }

    fn feed_anthropic(&mut self, data: &str) {
        let Ok(d) = serde_json::from_str::<Value>(data) else {
            self.error = Some("The AI service returned an invalid Anthropic stream.".into());
            return;
        };
        if !d.is_object() {
            self.error = Some("The AI service returned an invalid Anthropic stream.".into());
            return;
        }
        self.events += 1;
        let index = d.get("index").and_then(num).map(|x| x as i64).unwrap_or(0);
        match d.get("type").and_then(Value::as_str).unwrap_or("") {
            "error" => {
                let m = provider_error(&d, true).unwrap_or_else(|| "streaming request failed".into());
                self.error = Some(format!("AI provider error: {m}"));
            }
            "message_start" => {
                if let Some(u) = d.get("message").and_then(|m| m.get("usage")).and_then(anthropic_usage) {
                    self.usage = Some(u);
                }
            }
            "content_block_start" => {
                let b = &d["content_block"];
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let input = b.get("input").filter(|v| v.is_object()).cloned();
                        self.calls.insert(
                            index,
                            PartialCall {
                                id: b.get("id").and_then(Value::as_str).unwrap_or("").into(),
                                name: b.get("name").and_then(Value::as_str).unwrap_or("").into(),
                                input,
                                args: String::new(),
                            },
                        );
                    }
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            self.text.push_str(t);
                        }
                    }
                    Some("thinking") => self.reasoning = true,
                    _ => {}
                }
            }
            "content_block_delta" => {
                let dl = &d["delta"];
                match dl.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = dl.get("text").and_then(Value::as_str) {
                            self.text.push_str(t);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(c) = self.calls.get_mut(&index) {
                            c.args.push_str(dl.get("partial_json").and_then(Value::as_str).unwrap_or(""));
                        }
                    }
                    Some("thinking_delta") => self.reasoning = true,
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(r) = d["delta"].get("stop_reason").and_then(Value::as_str) {
                    self.finish = Some(r.into());
                }
                if let Some(du) = d.get("usage").and_then(anthropic_usage) {
                    let start_in = self.usage.map(|u| u.input_tokens).unwrap_or(0);
                    let input = if du.input_tokens > 0 { du.input_tokens } else { start_in };
                    self.usage = Some(Usage { input_tokens: input, output_tokens: du.output_tokens, total_tokens: input + du.output_tokens, cost_usd: None });
                }
            }
            _ => {}
        }
    }

    /// Ends the stream: the complete turn, or a failure.
    pub fn finish(self) -> Result<Reply, Failure> {
        let usage = self.usage;
        if let Some(e) = self.error {
            return Err(Failure::new(e, usage));
        }
        let anthropic = self.kind == Kind::Anthropic;
        if self.events == 0 {
            let m = if anthropic { "The AI service returned an invalid Anthropic stream." } else { "The AI service returned an invalid streaming response." };
            return Err(Failure::new(m, usage));
        }
        if let Some(r) = &self.finish {
            if let Some(m) = if anthropic { stop_error(r) } else { finish_error(r) } {
                return Err(Failure::new(m, usage));
            }
        }
        let mut calls = Vec::new();
        for (_, c) in self.calls {
            if !anthropic && c.name.is_empty() {
                return Err(Failure::new("The AI service returned an invalid tool call.", usage));
            }
            let arguments = if c.args.is_empty() { c.input.unwrap_or_else(|| json!({})) } else { decode_arguments(&c.args).map_err(|m| Failure::new(m, usage))? };
            calls.push(ToolCall { id: c.id, name: c.name, arguments });
        }
        let state = (!self.state.is_empty()).then_some(self.state);
        Ok(Reply { text: (!self.text.is_empty()).then_some(self.text), tool_calls: calls, provider_state: state, usage })
    }
}

/// Reads a whole (non-streamed) 2xx response body.
pub fn parse_whole(kind: Kind, body: &[u8]) -> Result<Reply, Failure> {
    let Ok(d) = serde_json::from_slice::<Value>(body) else {
        return Err(Failure::new("The AI service returned invalid JSON.", None));
    };
    if !d.is_object() {
        return Err(Failure::new("The AI service returned invalid JSON.", None));
    }
    if kind == Kind::Anthropic {
        let usage = d.get("usage").and_then(anthropic_usage);
        if let Some(e) = provider_error(&d, true) {
            return Err(Failure::new(format!("AI provider error: {e}"), usage));
        }
        if let Some(m) = d.get("stop_reason").and_then(Value::as_str).and_then(stop_error) {
            return Err(Failure::new(m, usage));
        }
        let (mut text, mut calls) = (String::new(), Vec::new());
        for b in d.get("content").and_then(Value::as_array).into_iter().flatten() {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(b.get("text").and_then(Value::as_str).unwrap_or("")),
                Some("tool_use") => calls.push(ToolCall {
                    id: b.get("id").and_then(Value::as_str).unwrap_or("").into(),
                    name: b.get("name").and_then(Value::as_str).unwrap_or("").into(),
                    arguments: b.get("input").filter(|v| v.is_object()).cloned().unwrap_or_else(|| json!({})),
                }),
                _ => {}
            }
        }
        return Ok(Reply { text: (!text.is_empty()).then_some(text), tool_calls: calls, provider_state: None, usage });
    }
    let usage = d.get("usage").and_then(|u| compatible_usage(u, kind == Kind::OpenRouter));
    if let Some(e) = provider_error(&d, false) {
        return Err(Failure::new(format!("AI provider error: {e}"), usage));
    }
    let choice = d.get("choices").and_then(|c| c.get(0));
    let Some(msg) = choice.and_then(|c| c.get("message")).filter(|m| m.is_object()) else {
        return Err(Failure::new("The AI service returned an invalid response.", usage));
    };
    if let Some(m) = choice.and_then(|c| c.get("finish_reason")).and_then(Value::as_str).and_then(finish_error) {
        return Err(Failure::new(m, usage));
    }
    let mut calls = Vec::new();
    for raw in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = raw.get("function").and_then(|f| f.get("name")).and_then(Value::as_str) else {
            return Err(Failure::new("The AI service returned an invalid tool call.", usage));
        };
        let arguments = match raw["function"].get("arguments") {
            Some(Value::String(s)) => decode_arguments(s).map_err(|m| Failure::new(m, usage))?,
            Some(v @ Value::Object(_)) => v.clone(),
            _ => json!({}),
        };
        calls.push(ToolCall { id: raw.get("id").and_then(Value::as_str).unwrap_or("").into(), name: name.into(), arguments });
    }
    let text = match msg.get("content") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(parts)) => {
            let t: String = parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect();
            (!t.is_empty()).then_some(t)
        }
        _ => None,
    };
    let mut state = Map::new();
    for f in kind.state_fields() {
        if let Some(v) = msg.get(*f).filter(|v| !v.is_null()) {
            state.insert(f.to_string(), v.clone());
        }
    }
    Ok(Reply { text, tool_calls: calls, provider_state: (!state.is_empty()).then_some(state), usage })
}

#[cfg(test)]
mod tests;
