//! Ruby: `<ruby>` markup and annotations of a [`TextAnnotator`] drawn above the text,
//! checked on the laid-out page (draw items), and the document text (positions,
//! selection, search, page text) unchanged by them.

use ink_device::Point;
use ink_doc::{Annotation, AnnotationPosition, AnnotatorHandle, Document, LayoutSettings, MemoryLoader, Metadata, TextAnnotator};
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

/// Text runs of page 0: (document text, ruby text). The ruby text is the book's (draw
/// items) and the annotations as drawn (computed while drawing, page 0 drawn now).
fn page_runs(d: &mut HtmlDocument) -> (Vec<TextRun>, Vec<TextRun>) {
    d.set_annotations_sync(true);
    d.set_record_annotations(true);
    let mut canvas = ink_device::Canvas::new(W as usize, H as usize);
    d.render_page(0, &mut canvas).unwrap();
    let (l, span) = d.page_layout(0).unwrap();
    let a = l.slices[span.first as usize].first_item as usize;
    let b = l.slices[span.end as usize - 1].end_item as usize;
    let dy = d.layout().margin_top - span.y0;
    let (text, mut ruby): (Vec<TextRun>, Vec<TextRun>) = l.items[a..b]
        .iter()
        .filter_map(|i| match i {
            Draw::Text(t) => Some(t.clone()),
            _ => None,
        })
        .partition(|t| t.node != NONE);
    // Annotations are drawn in page coordinates: back to flow coordinates.
    ruby.extend(d.placed_annotations().into_iter().map(|p| TextRun { x: p.x, width: p.width, baseline: p.baseline - dy, px: p.px, text: p.text, node: NONE, ..Default::default() }));
    (text, ruby)
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
    let mut d = doc("<p>one <ruby>base<rt>top</rt></ruby> two</p>", "");
    let (text, ruby) = page_runs(&mut d);
    let (base, top) = (run(&text, "base"), run(&ruby, "top"));
    assert_eq!(top.px, base.px / 2.0, "rt is 50 % by default");
    assert!((mid(top) - mid(base)).abs() < 0.5, "centred: {} vs {}", mid(top), mid(base));
    assert!(top.baseline < base.baseline - base.px * 0.6, "above the base: {} vs {}", top.baseline, base.baseline);
    assert!(top.baseline > base.line_top, "inside the line box");
    // The base keeps its DOM position and the words around it are where they were.
    let mut plain = doc("<p>one base two</p>", "");
    let (ptext, pruby) = page_runs(&mut plain);
    assert!(pruby.is_empty());
    for w in ["one", "base", "two"] {
        assert_eq!(run(&text, w).x, run(&ptext, w).x, "{w}");
    }
}

#[test]
fn rt_font_size_from_css() {
    let mut d = doc("<p><ruby>base<rt>top</rt></ruby></p>", "rt { font-size: 40% }");
    let (text, ruby) = page_runs(&mut d);
    assert!((run(&ruby, "top").px - run(&text, "base").px * 0.4).abs() < 0.01);
}

#[test]
fn several_pairs_in_one_ruby_with_and_without_rb() {
    // Bases between the rt elements.
    let mut d = doc("<p><ruby>one<rt>uno</rt>two<rt>dos</rt></ruby></p>", "");
    let (text, ruby) = page_runs(&mut d);
    assert!((mid(run(&ruby, "uno")) - mid(run(&text, "one"))).abs() < 0.5);
    assert!((mid(run(&ruby, "dos")) - mid(run(&text, "two"))).abs() < 0.5);
    // rb elements, the rts after them in order, rp left out.
    let mut d = doc("<p><ruby><rb>one</rb> <rb>two</rb><rp>(</rp><rt>uno</rt> <rt>dos</rt><rp>)</rp></ruby></p>", "");
    let (text, ruby) = page_runs(&mut d);
    assert!((mid(run(&ruby, "uno")) - mid(run(&text, "one"))).abs() < 0.5);
    assert!((mid(run(&ruby, "dos")) - mid(run(&text, "two"))).abs() < 0.5);
    assert!(text.iter().chain(&ruby).all(|t| !t.text.contains(['(', ')'])), "rp is not drawn");
    // One rb with an rp-wrapped rt, as most books write it.
    let mut d = doc("<p><ruby><rb>one</rb><rp>(</rp><rt>uno</rt><rp>)</rp></ruby>.</p>", "");
    let (text, ruby) = page_runs(&mut d);
    assert_eq!(ruby.len(), 1);
    assert!(text.iter().all(|t| !t.text.contains('(')));
}

