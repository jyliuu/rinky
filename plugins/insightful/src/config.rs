//! Settings: `insightful.toml`, and a one-time converter from KOReader's
//! `insightful.koplugin/configuration.lua` provider table.
//!
//! ```toml
//! provider = "openai"          # default profile
//! timeout = 60                 # seconds, shared by all profiles
//! stream = true
//!
//! [providers.openai]
//! base_url = "https://api.openai.com/v1/chat/completions"
//! model = "gpt-4.1-mini"
//! api_key = "sk-..."
//! # max_tokens, max_completion_tokens, temperature, anthropic_version,
//! # [providers.openai.headers], [providers.openai.parameters]
//! ```
//!
//! Profile selection follows `provider_profiles.lua`: only profiles with a key are
//! offered, in the order openai, deepseek, openrouter, anthropic; a requested profile
//! that is not offered falls back to `provider`, then to the first offered one.

use crate::provider::{Kind, Profile, PROVIDER_ORDER};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Where KOReader keeps the plugin's settings on the Kindle (read once, never written).
pub const KOREADER_CONFIGURATION: &str = "/mnt/us/koreader/plugins/insightful.koplugin/configuration.lua";
/// File name of the settings in the plugin's data folder.
pub const CONFIG_FILE: &str = "insightful.toml";

/// One provider profile as stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileConfig {
    /// Endpoint URL.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// Model.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Secret key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// Output token limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Output token limit (OpenAI name; used when `max_tokens` is unset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
    /// Temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// `anthropic-version` header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_version: Option<String>,
    /// Extra headers.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Extra body fields.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub parameters: Map<String, Value>,
}

/// The whole settings file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default provider id.
    pub provider: String,
    /// Timeout in seconds.
    pub timeout: u64,
    /// Stream answers.
    pub stream: bool,
    /// Model chosen in the menu, per provider (overrides the profile's model).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
    /// Profiles by provider id.
    pub providers: BTreeMap<String, ProfileConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config { provider: "openai".into(), timeout: 60, stream: true, models: BTreeMap::new(), providers: BTreeMap::new() }
    }
}

impl Config {
    /// Parses `insightful.toml`.
    pub fn from_toml(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| format!("{CONFIG_FILE}: {e}"))
    }

    /// The TOML text.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Writes the file through `<name>.tmp` and a rename, so a failed write leaves the
    /// old file (it holds the API keys). The temp file gets the old file's permissions.
    /// Comments in the old file are not kept.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| format!("{CONFIG_FILE}: {e}"))?;
        if text.trim().is_empty() {
            return Err(format!("{CONFIG_FILE}: nothing to write"));
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        let write = || -> std::io::Result<()> {
            std::fs::write(&tmp, &text)?;
            if let Ok(m) = std::fs::metadata(path) {
                std::fs::set_permissions(&tmp, m.permissions())?;
            }
            std::fs::rename(&tmp, path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
    }

    /// Provider ids that have a key, in menu order.
    pub fn available(&self) -> Vec<&'static str> {
        PROVIDER_ORDER.into_iter().filter(|id| self.providers.get(*id).is_some_and(|p| !p.api_key.trim().is_empty())).collect()
    }

    /// The profile to use for `requested` (or the default), or `None` when no profile
    /// has a key.
    pub fn resolve(&self, requested: Option<&str>) -> Option<Profile> {
        let available = self.available();
        let req = requested.unwrap_or("").trim().to_ascii_lowercase();
        let default = self.provider.trim().to_ascii_lowercase();
        let id = if available.contains(&req.as_str()) {
            req
        } else if available.contains(&default.as_str()) {
            default
        } else {
            available.first()?.to_string()
        };
        let pc = self.providers.get(&id)?;
        let kind = Kind::from_id(&id)?;
        let model = self.models.get(&id).filter(|m| !m.trim().is_empty()).cloned().unwrap_or_else(|| pc.model.clone());
        let mut p = Profile::new(kind, pc.base_url.trim(), model.trim(), pc.api_key.trim());
        p.timeout = self.timeout;
        p.stream = self.stream;
        p.max_tokens = pc.max_tokens.or(pc.max_completion_tokens);
        p.temperature = pc.temperature;
        p.headers = pc.headers.clone();
        p.parameters = pc.parameters.clone();
        p.anthropic_version = pc.anthropic_version.clone();
        Some(p)
    }
}

