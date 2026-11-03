//! The five book tools of insightful.koplugin (`book_tools.lua`) with the same limits:
//! `search_book`, `read_around`, `list_links`, `toc`, `current_position`.
//!
//! They run on [`DocumentAccess`]. Positions are passed to the model as `locator`
//! strings and come back unchanged. Pages are 1-based in tool arguments and results.

use ink_doc::{LinkTarget, Position, TocEntry};
use ink_plugin::DocumentAccess;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Matches returned per call (over all queries).
pub const MAX_RETURNED_HITS: usize = 15;
/// Matches scanned per query.
pub const SEARCH_SCAN_LIMIT: usize = 200;
/// Snippet length.
pub const MAX_SNIPPET_CHARS: usize = 250;
/// Text returned by `read_around`.
pub const MAX_READ_CHARS: usize = 8000;
/// TOC entries returned.
pub const MAX_TOC_ENTRIES: usize = 120;
/// Links returned.
pub const MAX_LINKS: usize = 40;
/// Link label length.
pub const MAX_LINK_LABEL_CHARS: usize = 200;
/// Link target length.
pub const MAX_LINK_TARGET_CHARS: usize = 500;
/// Characters read per page step of `read_around` (`before_pages`/`after_pages`).
pub const CHARS_PER_PAGE: usize = 2000;

/// Tool schemas sent to the model (same names, descriptions and parameters as
/// `agent.lua`).
pub fn schemas() -> Vec<Value> {
    vec![
        json!({"name": "search_book", "description": "Search the full text of the currently open book for names, phrases, terms, or textual details. Returns compact matching snippets and hit identifiers. Use read_around when more context is needed.",
            "parameters": {"type": "object", "properties": {
                "query": {"type": "string", "description": "One phrase or term to search for."},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "Optional batch of phrases or terms to search for in one call."}}}}),
        json!({"name": "read_around", "description": "Read a bounded amount of surrounding text near a search hit, internal link, or page/location in the currently open book. Use this to inspect evidence instead of relying only on a search snippet or link target.",
            "parameters": {"type": "object", "properties": {
                "hit_id": {"type": "string", "description": "A hit identifier returned by search_book in this tool run."},
                "link_id": {"type": "string", "description": "An internal link identifier returned by list_links in this tool run."},
                "page": {"type": "integer", "description": "A document page or virtual page."},
                "locator": {"type": "string", "description": "A KOReader XPointer locator when available."},
                "before_pages": {"type": "integer", "minimum": 0, "maximum": 2},
                "after_pages": {"type": "integer", "minimum": 0, "maximum": 2}}}}),
        json!({"name": "list_links", "description": "List bounded hyperlinks on the current page or on a page/location found with another book tool. Returns internal link IDs that read_around can follow, plus external URLs for reference. This does not open a URL or leave the reader at another location.",
            "parameters": {"type": "object", "properties": {
                "hit_id": {"type": "string", "description": "A hit identifier returned by search_book in this tool run."},
                "page": {"type": "integer", "description": "A document page or virtual page. Defaults to the current page."},
                "locator": {"type": "string", "description": "A KOReader XPointer locator when available."},
                "include_external": {"type": "boolean", "description": "Include external URLs. Defaults to true."}}}}),
        json!({"name": "toc", "description": "Inspect the currently open book's table of contents and document structure.", "parameters": {"type": "object", "properties": {}}}),
        json!({"name": "current_position", "description": "Return the user's current reading location in the open book.", "parameters": {"type": "object", "properties": {}}}),
    ]
}

/// Cut to at most `limit` bytes on a character boundary, with `...` when cut
/// (`utf8Prefix` in `book_tools.lua`).
pub fn utf8_prefix(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut cut = limit.saturating_sub(3);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &text[..cut])
}

fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Debug)]
struct Hit {
    page: Option<usize>,
    locator: String,
}

#[derive(Clone, Debug)]
struct LinkRef {
    internal: bool,
    locator: Option<String>,
    page: Option<usize>,
}