#[test]
fn rt_display_none_draws_no_ruby_and_no_extra_space() {
    let body = "<p><ruby>base<rt>top</rt></ruby> and more</p>";
    let mut d = doc(body, "rt { display: none }");
    let (_, ruby) = page_runs(&mut d);
    assert!(ruby.is_empty());
    assert_eq!(line_heights(&d), line_heights(&doc("<p>base and more</p>", "")));
}

#[test]
fn ruby_setting_off_draws_the_base_only() {
    let mut s = settings();
    s.ruby = false;
    let mut d = doc_with(&["<p><ruby>base<rt>top</rt></ruby> and more</p>"], "", &s);
    let (text, ruby) = page_runs(&mut d);
    assert!(ruby.is_empty());
    assert!(text.iter().all(|t| t.text != "top"));
    assert_eq!(line_heights(&d), line_heights(&doc("<p>base and more</p>", "")));
}

#[test]
fn a_wide_ruby_reaches_over_plain_neighbours_without_moving_them() {
    let mut d = doc("<p>xx <ruby>ab<rt>a much longer reading</rt></ruby> zz</p>", "");
    let mut plain = doc("<p>xx ab zz</p>", "");
    let (text, ruby) = page_runs(&mut d);
    let (ptext, _) = page_runs(&mut plain);
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
    let mut d = doc("<p>x<ruby>a<rt>wide reading</rt>b<rt>wide reading</rt>c<rt>c</rt></ruby>xxxxxxxx</p>", "p { text-indent: 0 }");
    let (text, ruby) = page_runs(&mut d);
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
        let mut d = doc(body, "");
        let (_, ruby) = page_runs(&mut d);
        let r = &ruby[0];
        assert!(r.x >= s.margin_left - 0.5, "{body}: starts at {}", r.x);
        assert!(r.x + r.width <= W as f32 - s.margin_right + 0.5, "{body}: ends at {}", r.x + r.width);
        let _ = at_end;
    }
}

