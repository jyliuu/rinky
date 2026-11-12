//! Ruby: `<ruby>` markup drawn above the text, checked on the laid-out page (draw
//! items), and the document text (positions, selection, search, page text) unchanged by
//! it.

use ink_device::Point;
use ink_doc::{Document, LayoutSettings, MemoryLoader, Metadata};
use ink_html::layout::{Draw, TextRun};
use ink_html::{HtmlChapter, HtmlDocument};

const W: u32 = 1000;
const H: u32 = 1400;
const NONE: u32 = u32::MAX;

fn settings() -> LayoutSettings {
    let mut s = LayoutSettings::for_screen(W, H);
    s.margin_left = 50.0;
    s.margin_right = 50.0;
    s.margin_top = 50.0;
    s.margin_bottom = 50.0;
    s.font_px = 32.0;
    s.line_spacing = 1.2;
    s.justify = false;
    s
}

fn doc_with(chapters: &[&str], css: &str, s: &LayoutSettings) -> HtmlDocument {
    let mut m = MemoryLoader::default();
    m.0.insert("t/style.css".into(), css.as_bytes().to_vec());
    let chs = chapters
        .iter()
        .enumerate()
        .map(|(i, body)| HtmlChapter {
            path: format!("t/c{i}.xhtml"),
            html: format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><link rel=\"stylesheet\" href=\"style.css\"/></head><body>{body}</body></html>"),
        })
        .collect();
    let meta = Metadata { title: "T".into(), language: Some("en".into()), ..Default::default() };
    let mut d = HtmlDocument::new(meta, chs, Box::new(m)).unwrap();
    d.set_layout(s).unwrap();
    d
}

fn doc(body: &str, css: &str) -> HtmlDocument {
    doc_with(&[body], css, &settings())
}

/// Text runs of page 0: (document text, ruby text).
fn page_runs(d: &HtmlDocument) -> (Vec<TextRun>, Vec<TextRun>) {
    let (l, span) = d.page_layout(0).unwrap();
    let a = l.slices[span.first as usize].first_item as usize;
    let b = l.slices[span.end as usize - 1].end_item as usize;
    l.items[a..b]
        .iter()
        .filter_map(|i| match i {
            Draw::Text(t) => Some(t.clone()),
            _ => None,
        })
        .partition(|t| t.node != NONE)
}

fn run<'a>(runs: &'a [TextRun], text: &str) -> &'a TextRun {
    runs.iter().find(|t| t.text == text).unwrap_or_else(|| panic!("no run {text:?} in {:?}", runs.iter().map(|t| &t.text).collect::<Vec<_>>()))
}

/// Centre of a run.
fn mid(t: &TextRun) -> f32 {
    t.x + t.width / 2.0
}

fn line_heights(d: &HtmlDocument) -> Vec<f32> {
    let (l, span) = d.page_layout(0).unwrap();
    l.slices[span.first as usize..span.end as usize].iter().map(|s| s.bottom - s.top).collect()
}

#[test]
fn ruby_text_is_centred_above_its_base_at_half_size() {
    let d = doc("<p>one <ruby>base<rt>top</rt></ruby> two</p>", "");
    let (text, ruby) = page_runs(&d);
    let (base, top) = (run(&text, "base"), run(&ruby, "top"));
    assert_eq!(top.px, base.px / 2.0, "rt is 50 % by default");
    assert!((mid(top) - mid(base)).abs() < 0.5, "centred: {} vs {}", mid(top), mid(base));
    assert!(top.baseline < base.baseline - base.px * 0.6, "above the base: {} vs {}", top.baseline, base.baseline);
    assert!(top.baseline > base.line_top, "inside the line box");
    // The base keeps its DOM position and the words around it are where they were.
    let plain = doc("<p>one base two</p>", "");
    let (ptext, pruby) = page_runs(&plain);
    assert!(pruby.is_empty());
    for w in ["one", "base", "two"] {
        assert_eq!(run(&text, w).x, run(&ptext, w).x, "{w}");
    }
}

#[test]
fn rt_font_size_from_css() {
    let d = doc("<p><ruby>base<rt>top</rt></ruby></p>", "rt { font-size: 40% }");
    let (text, ruby) = page_runs(&d);
    assert!((run(&ruby, "top").px - run(&text, "base").px * 0.4).abs() < 0.01);
}

#[test]
fn several_pairs_in_one_ruby_with_and_without_rb() {
    // Bases between the rt elements.
    let d = doc("<p><ruby>one<rt>uno</rt>two<rt>dos</rt></ruby></p>", "");
    let (text, ruby) = page_runs(&d);
    assert!((mid(run(&ruby, "uno")) - mid(run(&text, "one"))).abs() < 0.5);
    assert!((mid(run(&ruby, "dos")) - mid(run(&text, "two"))).abs() < 0.5);
    // rb elements, the rts after them in order, rp left out.
    let d = doc("<p><ruby><rb>one</rb> <rb>two</rb><rp>(</rp><rt>uno</rt> <rt>dos</rt><rp>)</rp></ruby></p>", "");
    let (text, ruby) = page_runs(&d);
    assert!((mid(run(&ruby, "uno")) - mid(run(&text, "one"))).abs() < 0.5);
    assert!((mid(run(&ruby, "dos")) - mid(run(&text, "two"))).abs() < 0.5);
    assert!(text.iter().chain(&ruby).all(|t| !t.text.contains(['(', ')'])), "rp is not drawn");
    // One rb with an rp-wrapped rt, as most books write it.
    let d = doc("<p><ruby><rb>one</rb><rp>(</rp><rt>uno</rt><rp>)</rp></ruby>.</p>", "");
    let (text, ruby) = page_runs(&d);
    assert_eq!(ruby.len(), 1);
    assert!(text.iter().all(|t| !t.text.contains('(')));
}