/// Tool state for one agent run (hit and link ids refer to it).
#[derive(Debug, Default)]
pub struct BookTools {
    hits: HashMap<String, Hit>,
    links: HashMap<String, LinkRef>,
    next_link: usize,
}

fn err(msg: &str) -> Value {
    json!({"ok": false, "error": msg})
}

fn section_for(toc: &[TocEntry], page0: usize) -> Option<String> {
    toc.iter().rfind(|e| e.page.is_some_and(|p| p <= page0)).map(|e| e.title.clone())
}

fn arg_usize(args: &Value, k: &str) -> Option<usize> {
    args.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|x| *x >= 0.0).map(|x| x as usize)
}

impl BookTools {
    /// Fresh state.
    pub fn new() -> BookTools {
        BookTools::default()
    }

    /// Runs tool `name`; always returns a JSON object with `ok`.
    pub fn execute(&mut self, doc: &mut dyn DocumentAccess, name: &str, args: &Value) -> Value {
        match name {
            "search_book" => self.search_book(doc, args),
            "read_around" => self.read_around(doc, args),
            "list_links" => self.list_links(doc, args),
            "toc" => toc(doc),
            "current_position" => current_position(doc),
            other => err(&format!("Unknown book tool: {other}")),
        }
    }

    fn search_book(&mut self, doc: &mut dyn DocumentAccess, args: &Value) -> Value {
        let mut queries: Vec<String> = Vec::new();
        let mut add = |q: &str| {
            let q = q.trim();
            if !q.is_empty() && !queries.iter().any(|x| x.to_lowercase() == q.to_lowercase()) {
                queries.push(q.to_string());
            }
        };
        if let Some(q) = args.get("query").and_then(Value::as_str) {
            add(q);
        }
        for q in args.get("queries").and_then(Value::as_array).into_iter().flatten() {
            if let Some(q) = q.as_str() {
                add(q);
            }
        }
        if queries.is_empty() {
            return err("query or queries is required");
        }
        self.hits.clear();
        let toc = doc.toc();
        let (mut blocks, mut all, mut total, mut remaining) = (Vec::new(), Vec::new(), 0usize, MAX_RETURNED_HITS);
        for (qi, q) in queries.iter().enumerate() {
            let raw = match doc.search(q, SEARCH_SCAN_LIMIT) {
                Ok(r) => r,
                Err(e) => return err(&format!("Book search failed: {e}")),
            };
            let mut hits = Vec::new();
            for (i, h) in raw.iter().enumerate() {
                if hits.len() >= remaining {
                    break;
                }
                let id = format!("q{}:h{}", qi + 1, i + 1);
                let page = h.page.map(|p| p + 1);
                let locator = h.position.to_string();
                let hit = json!({"id": id, "query": q, "page": page, "section": h.page.and_then(|p| section_for(&toc, p)),
                    "locator": utf8_prefix(&locator, MAX_LINK_TARGET_CHARS), "snippet": utf8_prefix(&squeeze(&h.excerpt), MAX_SNIPPET_CHARS)});
                self.hits.insert(id, Hit { page, locator });
                hits.push(hit);
            }
            remaining -= hits.len();
            total += raw.len();
            all.extend(hits.iter().cloned());
            blocks.push(json!({"query": q, "hits": hits, "total_hits": raw.len(),
                "truncated": raw.len() >= SEARCH_SCAN_LIMIT || raw.len() > hits.len(), "total_is_lower_bound": raw.len() >= SEARCH_SCAN_LIMIT}));
        }
        json!({"ok": true, "queries": blocks, "hits": all, "total_hits": total})
    }

