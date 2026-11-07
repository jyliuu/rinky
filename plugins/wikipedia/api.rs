//! Wikipedia API: request URLs, JSON parsing, text cleanup, the summary cache.
//!
//! Everything here is pure (no network): the plugin sends the requests and hands the
//! bodies to the parsers. The shapes are those of the documented endpoints:
//!
//! - REST summary `/api/rest_v1/page/summary/<title>`: `{type, title, description,
//!   extract, content_urls: {desktop: {page}}}`; a missing page answers HTTP 404.
//! - `w/api.php?action=query&list=search&formatversion=2`: `{query: {search: [{title,
//!   snippet}]}}`, snippets with `<span class="searchmatch">` markup and HTML entities.
//! - `w/api.php?action=query&prop=extracts&explaintext=1&formatversion=2`: `{query:
//!   {pages: [{title, extract}]}}`, a missing page has `missing: true`; headings read
//!   `== Heading ==`.

use ink_net::url::encode_component;
pub use ink_plugin::kit::{cut_at, md_escape, squeeze};
use ink_plugin::serde_json::{json, Map, Value};
use std::path::Path;

/// Summaries kept in the cache file.
pub const CACHE_SIZE: usize = 50;
/// Hits asked of the search API.
pub const SEARCH_LIMIT: usize = 10;
/// Characters of an article shown in the full view (longer ones are cut).
pub const ARTICLE_CHARS: usize = 400_000;

/// True for a Wikipedia language code (`en`, `da`, `zh-yue`, `simple`): it becomes part of
/// the host name, so only lowercase letters, digits and inner hyphens pass.
pub fn valid_lang(s: &str) -> bool {
    (2..=12).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && !s.ends_with('-')
        && !s.contains("--")
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Host name of the language's Wikipedia.
pub fn host(lang: &str) -> String {
    format!("{lang}.wikipedia.org")
}

/// A page title as a URL path segment: spaces become `_`, everything but unreserved
/// characters is percent-encoded (also `/`, as in `AC/DC`).
pub fn title_segment(title: &str) -> String {
    encode_component(&title.trim().replace(' ', "_"))
}

/// REST summary URL of `title`.
pub fn summary_url(lang: &str, title: &str) -> String {
    format!("https://{}/api/rest_v1/page/summary/{}", host(lang), title_segment(title))
}

/// Search API URL for `query`.
pub fn search_url(lang: &str, query: &str) -> String {
    format!(
        "https://{}/w/api.php?action=query&list=search&srsearch={}&srlimit={SEARCH_LIMIT}&format=json&formatversion=2",
        host(lang),
        encode_component(query)
    )
}

/// Plain-text article URL for `title` (redirects followed by the API).
pub fn article_url(lang: &str, title: &str) -> String {
    format!(
        "https://{}/w/api.php?action=query&prop=extracts&explaintext=1&exsectionformat=wiki&redirects=1&titles={}&format=json&formatversion=2",
        host(lang),
        encode_component(title.trim())
    )
}

/// The selected text as a search term: whitespace collapsed, quotes and sentence
/// punctuation cut off both ends, at most 120 characters.
pub fn clean_query(sel: &str) -> String {
    let joined = squeeze(sel);
    let t = joined.trim_matches(|c: char| {
        matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’' | '«' | '»' | '(' | ')' | '[' | ']' | '{' | '}' | '.' | ',' | ';' | ':' | '!' | '?' | '…' | '*' | '—' | '–' | '-')
    });
    t.chars().take(120).collect::<String>().trim().to_string()
}

/// A page summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Page title as Wikipedia writes it.
    pub title: String,
    /// One-line description (`American singer`), when there is one.
    pub description: Option<String>,
    /// The first paragraph(s) as plain text.
    pub extract: String,
    /// Address of the page on the desktop site.
    pub url: Option<String>,
    /// REST page type: `standard`, `disambiguation`, `no-extract`, `mainpage`.
    pub kind: String,
}