/// Converts a Lua value into JSON (tables with keys 1..n become arrays). Functions and
/// other non-data values become `null`.
fn lua_to_json(v: &mlua::Value, depth: usize) -> Result<Value, String> {
    if depth > 16 {
        return Err("configuration.lua: tables nested too deeply".into());
    }
    Ok(match v {
        mlua::Value::Nil => Value::Null,
        mlua::Value::Boolean(b) => Value::Bool(*b),
        mlua::Value::Integer(i) => Value::from(*i),
        mlua::Value::Number(n) => serde_json::Number::from_f64(*n).map(Value::Number).unwrap_or(Value::Null),
        mlua::Value::String(s) => Value::String(s.to_str().map(|s| s.to_string()).unwrap_or_default()),
        mlua::Value::Table(t) => {
            let len = t.raw_len();
            let pairs = t.clone().pairs::<mlua::Value, mlua::Value>().count();
            if len > 0 && len == pairs {
                let mut a = Vec::with_capacity(len);
                for i in 1..=len {
                    a.push(lua_to_json(&t.raw_get::<mlua::Value>(i).map_err(|e| e.to_string())?, depth + 1)?);
                }
                Value::Array(a)
            } else {
                let mut m = Map::new();
                for pair in t.clone().pairs::<mlua::Value, mlua::Value>() {
                    let (k, val) = pair.map_err(|e| e.to_string())?;
                    let key = match &k {
                        mlua::Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
                        mlua::Value::Integer(i) => i.to_string(),
                        _ => continue,
                    };
                    m.insert(key, lua_to_json(&val, depth + 1)?);
                }
                Value::Object(m)
            }
        }
        _ => Value::Null,
    })
}

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn u(v: Option<&Value>) -> Option<u64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| *x >= 0.0).map(|x| x as u64),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

fn profile_from(t: &Map<String, Value>) -> ProfileConfig {
    let mut headers = BTreeMap::new();
    if let Some(Value::Object(h)) = t.get("headers") {
        for (k, v) in h {
            if let Value::String(v) = v {
                headers.insert(k.clone(), v.clone());
            }
        }
    }
    ProfileConfig {
        base_url: s(t.get("base_url")),
        model: s(t.get("model")),
        api_key: s(t.get("api_key")),
        max_tokens: u(t.get("max_tokens")),
        max_completion_tokens: u(t.get("max_completion_tokens")),
        temperature: t.get("temperature").and_then(Value::as_f64),
        anthropic_version: t.get("anthropic_version").and_then(Value::as_str).map(str::to_string),
        headers,
        parameters: t.get("parameters").and_then(Value::as_object).cloned().unwrap_or_default(),
    }
}

/// Converts the text of a `configuration.lua` into a [`Config`].
///
/// The file runs in a Lua state without any standard library, with a memory limit, so it
/// can only build tables. Both forms are read: a `providers = { id = {...} }` table, and
/// the older single-profile form with `base_url`/`model`/`api_key` at the top level.
pub fn convert_lua(source: &str) -> Result<Config, String> {
    let lua = mlua::Lua::new_with(mlua::StdLib::NONE, mlua::LuaOptions::default()).map_err(|e| e.to_string())?;
    let _ = lua.set_memory_limit(8 << 20);
    let value: mlua::Value = lua.load(source).set_name("configuration.lua").set_mode(mlua::chunk::ChunkMode::Text).eval().map_err(|e| format!("configuration.lua: {e}"))?;
    let json = lua_to_json(&value, 0)?;
    let Value::Object(top) = json else {
        return Err("configuration.lua: does not return a table".into());
    };
    let mut c = Config::default();
    let provider = s(top.get("provider")).to_ascii_lowercase();
    if let Some(t) = u(top.get("timeout")) {
        c.timeout = t.max(1);
    }
    if let Some(Value::Bool(b)) = top.get("stream") {
        c.stream = *b;
    }
    if let Some(Value::Object(ps)) = top.get("providers") {
        for (id, p) in ps {
            let id = id.to_ascii_lowercase();
            if Kind::from_id(&id).is_some() {
                if let Value::Object(t) = p {
                    c.providers.insert(id, profile_from(t));
                }
            }
        }
    }
    let legacy = profile_from(&top);
    let id = if provider.is_empty() { Kind::infer(&legacy.base_url).id().to_string() } else { provider.clone() };
    if !legacy.api_key.is_empty() && !c.providers.contains_key(&id) && Kind::from_id(&id).is_some() {
        c.providers.insert(id.clone(), legacy);
    }
    c.provider = if Kind::from_id(&id).is_some() { id } else { "openai".into() };
    Ok(c)
}

/// What [`load_or_migrate`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Read `insightful.toml`.
    Toml,
    /// Converted `configuration.lua` and wrote `insightful.toml`.
    Converted,
    /// Neither file exists: defaults.
    Default,
}