/// Gives each ideograph a reading whose length cycles through `.0` (0: none), as
/// readings of one syllable per character in Latin letters do; asks for `.1` em between
/// ideographs.
struct MixedReadings(Vec<usize>, f32);

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
    fn ideograph_spacing(&self, _: Option<&str>) -> f32 {
        self.1
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
fn per_character_readings_of_mixed_width_fit_the_pitch_the_layout_kept() {
    // One syllable per character, as Latin-letter readings of Chinese have: the
    // lengths of the readings of the first two lines of the fixture's first poem.
    let text = "床前明月光疑是地上霜";
    let lens = vec![6, 4, 4, 3, 5, 2, 3, 2, 5, 6];
    let body = format!("<p>{text}</p>");
    for spacing in [0.0, 0.15, 0.25] {
        let mut s = settings();
        s.annotators = vec![AnnotatorHandle::new(Arc::new(MixedReadings(lens.clone(), spacing)))];
        let mut d = cjk_doc(&body, &s);
        let (bases, ruby) = page_runs(&mut d);
        // The pitch: from one character's left edge to the next one's (32 px plain).
        let gaps: Vec<f32> = bases.windows(2).map(|w| w[1].x - w[0].x).collect();
        let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
        let sd = (gaps.iter().map(|g| (g - mean) * (g - mean)).sum::<f32>() / gaps.len() as f32).sqrt();
        let between: Vec<f32> = ruby.windows(2).map(|w| w[1].x - (w[0].x + w[0].width)).collect();
        let off: f32 = ruby.iter().zip(&bases).map(|(r, b)| (mid(r) - mid(b)).abs()).fold(0.0, f32::max);
        println!(
            "spacing {spacing} em: pitch {mean:.1} px (sd {sd:.2}), readings {:.1} px, least gap between readings {:.1} px, most off centre {off:.1} px",
            ruby[0].px,
            between.iter().cloned().fold(f32::MAX, f32::min)
        );
        assert_eq!(ruby.len(), 10);
        assert!(sd < 0.01, "the layout's pitch: {gaps:?}");
        assert!((mean - 32.0 * (1.0 + spacing)).abs() < 0.5, "{mean}");
        assert!(ruby.iter().all(|r| r.px >= 16.0 * 0.55 - 0.01 && r.px == ruby[0].px), "one size, at least 55 %");
        assert!(ruby[0].x >= s.margin_left - 0.5, "inside the line");
        if ruby[0].px > 16.0 * 0.55 + 0.01 {
            // Not at the floor: centred, a gap apart.
            assert!(off < 0.5 && between.iter().all(|&g| g >= ruby[0].px * 0.35 - 0.5));
        }
    }
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

/// Counts its `annotate` calls; readings of `.1` letters for every ideograph.
struct Counting(std::sync::atomic::AtomicUsize, usize, f32);

impl TextAnnotator for Counting {
    fn id(&self) -> &str {
        "counting"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, p: &str, _: Option<&str>) -> bool {
        p.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    }
    fn ideograph_spacing(&self, _: Option<&str>) -> f32 {
        self.2
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<Annotation> {
        self.0.fetch_add(1, Ordering::Relaxed);
        p.char_indices().filter(|&(_, c)| ('\u{4e00}'..='\u{9fff}').contains(&c)).map(|(i, c)| Annotation::over(i..i + c.len_utf8(), "a".repeat(self.1))).collect()
    }
}

#[test]
fn pagination_never_calls_annotate_and_does_not_depend_on_its_output() {
    let body = "<p>床前明月光，疑是地上霜。举头望明月，低头思故乡。</p>".repeat(40);
    let mut maps = Vec::new();
    for letters in [1, 12] {
        let a = Arc::new(Counting(Default::default(), letters, 0.2));
        let mut s = settings();
        s.annotators = vec![AnnotatorHandle::new(a.clone())];
        let mut d = cjk_doc(&body, &s);
        assert!(d.wait_paginated(std::time::Duration::from_secs(60)));
        assert_eq!(a.0.load(Ordering::Relaxed), 0, "no annotate() while paginating");
        let n = d.page_count().pages;
        let map: Vec<String> = (0..n).map(|p| d.position_of_page(p).unwrap().to_string()).collect();
        let (bases, _) = page_runs(&mut d);
        assert!(a.0.load(Ordering::Relaxed) > 0, "drawing asks for them");
        maps.push((map, bases.iter().map(|b| (b.x, b.baseline)).collect::<Vec<_>>()));
    }
    assert_eq!(maps[0], maps[1], "the same pages and positions whatever annotate() returns");
}

#[test]
fn readings_stay_centred_when_they_fit_and_are_condensed_otherwise() {
    // Narrow readings: every one centred, at the normal size.
    let mut d = cjk_annotated("<p>床前明</p>", vec!["a", "bb", "ccc"]);
    let (bases, ruby) = page_runs(&mut d);
    for (r, b) in ruby.iter().zip(&bases) {
        assert!((mid(r) - mid(b)).abs() < 0.5, "{:?} off centre", r.text);
        assert_eq!(r.px, 16.0);
    }
    // A wide reading in the middle: the paragraph's readings get smaller, all still
    // centred and a gap apart; the characters do not move.
    let mut d = cjk_annotated("<p>床前明月光疑</p>", vec!["a", "a", "ccccccc", "a", "a", "a"]);
    let (bases, ruby) = page_runs(&mut d);
    assert!(ruby[0].px < 16.0 && ruby.iter().all(|r| r.px == ruby[0].px));
    for (r, b) in ruby.iter().zip(&bases) {
        assert!((mid(r) - mid(b)).abs() < 0.5, "{:?} off centre", r.text);
    }
    let gap = ruby[0].px * 0.35;
    for w in ruby.windows(2) {
        assert!(w[1].x - (w[0].x + w[0].width) >= gap - 0.5, "{:?} and {:?} too close", w[0].text, w[1].text);
    }
    assert!(bases.windows(2).all(|w| (w[1].x - w[0].x - 32.0).abs() < 0.01));
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
    let (text, ruby) = page_runs(&mut d);
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
    let mut d = annotated(body, Arc::new(Fixed(list)));
    let (_, ruby) = page_runs(&mut d);
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
    let mut d = annotated("<p>first</p><p>boom</p><p>third</p><p>fourth</p>", Arc::new(Boom));
    let (text, ruby) = page_runs(&mut d);
    let words: Vec<&str> = text.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(words, ["first", "boom", "third", "fourth"], "every paragraph is laid out");
    assert_eq!(ruby.len(), 1, "only the paragraph before the panic is annotated: {:?}", ruby.iter().map(|r| &r.text).collect::<Vec<_>>());
    assert!(ruby[0].baseline < run(&text, "first").baseline);
}

#[test]
fn a_version_change_redraws_without_paginating_again() {
    let a = Words::new("base", "old");
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(a.clone())];
    let mut d = doc_with(&["<p>one base two</p>"], "", &s);
    assert_eq!(page_runs(&mut d).1[0].text, "old");
    let pos = d.position_of_page(0).unwrap();
    // The annotator changes its output and its version: the settings stay equal (no new
    // pagination), the page drawn next has the new annotations.
    *a.text.lock().unwrap() = "new".into();
    a.version.store(2, Ordering::Relaxed);
    let mut s2 = s.clone();
    s2.annotators = vec![AnnotatorHandle::new(a.clone())];
    assert_eq!(s, s2);
    assert_eq!(s.cache_key(), s2.cache_key());
    assert_eq!(page_runs(&mut d).1[0].text, "new");
    assert_eq!(d.position_of_page(0).unwrap(), pos);
    // Turning annotations off (no annotator) or ruby off removes them.
    let mut off = s2.clone();
    off.annotators = Vec::new();
    assert_ne!(off.cache_key(), s2.cache_key());
    d.set_layout(&off).unwrap();
    assert!(page_runs(&mut d).1.is_empty());
    let mut hidden = s2.clone();
    hidden.ruby = false;
    d.set_layout(&hidden).unwrap();
    assert!(page_runs(&mut d).1.is_empty());
}

#[test]
fn giant_paragraphs_keep_annotations_whole() {
    // A paragraph over the giant-paragraph limit is broken into lines in chunks; an
    // annotated range never straddles two chunks.
    let body = format!("<p>{}</p>", "base ".repeat(70_000));
    let mut d = annotated(&body, Words::new("base", "B"));
    let (text, ruby) = page_runs(&mut d);
    assert!(!text.is_empty());
    assert!(ruby.len() > 10 && ruby.iter().all(|r| r.text == "B"));
}

/// Wants every paragraph with an ideograph (or, `idle`, none), asks for 0.15 em
/// between ideographs, and gives each ideograph a reading of `letters` letters.
struct EveryIdeograph {
    idle: bool,
    letters: usize,
    calls: std::sync::atomic::AtomicUsize,
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
    fn ideograph_spacing(&self, _: Option<&str>) -> f32 {
        0.15
    }
    fn annotate(&self, p: &str, _lang: Option<&str>) -> Vec<Annotation> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let reading = "a".repeat(self.letters);
        p.char_indices().filter(|&(_, c)| ('\u{4e00}'..='\u{9fff}').contains(&c)).map(|(i, c)| Annotation::over(i..i + c.len_utf8(), reading.as_str())).collect()
    }
}

/// A generated CJK book: `chapters` of 60 paragraphs of 150 ideographs and
/// punctuation (characters of the Thousand Character Classic, fixed pseudo-random order).
fn cjk_book(chapters: usize) -> Vec<String> {
    let pool: Vec<char> = "天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏闰余成岁律吕调阳云腾致雨露结为霜金生丽水玉出昆冈剑号巨阙珠称夜光果珍李柰菜重芥姜海咸河淡鳞潜羽翔".chars().collect();
    let mut seed = 12345u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (seed >> 16) as usize
    };
    (0..chapters)
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
        .collect()
}

