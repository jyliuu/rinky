//! OPDS catalogs: OPDS 1.x (Atom) and basic OPDS 2 (JSON) into one model, OpenSearch
//! search templates, and a small client that fetches, paginates and searches.
//!
//! The Atom parser is lenient, as catalogs in the wild need (KOReader's `opds_spec.lua`
//! lists the cases): elements are matched by local name, end tags need not match,
//! `<content type="xhtml">` and `type="html"` become plain text, unknown entities stay as
//! written, single-quoted and non-ASCII attributes are fine, and every `href` is resolved
//! against the URL the feed was fetched from (after redirects). `data:` URIs pass through.

use crate::download::{extension_for_mime, sanitize_filename, Download};
use crate::http::{HttpClient, Request};
use crate::url::{encode_component, resolve};
use crate::{NetError, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::path::Path;

/// Prefix of every acquisition relation.
pub const REL_ACQUISITION: &str = "http://opds-spec.org/acquisition";
/// Cover image relations.
pub const IMAGE_RELS: &[&str] = &["http://opds-spec.org/image", "http://opds-spec.org/cover", "x-stanza-cover-image"];
/// Thumbnail relations.
pub const THUMBNAIL_RELS: &[&str] = &["http://opds-spec.org/image/thumbnail", "http://opds-spec.org/thumbnail", "x-stanza-cover-image-thumbnail"];
/// Facet relation.
pub const REL_FACET: &str = "http://opds-spec.org/facet";
/// Page streaming (OPDS-PSE) relation.
pub const REL_PSE_STREAM: &str = "http://vaemendis.net/opds-pse/stream";

/// A link in a feed or entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpdsLink {
    /// `href`, resolved against the feed URL.
    pub href: String,
    /// `rel` (`subsection`, `http://opds-spec.org/acquisition`, `search`, `next`, ...);
    /// empty when the feed gave none (Atom reads that as `alternate`).
    pub rel: String,
    /// MIME type.
    pub mime: String,
    /// Title.
    pub title: Option<String>,
    /// `length` in bytes.
    pub length: Option<u64>,
    /// `opds:indirectAcquisition` types, outermost first.
    pub indirect: Vec<String>,
    /// `opds:facetGroup`.
    pub facet_group: Option<String>,
    /// `opds:activeFacet="true"`.
    pub active_facet: bool,
    /// `thr:count`, `pse:count` or OPDS 2 `numberOfItems`.
    pub count: Option<u64>,
    /// `opds:price` as `"VALUE CURRENCY"`.
    pub price: Option<String>,
    /// OPDS 2 `templated`.
    pub templated: bool,
}

impl OpdsLink {
    /// An acquisition link (`rel` starts with `http://opds-spec.org/acquisition`).
    pub fn is_acquisition(&self) -> bool {
        self.rel.starts_with(REL_ACQUISITION)
    }

    /// Open access or plain acquisition (no borrow/buy/sample/subscribe).
    pub fn is_free_acquisition(&self) -> bool {
        self.rel == REL_ACQUISITION || self.rel == "http://opds-spec.org/acquisition/open-access"
    }

    /// A link to another catalog feed.
    pub fn is_catalog(&self) -> bool {
        let m = self.mime.to_ascii_lowercase();
        m.starts_with("application/atom+xml") || m.starts_with("application/opds+json")
    }

    /// The type of the file finally received (innermost indirect type, else `mime`).
    pub fn final_mime(&self) -> &str {
        self.indirect.last().map(String::as_str).unwrap_or(&self.mime)
    }
}

/// One catalog entry: a navigation item or a book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpdsEntry {
    /// `id`.
    pub id: String,
    /// Title.
    pub title: String,
    /// Authors.
    pub authors: Vec<String>,
    /// Summary text.
    pub summary: Option<String>,
    /// Content as plain text.
    pub content: Option<String>,
    /// `updated`.
    pub updated: Option<String>,
    /// `published` / `dc:issued`.
    pub published: Option<String>,
    /// `dc:language`.
    pub language: Option<String>,
    /// `dc:publisher`.
    pub publisher: Option<String>,
    /// Category labels (or terms).
    pub categories: Vec<String>,
    /// Links (navigation, acquisition, covers).
    pub links: Vec<OpdsLink>,
}

impl OpdsEntry {
    /// Acquisition links.
    pub fn acquisitions(&self) -> Vec<&OpdsLink> {
        self.links.iter().filter(|l| l.is_acquisition()).collect()
    }

    /// The best acquisition for a reader: free first, then EPUB, then any.
    pub fn best_acquisition(&self) -> Option<&OpdsLink> {
        let acq = self.acquisitions();
        let rank = |l: &&OpdsLink| {
            let free = if l.is_free_acquisition() { 0 } else { 10 };
            let m = l.final_mime();
            let fmt = if m.contains("epub") {
                0
            } else if m.contains("pdf") {
                2
            } else if m.contains("mobi") || m.contains("fb2") || m.contains("cbz") || m.contains("comicbook") {
                1
            } else {
                3
            };
            free + fmt
        };
        acq.into_iter().min_by_key(rank)
    }

