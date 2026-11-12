//! Ruby: `<ruby>` markup and annotations of a [`TextAnnotator`] drawn above the text,
//! checked on the laid-out page (draw items), and the document text (positions,
//! selection, search, page text) unchanged by them.

use ink_device::Point;
use ink_doc::{Annotation, AnnotatorHandle, Document, LayoutSettings, MemoryLoader, Metadata, TextAnnotator};
use ink_html::layout::{Draw, TextRun};
use ink_html::{HtmlChapter, HtmlDocument};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

fn annotated(body: &str, a: Arc<dyn TextAnnotator>) -> HtmlDocument {
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(a)];
    doc_with(&[body], "", &s)
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
fn a_wide_ruby_reaches_over_plain_neighbours_without_moving_them() {
    let d = doc("<p>xx <ruby>ab<rt>a much longer reading</rt></ruby> zz</p>", "");
    let plain = doc("<p>xx ab zz</p>", "");
    let (text, ruby) = page_runs(&d);
    let (ptext, _) = page_runs(&plain);
    let (base, top) = (run(&text, "ab"), run(&ruby, "a much longer reading"));
    assert!(top.width > base.width + 20.0, "the reading is wider than its base");
    for w in ["xx", "ab", "zz"] {
        assert_eq!(run(&text, w).x, run(&ptext, w).x, "{w} stays where it was");
    }
    assert_eq!(base.letter_spacing, 0.0);
    assert!((mid(base) - mid(top)).abs() < 0.5, "centred over its base");
}

#[test]
fn neighbouring_wide_rubies_are_condensed_then_keep_a_gap() {
    let d = doc("<p>x<ruby>a<rt>wide reading</rt>b<rt>wide reading</rt>c<rt>c</rt></ruby>xxxxxxxx</p>", "p { text-indent: 0 }");
    let (text, ruby) = page_runs(&d);
    let normal = run(&text, "a").px / 2.0;
    for r in &ruby {
        assert!(r.px >= normal * 0.7 - 0.01 && r.px < normal, "condensed, not below 70 %: {} of {normal}", r.px);
        assert_eq!(r.px, ruby[0].px, "one size for the paragraph");
    }
    let gap = ruby[0].px * 0.35;
    for w in ruby.windows(2) {
        assert!(w[1].x - (w[0].x + w[0].width) >= gap - 0.5, "{:?} and {:?} too close", w[0].text, w[1].text);
    }
    // A ruby text sits within a quarter of its base's width of the base's centre.
    for (r, b) in ruby.iter().zip(["a", "b", "c"]) {
        let b = run(&text, b);
        assert!((mid(r) - mid(b)).abs() <= b.width * 0.25 + 0.5, "{:?} off its base", r.text);
    }
    // The bases of the run keep one pitch.
    let (a, b, c) = (run(&text, "a"), run(&text, "b"), run(&text, "c"));
    assert!(((b.x - (a.x + a.width)) - (c.x - (b.x + b.width))).abs() < 0.5, "one pitch: {} vs {}", b.x - (a.x + a.width), c.x - (b.x + b.width));
    // The leading x stays at the line start unless the first reading would reach past it.
    assert!((text[0].x - 50.0).abs() < 0.5 || (ruby[0].x - 50.0).abs() < 0.5, "x at {}, reading at {}", text[0].x, ruby[0].x);
}

#[test]
fn a_ruby_text_does_not_reach_past_the_line_start_or_end() {
    let s = settings();
    for (body, at_end) in [("<p><ruby>i<rt>a very wide reading</rt></ruby> b</p>", false), ("<p style=\"text-align: right\">b <ruby>i<rt>a very wide reading</rt></ruby></p>", true)] {
        let d = doc(body, "");
        let (_, ruby) = page_runs(&d);
        let r = &ruby[0];
        assert!(r.x >= s.margin_left - 0.5, "{body}: starts at {}", r.x);
        assert!(r.x + r.width <= W as f32 - s.margin_right + 0.5, "{body}: ends at {}", r.x + r.width);
        let _ = at_end;
    }
}

/// Gives each ideograph a reading whose length cycles through `lens` (0: none), as
/// readings of one syllable per character in Latin letters do.
struct MixedReadings(Vec<usize>);