    fn read_around(&mut self, doc: &mut dyn DocumentAccess, args: &Value) -> Value {
        let hit = args.get("hit_id").and_then(Value::as_str).and_then(|id| self.hits.get(id)).cloned();
        let link_id = args.get("link_id").and_then(Value::as_str);
        let link = link_id.and_then(|id| self.links.get(id)).cloned();
        if link_id.is_some() && link.is_none() {
            return err("Unknown link_id. Call list_links again.");
        }
        if link.as_ref().is_some_and(|l| !l.internal) {
            return err("External links cannot be read as book locations.");
        }
        let locator = args.get("locator").and_then(Value::as_str).map(str::to_string).or_else(|| link.as_ref().and_then(|l| l.locator.clone())).or_else(|| hit.as_ref().map(|h| h.locator.clone()));
        let page = arg_usize(args, "page").or_else(|| link.as_ref().and_then(|l| l.page)).or_else(|| hit.as_ref().and_then(|h| h.page));
        let before = arg_usize(args, "before_pages").unwrap_or(1).min(2);
        let after = arg_usize(args, "after_pages").unwrap_or(1).min(2);
        let total = doc.page_count().unwrap_or(0);
        let toc = doc.toc();
        // A position: read about (1 + before + after) pages of characters around it.
        let pos = locator.as_deref().and_then(|l| l.parse::<Position>().ok());
        let (text, page, start, end) = if let (Some(pos), true) = (&pos, args.get("page").is_none() || locator.is_some()) {
            let chars = ((1 + before + after) * CHARS_PER_PAGE).min(MAX_READ_CHARS);
            match doc.read_around(pos, chars) {
                Ok(t) => (t, page, page.map(|p| p.saturating_sub(before).max(1)), page.map(|p| (p + after).min(total.max(p)))),
                Err(_) => return err("No readable text is available near that location."),
            }
        } else if let Some(p) = page {
            if total == 0 {
                return err("No readable text is available for this document.");
            }
            let p = p.clamp(1, total);
            let (s, e) = (p.saturating_sub(before).max(1), (p + after).min(total));
            let mut parts = Vec::new();
            for pg in s..=e {
                match doc.page_text(pg - 1) {
                    Ok(t) => parts.push(t),
                    Err(_) => return err("No readable text is available near that location."),
                }
            }
            (parts.join("\n"), Some(p), Some(s), Some(e))
        } else {
            return err("hit_id, link_id, page, or locator is required");
        };
        if text.trim().is_empty() {
            return err("No readable text is available near that location.");
        }
        let text = utf8_prefix(&text, MAX_READ_CHARS);
        json!({"ok": true, "hit_id": hit.map(|_| args["hit_id"].clone()), "link_id": link.map(|_| args["link_id"].clone()),
            "section": page.and_then(|p| section_for(&toc, p - 1)), "page": page, "page_start": start, "page_end": end,
            "locator": locator, "chars": text.len(), "text": text})
    }

    fn list_links(&mut self, doc: &mut dyn DocumentAccess, args: &Value) -> Value {
        let include_external = args.get("include_external").and_then(Value::as_bool).unwrap_or(true);
        let hit = args.get("hit_id").and_then(Value::as_str).and_then(|id| self.hits.get(id)).cloned();
        let current = doc.current_page().map(|p| p + 1);
        let locator = args.get("locator").and_then(Value::as_str).map(str::to_string).or_else(|| hit.as_ref().map(|h| h.locator.clone()));
        let page = arg_usize(args, "page").or_else(|| hit.as_ref().and_then(|h| h.page)).or(current);
        let pos = match locator.as_deref() {
            Some(l) => match l.parse::<Position>() {
                Ok(p) => p,
                Err(_) => return err("Unknown locator."),
            },
            None if page.is_none() || page == current => match doc.current_position() {
                Ok(p) => p,
                Err(e) => return err(&format!("No position is available: {e}")),
            },
            None => return err("list_links needs a hit_id or locator for a page other than the current one."),
        };
        let raw = match doc.links(&pos) {
            Ok(l) => l,
            Err(e) => return err(&format!("No links are available here: {e}")),
        };
        let mut links = Vec::new();
        let mut total = 0;
        for l in &raw {
            let label = utf8_prefix(&squeeze(&l.text), MAX_LINK_LABEL_CHARS);
            match &l.target {
                LinkTarget::External(url) => {
                    if !include_external {
                        continue;
                    }
                    total += 1;
                    if links.len() < MAX_LINKS {
                        links.push(json!({"kind": "external", "label": label, "target_uri": utf8_prefix(url, MAX_LINK_TARGET_CHARS)}));
                    }
                }
                LinkTarget::Internal(p) => {
                    total += 1;
                    if links.len() < MAX_LINKS {
                        self.next_link += 1;
                        let id = format!("l{}", self.next_link);
                        let loc = utf8_prefix(&p.to_string(), MAX_LINK_TARGET_CHARS);
                        self.links.insert(id.clone(), LinkRef { internal: true, locator: Some(p.to_string()), page: None });
                        links.push(json!({"id": id, "kind": "internal", "label": label, "footnote": l.footnote, "target_locator": loc}));
                    }
                }
            }
        }
        json!({"ok": true, "page": page, "locator": utf8_prefix(&pos.to_string(), MAX_LINK_TARGET_CHARS), "links": links, "total_links": total, "truncated": total > links.len()})
    }
}