    /// The link to follow for a navigation entry: a catalog link with no `rel`,
    /// `subsection`, `alternate`, `collection` or an OPDS sort/featured relation; else any
    /// catalog link that is not `self`, `related` or a facet.
    pub fn navigation(&self) -> Option<&OpdsLink> {
        let nav_rel = |r: &str| {
            r.is_empty()
                || matches!(r, "subsection" | "alternate" | "collection" | "start")
                || r.starts_with("http://opds-spec.org/sort/")
                || r.starts_with("http://opds-spec.org/featured")
                || r.starts_with("http://opds-spec.org/recommended")
        };
        self.links
            .iter()
            .find(|l| l.is_catalog() && nav_rel(&l.rel))
            .or_else(|| self.links.iter().find(|l| l.is_catalog() && !l.is_acquisition() && l.rel != "self" && l.rel != "related" && !l.rel.contains("facet")))
    }

    /// Cover image.
    pub fn cover(&self) -> Option<&OpdsLink> {
        self.links.iter().find(|l| IMAGE_RELS.contains(&l.rel.as_str())).or_else(|| self.thumbnail())
    }

    /// Thumbnail.
    pub fn thumbnail(&self) -> Option<&OpdsLink> {
        self.links.iter().find(|l| THUMBNAIL_RELS.contains(&l.rel.as_str()))
    }

    /// True for a book (it has an acquisition or page-streaming link).
    pub fn is_book(&self) -> bool {
        self.links.iter().any(|l| l.is_acquisition() || l.rel == REL_PSE_STREAM)
    }

    /// Summary, else content.
    pub fn description(&self) -> Option<&str> {
        self.summary.as_deref().or(self.content.as_deref())
    }
}

/// Atom or JSON.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpdsVersion {
    /// OPDS 1.x (Atom).
    #[default]
    Atom1,
    /// OPDS 2 (JSON).
    Json2,
}

/// Navigation or acquisition feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    /// Entries lead to other feeds.
    Navigation,
    /// Entries are books.
    Acquisition,
}

/// Where search starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchLink {
    /// An OpenSearch description document to fetch first.
    Description(String),
    /// A URL template with `{searchTerms}` (or OPDS 2 `{?query}`).
    Template(String),
}

/// A parsed feed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpdsFeed {
    /// `id`.
    pub id: String,
    /// Feed title.
    pub title: String,
    /// Subtitle.
    pub subtitle: Option<String>,
    /// `updated`.
    pub updated: Option<String>,
    /// Icon URL.
    pub icon: Option<String>,
    /// Entries.
    pub entries: Vec<OpdsEntry>,
    /// Feed-level links (`next`, `search`, `start`, facets).
    pub links: Vec<OpdsLink>,
    /// `opensearch:totalResults`.
    pub total_results: Option<u64>,
    /// `opensearch:itemsPerPage`.
    pub items_per_page: Option<u64>,
    /// `opensearch:startIndex`.
    pub start_index: Option<u64>,
    /// Format of the source.
    pub version: OpdsVersion,
}

impl OpdsFeed {
    /// First link with `rel`.
    pub fn link(&self, rel: &str) -> Option<&OpdsLink> {
        self.links.iter().find(|l| l.rel == rel)
    }

    /// Next page.
    pub fn next_url(&self) -> Option<&str> {
        self.link("next").map(|l| l.href.as_str())
    }

    /// Previous page (`previous` or `prev`).
    pub fn prev_url(&self) -> Option<&str> {
        self.link("previous").or_else(|| self.link("prev")).map(|l| l.href.as_str())
    }

    /// First page.
    pub fn first_url(&self) -> Option<&str> {
        self.link("first").map(|l| l.href.as_str())
    }

    /// Last page.
    pub fn last_url(&self) -> Option<&str> {
        self.link("last").map(|l| l.href.as_str())
    }

    /// Catalog root.
    pub fn start_url(&self) -> Option<&str> {
        self.link("start").map(|l| l.href.as_str())
    }

    /// Navigation or acquisition.
    pub fn kind(&self) -> FeedKind {
        if self.entries.iter().any(OpdsEntry::is_book) {
            FeedKind::Acquisition
        } else {
            FeedKind::Navigation
        }
    }

    /// The search entry point: an OpenSearch description is preferred (as KOReader does),
    /// then an Atom/OPDS 2 template (Calibre, OPDS 2).
    pub fn search(&self) -> Option<SearchLink> {
        let search = || self.links.iter().filter(|l| l.rel == "search" || l.rel.ends_with("/search"));
        if let Some(l) = search().find(|l| l.mime.to_ascii_lowercase().starts_with("application/opensearchdescription+xml")) {
            return Some(SearchLink::Description(l.href.clone()));
        }
        search().find(|l| l.templated || l.href.contains('{')).map(|l| SearchLink::Template(l.href.clone()))
    }