impl TextAnnotator for MixedReadings {
    fn id(&self) -> &str {
        "mixed"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, _: &str, _lang: Option<&str>) -> bool {
        true
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<Annotation> {
        p.char_indices()
            .filter(|&(_, c)| ('\u{4e00}'..='\u{9fff}').contains(&c))
            .enumerate()
            .filter_map(|(k, (i, c))| {
                let n = self.0[k % self.0.len()];
                (n > 0).then(|| Annotation::over(i..i + c.len_utf8(), "n".repeat(n)))
            })
            .collect()
    }
}

/// A layout with the CJK subset font of the ruby fixture as the body font.
fn cjk_doc(body: &str, s: &LayoutSettings) -> HtmlDocument {
    let font = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/src/NotoSerifSC-Subset.otf")).unwrap();
    let mut m = MemoryLoader::default();
    m.0.insert("t/style.css".into(), b"@font-face { font-family: Cjk; src: url(cjk.otf) } body { font-family: Cjk }".to_vec());
    m.0.insert("t/cjk.otf".into(), font);
    let chs = vec![HtmlChapter {
        path: "t/c0.xhtml".into(),
        html: format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><link rel=\"stylesheet\" href=\"style.css\"/></head><body>{body}</body></html>"),
    }];
    let meta = Metadata { title: "T".into(), language: Some("zh".into()), ..Default::default() };
    let mut d = HtmlDocument::new(meta, chs, Box::new(m)).unwrap();
    d.set_layout(s).unwrap();
    d
}

#[test]
fn per_character_readings_of_mixed_width_keep_one_pitch() {
    // One syllable per character, as Latin-letter readings of Chinese have: the
    // lengths of the readings of the first two lines of the fixture's first poem.
    let text = "床前明月光疑是地上霜";
    let lens = vec![6, 4, 4, 3, 5, 2, 3, 2, 5, 6];
    let body = format!("<p>{text}</p>");
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Arc::new(MixedReadings(lens)))];
    let d = cjk_doc(&body, &s);
    s.annotators = Vec::new();
    let plain = cjk_doc(&body, &s);
    let (bases, ruby) = page_runs(&d);
    let (pbases, _) = page_runs(&plain);
    // The pitch: from one character's left edge to the next one's (32 px plain).
    let gaps: Vec<f32> = bases.windows(2).map(|w| w[1].x - w[0].x).collect();
    let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
    let sd = (gaps.iter().map(|g| (g - mean) * (g - mean)).sum::<f32>() / gaps.len() as f32).sqrt();
    let (lo, hi) = gaps.iter().fold((f32::MAX, f32::MIN), |(a, b), &g| (a.min(g), b.max(g)));
    let right = |r: &[TextRun]| r.iter().map(|t| t.x + t.width).fold(f32::MIN, f32::max);
    let left = |r: &[TextRun]| r.iter().map(|t| t.x).fold(f32::MAX, f32::min);
    println!("character pitch: min {lo:.1} max {hi:.1} mean {mean:.1} sd {sd:.1} px; reading size {} px", ruby[0].px);
    assert_eq!(ruby.len(), 10);
    assert!(bases.iter().all(|b| b.line_top == bases[0].line_top), "one line");
    let gap = ruby[0].px * 0.35;
    for w in ruby.windows(2) {
        assert!(w[1].x - (w[0].x + w[0].width) >= gap - 0.5, "{:?} and {:?} too close", w[0].text, w[1].text);
    }
    assert!(ruby[0].x >= s.margin_left - 0.5, "inside the line");
    // One pitch for the whole run, readings condensed to no less than 70 %.
    assert!(sd < 0.5, "uneven pitch: sd {sd}");
    assert!(ruby.iter().all(|r| r.px >= 16.0 * 0.7 - 0.01 && r.px == ruby[0].px));
    let plain_w = right(&pbases) - left(&pbases);
    let now = right(&bases) - left(&bases) - plain_w;
    println!("extra width of the line: {now:.1} px over {plain_w:.1} px of plain text");
}

/// Readings given by position: `texts[k]` for the k-th ideograph ("" none).
struct ByPosition(Vec<&'static str>);

impl TextAnnotator for ByPosition {
    fn id(&self) -> &str {
        "by-position"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, p: &str, _lang: Option<&str>) -> bool {
        p.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<Annotation> {
        p.char_indices()
            .filter(|&(_, c)| ('\u{4e00}'..='\u{9fff}').contains(&c))
            .enumerate()
            .filter_map(|(k, (i, c))| self.0.get(k).filter(|t| !t.is_empty()).map(|t| Annotation::over(i..i + c.len_utf8(), *t)))
            .collect()
    }
}

fn cjk_annotated(body: &str, texts: Vec<&'static str>) -> HtmlDocument {
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Arc::new(ByPosition(texts)))];
    cjk_doc(body, &s)
}