/// Run with `cargo test --release -p ink-html --test ruby -- --ignored --nocapture measure`.
#[test]
#[ignore = "measurement: prints pagination times of generated CJK books (1 MB and novel length) without and with annotators, and the time to draw a page's annotations"]
fn measure_annotator_cost_on_big_cjk_books() {
    for (label, n) in [("1 MB book (40 chapters)", 40), ("novel (96 chapters, 0.9 M characters)", 96)] {
        let chapters = cjk_book(n);
        let refs: Vec<&str> = chapters.iter().map(String::as_str).collect();
        let chars: usize = chapters.iter().map(|c| c.chars().count()).sum();
        println!("{label}: {} KB of XHTML, {chars} characters", chapters.iter().map(String::len).sum::<usize>() / 1024);
        let variants: [(&str, Option<Arc<EveryIdeograph>>); 3] = [
            ("no annotator", None),
            ("annotator that wants nothing", Some(Arc::new(EveryIdeograph { idle: true, letters: 4, calls: Default::default() }))),
            ("annotator that wants every paragraph", Some(Arc::new(EveryIdeograph { idle: false, letters: 4, calls: Default::default() }))),
        ];
        for (name, a) in variants {
            let mut times = Vec::new();
            let mut pages = 0;
            for _ in 0..5 {
                let mut s = LayoutSettings::for_screen(1272, 1696);
                s.annotators = a.clone().map(|a| AnnotatorHandle::new(a as Arc<dyn TextAnnotator>)).into_iter().collect();
                let t = std::time::Instant::now();
                let mut d = doc_with(&refs, "", &s);
                assert!(d.wait_paginated(std::time::Duration::from_secs(600)));
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                pages = d.page_count().pages;
            }
            if let Some(a) = &a {
                assert_eq!(a.calls.load(Ordering::Relaxed), 0, "no annotate() while paginating");
            }
            times.sort_by(f64::total_cmp);
            println!("  {name}: {pages} pages, open + full pagination median {:.0} ms (min {:.0}, max {:.0}, n=5)", times[2], times[0], times[4]);
        }
        // Drawing pages with annotations: computed while drawing, then from the cache.
        let a = Arc::new(EveryIdeograph { idle: false, letters: 4, calls: Default::default() });
        let mut s = LayoutSettings::for_screen(1272, 1696);
        s.annotators = vec![AnnotatorHandle::new(a.clone())];
        let mut d = doc_with(&refs, "", &s);
        let mut plain_s = s.clone();
        plain_s.annotators = Vec::new();
        let mut plain = doc_with(&refs, "", &plain_s);
        d.set_annotations_sync(true);
        let mut canvas = ink_device::Canvas::new(1272, 1696);
        let pages = 20;
        let time = |d: &mut HtmlDocument, canvas: &mut ink_device::Canvas| {
            let t = std::time::Instant::now();
            for p in 0..pages {
                d.render_page(p, canvas).unwrap();
            }
            t.elapsed().as_secs_f64() * 1000.0 / pages as f64
        };
        let plain_ms = time(&mut plain, &mut canvas);
        let first = time(&mut d, &mut canvas);
        let cached = time(&mut d, &mut canvas);
        d.set_record_annotations(true);
        d.render_page(pages, &mut canvas).unwrap();
        let on_page = d.placed_annotations().len();
        assert!(on_page > 100, "{on_page}");
        println!("  page draw ({on_page} annotations a page): plain {plain_ms:.2} ms, with annotations computed while drawing {first:.2} ms, from the cache {cached:.2} ms (mean of {pages} pages)");
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
    let mut d = doc_with(&["<p>one base two</p>"], "", &s);
    let (_, ruby) = page_runs(&mut d);
    let mut got: Vec<&str> = ruby.iter().map(|r| r.text.as_str()).collect();
    got.sort();
    assert_eq!(got, ["FIRST", "THIRD"], "the second overlaps the first and is dropped");
    assert_eq!(first.seen.lock().unwrap().first().cloned().flatten().as_deref(), Some("en"), "wants gets the paragraph's language");
    // The cache key names every annotator in order.
    assert!(s.cache_key().ends_with("-aFIRST@0,SECOND@0,THIRD@0"), "{}", s.cache_key());
}

#[test]
fn a_language_check_in_wants_skips_other_paragraphs() {
    let a = lang("base", "B", Some("de"));
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(a)];
    let mut d = doc_with(&["<p>one base</p><p lang=\"de\">eine base</p>"], "", &s);
    let (_, ruby) = page_runs(&mut d);
    assert_eq!(ruby.len(), 1, "only the German paragraph");
}