    /// Facet links grouped by `opds:facetGroup` (in document order).
    pub fn facets(&self) -> Vec<(String, Vec<&OpdsLink>)> {
        let mut groups: Vec<(String, Vec<&OpdsLink>)> = Vec::new();
        for l in self.links.iter().filter(|l| l.rel == REL_FACET) {
            let g = l.facet_group.clone().unwrap_or_else(|| "Filters".into());
            match groups.iter_mut().find(|(n, _)| *n == g) {
                Some((_, v)) => v.push(l),
                None => groups.push((g, vec![l])),
            }
        }
        groups
    }
}

// ---------------------------------------------------------------------------------------
// Text helpers.

/// Decodes XML/HTML character references: the five XML entities, numeric references and
/// common HTML names. Unknown references are kept as written.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace()).map(|j| j + 1);
        let Some(end) = end.filter(|&e| rest.as_bytes().get(e) == Some(&b';') && e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch: Option<String> = if let Some(num) = name.strip_prefix('#') {
            let v = match num.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok(),
                None => num.parse().ok(),
            };
            v.and_then(char::from_u32).map(String::from)
        } else {
            named_entity(name).map(str::to_string)
        };
        match ch {
            Some(c) => {
                out.push_str(&c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "hellip" => "\u{2026}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201c}",
        "rdquo" => "\u{201d}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "trade" => "\u{2122}",
        "eacute" => "\u{e9}",
        "egrave" => "\u{e8}",
        "aacute" => "\u{e1}",
        "agrave" => "\u{e0}",
        "ouml" => "\u{f6}",
        "uuml" => "\u{fc}",
        "auml" => "\u{e4}",
        "aelig" => "\u{e6}",
        "oslash" => "\u{f8}",
        "aring" => "\u{e5}",
        "AElig" => "\u{c6}",
        "Oslash" => "\u{d8}",
        "Aring" => "\u{c5}",
        "szlig" => "\u{df}",
        "ccedil" => "\u{e7}",
        "bull" => "\u{2022}",
        "middot" => "\u{b7}",
        _ => return None,
    })
}

fn is_block(tag: &str) -> bool {
    matches!(tag.to_ascii_lowercase().as_str(), "p" | "div" | "br" | "li" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "blockquote" | "pre" | "dt" | "dd" | "hr" | "table")
}