/// Pitch from one character's left edge to the next.
fn pitches(bases: &[TextRun]) -> Vec<f32> {
    let xs: Vec<f32> = bases.iter().flat_map(|b| {
        // A run of several characters: its characters at the plain advance.
        let n = b.text.chars().count();
        (0..n).map(move |k| b.x + b.width * k as f32 / n as f32)
    }).collect();
    xs.windows(2).map(|w| w[1] - w[0]).collect()
}

#[test]
fn one_or_two_characters_without_a_reading_keep_the_run_pitch() {
    // Readings too wide to fit even condensed: the bases move apart.
    let texts = vec!["aaaaaaaaaa", "", "aaaaaaaaaa", "aaaaaaaaaa", "", "", "aaaaaaaaaa", "aaaaaaaaaa"];
    let d = cjk_annotated("<p>床前明月光疑是地</p>", texts);
    let (bases, ruby) = page_runs(&d);
    assert_eq!(ruby.len(), 5, "empty readings draw nothing");
    let p = pitches(&bases);
    let (lo, hi) = p.iter().fold((f32::MAX, f32::MIN), |(a, b), &g| (a.min(g), b.max(g)));
    println!("partly annotated pitch: min {lo:.1} max {hi:.1} px");
    assert!(lo > 33.0, "the readings needed room: {p:?}");
    assert!(hi - lo < 0.5, "one pitch with gaps of one and two characters: {p:?}");
}

#[test]
fn readings_stay_centred_when_they_fit_and_shift_little_otherwise() {
    // Narrow readings: every one centred.
    let d = cjk_annotated("<p>床前明</p>", vec!["a", "bb", "ccc"]);
    let (bases, ruby) = page_runs(&d);
    for (r, b) in ruby.iter().zip(&bases) {
        assert!((mid(r) - mid(b)).abs() < 0.5, "{:?} off centre", r.text);
    }
    // A wide reading in the middle moves its neighbours a little; the readings far from
    // it stay centred.
    let d = cjk_annotated("<p>床前明月光疑</p>", vec!["a", "a", "ccccccc", "a", "a", "a"]);
    let (bases, ruby) = page_runs(&d);
    let (first, last) = (ruby.first().unwrap(), ruby.last().unwrap());
    assert!((mid(first) - mid(&bases[0])).abs() < 0.5 && (mid(last) - mid(bases.last().unwrap())).abs() < 0.5, "{ruby:?}");
    let gap = ruby[0].px * 0.35;
    for w in ruby.windows(2) {
        assert!(w[1].x - (w[0].x + w[0].width) >= gap - 0.5, "{:?} and {:?} too close", w[0].text, w[1].text);
    }
}

#[test]
fn every_paragraph_the_annotator_wants_gets_the_ruby_room() {
    // The annotator reads both paragraphs but has a reading for the first one only.
    let d = cjk_annotated("<p>床前</p><p>明月</p><p>abc</p>", vec!["a"]);
    let h = line_heights(&d);
    assert!((h[0] - h[1]).abs() < 0.01, "the same line pitch without a reading: {h:?}");
    assert!(h[2] < h[1] - 1.0, "a paragraph it does not want keeps its spacing: {h:?}");
}

/// Annotates every occurrence of a word with a text; `version` and `text` can change.
struct Words {
    word: &'static str,
    text: std::sync::Mutex<String>,
    version: AtomicU64,
}

impl Words {
    fn new(word: &'static str, text: &str) -> Arc<Self> {
        Arc::new(Words { word, text: std::sync::Mutex::new(text.into()), version: AtomicU64::new(1) })
    }
}

impl TextAnnotator for Words {
    fn id(&self) -> &str {
        "words"
    }
    fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }
    fn wants(&self, p: &str, _lang: Option<&str>) -> bool {
        p.contains(self.word)
    }
    fn annotate(&self, p: &str, _lang: Option<&str>) -> Vec<Annotation> {
        let t = self.text.lock().unwrap().clone();
        p.match_indices(self.word).map(|(i, w)| Annotation::over(i..i + w.len(), t.clone())).collect()
    }
}