/// Reads `toml_path`; when it does not exist and `lua_path` does, converts that once
/// and writes `toml_path`. The Lua file is only read.
pub fn load_or_migrate(toml_path: &Path, lua_path: &Path) -> Result<(Config, Origin), String> {
    match std::fs::read_to_string(toml_path) {
        Ok(t) => return Ok((Config::from_toml(&t)?, Origin::Toml)),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", toml_path.display())),
        Err(_) => {}
    }
    let Ok(src) = std::fs::read_to_string(lua_path) else {
        return Ok((Config::default(), Origin::Default));
    };
    let c = convert_lua(&src)?;
    if let Some(dir) = toml_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    ink_store::write_atomic(toml_path, c.to_toml().as_bytes()).map_err(|e| format!("{}: {e}", toml_path.display()))?;
    Ok((c, Origin::Converted))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fake configuration.lua (same shape as configuration.lua.sample, fake keys).
    const FAKE: &str = r#"
-- comment
return {
    provider = "deepseek",
    timeout = 90,
    stream = false,
    verify_ssl = true,
    providers = {
        openai = { base_url = "https://api.openai.com/v1/chat/completions", model = "gpt-4.1-mini", api_key = "" },
        deepseek = { base_url = "https://api.deepseek.com/chat/completions", model = "deepseek-v4-flash", api_key = "sk-fake-deepseek",
                     headers = { ["X-Test"] = "1" }, parameters = { top_p = 0.9, stop = { "a", "b" } } },
        openrouter = { base_url = "https://openrouter.ai/api/v1/chat/completions", model = "openrouter/auto", api_key = " sk-fake-or " },
        anthropic = { base_url = "https://api.anthropic.com/v1/messages", model = "claude-sonnet-4-5", api_key = "sk-ant-fake", max_tokens = 4096 },
    },
}
"#;

    #[test]
    fn converts_provider_table() {
        let c = convert_lua(FAKE).unwrap();
        assert_eq!((c.provider.as_str(), c.timeout, c.stream), ("deepseek", 90, false));
        assert_eq!(c.available(), ["deepseek", "openrouter", "anthropic"]);
        let ds = &c.providers["deepseek"];
        assert_eq!(ds.headers["X-Test"], "1");
        assert_eq!(ds.parameters["stop"], serde_json::json!(["a", "b"]));
        assert_eq!(c.providers["openrouter"].api_key, "sk-fake-or");
        assert_eq!(c.providers["anthropic"].max_tokens, Some(4096));
        // Round trip through TOML.
        let back = Config::from_toml(&c.to_toml()).unwrap();
        assert_eq!(back, c);
        let p = back.resolve(None).unwrap();
        assert_eq!((p.kind, p.model.as_str(), p.timeout, p.stream), (Kind::DeepSeek, "deepseek-v4-flash", 90, false));
        assert_eq!(back.resolve(Some("openai")).unwrap().kind, Kind::DeepSeek, "openai has no key: falls back to the default");
        let mut picked = back.clone();
        picked.models.insert("anthropic".into(), "claude-haiku-x".into());
        let a = picked.resolve(Some("Anthropic")).unwrap();
        assert_eq!((a.kind, a.model.as_str(), a.max_tokens), (Kind::Anthropic, "claude-haiku-x", Some(4096)));
    }

    #[test]
    fn legacy_form_and_errors() {
        let c = convert_lua(r#"return { base_url = "https://openrouter.ai/api/v1/chat/completions", model = "x", api_key = "sk-fake" }"#).unwrap();
        assert_eq!(c.provider, "openrouter");
        assert_eq!(c.resolve(None).unwrap().kind, Kind::OpenRouter);
        assert!(convert_lua("return os.getenv('HOME')").unwrap_err().contains("configuration.lua"), "no standard library");
        assert!(convert_lua("return 5").is_err());
        assert!(convert_lua("return {").is_err());
        assert_eq!(Config::default().resolve(None), None);
    }

    #[test]
    fn migrates_once() {
        let dir = std::env::temp_dir().join(format!("insightful-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (toml_path, lua_path) = (dir.join("data/insightful.toml"), dir.join("configuration.lua"));
        assert_eq!(load_or_migrate(&toml_path, &lua_path).unwrap().1, Origin::Default);
        std::fs::write(&lua_path, FAKE).unwrap();
        let (c, o) = load_or_migrate(&toml_path, &lua_path).unwrap();
        assert_eq!(o, Origin::Converted);
        assert!(std::fs::read_to_string(&toml_path).unwrap().contains("sk-fake-deepseek"));
        std::fs::write(&lua_path, "return { provider = 'openai' }").unwrap();
        let (c2, o2) = load_or_migrate(&toml_path, &lua_path).unwrap();
        assert_eq!((o2, c2), (Origin::Toml, c), "the Lua file is read only once");
        assert_eq!(std::fs::read_to_string(&lua_path).unwrap(), "return { provider = 'openai' }", "the Lua file is unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