/// Normalizes whitespace per line and drops empty lines.
fn tidy_lines(s: &str) -> String {
    s.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// HTML markup (already unescaped from the XML) to plain text: tags removed, block tags
/// become line breaks, entities decoded, whitespace tidied.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    // Line breaks in the markup are spaces; block elements make the lines.
    let flat = |t: &str| t.replace(['\n', '\r'], " ");
    while let Some(i) = rest.find('<') {
        out.push_str(&flat(&rest[..i]));
        let Some(j) = rest[i..].find('>') else {
            out.push_str(&flat(&rest[i..]));
            rest = "";
            break;
        };
        let tag = rest[i + 1..i + j].trim_start_matches('/');
        let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if is_block(&name) {
            out.push('\n');
        } else if !name.is_empty() {
            out.push(' ');
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(&flat(rest));
    tidy_lines(&decode_entities(&out))
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------------------
// Atom parser.

fn local(name: &[u8]) -> String {
    let n = match name.iter().position(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    };
    String::from_utf8_lossy(n).into_owned()
}

fn attrs(e: &BytesStart<'_>) -> Vec<(String, String, String)> {
    // (full name, local name, value)
    e.attributes()
        .with_checks(false)
        .flatten()
        .map(|a| {
            let full = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let value = decode_entities(&String::from_utf8_lossy(&a.value));
            (full.clone(), local(full.as_bytes()), value)
        })
        .collect()
}

fn attr<'a>(list: &'a [(String, String, String)], local_name: &str) -> Option<&'a str> {
    list.iter().find(|(_, l, _)| l == local_name).map(|(_, _, v)| v.as_str())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Text,
    Html,
    Xhtml,
}

struct Capture {
    depth: usize,
    name: String,
    mode: Mode,
    buf: String,
}

/// Parses an OPDS Atom feed (or a single `<entry>` document) fetched from `base_url`.
pub fn parse_feed(xml: &str, base_url: &str) -> Result<OpdsFeed> {
    let mut reader = Reader::from_str(xml);
    {
        let c = reader.config_mut();
        c.trim_text(false);
        c.check_end_names = false;
        c.allow_unmatched_ends = true;
        c.expand_empty_elements = true;
    }
    let mut feed = OpdsFeed { version: OpdsVersion::Atom1, ..Default::default() };
    let mut path: Vec<String> = Vec::new();
    let mut entry: Option<OpdsEntry> = None;
    let mut entry_depth = 0usize;
    let mut cap: Option<Capture> = None;
    let mut root: Option<String> = None;
    let mut author_depth: Option<usize> = None;
    // (index into entry.links or feed.links, in entry?, depth)
    let mut link: Option<(usize, bool, usize)> = None;
    loop {
        let ev = reader.read_event().map_err(|e| NetError::Format(format!("xml at byte {}: {e}", reader.buffer_position())))?;
        match ev {
            Event::Start(e) => {
                let name = local(e.name().as_ref());
                path.push(name.clone());
                let depth = path.len();
                if let Some(c) = cap.as_mut() {
                    if c.mode == Mode::Xhtml && is_block(&name) {
                        c.buf.push('\n');
                    }
                    continue;
                }
                if root.is_none() {
                    root = Some(name.clone());
                    if name != "feed" && name != "entry" {
                        return Err(NetError::Format(format!("not an Atom feed (root <{name}>)")));
                    }
                }
                let a = attrs(&e);
                match name.as_str() {
                    "entry" if entry.is_none() => {
                        entry = Some(OpdsEntry::default());
                        entry_depth = depth;
                    }
                    "author" => author_depth = Some(depth),
                    "link" => {
                        let l = OpdsLink {
                            href: resolve(base_url, attr(&a, "href").unwrap_or("")),
                            rel: attr(&a, "rel").unwrap_or("").trim().to_string(),
                            mime: attr(&a, "type").unwrap_or("").trim().to_string(),
                            title: attr(&a, "title").map(one_line).filter(|t| !t.is_empty()),
                            length: attr(&a, "length").and_then(|v| v.trim().parse().ok()),
                            facet_group: attr(&a, "facetGroup").map(str::to_string),
                            active_facet: attr(&a, "activeFacet") == Some("true"),
                            count: attr(&a, "count").and_then(|v| v.trim().parse().ok()),
                            ..Default::default()
                        };
                        match entry.as_mut() {
                            Some(en) => {
                                en.links.push(l);
                                link = Some((en.links.len() - 1, true, depth));
                            }
                            None => {
                                feed.links.push(l);
                                link = Some((feed.links.len() - 1, false, depth));
                            }
                        }
                    }
                    "indirectAcquisition" => {
                        if let (Some((i, in_entry, _)), Some(t)) = (link, attr(&a, "type")) {
                            let target = if in_entry { entry.as_mut().and_then(|en| en.links.get_mut(i)) } else { feed.links.get_mut(i) };
                            if let Some(l) = target {
                                l.indirect.push(t.to_string());
                            }
                        }
                    }
                    "price" => {
                        let cur = attr(&a, "currencycode").or(attr(&a, "currencyCode")).unwrap_or("").to_string();
                        cap = Some(Capture { depth, name: format!("price {cur}"), mode: Mode::Text, buf: String::new() });
                    }
                    "category" => {
                        if let Some(en) = entry.as_mut() {
                            if let Some(c) = attr(&a, "label").or(attr(&a, "term")).map(one_line).filter(|c| !c.is_empty()) {
                                en.categories.push(c);
                            }
                        }
                    }
                    "content" | "summary" => {
                        let t = attr(&a, "type").unwrap_or("text").to_ascii_lowercase();
                        let mode = if t == "xhtml" {
                            Mode::Xhtml
                        } else if t == "html" || t.contains("html") {
                            Mode::Html
                        } else {
                            Mode::Text
                        };
                        cap = Some(Capture { depth, name, mode, buf: String::new() });
                    }
                    "title" | "id" | "updated" | "published" | "issued" | "language" | "publisher" | "subtitle" | "icon" | "name" | "totalResults" | "itemsPerPage" | "startIndex" => {
                        cap = Some(Capture { depth, name, mode: Mode::Text, buf: String::new() });
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some(c) = cap.as_mut() {
                    let text = decode_entities(&String::from_utf8_lossy(&t));
                    if c.mode == Mode::Xhtml {
                        // XML line breaks are spaces; block elements make the lines.
                        c.buf.push_str(&text.replace(['\n', '\r'], " "));
                    } else {
                        c.buf.push_str(&text);
                    }
                }
            }
            Event::CData(t) => {
                if let Some(c) = cap.as_mut() {
                    c.buf.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::End(e) => {
                let name = local(e.name().as_ref());
                // Pop to the matching open element (tolerates unclosed children).
                let pos = path.iter().rposition(|p| *p == name);
                if let Some(c) = cap.as_mut() {
                    match pos {
                        // An end tag that matches no open element: ignored.
                        None => continue,
                        // Closes an element inside the capture, and any unclosed elements
                        // inside that one (`<br>`, an HTML `<p>` left open).
                        Some(p) if p + 1 > c.depth => {
                            if c.mode == Mode::Xhtml && is_block(&name) {
                                c.buf.push('\n');
                            }
                            path.truncate(p);
                            continue;
                        }
                        // Closes the captured element itself, or an ancestor of it when
                        // its own end tag is missing: the capture ends here.
                        Some(_) => {}
                    }
                }
                if let Some(c) = cap.take() {
                    let in_author = author_depth.is_some();
                    let text = match c.mode {
                        Mode::Text => c.buf.trim().to_string(),
                        Mode::Html => html_to_text(&c.buf),
                        Mode::Xhtml => tidy_lines(&c.buf),
                    };
                    if let Some(cur) = c.name.strip_prefix("price ") {
                        if let Some((i, in_entry, _)) = link {
                            let v = format!("{} {}", one_line(&text), cur.trim()).trim().to_string();
                            let l = if in_entry { entry.as_mut().and_then(|en| en.links.get_mut(i)) } else { feed.links.get_mut(i) };
                            if let Some(l) = l {
                                l.price = Some(v);
                            }
                        }
                    } else if let Some(en) = entry.as_mut() {
                        match c.name.as_str() {
                            "name" if in_author => {
                                let n = one_line(&text);
                                if !n.is_empty() {
                                    en.authors.push(n);
                                }
                            }
                            "title" if en.title.is_empty() => en.title = one_line(&text),
                            "id" if !in_author => en.id = text,
                            "updated" => en.updated = Some(text),
                            "published" | "issued" if en.published.is_none() => en.published = Some(text),
                            "language" => en.language = Some(text),
                            "publisher" => en.publisher = Some(one_line(&text)),
                            "summary" => en.summary = Some(text).filter(|t| !t.is_empty()),
                            "content" => en.content = Some(text).filter(|t| !t.is_empty()),
                            "totalResults" => feed.total_results = text.parse().ok(),
                            _ => {}
                        }
                    } else if !in_author {
                        match c.name.as_str() {
                            "title" if feed.title.is_empty() => feed.title = one_line(&text),
                            "subtitle" => feed.subtitle = Some(one_line(&text)),
                            "id" => feed.id = text,
                            "updated" => feed.updated = Some(text),
                            "icon" => feed.icon = Some(resolve(base_url, &text)),
                            "totalResults" => feed.total_results = text.parse().ok(),
                            "itemsPerPage" => feed.items_per_page = text.parse().ok(),
                            "startIndex" => feed.start_index = text.parse().ok(),
                            _ => {}
                        }
                    }
                }
                if let Some(p) = pos {
                    let closing_depth = p + 1;
                    if author_depth.is_some_and(|d| d >= closing_depth) {
                        author_depth = None;
                    }
                    if link.is_some_and(|(_, _, d)| d >= closing_depth) {
                        link = None;
                    }
                    if entry.is_some() && entry_depth >= closing_depth {
                        let en = entry.take().unwrap();
                        feed.entries.push(en);
                    }
                    path.truncate(p);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some(en) = entry.take() {
        feed.entries.push(en);
    }
    if root.is_none() {
        return Err(NetError::Format("empty document".into()));
    }
    if root.as_deref() == Some("entry") && feed.title.is_empty() {
        feed.title = feed.entries.first().map(|e| e.title.clone()).unwrap_or_default();
    }
    Ok(feed)
}

// ---------------------------------------------------------------------------------------
// OPDS 2.

fn json_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        // Language map: {"en": "...", "fr": "..."}; take English, else the first.
        serde_json::Value::Object(m) => m.get("en").or_else(|| m.values().next()).and_then(json_str),
        _ => None,
    }
}

fn json_names(v: Option<&serde_json::Value>) -> Vec<String> {
    match v {
        Some(serde_json::Value::Array(a)) => a.iter().flat_map(|x| json_names(Some(x))).collect(),
        Some(serde_json::Value::Object(m)) => m.get("name").and_then(json_str).into_iter().collect(),
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn json_link(v: &serde_json::Value, base: &str) -> Option<OpdsLink> {
    let href = v.get("href")?.as_str()?;
    let templated = v.get("templated").and_then(|t| t.as_bool()).unwrap_or(false);
    let rel = match v.get("rel") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).next().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let props = v.get("properties");
    let mut indirect = Vec::new();
    let mut ia = props.and_then(|p| p.get("indirectAcquisition")).and_then(|x| x.as_array()).and_then(|a| a.first()).cloned();
    while let Some(i) = ia {
        if let Some(t) = i.get("type").and_then(|t| t.as_str()) {
            indirect.push(t.to_string());
        }
        ia = i.get("child").and_then(|c| c.as_array()).and_then(|a| a.first()).cloned();
    }
    let price = props.and_then(|p| p.get("price")).map(|p| format!("{} {}", p.get("value").map(|v| v.to_string()).unwrap_or_default(), p.get("currency").and_then(|c| c.as_str()).unwrap_or("")).trim().to_string());
    Some(OpdsLink {
        // A template must not be resolved through the percent-encoding-free resolver
        // in a way that breaks `{`: resolution keeps braces as they are.
        href: resolve(base, href),
        rel,
        mime: v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        title: v.get("title").and_then(json_str),
        length: None,
        indirect,
        facet_group: None,
        active_facet: false,
        count: props.and_then(|p| p.get("numberOfItems")).and_then(|n| n.as_u64()),
        price,
        templated,
    })
}

fn json_publication(p: &serde_json::Value, base: &str) -> OpdsEntry {
    let md = p.get("metadata").cloned().unwrap_or_default();
    let mut e = OpdsEntry {
        id: md.get("identifier").and_then(json_str).unwrap_or_default(),
        title: md.get("title").and_then(json_str).map(|t| one_line(&t)).unwrap_or_default(),
        authors: json_names(md.get("author")),
        summary: md.get("description").and_then(json_str).map(|d| html_to_text(&d)),
        updated: md.get("modified").and_then(json_str),
        published: md.get("published").and_then(json_str),
        language: match md.get("language") {
            Some(serde_json::Value::Array(a)) => a.first().and_then(json_str),
            Some(v) => json_str(v),
            None => None,
        },
        publisher: json_names(md.get("publisher")).into_iter().next(),
        categories: json_names(md.get("subject")),
        ..Default::default()
    };
    if let Some(links) = p.get("links").and_then(|l| l.as_array()) {
        e.links.extend(links.iter().filter_map(|l| json_link(l, base)));
    }
    if let Some(images) = p.get("images").and_then(|l| l.as_array()) {
        // The largest image is the cover, the smallest (if there are several) the thumbnail.
        let area = |l: &serde_json::Value| l.get("width").and_then(|w| w.as_u64()).unwrap_or(0) * l.get("height").and_then(|h| h.as_u64()).unwrap_or(0);
        let imgs: Vec<(u64, OpdsLink)> = images.iter().filter_map(|l| json_link(l, base).map(|k| (area(l), k))).collect();
        let largest = imgs.iter().enumerate().max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.0.cmp(&a.0))).map(|(i, _)| i);
        let smallest = imgs.iter().enumerate().min_by(|a, b| a.1 .0.cmp(&b.1 .0).then(a.0.cmp(&b.0))).map(|(i, _)| i);
        if let Some(i) = largest {
            let mut c = imgs[i].1.clone();
            c.rel = IMAGE_RELS[0].into();
            e.links.push(c);
        }
        if let Some(i) = smallest.filter(|_| imgs.len() > 1) {
            let mut t = imgs[i].1.clone();
            t.rel = THUMBNAIL_RELS[0].into();
            e.links.push(t);
        }
    }
    e
}

fn json_navigation(n: &serde_json::Value, base: &str) -> Option<OpdsEntry> {
    let mut l = json_link(n, base)?;
    if l.rel.is_empty() {
        l.rel = "subsection".into();
    }
    if l.mime.is_empty() {
        l.mime = "application/opds+json".into();
    }
    Some(OpdsEntry { id: l.href.clone(), title: l.title.clone().unwrap_or_default(), links: vec![l], ..Default::default() })
}

/// Parses a basic OPDS 2 feed: metadata, links, navigation, publications, groups and
/// facets.
pub fn parse_opds2(json: &str, base_url: &str) -> Result<OpdsFeed> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| NetError::Format(format!("opds2 json: {e}")))?;
    if !v.is_object() {
        return Err(NetError::Format("opds2: not a JSON object".into()));
    }
    let md = v.get("metadata").cloned().unwrap_or_default();
    let mut feed = OpdsFeed {
        id: md.get("identifier").and_then(json_str).unwrap_or_default(),
        title: md.get("title").and_then(json_str).unwrap_or_default(),
        subtitle: md.get("subtitle").and_then(json_str),
        updated: md.get("modified").and_then(json_str),
        total_results: md.get("numberOfItems").and_then(|n| n.as_u64()),
        items_per_page: md.get("itemsPerPage").and_then(|n| n.as_u64()),
        version: OpdsVersion::Json2,
        ..Default::default()
    };
    if let (Some(per), Some(page)) = (feed.items_per_page, md.get("currentPage").and_then(|n| n.as_u64())) {
        // Saturating: a hostile catalog may send page and size near u64::MAX.
        feed.start_index = Some(page.saturating_sub(1).saturating_mul(per).saturating_add(1));
    }
    if let Some(links) = v.get("links").and_then(|l| l.as_array()) {
        feed.links.extend(links.iter().filter_map(|l| json_link(l, base_url)));
    }
    let add_items = |obj: &serde_json::Value, feed: &mut OpdsFeed| {
        if let Some(nav) = obj.get("navigation").and_then(|n| n.as_array()) {
            feed.entries.extend(nav.iter().filter_map(|n| json_navigation(n, base_url)));
        }
        if let Some(pubs) = obj.get("publications").and_then(|n| n.as_array()) {
            feed.entries.extend(pubs.iter().map(|p| json_publication(p, base_url)));
        }
    };
    add_items(&v, &mut feed);
    if let Some(groups) = v.get("groups").and_then(|g| g.as_array()) {
        for g in groups {
            let title = g.get("metadata").and_then(|m| m.get("title")).and_then(json_str).unwrap_or_default();
            if let Some(more) = g.get("links").and_then(|l| l.as_array()).and_then(|a| a.iter().filter_map(|l| json_link(l, base_url)).find(|l| l.rel == "self")) {
                let mut l = more;
                l.rel = "subsection".into();
                if l.mime.is_empty() {
                    l.mime = "application/opds+json".into();
                }
                feed.entries.push(OpdsEntry { id: l.href.clone(), title: title.clone(), links: vec![l], ..Default::default() });
            }
            add_items(g, &mut feed);
        }
    }
    if let Some(facets) = v.get("facets").and_then(|f| f.as_array()) {
        for f in facets {
            let group = f.get("metadata").and_then(|m| m.get("title")).and_then(json_str);
            for l in f.get("links").and_then(|l| l.as_array()).into_iter().flatten() {
                if let Some(mut link) = json_link(l, base_url) {
                    link.active_facet = link.rel == "self";
                    link.rel = REL_FACET.into();
                    link.facet_group = group.clone();
                    feed.links.push(link);
                }
            }
        }
    }
    Ok(feed)
}

/// Parses Atom or JSON, chosen by MIME type, else by the first non-space byte.
pub fn parse_any(body: &[u8], mime: &str, base_url: &str) -> Result<OpdsFeed> {
    let text = String::from_utf8_lossy(body);
    let text = text.trim_start_matches('\u{feff}');
    let m = mime.to_ascii_lowercase();
    if m.contains("json") || (!m.contains("xml") && text.trim_start().starts_with('{')) {
        parse_opds2(text, base_url)
    } else {
        parse_feed(text, base_url)
    }
}

// ---------------------------------------------------------------------------------------
// OpenSearch.

/// A `<Url>` of an OpenSearch description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenSearchUrl {
    /// Template (resolved against the description URL).
    pub template: String,
    /// `type`.
    pub mime: String,
    /// `rel` (default `results`).
    pub rel: String,
}

/// Parses an OpenSearch description document.
pub fn parse_opensearch(xml: &str, base_url: &str) -> Result<Vec<OpenSearchUrl>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut out = Vec::new();
    let mut root_ok = false;
    loop {
        match reader.read_event().map_err(|e| NetError::Format(format!("opensearch xml: {e}")))? {
            Event::Start(e) | Event::Empty(e) => {
                let name = local(e.name().as_ref());
                if name == "OpenSearchDescription" {
                    root_ok = true;
                }
                if name == "Url" {
                    let a = attrs(&e);
                    if let Some(t) = attr(&a, "template") {
                        out.push(OpenSearchUrl {
                            template: resolve(base_url, t),
                            mime: attr(&a, "type").unwrap_or("").to_string(),
                            rel: attr(&a, "rel").unwrap_or("results").to_string(),
                        });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_ok {
        return Err(NetError::Format("not an OpenSearch description".into()));
    }
    Ok(out)
}

/// The template to use for catalog search: an Atom template (OPDS profile first), then
/// OPDS 2 JSON; `rel="results"` only.
pub fn pick_search_template(urls: &[OpenSearchUrl]) -> Option<&str> {
    let results = || urls.iter().filter(|u| u.rel == "results");
    results()
        .find(|u| u.mime.contains("atom+xml") && u.mime.contains("opds-catalog"))
        .or_else(|| results().find(|u| u.mime.contains("atom+xml")))
        .or_else(|| results().find(|u| u.mime.contains("opds+json")))
        .map(|u| u.template.as_str())
}

/// Fills a search template: `{searchTerms}` gets the percent-encoded terms; other
/// OpenSearch parameters get defaults (`startPage`/`startIndex` 1, encodings UTF-8,
/// `language` `*`), optional ones (`{name?}`) become empty; RFC 6570 query expansions
/// (`{?query}`, `{?query,title}`) become `?query=TERMS`.
pub fn fill_template(template: &str, terms: &str) -> String {
    let enc = encode_component(terms.trim());
    let mut out = String::with_capacity(template.len() + enc.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('}') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let inner = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        if let Some(vars) = inner.strip_prefix('?').or_else(|| inner.strip_prefix('&')) {
            let lead = if inner.starts_with('?') { '?' } else { '&' };
            let names: Vec<&str> = vars.split(',').map(str::trim).collect();
            if let Some(n) = names.iter().find(|n| matches!(**n, "query" | "searchTerms" | "q" | "search")) {
                out.push(lead);
                out.push_str(&format!("{n}={enc}"));
            }
            continue;
        }
        let optional = inner.ends_with('?');
        let name = inner.trim_end_matches('?');
        let local_name = name.rsplit(':').next().unwrap_or(name);
        let value = match local_name {
            "searchTerms" | "query" => enc.clone(),
            "startPage" | "startIndex" if !optional => "1".into(),
            "inputEncoding" | "outputEncoding" if !optional => "UTF-8".into(),
            "language" if !optional => "*".into(),
            _ => String::new(),
        };
        out.push_str(&value);
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------
// Client.

/// `Accept` for catalog requests.
pub const OPDS_ACCEPT: &str = "application/atom+xml;profile=opds-catalog, application/opds+json;q=0.9, application/atom+xml;q=0.8, application/xml;q=0.7, */*;q=0.1";

/// Fetches and searches catalogs.
pub struct OpdsClient<'a> {
    /// Transport.
    pub http: &'a dyn HttpClient,
    /// Basic auth (Calibre-Web and other private catalogs).
    pub auth: Option<(String, String)>,
    /// Largest feed accepted.
    pub max_bytes: usize,
}

impl std::fmt::Debug for OpdsClient<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpdsClient").field("auth", &self.auth.as_ref().map(|(u, _)| u)).field("max_bytes", &self.max_bytes).finish_non_exhaustive()
    }
}

impl<'a> OpdsClient<'a> {
    /// A client without credentials, 16 MB feed limit.
    pub fn new(http: &'a dyn HttpClient) -> Self {
        OpdsClient { http, auth: None, max_bytes: 16 << 20 }
    }

    fn request(&self, url: &str) -> Request {
        let r = Request::get(url).header("Accept", OPDS_ACCEPT);
        match &self.auth {
            Some((u, p)) => r.basic_auth(u, p),
            None => r,
        }
    }

    /// Fetches and parses a feed; links resolve against the final URL.
    pub fn fetch(&self, url: &str) -> Result<OpdsFeed> {
        let resp = self.http.send(self.request(url))?.error_for_status()?;
        let mime = resp.mime();
        let base = resp.url.clone();
        let body = resp.bytes(self.max_bytes)?;
        parse_any(&body, &mime, &base)
    }

    /// The next page of `feed`, if it has one.
    pub fn next_page(&self, feed: &OpdsFeed) -> Result<Option<OpdsFeed>> {
        match feed.next_url() {
            Some(u) => self.fetch(u).map(Some),
            None => Ok(None),
        }
    }

    /// Follows `next` links from `url`, at most `max_pages` feeds, collecting entries.
    pub fn fetch_pages(&self, url: &str, max_pages: usize) -> Result<Vec<OpdsFeed>> {
        let mut out = vec![self.fetch(url)?];
        let mut seen = vec![url.to_string()];
        while out.len() < max_pages {
            let Some(next) = out.last().and_then(|f| f.next_url()).map(str::to_string) else { break };
            if seen.contains(&next) {
                break;
            }
            seen.push(next.clone());
            out.push(self.fetch(&next)?);
        }
        Ok(out)
    }

    /// The search URL template for `feed` (fetching the OpenSearch description if needed).
    pub fn search_template(&self, feed: &OpdsFeed) -> Result<Option<String>> {
        match feed.search() {
            None => Ok(None),
            Some(SearchLink::Template(t)) => Ok(Some(t)),
            Some(SearchLink::Description(u)) => {
                let resp = self.http.send(self.request(&u))?.error_for_status()?;
                let base = resp.url.clone();
                let xml = resp.text(1 << 20)?;
                let urls = parse_opensearch(&xml, &base)?;
                Ok(pick_search_template(&urls).map(str::to_string))
            }
        }
    }

    /// Searches the catalog `feed` belongs to.
    pub fn search(&self, feed: &OpdsFeed, terms: &str) -> Result<OpdsFeed> {
        let t = self.search_template(feed)?.ok_or_else(|| NetError::Unsupported("this catalog has no search".into()))?;
        self.fetch(&fill_template(&t, terms))
    }

    /// A [`Download`] of `link` into `dir`, named after the entry title and the file type,
    /// carrying this client's credentials.
    pub fn download_for(&self, entry: &OpdsEntry, link: &OpdsLink, dir: &Path) -> Download {
        let ext = extension_for_mime(link.final_mime()).unwrap_or("");
        let base = if entry.title.trim().is_empty() { crate::url::last_segment(&link.href) } else { entry.title.clone() };
        let name = sanitize_filename(&format!("{}{ext}", base.trim()));
        // The catalog's size confirms a body the server sends without a length.
        let mut d = Download::new(&link.href, dir.join(name)).expected_len(link.length);
        if let Some((u, p)) = &self.auth {
            let r = Request::get("").basic_auth(u, p);
            if let Some(v) = r.header_value("authorization") {
                d = d.header("Authorization", v);
            }
        }
        d
    }
}

#[cfg(test)]
mod tests;