#[test]
fn annotations_are_drawn_like_ruby_and_are_not_text() {
    let body = "<p>one base two base</p>";
    let mut d = annotated(body, Words::new("base", "B"));
    let (text, ruby) = page_runs(&d);
    assert_eq!(ruby.len(), 2);
    for r in &ruby {
        assert_eq!(r.text, "B");
        assert_eq!(r.px, 16.0, "half the base size");
    }
    let bases: Vec<&TextRun> = text.iter().filter(|t| t.text == "base").collect();
    assert!((mid(&ruby[0]) - mid(bases[0])).abs() < 0.5 && (mid(&ruby[1]) - mid(bases[1])).abs() < 0.5);
    let plain = doc(body, "");
    assert_eq!(d.position_of_page(0).unwrap(), plain.position_of_page(0).unwrap());
    assert!(d.search("B", 10).unwrap().iter().all(|h| !h.excerpt.contains('B')), "annotations are not searched");
    let around = d.read_around(&d.position_of_page(0).unwrap(), 100).unwrap();
    assert_eq!(around.trim(), "one base two base");
    let (a, b) = (run(&text, "one"), text.last().unwrap());
    let y = ((a.line_top + a.line_bottom) / 2.0) as i32;
    let sel = d.select(0, Point::new(a.x as i32 + 1, y), Point::new((b.x + b.width) as i32 - 1, y)).unwrap().unwrap();
    assert_eq!(sel.text, "one base two base");
}

/// Returns a fixed list of annotations for every paragraph.
struct Fixed(Vec<Annotation>);

impl TextAnnotator for Fixed {
    fn id(&self) -> &str {
        "fixed"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, _: &str, _lang: Option<&str>) -> bool {
        true
    }
    fn annotate(&self, _: &str, _: Option<&str>) -> Vec<Annotation> {
        self.0.clone()
    }
}

#[test]
fn invalid_annotations_are_dropped() {
    // The paragraph text is "café base end x".
    let body = "<p>café <ruby>base<rt>r</rt></ruby> end x</p>";
    let list = vec![
        Annotation::over(0..0, "empty"),
        Annotation::over(10..400, "out"),
        Annotation::over(0..4, "mid-char"), // é is bytes 3..5
        Annotation::over(11..14, "ok"),     // "end"
        Annotation::over(12..15, "overlap"),
        Annotation::over(6..9, "book"), // inside the book's ruby base "base" (6..10)
        Annotation::over(14..15, "  "), // nothing to draw
        Annotation::over(14..15, "sp"), // only a space: no piece of text to draw it over
        Annotation::over(15..16, "x"),
    ];
    let d = annotated(body, Arc::new(Fixed(list)));
    let (_, ruby) = page_runs(&d);
    let mut got: Vec<&str> = ruby.iter().map(|r| r.text.as_str()).collect();
    got.sort();
    assert_eq!(got, ["ok", "r", "x"]);
}

/// Panics on paragraphs holding "boom", annotates the others.
struct Boom;

impl TextAnnotator for Boom {
    fn id(&self) -> &str {
        "boom"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, _: &str, _lang: Option<&str>) -> bool {
        true
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<Annotation> {
        assert!(!p.contains("boom"), "test annotator panics on purpose");
        vec![Annotation::over(0..1, "a")]
    }
}

#[test]
fn a_panicking_annotator_leaves_the_rest_of_the_book_plain() {
    let d = annotated("<p>first</p><p>boom</p><p>third</p><p>fourth</p>", Arc::new(Boom));
    let (text, ruby) = page_runs(&d);
    let words: Vec<&str> = text.iter().map(|t| t.text.as_str()).collect();
    // The annotated "f" is a piece of its own.
    assert_eq!(words, ["f", "irst", "boom", "third", "fourth"], "every paragraph is laid out");
    assert_eq!(ruby.len(), 1, "only the paragraph before the panic is annotated: {:?}", ruby.iter().map(|r| &r.text).collect::<Vec<_>>());
    assert!(ruby[0].baseline < run(&text, "f").baseline);
}

#[test]
fn a_version_change_lays_the_book_out_again() {
    let a = Words::new("base", "old");
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(a.clone())];
    let mut d = doc_with(&["<p>one base two</p>"], "", &s);
    assert_eq!(page_runs(&d).1[0].text, "old");
    // The annotator changes its output and its version: settings made now differ.
    *a.text.lock().unwrap() = "new".into();
    a.version.store(2, Ordering::Relaxed);
    let mut s2 = s.clone();
    s2.annotators = vec![AnnotatorHandle::new(a.clone())];
    assert_ne!(s, s2);
    assert_ne!(s.cache_key(), s2.cache_key());
    d.set_layout(&s2).unwrap();
    assert_eq!(page_runs(&d).1[0].text, "new");
    // Turning annotations off (no annotator) or ruby off removes them.
    let mut off = s2.clone();
    off.annotators = Vec::new();
    assert_ne!(off.cache_key(), s2.cache_key());
    d.set_layout(&off).unwrap();
    assert!(page_runs(&d).1.is_empty());
    let mut hidden = s2.clone();
    hidden.ruby = false;
    d.set_layout(&hidden).unwrap();
    assert!(page_runs(&d).1.is_empty());
}

