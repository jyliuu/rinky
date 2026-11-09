//! Document-level behaviour on EPUBs: positions (xpointers, stability, round trips,
//! crengine comparison), pagination (background thread, page-map cache), Kindle
//! locations, page labels, TOC, search, read_around, links, notes, selection, images,
//! tables and lists.

mod common;

use ink_device::{Canvas, Point};
use ink_doc::{Document, FlowAnchor, LayoutSettings, LinkTarget, NoteKind, Position, XPointer};
use ink_html::layout::{Draw, TextRun};
use ink_html::HtmlDocument;
use std::time::Duration;

fn open(name: &str) -> HtmlDocument {
    ink_formats::open(&common::book_path(name)).unwrap()
}

fn layout(d: &mut HtmlDocument, s: &LayoutSettings) {
    d.set_layout(s).unwrap();
    assert!(d.wait_paginated(Duration::from_secs(300)));
}

fn page_runs(d: &HtmlDocument, page: usize) -> Vec<TextRun> {
    let (l, span) = d.page_layout(page).unwrap();
    let a = l.slices[span.first as usize].first_item as usize;
    let b = l.slices[span.end as usize - 1].end_item as usize;
    l.items[a..b]
        .iter()
        .filter_map(|i| match i {
            Draw::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// The page's text: runs that touch are joined directly (words split at hyphens or
/// style changes), others with a space.
fn page_text(d: &HtmlDocument, page: usize) -> String {
    let mut out = String::new();
    let mut prev: Option<TextRun> = None;
    for t in page_runs(d, page) {
        if let Some(p) = &prev {
            let touching = p.line_top == t.line_top && (t.x - (p.x + p.width)).abs() < 1.5;
            if !touching {
                out.push(' ');
            }
        }
        out.push_str(&t.text);
        prev = Some(t);
    }
    out
}

#[test]
fn positions_round_trip_on_every_page() {
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let n = d.page_count().pages;
    assert!(n > 150);
    for p in 0..n {
        let pos = d.position_of_page(p).unwrap();
        let s = pos.to_string();
        assert!(s.starts_with("/body/DocFragment["), "{s}");
        let back: Position = s.parse().unwrap();
        assert_eq!(d.page_of(&back).unwrap(), p, "{s}");
    }
}

#[test]
fn crengine_xpointers_land_on_the_right_page() {
    // Word positions computed by KOReader's crengine (tests/crengine/juliet.json).
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(common::manifest_dir().join("tests/crengine/juliet.json")).unwrap()).unwrap();
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let mut checked = 0;
    for w in json["words"].as_array().unwrap().iter().step_by(23) {
        let x: Position = w[0].as_str().unwrap().parse().unwrap();
        let word = w[1].as_str().unwrap();
        let p = d.page_of(&x).unwrap();
        let text = page_text(&d, p);
        assert!(text.contains(word), "{x} {word:?} not on page {p}: {text}");
        checked += 1;
    }
    assert!(checked > 100);
    // KOReader's TOC xpointers (element points) resolve to the chapter starts.
    for t in json["toc"].as_array().unwrap() {
        let x: Position = t["xpointer"].as_str().unwrap().parse().unwrap();
        let title = t["title"].as_str().unwrap();
        let p = d.page_of(&x).unwrap();
        let first_word = title.split_whitespace().next().unwrap();
        let text = page_text(&d, p);
        if title != "Title" && title != "About" {
            assert!(text.contains(first_word), "{x} {title}: page {p}: {text}");
        }
    }
}

#[test]
fn toc_matches_crengine_and_has_pages() {
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(common::manifest_dir().join("tests/crengine/juliet.json")).unwrap()).unwrap();
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let toc = d.toc();
    let ko = json["toc"].as_array().unwrap();
    assert_eq!(toc.len(), ko.len());
    for (ours, theirs) in toc.iter().zip(ko) {
        assert_eq!(ours.title, theirs["title"].as_str().unwrap());
        assert_eq!(ours.level as u64 + 1, theirs["depth"].as_u64().unwrap());
        assert_eq!(ours.position.to_string(), theirs["xpointer"].as_str().unwrap(), "{}", ours.title);
        assert!(ours.page.is_some());
    }
    let pages: Vec<usize> = toc.iter().map(|t| t.page.unwrap()).collect();
    assert!(pages.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn old_flow_positions_still_open() {
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    // flow:chapter.block.word from the phase 0 engine: chapter 2 (counting chapters with
    // text from 0: title, about, Act I) is Act I's prologue.
    let p = d.page_of(&Position::Flow(FlowAnchor { spine: 2, block: 2, word: 3 })).unwrap();
    assert!(page_text(&d, p).contains("households"), "{}", page_text(&d, p));
    // The cover (no text) was not a chapter for the phase 0 engine: flow:0 is the title page.
    let title = d.page_of(&"flow:0.0.0".parse().unwrap()).unwrap();
    assert!(page_text(&d, title).contains("Published"), "{}", page_text(&d, title));
}

#[test]
fn background_pagination_and_page_map_cache() {
    let dir = std::env::temp_dir().join(format!("ink-html-pagemap-{}", std::process::id()));
    let book = common::book_path("juliet");
    let id = ink_store::partial_md5(&book).unwrap();
    let s = LayoutSettings::for_screen(1272, 1696);
    let mut d = open("juliet");
    d.set_page_cache(&dir, &id);
    d.set_layout(&s).unwrap();
    let first = d.page_count();
    assert!(first.pages >= 1);
    assert!(d.wait_paginated(Duration::from_secs(300)));
    let full = d.page_count();
    assert!(full.complete && full.pages >= first.pages);
    assert!(!d.cache_hit());
    // The cache file is written before wait_paginated returns.
    let file = ink_html::paginate::cache_file(&dir, &id, &s);
    assert!(file.exists(), "{}", file.display());
    let positions: Vec<String> = (0..full.pages).step_by(20).map(|p| d.position_of_page(p).unwrap().to_string()).collect();
    // A second open reads the cache: complete at once, same pages.
    let mut d2 = open("juliet");
    d2.set_page_cache(&dir, &id);
    d2.set_layout(&s).unwrap();
    assert!(d2.cache_hit());
    assert_eq!(d2.page_count(), full);
    let positions2: Vec<String> = (0..full.pages).step_by(20).map(|p| d2.position_of_page(p).unwrap().to_string()).collect();
    assert_eq!(positions, positions2);
    // Other layout settings use another cache file.
    let mut s3 = s.clone();
    s3.font_px = 30.0;
    d2.set_layout(&s3).unwrap();
    assert!(!d2.cache_hit());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn relayout_keeps_the_reading_position() {
    let mut d = open("juliet");
    let mut s = LayoutSettings::for_screen(1272, 1696);
    layout(&mut d, &s);
    d.set_current_page(100);
    let before = d.current_position().unwrap();
    let words: Vec<String> = page_runs(&d, 100).iter().take(3).map(|t| t.text.clone()).collect();
    s.font_px = 30.0;
    d.set_layout(&s).unwrap();
    let now = d.current_page();
    assert_eq!(now, d.page_of(&before).unwrap());
    let text = page_text(&d, now);
    assert!(words.iter().all(|w| text.contains(w.as_str())), "{words:?} not in {text}");
}

#[test]
fn locations_and_page_labels() {
    let mut d = open("pagelist");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let labels = d.page_labels();
    assert_eq!(labels.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), vec!["i", "ii", "iii", "1", "2", "3"]);
    let pages: Vec<usize> = labels.iter().map(|l| d.page_of(&l.1).unwrap()).collect();
    assert!(pages.windows(2).all(|w| w[0] <= w[1]), "{pages:?}");
    assert!(labels[0].1.to_string().starts_with("/body/DocFragment[1]/body/p[1]/span"), "{}", labels[0].1);
    for (label, pos) in &labels {
        let p = d.page_of(pos).unwrap();
        assert!(page_text(&d, p).contains(&format!("Printed page {label}")), "{label}");
    }
    // Kindle locations: 128 bytes of text each, increasing through the book.
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let n = d.page_count().pages;
    let locs: Vec<u32> = (0..n).map(|p| d.location(&d.position_of_page(p).unwrap()).unwrap().0).collect();
    assert_eq!(locs[0], 1);
    assert!(locs.windows(2).all(|w| w[0] <= w[1]));
    let last = d.location_count().unwrap().0;
    assert!(last > 1000 && last >= *locs.last().unwrap(), "{last}");
    // Text bytes / 128: juliet has about 150 KB of text.
    assert!((1000..1600).contains(&last), "{last}");
}

#[test]
fn search_read_around_current_position() {
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let hits = d.search("star-cross'd", 10).unwrap();
    assert_eq!(hits.len(), 1);
    let h = &hits[0];
    assert_eq!(&h.excerpt[h.matched.clone()], "star-cross'd");
    let p = h.page.unwrap();
    assert!(page_text(&d, p).contains("star-cross'd"));
    assert_eq!(d.page_of(&h.position).unwrap(), p);
    let case = d.search("ROMEO AND JULIET", 3).unwrap();
    assert!(!case.is_empty());
    assert!(d.search("   ", 5).unwrap().is_empty());
    let around = d.read_around(&h.position, 300).unwrap();
    assert!(around.contains("star-cross'd") && around.chars().count() >= 280, "{around}");
    d.set_current_page(p);
    assert_eq!(d.current_page(), p);
    let cur = d.current_position().unwrap();
    assert_eq!(d.page_of(&cur).unwrap(), p);
}

#[test]
fn links_footnotes_endnotes() {
    let mut d = open("notes");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let links = d.links(&d.position_of_page(0).unwrap()).unwrap();
    let by_text = |t: &str| links.iter().find(|l| l.text == t).unwrap_or_else(|| panic!("{t} in {links:?}"));
    let fn1 = by_text("1");
    assert!(fn1.footnote && fn1.rect.is_some());
    let LinkTarget::Internal(t1) = &fn1.target else { panic!() };
    let n1 = d.note(t1).unwrap().unwrap();
    assert_eq!(n1.kind, NoteKind::Footnote);
    assert!(n1.text.contains("The footnote text"), "{}", n1.text);
    let fn2 = by_text("2");
    assert!(fn2.footnote);
    let LinkTarget::Internal(t2) = &fn2.target else { panic!() };
    assert!(t2.to_string().starts_with("/body/DocFragment[2]/"), "{t2}");
    let n2 = d.note(t2).unwrap().unwrap();
    assert_eq!(n2.kind, NoteKind::Endnote);
    assert!(n2.text.contains("second note"));
    let fn3 = by_text("[3]");
    assert!(fn3.footnote, "short bracketed text");
    let plain = by_text("the notes chapter");
    assert!(!plain.footnote);
    let LinkTarget::Internal(tp) = &plain.target else { panic!() };
    assert_eq!(tp.to_string(), "/body/DocFragment[2].0");
    let ext = by_text("Standard Ebooks");
    assert_eq!(ext.target, LinkTarget::External("https://standardebooks.org/".into()));
    assert!(!ext.footnote);
}

#[test]
fn selection_by_points() {
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let p = 6;
    let runs = page_runs(&d, p);
    let (a, b) = (&runs[3], &runs[8]);
    let dy = 70.0 - d.page_layout(p).unwrap().1.y0;
    let from = Point::new((a.x + 2.0) as i32, ((a.line_top + a.line_bottom) / 2.0 + dy) as i32);
    let to = Point::new((b.x + b.width - 2.0) as i32, ((b.line_top + b.line_bottom) / 2.0 + dy) as i32);
    let sel = d.select(p, from, to).unwrap().unwrap();
    assert!(sel.text.starts_with(a.text.trim_end_matches('-')), "{} vs {}", sel.text, a.text);
    assert!(sel.text.contains(b.text.trim_end_matches(['-', ',', '.', ';'])), "{}", sel.text);
    assert!(!sel.rects.is_empty());
    for pos in [&sel.start, &sel.end] {
        let Position::XPointer(x) = pos else { panic!() };
        assert!(x.text.is_some(), "{x}");
        assert_eq!(d.page_of(pos).unwrap(), p);
    }
    assert_eq!(d.selection(), Some(&sel));
    // Backwards selection gives the same range.
    let back = d.select(p, to, from).unwrap().unwrap();
    assert_eq!((back.start, back.end), (sel.start.clone(), sel.end.clone()));
    d.clear_selection();
    assert!(d.selection().is_none());
}

/// A word the layout hyphenated across two lines ("in-" | "finities") is one word in
/// the selected text: no added hyphen, no space or newline in the middle.
#[test]
fn selection_joins_a_word_split_by_hyphenation() {
    let mut d = open("juliet");
    let mut s = LayoutSettings::for_screen(1272, 1696);
    s.hyphenate = true;
    layout(&mut d, &s);
    let (p, i, runs) = (0..d.page_count().pages)
        .find_map(|p| {
            let runs = page_runs(&d, p);
            let i = runs.iter().position(|t| t.hyphenated && t.text.chars().count() > 3)?;
            (i + 1 < runs.len()).then_some((p, i, runs))
        })
        .expect("a hyphenated word in the book");
    let (a, b) = (&runs[i], &runs[i + 1]);
    assert!(a.text.ends_with('-') && b.line_top > a.line_top, "{} | {}", a.text, b.text);
    let dy = 70.0 - d.page_layout(p).unwrap().1.y0;
    let from = Point::new((a.x + 2.0) as i32, ((a.line_top + a.line_bottom) / 2.0 + dy) as i32);
    let to = Point::new((b.x + b.width - 2.0) as i32, ((b.line_top + b.line_bottom) / 2.0 + dy) as i32);
    let sel = d.select(p, from, to).unwrap().unwrap();
    let word = format!("{}{}", a.text.trim_end_matches('-'), b.text);
    assert_eq!(sel.text, word.trim(), "{:?} + {:?}", a.text, b.text);
}

#[test]
fn images_scaled_and_dithered() {
    let mut d = open("images");
    let s = LayoutSettings::for_screen(1272, 1696);
    layout(&mut d, &s);
    let content_w = 1272.0 - 140.0;
    let content_h = 1696.0 - 180.0;
    let mut kinds = Vec::new();
    for p in 0..d.page_count().pages {
        let (l, span) = d.page_layout(p).unwrap();
        let a = l.slices[span.first as usize].first_item as usize;
        let b = l.slices[span.end as usize - 1].end_item as usize;
        let mut c = Canvas::new(1272, 1696);
        d.render_page(p, &mut c).unwrap();
        for it in &l.items[a..b] {
            if let Draw::Image(i) = it {
                assert!(i.w <= content_w + 0.5 && i.h <= content_h + 0.5, "{} {}x{}", i.src, i.w, i.h);
                kinds.push(i.src.rsplit('.').next().unwrap().to_string());
                // Pixels inside the image are on the 16-level gray scale.
                let dy = 70.0 - span.y0;
                let (x0, y0) = (i.x as usize + 2, (i.y + dy) as usize + 2);
                let (x1, y1) = ((i.x + i.w) as usize - 2, (i.y + dy + i.h) as usize - 2);
                for y in (y0..y1).step_by(7) {
                    for x in (x0..x1).step_by(7) {
                        let v = c.pixels[y * 1272 + x];
                        assert_eq!(v % 17, 0, "{} pixel {v} at {x},{y}", i.src);
                    }
                }
            }
        }
    }
    for k in ["jpg", "png", "gif", "webp", "svg"] {
        assert!(kinds.iter().any(|x| x == k), "{k} in {kinds:?}");
    }
    assert!(kinds.iter().any(|x| x.contains("#svg:") || x.starts_with('#')), "inline svg in {kinds:?}");
}

#[test]
fn tables_lists_quotes_pre() {
    let mut d = open("tables-lists");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let r0 = page_runs(&d, 0);
    let find = |rs: &[TextRun], t: &str| rs.iter().find(|r| r.text == t).cloned().unwrap_or_else(|| panic!("{t}"));
    let (romeo, verona) = (find(&r0, "Romeo"), find(&r0, "Verona"));
    assert_eq!(romeo.baseline, verona.baseline, "cells of a row share a baseline");
    assert!(verona.x > romeo.x + romeo.width);
    let (l, _) = d.page_layout(0).unwrap();
    assert!(l.decor.len() > 20, "cell borders");
    // The long table continues on the next pages with its rows intact.
    let rows: usize = (0..d.page_count().pages).map(|p| page_runs(&d, p).iter().filter(|t| t.text == "units").count()).sum();
    assert_eq!(rows, 40);
    let lists = (0..d.page_count().pages).find(|&p| page_text(&d, p).contains("Lists")).unwrap();
    let rl = page_runs(&d, lists);
    for m in ["•", "1.", "7.", "8.", "IV.", "V.", "a.", "b."] {
        assert!(rl.iter().any(|t| t.text == m), "marker {m}");
    }
    let quote = (0..d.page_count().pages).find(|&p| page_text(&d, p).contains("block quote")).unwrap();
    let rq = page_runs(&d, quote);
    let qa = find(&rq, "A");
    assert!(qa.x > 70.0 + 2.0 * 40.0 - 1.0, "blockquote indent {}", qa.x);
    let pre = rq.iter().find(|t| t.text.contains("println!")).unwrap();
    assert_eq!(pre.face.family, 1, "pre is monospace");
    assert!(page_text(&d, quote).contains("keeps    spacing"));
}

#[test]
fn malformed_markup_opens() {
    let mut d = open("malformed");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let all: String = (0..d.page_count().pages).map(|p| page_text(&d, p)).collect::<Vec<_>>().join(" ");
    for w in ["soup", "Unclosed", "void", "ampersand,", "©", "two", "cell", "Self-closing", "body", "CDATA:", "<not a tag>", "custom"] {
        assert!(all.contains(w), "{w} missing from {all}");
    }
    let x: XPointer = d.position_of_page(0).unwrap().to_string().parse().unwrap();
    assert_eq!(x.fragment(), Some(1));
}

/// The notes behind each footnote reference of a one-chapter book (plus a notes chapter
/// `n.xhtml`), by the reference's text.
fn notes_of(text: &str, notes: &str) -> Vec<(String, ink_doc::Note)> {
    use ink_doc::{MemoryLoader, Metadata};
    use ink_html::HtmlChapter;
    let chapters = vec![
        HtmlChapter { path: "a.xhtml".into(), html: format!("<html xmlns:epub=\"http://www.idpf.org/2007/ops\"><body>{text}</body></html>") },
        HtmlChapter { path: "n.xhtml".into(), html: format!("<html xmlns:epub=\"http://www.idpf.org/2007/ops\"><body>{notes}</body></html>") },
    ];
    let mut d = HtmlDocument::new(Metadata::default(), chapters, Box::new(MemoryLoader::default())).unwrap();
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let links = d.links(&d.position_of_page(0).unwrap()).unwrap();
    links
        .into_iter()
        .filter(|l| l.footnote)
        .map(|l| {
            let LinkTarget::Internal(t) = &l.target else { panic!("{l:?}") };
            let n = d.note(t).unwrap().unwrap_or_else(|| panic!("no note for {l:?}"));
            (l.text.clone(), n)
        })
        .collect()
}

fn spans(n: &ink_doc::Note) -> Vec<(&str, bool, bool)> {
    n.spans.iter().map(|s| (s.text.as_str(), s.bold, s.italic)).collect()
}

#[test]
fn note_text_as_the_book_prints_it() {
    let text = r##"<p>EPUB 3<a epub:type="noteref" href="#fn1">1</a>, classic<a href="n.xhtml#fn2" id="ref2"><sup>2</sup></a>,
        bracketed<a href="n.xhtml#fn3">[3]</a>, sup<sup><a href="n.xhtml#fn4">4</a></sup>, pandoc<a href="n.xhtml#fn5" role="doc-noteref"><sup>5</sup></a>,
        dotted<a href="n.xhtml#fn6">6</a>, plain<a href="n.xhtml#fn7">7</a>, star<a href="n.xhtml#fn8">*</a>.</p>
        <aside epub:type="footnote" id="fn1"><p>1. The <i>first</i> note.</p></aside>"##;
    let notes = r##"<p id="fn2"><a href="a.xhtml#ref2">2</a> Classic note with <em>emphasis</em> and <b>bold</b>.</p>
        <p id="fn3"><a href="a.xhtml#r3">[3]</a> Bracketed backlink.</p>
        <div class="footnote" id="fn4"><p><sup>4</sup> Sup number, <cite>A Book</cite>.</p></div>
        <section role="doc-endnotes"><ol><li id="fn5"><p>Pandoc note. <a href="a.xhtml#fnref5" role="doc-backlink">↩︎</a></p></li></ol></section>
        <p id="fn6">[6] Bracketed text number, then 1984 stays.</p>
        <p id="fn7">7 Bare number in the text.</p>
        <p id="fn8">* Starred note <a href="a.xhtml#x">↩</a></p>"##;
    let n = notes_of(text, notes);
    let by = |t: &str| n.iter().find(|(l, _)| l == t).map(|(_, n)| n.clone()).unwrap_or_else(|| panic!("{t} in {n:?}"));

    // An aside whose first child is a paragraph: no empty paragraph before the text.
    let n1 = by("1");
    assert_eq!(n1.text, "1. The first note.");
    assert_eq!(spans(&n1), [("1. The ", false, false), ("first", false, true), (" note.", false, false)]);
    let n2 = by("2");
    assert_eq!(n2.text, "2 Classic note with emphasis and bold.");
    assert_eq!(spans(&n2), [("2 Classic note with ", false, false), ("emphasis", false, true), (" and ", false, false), ("bold", true, false), (".", false, false)]);
    assert_eq!(by("[3]").text, "[3] Bracketed backlink.");
    // A <sup> number stays inline.
    let n4 = by("4");
    assert_eq!(n4.text, "4 Sup number, A Book.");
    assert_eq!(n4.spans[1].text, "A Book");
    assert!(n4.spans[1].italic);
    // A backlink marked role="doc-backlink" goes.
    assert_eq!(by("5").text, "Pandoc note.");
    assert_eq!(by("6").text, "[6] Bracketed text number, then 1984 stays.");
    assert_eq!(by("7").text, "7 Bare number in the text.");
    // An unmarked arrow link stays: only marked backlinks go.
    assert_eq!(by("*").text, "* Starred note ↩");
}

#[test]
fn deep_nesting_does_not_overflow() {
    use ink_doc::{MemoryLoader, Metadata};
    use ink_html::HtmlChapter;
    let n = 3000;
    let xml = format!("<html><body>{}deep text{}</body></html>", "<div><span>".repeat(n), "</span></div>".repeat(n));
    let soup = format!("<body>{}soup text", "<div><b><i>".repeat(n));
    for html in [xml, soup] {
        let chapters = vec![HtmlChapter { path: "a.xhtml".into(), html }];
        let mut d = HtmlDocument::new(Metadata::default(), chapters, Box::new(MemoryLoader::default())).unwrap();
        layout(&mut d, &LayoutSettings::for_screen(600, 800));
        let all: String = (0..d.page_count().pages).map(|p| page_text(&d, p)).collect();
        assert!(all.contains("text"), "{all}");
        let mut c = Canvas::new(600, 800);
        d.render_page(0, &mut c).unwrap();
        assert!(!d.position_of_page(0).unwrap().to_string().is_empty());
    }
}

#[test]
fn range_rects_match_the_selection_and_mark_search_hits() {
    let mut d = open("juliet");
    layout(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let p = 6;
    let runs = page_runs(&d, p);
    let (a, b) = (&runs[3], &runs[8]);
    let dy = 70.0 - d.page_layout(p).unwrap().1.y0;
    let from = Point::new((a.x + 2.0) as i32, ((a.line_top + a.line_bottom) / 2.0 + dy) as i32);
    let to = Point::new((b.x + b.width - 2.0) as i32, ((b.line_top + b.line_bottom) / 2.0 + dy) as i32);
    let sel = d.select(p, from, to).unwrap().unwrap();
    assert_eq!(d.range_rects(p, &sel.start, &sel.end), sel.rects);
    // Reversed arguments give the same; other pages have none of it.
    assert_eq!(d.range_rects(p, &sel.end, &sel.start), sel.rects);
    assert!(d.range_rects(p + 1, &sel.start, &sel.end).is_empty());
    assert!(d.range_rects(p, &sel.start, &sel.start).is_empty());
    // After a layout change the same range maps to its new page.
    let mut big = LayoutSettings::for_screen(1272, 1696);
    big.font_px *= 1.5;
    layout(&mut d, &big);
    let np = d.page_of(&sel.start).unwrap();
    let rs = d.range_rects(np, &sel.start, &sel.end);
    assert!(!rs.is_empty());
    let ep = d.page_of(&sel.end).unwrap();
    if ep != np {
        assert!(!d.range_rects(ep, &sel.start, &sel.end).is_empty());
    }
    // A search hit: one rectangle about the width of the word.
    let hits = d.search("Romeo", 40).unwrap();
    assert!(!hits.is_empty());
    for h in &hits {
        let hp = d.page_of(&h.position).unwrap();
        let rs = d.hit_rects(hp, h);
        assert_eq!(rs.len(), 1, "{h:?}");
        assert!(rs[0].w > 40 && rs[0].w < 400, "{:?}", rs[0]);
        // The whole word: as wide as a selection of it.
        let mid = rs[0].y + rs[0].h as i32 / 2;
        let sel = d.select(hp, Point::new(rs[0].x + 2, mid), Point::new(rs[0].right() - 2, mid)).unwrap().unwrap();
        assert!(sel.text.to_lowercase().starts_with("romeo"), "{:?} {h:?}", sel.text);
    }
}

/// The page map lays chapters out without the draw items of their lines: its page starts
/// must be the starts of the pages the reader draws from the full layout, for books with
/// text, images, tables, lists, notes and embedded fonts, with and without hyphenation.
#[test]
fn page_map_layout_has_the_full_layouts_page_starts() {
    use ink_html::paginate::{chapter_pages, layout_chapter, paginate};
    for name in ["juliet", "tables-lists", "images", "css-box", "css-text", "notes", "ws", "lang", "fonts", "pagelist"] {
        let path = common::book_path(name);
        let (meta, chapters, loader) = common::spine(&path);
        let book = ink_html::book::Book::new(meta, chapters, &loader);
        for (px, hyphenate) in [(28.0, false), (44.0, true)] {
            let mut s = LayoutSettings::for_screen(1072, 1448);
            s.font_px = px;
            s.hyphenate = hyphenate;
            let params = ink_html::layout::Params::new(&s);
            let mut fonts = ink_html::fonts::FontBook::builtin();
            let mut size = |p: &str| book.image_size(p, Some(&loader)).flatten();
            let mut env = ink_html::layout::Env { fonts: &mut fonts, image_size: &mut size };
            for i in 0..book.chapters.len() {
                let full = paginate(&layout_chapter(&book, i, &params, &mut env), params.content_h);
                let map = chapter_pages(&book, i, &params, &mut env);
                assert_eq!(map.keys, full.iter().map(|p| p.key).collect::<Vec<_>>(), "{name} spine item {i} at {px} px");
            }
        }
    }
}

/// Notes the way books really write them, found as KOReader finds them (which blocks
/// make up the note) and shown as the book prints them: the number and its punctuation
/// stay where they are, inline elements stay inline, paragraphs stay apart, and only
/// backlinks the book marks as such go.
#[test]
fn note_content_like_koreader() {
    let text = r##"<p>A<a href="n.xhtml#fn57">57</a> B<a href="n.xhtml#fn58">58</a> C<a href="n.xhtml#fn59">59</a>
        D<a href="n.xhtml#fn60">60</a> E<a href="n.xhtml#en61">61</a> F<a href="n.xhtml#fn62">62</a>
        G<a href="n.xhtml#fn63">63</a> H<a href="n.xhtml#fn64">64</a> I<a href="n.xhtml#fn65">65</a>
        J<a epub:type="noteref" href="#fn66" id="r66">66</a> K<a href="n.xhtml#fn67">67</a> L<a href="n.xhtml#fn68">68</a>.</p>
        <aside epub:type="footnote" id="fn66"><p><a href="#r66">66</a>) Aside note,</p><p>second paragraph.</p></aside>"##;
    let notes = r##"<p id="fn57"><a href="a.xhtml#ref57">57</a>. The text of note 57.</p>
        <p id="fn58">The text of note 58. <a href="a.xhtml#ref58">[58]</a></p>
        <div class="calibre_note" id="fn59"><p><a href="a.xhtml#ref59">59</a>.</p><p>Calibre note, its number in a paragraph of its own.</p></div>
        <p id="fn60"><sup><a href="a.xhtml#ref60">[60]</a></sup>: First paragraph of note 60.</p>
        <p>Second paragraph of note 60, no id of its own.</p>
        <h2>Next part</h2>
        <section epub:type="endnotes" role="doc-endnotes"><ol>
          <li id="en61"><p>List endnote 61. <a href="a.xhtml#r61" role="doc-backlink">↩︎</a></p></li>
          <li id="fn62"><p>List endnote 62, a sibling. <span class="noprint"><a href="a.xhtml#r62">Back</a></span></p></li>
        </ol></section>
        <p><a id="fn63"></a><a href="a.xhtml#r63">63.</a> Mobi note with an empty anchor.</p>
        <a id="fn64"></a><p>64. Anchor before the paragraph.</p>
        <p id="fn65">65 <a href="a.xhtml#r65" epub:type="backlink">↑</a> Bare number, then a marked backlink.</p>
        <div class="footnotes"><p id="fn67"><span class="num">67.</span> In a list of notes.</p><p id="fn68">68. The next one.</p></div>
        "##;
    let n = notes_of(text, notes);
    let by = |t: &str| n.iter().find(|(l, _)| l == t).map(|(_, n)| n.clone()).unwrap_or_else(|| panic!("{t} in {n:?}"));
    let text = |t: &str| by(t).text;

    // The number link and the period after it on one line, as printed.
    assert_eq!(text("57"), "57. The text of note 57.");
    // A numbered link after the text is the book's, not marked as a backlink: it stays.
    assert_eq!(text("58"), "The text of note 58. [58]");
    // The number in a paragraph of its own stays a paragraph (as KOReader shows it).
    assert_eq!(text("59"), "59.\nCalibre note, its number in a paragraph of its own.");
    // A note of two paragraphs without a container: the following paragraph belongs
    // to it (up to the heading); the <sup> number stays inline.
    assert_eq!(text("60"), "[60]: First paragraph of note 60.\nSecond paragraph of note 60, no id of its own.");
    // One <li> of an endnotes list, not the whole list; marked backlinks go
    // (role="doc-backlink", class "noprint").
    assert_eq!(text("61"), "List endnote 61.");
    assert_eq!(by("61").kind, NoteKind::Endnote);
    assert_eq!(text("62"), "List endnote 62, a sibling.");
    // An empty anchor in the paragraph, or before it.
    assert_eq!(text("63"), "63. Mobi note with an empty anchor.");
    assert_eq!(text("64"), "64. Anchor before the paragraph.");
    assert_eq!(text("65"), "65 Bare number, then a marked backlink.");
    // An EPUB 3 aside of two paragraphs, its number then a bracket.
    assert_eq!(text("66"), "66) Aside note,\nsecond paragraph.");
    assert_eq!(by("66").kind, NoteKind::Footnote);
    // Notes side by side in a footnotes container: just the one.
    assert_eq!(text("67"), "67. In a list of notes.");
    assert_eq!(by("67").kind, NoteKind::Footnote);
    assert_eq!(text("68"), "68. The next one.");
}