#[test]
fn one_panicking_annotator_leaves_the_others_running() {
    let ok = lang("third", "T", None);
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Arc::new(Boom)), AnnotatorHandle::new(ok)];
    let mut d = doc_with(&["<p>boom</p><p>third</p>"], "", &s);
    let (_, ruby) = page_runs(&mut d);
    assert!(ruby.iter().any(|r| r.text == "T"), "the second annotator goes on after the first panicked");
}

/// Annotates every occurrence of `word` over and/or under it.
struct Tiers {
    word: &'static str,
    over: Option<&'static str>,
    under: Option<&'static str>,
}

impl TextAnnotator for Tiers {
    fn id(&self) -> &str {
        "tiers"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, p: &str, _: Option<&str>) -> bool {
        p.contains(self.word)
    }
    fn positions(&self) -> &[AnnotationPosition] {
        match (self.over.is_some(), self.under.is_some()) {
            (true, true) => &[AnnotationPosition::Over, AnnotationPosition::Under],
            (false, true) => &[AnnotationPosition::Under],
            _ => &[AnnotationPosition::Over],
        }
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<Annotation> {
        let mut v = Vec::new();
        for (i, w) in p.match_indices(self.word) {
            v.extend(self.over.map(|t| Annotation::over(i..i + w.len(), t)));
            v.extend(self.under.map(|t| Annotation::under(i..i + w.len(), t)));
        }
        v
    }
}

#[test]
fn annotations_under_the_text_are_drawn_below_it_and_coexist_with_those_over_it() {
    let mut d = annotated("<p>one base two</p>", Arc::new(Tiers { word: "base", over: Some("above"), under: Some("below") }));
    let (text, ruby) = page_runs(&mut d);
    let base = run(&text, "base");
    let (over, under) = (run(&ruby, "above"), run(&ruby, "below"));
    assert!(over.baseline < base.baseline - base.px * 0.6, "over above the base");
    assert!(under.baseline > base.baseline + under.px * 0.5, "under below the base: {} vs {}", under.baseline, base.baseline);
    assert!((mid(over) - mid(base)).abs() < 0.5 && (mid(under) - mid(base)).abs() < 0.5, "both centred");
    assert!(under.baseline < base.line_bottom, "inside the line box");
    // The line gets room below as well as above; a line with readings over only does not
    // grow below.
    let mut only_over = annotated("<p>one base two</p>", Arc::new(Tiers { word: "base", over: Some("above"), under: None }));
    let (t2, _) = page_runs(&mut only_over);
    let b2 = run(&t2, "base");
    assert!(base.line_bottom - base.baseline > b2.line_bottom - b2.baseline + 1.0);
    assert!(((base.baseline - base.line_top) - (b2.baseline - b2.line_top)).abs() < 0.01);
    // The text is the same as without them.
    let mut d = d;
    let around = d.read_around(&d.position_of_page(0).unwrap(), 100).unwrap();
    assert_eq!(around.trim(), "one base two");
}

#[test]
fn book_ruby_with_ruby_position_under_is_drawn_below() {
    let mut d = doc("<p>one <ruby>base<rt>below</rt></ruby> two</p>", "ruby { ruby-position: under }");
    let (text, ruby) = page_runs(&mut d);
    let (base, r) = (run(&text, "base"), run(&ruby, "below"));
    assert!(r.baseline > base.baseline, "under the base");
    // An annotation over the same base still shows: the book's ruby takes only its tier.
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Arc::new(Tiers { word: "base", over: Some("above"), under: Some("lost") }))];
    let mut d = doc_with(&["<p>one <ruby>base<rt>below</rt></ruby> two</p>"], "ruby { ruby-position: under }", &s);
    let (_, ruby) = page_runs(&mut d);
    let mut got: Vec<&str> = ruby.iter().map(|r| r.text.as_str()).collect();
    got.sort();
    assert_eq!(got, ["above", "below"], "the annotation under the base gives way to the book's");
}