#[test]
fn giant_paragraphs_keep_annotations_whole() {
    // A paragraph over the giant-paragraph limit is broken into lines in chunks; an
    // annotated range never straddles two chunks.
    let body = format!("<p>{}</p>", "base ".repeat(70_000));
    let d = annotated(&body, Words::new("base", "B"));
    let (text, ruby) = page_runs(&d);
    assert!(!text.is_empty());
    assert!(ruby.len() > 10 && ruby.iter().all(|r| r.text == "B"));
}

/// Gives every CJK ideograph the same short annotation (or, `idle`, wants nothing).
struct EveryIdeograph {
    idle: bool,
}

impl TextAnnotator for EveryIdeograph {
    fn id(&self) -> &str {
        "every-ideograph"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, p: &str, _lang: Option<&str>) -> bool {
        !self.idle && p.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    }
    fn annotate(&self, p: &str, _lang: Option<&str>) -> Vec<Annotation> {
        p.char_indices().filter(|&(_, c)| ('\u{4e00}'..='\u{9fff}').contains(&c)).map(|(i, c)| Annotation::over(i..i + c.len_utf8(), "abc")).collect()
    }
}

/// Run with `cargo test --release -p ink-html --test ruby -- --ignored --nocapture measure`.
#[test]
#[ignore = "measurement: prints the pagination time of a big CJK book without an annotator, with one that wants nothing and with one that annotates every ideograph"]
fn measure_annotator_cost_on_a_big_cjk_book() {
    // 40 chapters of 60 paragraphs of 150 ideographs and punctuation: about 1 MB of text
    // (characters drawn from the Thousand Character Classic, in a fixed pseudo-random
    // order).
    let pool: Vec<char> = "天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏闰余成岁律吕调阳云腾致雨露结为霜金生丽水玉出昆冈剑号巨阙珠称夜光果珍李柰菜重芥姜海咸河淡鳞潜羽翔".chars().collect();
    let mut seed = 12345u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (seed >> 16) as usize
    };
    let chapters: Vec<String> = (0..40)
        .map(|_| {
            let mut body = String::new();
            for _ in 0..60 {
                body.push_str("<p>");
                for k in 1..=150 {
                    body.push(pool[next() % pool.len()]);
                    if k % 12 == 0 {
                        body.push('，');
                    }
                }
                body.push_str("。</p>\n");
            }
            body
        })
        .collect();
    let refs: Vec<&str> = chapters.iter().map(String::as_str).collect();
    let bytes: usize = chapters.iter().map(String::len).sum();
    println!("book: {} chapters, {} KB of XHTML body", chapters.len(), bytes / 1024);
    let variants: [(&str, Option<Arc<dyn TextAnnotator>>); 3] = [
        ("no annotator", None),
        ("annotator that wants nothing", Some(Arc::new(EveryIdeograph { idle: true }))),
        ("every ideograph annotated", Some(Arc::new(EveryIdeograph { idle: false }))),
    ];
    for (name, a) in variants {
        let mut times = Vec::new();
        let mut pages = 0;
        for _ in 0..5 {
            let mut s = LayoutSettings::for_screen(1272, 1696);
            s.annotators = a.clone().map(AnnotatorHandle::new).into_iter().collect();
            let t = std::time::Instant::now();
            let mut d = doc_with(&refs, "", &s);
            assert!(d.wait_paginated(std::time::Duration::from_secs(600)));
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            pages = d.page_count().pages;
        }
        times.sort_by(f64::total_cmp);
        println!("{name}: {pages} pages, open + full pagination median {:.0} ms (min {:.0}, max {:.0}, n=5)", times[2], times[0], times[4]);
    }
}