#[test]
fn rt_display_none_draws_no_ruby_and_no_extra_space() {
    let body = "<p><ruby>base<rt>top</rt></ruby> and more</p>";
    let d = doc(body, "rt { display: none }");
    let (_, ruby) = page_runs(&d);
    assert!(ruby.is_empty());
    assert_eq!(line_heights(&d), line_heights(&doc("<p>base and more</p>", "")));
}

#[test]
fn ruby_setting_off_draws_the_base_only() {
    let mut s = settings();
    s.ruby = false;
    let d = doc_with(&["<p><ruby>base<rt>top</rt></ruby> and more</p>"], "", &s);
    let (text, ruby) = page_runs(&d);
    assert!(ruby.is_empty());
    assert!(text.iter().all(|t| t.text != "top"));
    assert_eq!(line_heights(&d), line_heights(&doc("<p>base and more</p>", "")));
}

#[test]
fn a_ruby_wider_than_its_base_spreads_the_base() {
    let d = doc("<p>a <ruby>ab<rt>a much longer reading</rt></ruby> z</p>", "");
    let (text, ruby) = page_runs(&d);
    let (base, top, z) = (run(&text, "ab"), run(&ruby, "a much longer reading"), run(&text, "z"));
    assert!(base.letter_spacing > 0.0, "the base letters move apart");
    // The base cell is as wide as the ruby plus a quarter em of the ruby text.
    let gap = base.width - top.width;
    assert!((gap - top.px * 0.25).abs() < 0.5, "cell {} vs ruby {}", base.width, top.width);
    // The run starts at its first letter, half a spacing into the cell.
    let cell_mid = base.x - base.letter_spacing / 2.0 + base.width / 2.0;
    assert!((cell_mid - mid(top)).abs() < 0.5, "{cell_mid} vs {}", mid(top));
    assert!(z.x >= top.x + top.width, "the next word does not overlap the ruby");
    assert!(run(&text, "a").x + run(&text, "a").width <= top.x, "nor the word before");
    // Two wide rubies side by side keep a gap between their texts.
    let d = doc("<p><ruby>a<rt>wide reading</rt>b<rt>wide reading</rt></ruby></p>", "");
    let (_, ruby) = page_runs(&d);
    assert!(ruby[1].x - (ruby[0].x + ruby[0].width) >= ruby[0].px * 0.25 - 0.5, "{ruby:?}");
}

#[test]
fn no_line_break_inside_a_ruby_base() {
    // Find a filler length where the plain text breaks between the two words of the
    // base; with ruby the base moves to the next line whole.
    for n in 1..80 {
        let filler = "word ".repeat(n);
        let plain = doc(&format!("<p>{filler}alpha beta</p>"), "");
        let (t, _) = page_runs(&plain);
        if run(&t, "alpha").line_top == run(&t, "beta").line_top {
            continue;
        }
        let d = doc(&format!("<p>{filler}<ruby>alpha beta<rt>r</rt></ruby></p>"), "");
        let (t, ruby) = page_runs(&d);
        // The base is one piece, on one line.
        let _ = run(&t, "alpha beta");
        assert_eq!(ruby.len(), 1);
        return;
    }
    panic!("no filler length broke the plain line");
}

#[test]
fn every_line_of_a_paragraph_with_ruby_gets_the_same_extra_space() {
    let long = "words ".repeat(60);
    let d = doc(&format!("<p><ruby>base<rt>top</rt></ruby> {long}</p><p>{long}</p>"), "");
    let plain = doc(&format!("<p>base {long}</p><p>{long}</p>"), "");
    let (h, ph) = (line_heights(&d), line_heights(&plain));
    assert_eq!(h.len(), ph.len());
    let n = h.len() / 2;
    assert!(n >= 3, "{h:?}");
    // The first paragraph: every line taller by the same amount; the second unchanged.
    let extra = h[0] - ph[0];
    assert!(extra > 0.0, "{h:?} vs {ph:?}");
    for i in 0..n {
        assert!((h[i] - ph[i] - extra).abs() < 0.01, "line {i}: {h:?} vs {ph:?}");
    }
    for i in n..h.len() {
        assert!((h[i] - ph[i]).abs() < 0.01, "a paragraph without ruby keeps its spacing: {h:?} vs {ph:?}");
    }
}

#[test]
fn ruby_is_not_document_text() {
    let body = "<p>one <ruby>base<rt>top</rt></ruby> two</p>";
    let mut d = doc(body, "");
    assert!(d.search("top", 10).unwrap().is_empty(), "rt text is not found");
    assert_eq!(d.search("base two", 10).unwrap().len(), 1, "the base text is");
    let around = d.read_around(&d.position_of_page(0).unwrap(), 100).unwrap();
    assert!(around.contains("one base two") && !around.contains("top"), "{around:?}");
    // A selection over the whole line holds the base text only.
    let (text, _) = page_runs(&d);
    let (a, b) = (run(&text, "one"), run(&text, "two"));
    let y = ((a.line_top + a.line_bottom) / 2.0) as i32;
    let sel = d.select(0, Point::new(a.x as i32 + 1, y), Point::new((b.x + b.width) as i32 - 1, y)).unwrap().unwrap();
    assert_eq!(sel.text, "one base two");
}
