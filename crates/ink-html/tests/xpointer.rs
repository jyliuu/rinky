//! xpointers against KOReader's crengine.
//!
//! `tests/crengine/<book>.json` holds what KOReader's crengine (DOM version 20240114) reports
//! for each book: every visible word start (every 7th for juliet) as an xpointer plus the
//! word, and the TOC xpointers. For each word this test checks that
//! 1. the xpointer resolves in our DOM to text that starts with that word, and
//! 2. generating an xpointer for the resolved node and offset gives the same string.
//!
//! Regenerate the expected files with `tests/crengine/dump-all.sh`.

mod common;

use ink_doc::XPointer;
use ink_html::xpointer::{char_to_byte, resolve, to_xpointer, DomPos};

fn check(name: &str) {
    check_fragments(name, |_| true);
}

/// Like [`check`], for the words and TOC entries in the DocFragments `keep` accepts.
fn check_fragments(name: &str, keep: impl Fn(u32) -> bool) {
    let json_path = common::manifest_dir().join(format!("tests/crengine/{name}.json"));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    let book = common::book(&common::book_path(name));
    let mut failures = Vec::new();
    let mut checked = 0;
    let words = json["words"].as_array().unwrap();
    for w in words {
        let xs = w[0].as_str().unwrap();
        let word = w[1].as_str().unwrap();
        let xp: XPointer = xs.parse().unwrap_or_else(|e| panic!("{xs}: {e}"));
        let frag = xp.fragment().unwrap();
        if !keep(frag) {
            continue;
        }
        let Some(ci) = book.chapter_of_fragment(frag) else {
            failures.push(format!("{xs}: no DocFragment {frag}"));
            continue;
        };
        let ch = book.chapter(ci);
        let Some(pos) = resolve(&ch.dom, &xp) else {
            failures.push(format!("{xs}: does not resolve"));
            continue;
        };
        if let DomPos::Text { node, offset } = pos {
            let t = ch.dom.text(node).unwrap();
            let at = &t[char_to_byte(t, offset)..];
            // crengine's word text drops soft hyphens.
            let at: String = at.chars().filter(|c| *c != '\u{ad}').collect();
            if !at.starts_with(word) {
                failures.push(format!("{xs}: expected {word:?}, text is {:?}", at.chars().take(30).collect::<String>()));
            }
        }
        let back = to_xpointer(&ch.dom, frag, book.fragments, pos).to_string();
        if back != xs {
            failures.push(format!("{xs}: regenerated as {back}"));
        }
        checked += 1;
    }
    for t in json["toc"].as_array().unwrap() {
        let xs = t["xpointer"].as_str().unwrap();
        let xp: XPointer = xs.parse().unwrap_or_else(|e| panic!("{xs}: {e}"));
        if !keep(xp.fragment().unwrap()) {
            continue;
        }
        let ci = book.chapter_of_fragment(xp.fragment().unwrap()).unwrap();
        let ch = book.chapter(ci);
        match resolve(&ch.dom, &xp) {
            Some(pos) if xp.steps.len() > 2 => {
                let back = to_xpointer(&ch.dom, xp.fragment().unwrap(), book.fragments, pos).to_string();
                if back != xs {
                    failures.push(format!("toc {xs}: regenerated as {back}"));
                }
            }
            Some(_) => {}
            None => failures.push(format!("toc {xs}: does not resolve")),
        }
    }
    let shown: Vec<_> = failures.iter().take(25).collect();
    assert!(failures.is_empty(), "{name}: {} of {checked} words fail:\n{shown:#?}", failures.len());
    assert!(checked > 0);
}

#[test]
fn ws_fixture_matches_crengine() {
    check("ws");
}

#[test]
fn juliet_matches_crengine() {
    check("juliet");
}

#[test]
fn feature_fixtures_match_crengine() {
    for name in ["css-selectors", "css-text", "css-box", "fonts", "images", "tables-lists", "notes", "lang", "pagelist"] {
        check(name);
    }
}

/// TOC and page-list targets: the document's positions equal crengine's strings.
#[test]
fn toc_and_page_map_strings_match_crengine() {
    use ink_doc::Document;
    for name in ["ws", "css-box", "tables-lists", "notes", "pagelist", "juliet"] {
        let json_path = common::manifest_dir().join(format!("tests/crengine/{name}.json"));
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        let mut d = ink_formats::open(&common::book_path(name)).unwrap();
        d.set_layout(&ink_doc::LayoutSettings::for_screen(1272, 1696)).unwrap();
        let toc: Vec<String> = d.toc().iter().map(|t| t.position.to_string()).collect();
        let ko: Vec<String> = json["toc"].as_array().unwrap().iter().map(|t| t["xpointer"].as_str().unwrap().to_string()).collect();
        assert_eq!(toc, ko, "{name} toc");
        let labels: Vec<(String, String)> = d.page_labels().into_iter().map(|(l, p)| (l, p.to_string())).collect();
        let ko: Vec<(String, String)> = json["page_map"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["label"].as_str().unwrap().to_string(), t["xpointer"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(labels, ko, "{name} page map");
    }
}

/// Broken markup goes through html5ever, whose tree differs from crengine's lenient
/// parser for tag soup (crengine nests the content after an unclosed `<p>` or `<br>`
/// inside it); only the well-formed chapters of malformed.epub must match.
#[test]
fn malformed_wellformed_chapters_match_crengine() {
    // DocFragment[1] is html5.xhtml (tag soup). [2] (nobody.xhtml: no <body>, unclosed
    // <em>) is rejected by crengine, which shows its own "Failed handling EPUB spine item"
    // <pre> page there; ink-html shows the text. [3] (CDATA, comment, processing
    // instruction, custom element) must match.
    check_fragments("malformed", |f| f == 3);
}

#[test]
#[ignore = "informational: prints how far html5ever's tree is from crengine's for tag soup"]
fn malformed_vs_crengine() {
    check("malformed");
}
