//! Model catalog: the provider's model list for the model menu (`model_catalog.lua`).
//!
//! Only DeepSeek (`<origin>/models`) and OpenRouter (`<origin>/api/v1/models?...`) have
//! a list. The answer is `{"data": [{"id", "name"}, ...]}`.

use ink_net::Request;
use serde_json::Value;
use std::time::Duration;

/// Models shown in the menu at most.
pub const LIMIT: usize = 50;
/// Size limit of the model list body (OpenRouter's full list is about 1 MB).
pub const BODY_LIMIT: usize = 4 << 20;

/// True when `provider` has a model list.
pub fn supports(provider: &str) -> bool {
    matches!(provider, "deepseek" | "openrouter")
}

/// `scheme://host[:port]` of `url`.
fn origin(url: &str) -> Option<&str> {
    let url = url.trim();
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(&url[..url.len() - rest.len() + host.len()])
}

/// The list URL for `provider` with endpoint `base_url`, or `None` without a list.
/// `query` is sent to OpenRouter only (DeepSeek's list is filtered locally).
pub fn url(provider: &str, base_url: &str, query: Option<&str>) -> Option<String> {
    let base = origin(base_url)?;
    match provider {
        "deepseek" => Some(format!("{base}/models")),
        "openrouter" => {
            let mut u = format!("{base}/api/v1/models?output_modalities=text&supported_parameters=tools&sort=most-popular");
            if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty()) {
                u.push_str("&q=");
                u.push_str(&ink_net::url::encode_component(q));
            }
            Some(u)
        }
        _ => None,
    }
}

/// The GET for the list. The key goes into a header only (never logged).
pub fn request(url: &str, api_key: &str, timeout: u64) -> Request {
    Request::get(url).set_header("Accept", "application/json").set_header("Authorization", &format!("Bearer {}", api_key.trim())).timeout(Duration::from_secs(timeout.max(1)))
}

/// Model ids from a list body: `current` first, no duplicates, only ids or names that
/// contain `query` (case-insensitive), at most [`LIMIT`]. The flag is true when more
/// models matched than are returned.
pub fn parse(body: &[u8], current: &str, query: Option<&str>) -> Result<(Vec<String>, bool), String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "The model service returned invalid JSON.".to_string())?;
    let Some(data) = v.get("data").and_then(Value::as_array) else {
        return Err("The model service returned invalid JSON.".into());
    };
    let q = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
    let matches = |id: &str, name: &str| q.as_deref().is_none_or(|q| id.to_lowercase().contains(q) || name.to_lowercase().contains(q));
    let mut ids: Vec<String> = Vec::new();
    let mut more = false;
    let mut add = |id: &str, name: &str| {
        let id = id.trim();
        if id.is_empty() || ids.iter().any(|i| i == id) || !matches(id, name) {
            return;
        }
        if ids.len() >= LIMIT {
            more = true;
            return;
        }
        ids.push(id.to_string());
    };
    add(current, current);
    for m in data {
        let id = m.get("id").and_then(Value::as_str).unwrap_or("");
        let name = m.get("name").and_then(Value::as_str).unwrap_or("");
        add(id, name);
    }
    Ok((ids, more))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(url("deepseek", "https://api.deepseek.com/chat/completions", Some("x")).unwrap(), "https://api.deepseek.com/models");
        assert_eq!(
            url("openrouter", "https://openrouter.ai/api/v1/chat/completions", Some(" claude 4/x ")).unwrap(),
            "https://openrouter.ai/api/v1/models?output_modalities=text&supported_parameters=tools&sort=most-popular&q=claude%204%2Fx"
        );
        assert!(url("openrouter", "https://openrouter.ai/api/v1/chat/completions", Some("")).unwrap().ends_with("sort=most-popular"));
        assert_eq!(url("openai", "https://api.openai.com/v1/chat/completions", None), None);
        assert_eq!(url("deepseek", "api.deepseek.com", None), None);
        assert_eq!(origin("http://localhost:8080/v1"), Some("http://localhost:8080"));
    }

    #[test]
    fn parses_filters_and_limits() {
        let body = br#"{"data":[{"id":"b","name":"Bee"},{"id":"a"},{"id":"b"},{"id":"Claude-x","name":"Anthropic"},{"id":""}]}"#;
        assert_eq!(parse(body, "a", None).unwrap(), (vec!["a".to_string(), "b".into(), "Claude-x".into()], false));
        assert_eq!(parse(body, "a", Some("ANTHRO")).unwrap().0, ["Claude-x"]);
        assert_eq!(parse(body, "a", Some("bee")).unwrap().0, ["b"], "the name matches too");
        let many = serde_json::json!({"data": (0..80).map(|i| serde_json::json!({"id": format!("m{i}")})).collect::<Vec<_>>()});
        let (ids, more) = parse(many.to_string().as_bytes(), "m5", None).unwrap();
        assert_eq!((ids.len(), ids[0].as_str(), ids[1].as_str(), more), (LIMIT, "m5", "m0", true));
        assert!(parse(b"{}", "a", None).is_err());
        assert!(parse(b"<html>", "a", None).is_err());
    }
}