fn toc(doc: &mut dyn DocumentAccess) -> Value {
    let raw = doc.toc();
    if raw.is_empty() {
        return err("No table of contents is available for this document.");
    }
    let entries: Vec<Value> = raw
        .iter()
        .take(MAX_TOC_ENTRIES)
        .map(|e| json!({"title": e.title, "depth": e.level as u32 + 1, "page": e.page.map(|p| p + 1), "locator": utf8_prefix(&e.position.to_string(), MAX_LINK_TARGET_CHARS)}))
        .collect();
    json!({"ok": true, "entries": entries, "total_entries": raw.len(), "truncated": raw.len() > MAX_TOC_ENTRIES})
}

fn current_position(doc: &mut dyn DocumentAccess) -> Value {
    let page = doc.current_page().map(|p| p + 1);
    let pages = doc.page_count().filter(|n| *n > 0);
    let toc = doc.toc();
    let locator = doc.current_position().ok().map(|p| p.to_string());
    let mut r = json!({"ok": true, "page": page, "pages": pages, "section": page.and_then(|p| section_for(&toc, p - 1)), "locator": locator});
    if let (Some(p), Some(n)) = (page, pages) {
        r["progress"] = json!(p as f64 / n as f64);
    }
    r
}

/// One line describing a call for the AGENT ACTION block (`search_book: "Verona"`).
pub fn describe(name: &str, args: &Value) -> String {
    let mut parts = Vec::new();
    for k in ["query", "hit_id", "link_id", "page", "locator"] {
        if let Some(v) = args.get(k).filter(|v| !v.is_null()) {
            let v = match v {
                Value::String(s) => format!("\"{}\"", utf8_prefix(s, 60)),
                other => other.to_string(),
            };
            parts.push(format!("{k} {v}"));
        }
    }
    if let Some(qs) = args.get("queries").and_then(Value::as_array) {
        let q: Vec<String> = qs.iter().filter_map(Value::as_str).map(|s| format!("\"{}\"", utf8_prefix(s, 40))).collect();
        parts.push(format!("queries {}", q.join(", ")));
    }
    let _ = name;
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeBook;

    #[test]
    fn limits_match_the_lua_plugin() {
        assert_eq!((MAX_RETURNED_HITS, SEARCH_SCAN_LIMIT, MAX_SNIPPET_CHARS, MAX_READ_CHARS), (15, 200, 250, 8000));
        assert_eq!((MAX_TOC_ENTRIES, MAX_LINKS, MAX_LINK_LABEL_CHARS, MAX_LINK_TARGET_CHARS), (120, 40, 200, 500));
        let sch = schemas();
        let names: Vec<&str> = sch.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["search_book", "read_around", "list_links", "toc", "current_position"]);
        assert_eq!(utf8_prefix("ab", 5), "ab");
        assert_eq!(utf8_prefix("aaaaéé", 7), "aaaa...");
        assert_eq!(utf8_prefix("aaaaaééé", 10), "aaaaaé...");
        assert_eq!(utf8_prefix("aaaaaééé", 9), "aaaaa...");
    }

    #[test]
    fn search_and_read_around() {
        let mut book = FakeBook::new(40);
        let mut t = BookTools::new();
        let r = t.execute(&mut book, "search_book", &json!({"query": "Verona", "queries": ["verona", "dignity"]}));
        assert_eq!(r["ok"], true);
        assert_eq!(r["queries"].as_array().unwrap().len(), 2, "duplicate query (case-insensitive) dropped");
        assert_eq!(r["hits"].as_array().unwrap().len(), 15, "15 hits over all queries");
        assert_eq!(r["queries"][0]["total_hits"], 40);
        assert_eq!(r["queries"][0]["truncated"], true);
        assert_eq!(r["queries"][1]["hits"].as_array().unwrap().len(), 0, "no hits left for the second query");
        let h = &r["hits"][2];
        assert_eq!(h["id"], "q1:h3");
        assert_eq!(h["page"], 3);
        assert_eq!(h["section"], "Act 1");
        assert!(h["snippet"].as_str().unwrap().len() <= 250);
        let a = t.execute(&mut book, "read_around", &json!({"hit_id": "q1:h3", "before_pages": 9}));
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(a["page_start"], 1, "before_pages capped at 2");
        assert!(a["text"].as_str().unwrap().contains("Verona"));
        assert!(a["chars"].as_u64().unwrap() <= 8000);
        let p = t.execute(&mut book, "read_around", &json!({"page": 30, "before_pages": 0, "after_pages": 1}));
        assert_eq!((p["page_start"].as_u64(), p["page_end"].as_u64()), (Some(30), Some(31)));
        assert!(p["text"].as_str().unwrap().starts_with("Page 30."));
        assert_eq!(t.execute(&mut book, "read_around", &json!({}))["error"], "hit_id, link_id, page, or locator is required");
        assert_eq!(t.execute(&mut book, "read_around", &json!({"link_id": "l9"}))["error"], "Unknown link_id. Call list_links again.");
        assert_eq!(t.execute(&mut book, "search_book", &json!({}))["error"], "query or queries is required");
        assert_eq!(t.execute(&mut book, "nope", &json!({}))["error"], "Unknown book tool: nope");
    }

    #[test]
    fn links_toc_position() {
        let mut book = FakeBook::new(200);
        let mut t = BookTools::new();
        let l = t.execute(&mut book, "list_links", &json!({}));
        assert_eq!(l["ok"], true, "{l}");
        assert_eq!(l["links"].as_array().unwrap().len(), 40);
        assert_eq!(l["total_links"], 60);
        assert_eq!(l["truncated"], true);
        assert_eq!(l["links"][0]["kind"], "internal");
        assert!(l["links"][0]["label"].as_str().unwrap().len() <= 200);
        let id = l["links"][0]["id"].as_str().unwrap().to_string();
        let ext = t.execute(&mut book, "list_links", &json!({"include_external": false}));
        assert_eq!(ext["total_links"], 30);
        let a = t.execute(&mut book, "read_around", &json!({"link_id": id}));
        assert_eq!(a["ok"], true, "{a}");
        let toc = t.execute(&mut book, "toc", &json!({}));
        assert_eq!(toc["entries"].as_array().unwrap().len(), 120);
        assert_eq!(toc["total_entries"], 200);
        assert_eq!(toc["entries"][0]["depth"], 1);
        let c = t.execute(&mut book, "current_position", &json!({}));
        assert_eq!((c["page"].as_u64(), c["pages"].as_u64()), (Some(3), Some(200)));
        assert!((c["progress"].as_f64().unwrap() - 0.015).abs() < 1e-9);
        assert_eq!(describe("search_book", &json!({"query": "Verona"})), "query \"Verona\"");
    }
}