#[test]
fn the_next_chapter_s_first_page_is_annotated_ahead_when_it_is_laid_out() {
    let ch = "<p>base one</p><p>base two</p><p>base three</p>";
    let wait_for = |d: &HtmlDocument, n: usize| {
        let t = std::time::Instant::now();
        while d.annotations_cached() < n && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        d.annotations_cached()
    };
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Words::new("base", "B"))];
    let mut canvas = ink_device::Canvas::new(W as usize, H as usize);
    // The next chapter is laid out already: its first page's paragraphs are queued too.
    let mut d = doc_with(&[ch, ch], "", &s);
    d.page_layout(1).unwrap();
    d.render_page(0, &mut canvas).unwrap();
    assert_eq!(wait_for(&d, 6), 6);
    // It is not: nothing is laid out for it.
    let mut d = doc_with(&[ch, ch], "", &s);
    d.render_page(0, &mut canvas).unwrap();
    assert_eq!(wait_for(&d, 3), 3);
}

/// CPU time of this process so far (`ps`, 10 ms steps).
fn cpu_ms() -> f64 {
    let out = std::process::Command::new("ps").args(["-o", "time=", "-p", &std::process::id().to_string()]).output().unwrap();
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // [[hh:]mm:]ss.cc
    t.split(':').fold(0.0, |acc, part| acc * 60.0 + part.trim().parse::<f64>().unwrap_or(0.0)) * 1000.0
}