fn nonempty(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Parses a REST summary body.
pub fn parse_summary(body: &[u8]) -> Result<Summary, String> {
    let v: Value = ink_plugin::serde_json::from_slice(body).map_err(|e| format!("bad summary JSON: {e}"))?;
    let title = nonempty(&v, "title").ok_or("the summary has no title")?;
    Ok(Summary {
        title,
        description: nonempty(&v, "description"),
        extract: nonempty(&v, "extract").unwrap_or_default(),
        url: v.pointer("/content_urls/desktop/page").and_then(Value::as_str).map(str::to_string),
        kind: nonempty(&v, "type").unwrap_or_else(|| "standard".into()),
    })
}

/// Parses a search API body into `(title, plain snippet)`, best match first.
pub fn parse_search(body: &[u8]) -> Result<Vec<(String, String)>, String> {
    let v: Value = ink_plugin::serde_json::from_slice(body).map_err(|e| format!("bad search JSON: {e}"))?;
    if let Some(e) = v.pointer("/error/info").and_then(Value::as_str) {
        return Err(format!("the search failed: {e}"));
    }
    let list = v.pointer("/query/search").and_then(Value::as_array).ok_or("the search answer has no query.search list")?;
    Ok(list.iter().filter_map(|h| Some((nonempty(h, "title")?, strip_html(h.get("snippet").and_then(Value::as_str).unwrap_or(""))))).collect())
}

/// The spelling suggestion of a search answer (`query.searchinfo.suggestion`), if any.
pub fn parse_suggestion(body: &[u8]) -> Option<String> {
    let v: Value = ink_plugin::serde_json::from_slice(body).ok()?;
    v.pointer("/query/searchinfo/suggestion").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Parses an extracts body: `(title, text)`, or `None` when the page is missing.
pub fn parse_article(body: &[u8]) -> Result<Option<(String, String)>, String> {
    let v: Value = ink_plugin::serde_json::from_slice(body).map_err(|e| format!("bad article JSON: {e}"))?;
    if let Some(e) = v.pointer("/error/info").and_then(Value::as_str) {
        return Err(format!("the request failed: {e}"));
    }
    let pages = v.pointer("/query/pages").and_then(Value::as_array).ok_or("the answer has no query.pages list")?;
    let Some(p) = pages.first() else { return Ok(None) };
    if p.get("missing").is_some() || p.get("invalid").is_some() {
        return Ok(None);
    }
    Ok(Some((nonempty(p, "title").unwrap_or_default(), nonempty(p, "extract").unwrap_or_default())))
}

/// Removes tags from a search snippet and decodes the common entities.
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let mut res = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(a) = rest.find('&') {
        res.push_str(&rest[..a]);
        rest = &rest[a..];
        let Some(semi) = rest.find(';').filter(|n| *n <= 10) else {
            res.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                res.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                res.push('&');
                rest = &rest[1..];
            }
        }
    }
    res.push_str(rest);
    squeeze(&res)
}

/// `== Heading ==` as `(level, text)`; level 1 is `==`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let n = line.chars().take_while(|c| *c == '=').count();
    let t = line.chars().rev().take_while(|c| *c == '=').count();
    if n < 2 || t != n || line.len() <= 2 * n {
        return None;
    }
    let text = line[n..line.len() - n].trim();
    (!text.is_empty()).then_some((n - 1, text))
}

/// The article text cut at [`ARTICLE_CHARS`] on a line end; the flag says it was cut.
fn limited(text: &str) -> (String, bool) {
    if text.chars().count() <= ARTICLE_CHARS {
        return (text.to_string(), false);
    }
    let cut: String = text.chars().take(ARTICLE_CHARS).collect();
    let end = cut.rfind('\n').unwrap_or(cut.len());
    (cut[..end].to_string(), true)
}

/// The text of the full view: Markdown with `##` headings (`:::top` first, so a viewer
/// that already shows a loading text starts at the top).
pub fn article_markdown(text: &str, url: Option<&str>) -> String {
    let (text, cut) = limited(text);
    let mut md = String::from(":::top\n\n");
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match heading(line) {
            Some((level, h)) => md.push_str(&format!("{} {}\n\n", "#".repeat(level.min(5) + 1), md_escape(h))),
            None => md.push_str(&format!("{}\n\n", md_escape(line))),
        }
    }
    if cut {
        md.push_str("*The article is cut here.*\n\n");
    }
    if let Some(u) = url {
        md.push_str(&format!("*{}*\n", md_escape(u)));
    }
    md
}