/// Selects from the first to the last base character of the page's first line.
fn select_line(d: &mut HtmlDocument) -> String {
    let (bases, _) = page_runs(d);
    let line: Vec<&TextRun> = bases.iter().filter(|b| b.line_top == bases[0].line_top).collect();
    let (a, b) = (line[0], line[line.len() - 1]);
    let y = ((a.line_top + a.line_bottom) / 2.0) as i32;
    d.select(0, Point::new(a.x as i32 + 1, y), Point::new((b.x + b.width) as i32 - 1, y)).unwrap().unwrap().text
}

#[test]
fn selected_text_is_the_same_with_and_without_readings() {
    let text = "床前明月光疑是地上霜";
    let body = format!("<p>{text}</p>");
    let wide = vec!["aaaaaaaaaa"; 10];
    let mut plain = cjk_doc(&body, &settings());
    let mut annotated = cjk_annotated(&body, wide);
    let ruby_body = format!("<p><ruby>{}</ruby></p>", text.chars().map(|c| format!("{c}<rt>aaaaaaaaaa</rt>")).collect::<String>());
    let mut book = cjk_doc(&ruby_body, &settings());
    let p = select_line(&mut plain);
    assert_eq!(p, text);
    assert_eq!(select_line(&mut annotated), p, "annotations add nothing to selected text");
    assert_eq!(select_line(&mut book), p, "nor book ruby");
}

/// Gives every occurrence of `word` the text `text`, for paragraphs in `lang` (any when
/// `None`); counts its `wants` calls with the language it was given.
struct Lang {
    word: &'static str,
    text: &'static str,
    lang: Option<&'static str>,
    seen: std::sync::Mutex<Vec<Option<String>>>,
}

impl TextAnnotator for Lang {
    fn id(&self) -> &str {
        self.text
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, p: &str, lang: Option<&str>) -> bool {
        self.seen.lock().unwrap().push(lang.map(str::to_string));
        p.contains(self.word) && self.lang.is_none_or(|l| Some(l) == lang)
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<Annotation> {
        p.match_indices(self.word).map(|(i, w)| Annotation::over(i..i + w.len(), self.text)).collect()
    }
}

fn lang(word: &'static str, text: &'static str, lang: Option<&'static str>) -> Arc<Lang> {
    Arc::new(Lang { word, text, lang, seen: Default::default() })
}

#[test]
fn several_annotators_merge_and_the_earlier_wins_an_overlap() {
    let first = lang("base", "FIRST", None);
    let second = lang("base two", "SECOND", None);
    let third = lang("two", "THIRD", None);
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(first.clone()), AnnotatorHandle::new(second), AnnotatorHandle::new(third)];
    let d = doc_with(&["<p>one base two</p>"], "", &s);
    let (_, ruby) = page_runs(&d);
    let mut got: Vec<&str> = ruby.iter().map(|r| r.text.as_str()).collect();
    got.sort();
    assert_eq!(got, ["FIRST", "THIRD"], "the second overlaps the first and is dropped");
    assert_eq!(first.seen.lock().unwrap().first().cloned().flatten().as_deref(), Some("en"), "wants gets the paragraph's language");
    // The cache key names every annotator in order.
    assert!(s.cache_key().ends_with("-aFIRST@1,SECOND@1,THIRD@1"), "{}", s.cache_key());
}

#[test]
fn a_language_check_in_wants_skips_other_paragraphs() {
    let a = lang("base", "B", Some("de"));
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(a)];
    let d = doc_with(&["<p>one base</p><p lang=\"de\">eine base</p>"], "", &s);
    let (_, ruby) = page_runs(&d);
    assert_eq!(ruby.len(), 1, "only the German paragraph");
}

#[test]
fn one_panicking_annotator_leaves_the_others_running() {
    let ok = lang("third", "T", None);
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Arc::new(Boom)), AnnotatorHandle::new(ok)];
    let d = doc_with(&["<p>boom</p><p>third</p>"], "", &s);
    let (_, ruby) = page_runs(&d);
    assert!(ruby.iter().any(|r| r.text == "T"), "the second annotator goes on after the first panicked");
}