/// Run alone: `cargo test --release -p ink-html --test ruby -- --ignored --nocapture idle`.
#[test]
#[ignore = "measurement: prints the CPU the annotation thread uses while idle"]
fn measure_idle_annotation_thread() {
    let mut s = settings();
    s.annotators = vec![AnnotatorHandle::new(Words::new("base", "B"))];
    let mut d = doc_with(&["<p>base one</p>".repeat(200).as_str()], "", &s);
    let mut canvas = ink_device::Canvas::new(W as usize, H as usize);
    d.render_page(0, &mut canvas).unwrap();
    while d.annotations_pending() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(d.wait_paginated(std::time::Duration::from_secs(30)));
    std::thread::sleep(std::time::Duration::from_millis(500));
    let before = cpu_ms();
    std::thread::sleep(std::time::Duration::from_secs(5));
    let used = cpu_ms() - before;
    println!("process CPU over 5 s with the annotation thread idle: {used:.0} ms");
}

/// Run with `cargo test --release -p ink-html --test ruby -- --ignored --nocapture heavy`.
#[test]
#[ignore = "measurement: prints the pagination time of a generated book with <ruby> on every ideograph"]
fn measure_heavy_book_ruby() {
    let plain = cjk_book(20);
    let ruby: Vec<String> = plain
        .iter()
        .map(|ch| {
            let mut out = String::with_capacity(ch.len() * 4);
            for c in ch.chars() {
                if ('\u{4e00}'..='\u{9fff}').contains(&c) {
                    out.push_str(&format!("<ruby>{c}<rt>abcd</rt></ruby>"));
                } else {
                    out.push(c);
                }
            }
            out
        })
        .collect();
    for (name, book) in [("plain", &plain), ("ruby on every ideograph", &ruby)] {
        let refs: Vec<&str> = book.iter().map(String::as_str).collect();
        let mut times = Vec::new();
        let mut pages = 0;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let mut d = doc_with(&refs, "", &LayoutSettings::for_screen(1272, 1696));
            assert!(d.wait_paginated(std::time::Duration::from_secs(600)));
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            pages = d.page_count().pages;
        }
        times.sort_by(f64::total_cmp);
        println!("{name}: {pages} pages, open + full pagination median {:.0} ms (min {:.0}, max {:.0}, n=5)", times[2], times[0], times[4]);
    }
}

/// The space an annotator asks for between ideographs goes by character, whichever face
/// draws them (here the book's font has no CJK glyphs, so another face or the missing
/// glyph box does), and pagination counts it.
#[test]
fn ideograph_spacing_applies_to_text_in_any_face_and_paginates() {
    let body = format!("<p>{}</p>", "天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏".repeat(6)).repeat(60);
    let pitch_and_pages = |spacing: f32| {
        let mut s = settings();
        s.annotators = vec![AnnotatorHandle::new(Arc::new(Counting(Default::default(), 2, spacing)))];
        let mut d = doc_with(&[body.as_str()], "", &s);
        assert!(d.wait_paginated(std::time::Duration::from_secs(60)));
        let (bases, _) = page_runs(&mut d);
        let line: Vec<&TextRun> = bases.iter().filter(|b| b.line_top == bases[0].line_top).collect();
        (line[1].x - line[0].x, d.page_count().pages)
    };
    let (p0, n0) = pitch_and_pages(0.0);
    let (p1, n1) = pitch_and_pages(0.25);
    println!("pitch {p0:.1} -> {p1:.1} px, pages {n0} -> {n1}");
    assert!((p1 - p0 - 0.25 * 32.0).abs() < 0.5, "a quarter em more: {p0} -> {p1}");
    assert!(n1 > n0, "more pages: {n0} -> {n1}");
}