/// Escapes text for HTML.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A small HTML book of the article for the library: title, description, headings and
/// paragraphs, and the source line (the text is CC BY-SA).
pub fn article_html(lang: &str, title: &str, description: Option<&str>, text: &str, url: &str) -> String {
    let (text, cut) = limited(text);
    let mut h = format!("<!DOCTYPE html>\n<html lang=\"{lang}\">\n<head><meta charset=\"utf-8\"><title>{t}</title></head>\n<body>\n<h1>{t}</h1>\n", t = html_escape(title));
    if let Some(d) = description {
        h.push_str(&format!("<p><i>{}</i></p>\n", html_escape(d)));
    }
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match heading(line) {
            Some((level, t)) => h.push_str(&format!("<h{n}>{}</h{n}>\n", html_escape(t), n = level.min(5) + 1)),
            None => h.push_str(&format!("<p>{}</p>\n", html_escape(line))),
        }
    }
    if cut {
        h.push_str("<p><i>The article is cut here.</i></p>\n");
    }
    h.push_str(&format!("<hr>\n<p>Source: <a href=\"{u}\">{u}</a>, Wikipedia, text available under CC BY-SA 4.0.</p>\n</body>\n</html>\n", u = html_escape(url)));
    h
}

/// A file name for a saved article: the title without path and shell characters, at
/// most 80 characters of it, without dots or spaces at its ends.
pub fn file_name(title: &str, lang: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let clean: String = clean.trim_matches(|c: char| c.is_whitespace() || c == '.').chars().take(80).collect();
    let clean = match clean.trim_end() {
        "" => "Article",
        c => c,
    };
    format!("{clean} (Wikipedia {lang}).html")
}

/// Cache key: lowercase, whitespace collapsed.
pub fn norm(s: &str) -> String {
    squeeze(s).to_lowercase()
}

/// The last [`CACHE_SIZE`] summaries, newest first, in `cache.json` of the plugin's data
/// folder. A hit needs the language and the lookup text (or the page title).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cache {
    entries: Vec<(String, String, Summary)>,
}

impl Cache {
    /// Reads `path`; a missing or damaged file is an empty cache.
    pub fn load(path: &Path) -> Cache {
        let Ok(text) = std::fs::read_to_string(path) else { return Cache::default() };
        let Ok(v) = ink_plugin::serde_json::from_str::<Value>(&text) else {
            log::warn!("wikipedia: damaged cache {}, starting empty", path.display());
            return Cache::default();
        };
        let entries = v
            .get("entries")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let lang = nonempty(e, "lang").filter(|l| valid_lang(l))?;
                        let key = nonempty(e, "key")?;
                        let s = Summary {
                            title: nonempty(e, "title")?,
                            description: nonempty(e, "description"),
                            extract: nonempty(e, "extract").unwrap_or_default(),
                            url: nonempty(e, "url"),
                            kind: nonempty(e, "kind").unwrap_or_else(|| "standard".into()),
                        };
                        Some((lang, key, s))
                    })
                    .take(CACHE_SIZE)
                    .collect()
            })
            .unwrap_or_default();
        Cache { entries }
    }

    /// Writes the cache through a `.part` file and a rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let list: Vec<Value> = self
            .entries
            .iter()
            .map(|(lang, key, s)| {
                let mut m = Map::new();
                m.insert("lang".into(), json!(lang));
                m.insert("key".into(), json!(key));
                m.insert("title".into(), json!(s.title));
                if let Some(d) = &s.description {
                    m.insert("description".into(), json!(d));
                }
                m.insert("extract".into(), json!(s.extract));
                if let Some(u) = &s.url {
                    m.insert("url".into(), json!(u));
                }
                m.insert("kind".into(), json!(s.kind));
                Value::Object(m)
            })
            .collect();
        let part = path.with_extension("json.part");
        std::fs::write(&part, json!({"version": 1, "entries": list}).to_string())?;
        std::fs::rename(&part, path)
    }

    /// The cached summary for `query` in `lang`; it moves to the front.
    pub fn get(&mut self, lang: &str, query: &str) -> Option<Summary> {
        let q = norm(query);
        let i = self.entries.iter().position(|(l, k, s)| l == lang && (*k == q || norm(&s.title) == q))?;
        let e = self.entries.remove(i);
        let s = e.2.clone();
        self.entries.insert(0, e);
        Some(s)
    }

    /// Adds `summary` for `query` (replacing the entry of the same page).
    pub fn put(&mut self, lang: &str, query: &str, summary: &Summary) {
        self.entries.retain(|(l, _, s)| !(l == lang && s.title == summary.title));
        self.entries.insert(0, (lang.to_string(), norm(query), summary.clone()));
        self.entries.truncate(CACHE_SIZE);
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True without entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
