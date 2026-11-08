//! One test per supported CSS property / feature, checked on the laid-out page
//! (draw items: positions, faces, sizes) of small documents.

use ink_device::Canvas;
use ink_doc::{Document, LayoutSettings, MemoryLoader, Metadata};
use ink_html::layout::{ChapterLayout, Draw, TextRun};
use ink_html::{HtmlChapter, HtmlDocument};

/// The crate directory: `INK_MANIFEST_DIR` when set (test binaries run on the Kindle
/// emulator with the fixtures copied elsewhere), else the build-time directory.
fn manifest_dir() -> std::path::PathBuf {
    std::env::var_os("INK_MANIFEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

const W: u32 = 1000;
const H: u32 = 1400;

fn settings() -> LayoutSettings {
    let mut s = LayoutSettings::for_screen(W, H);
    s.margin_left = 50.0;
    s.margin_right = 50.0;
    s.margin_top = 50.0;
    s.margin_bottom = 50.0;
    s.font_px = 32.0;
    s.line_spacing = 1.2;
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
            html: format!(
                "<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><link rel=\"stylesheet\" href=\"style.css\"/></head><body>{body}</body></html>"
            ),
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

fn layout(d: &HtmlDocument, page: usize) -> ChapterLayout {
    d.page_layout(page).unwrap().0
}

fn runs(l: &ChapterLayout) -> Vec<TextRun> {
    l.items
        .iter()
        .filter_map(|i| match i {
            Draw::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Text runs of one page only.
fn page_runs(d: &HtmlDocument, p: usize) -> Vec<TextRun> {
    let (l, span) = d.page_layout(p).unwrap();
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

fn run(l: &ChapterLayout, word: &str) -> TextRun {
    runs(l).into_iter().find(|t| t.text.trim_end_matches(['.', ',', ':']) == word).unwrap_or_else(|| {
        panic!("no run {word:?} in {:?}", runs(l).iter().map(|t| t.text.clone()).collect::<Vec<_>>())
    })
}

/// Right edge of the content box.
const RIGHT: f32 = (W - 50) as f32;
const LEFT: f32 = 50.0;

#[test]
fn selectors_type_class_id_descendant_child_attribute_first_child() {
    let d = doc(
        r#"<p class="c">Klass</p><p id="i">Ident</p><div><p>Desc</p></div><section><p>FirstKid</p><p>SecondKid</p></section>
           <div class="x"><p>ChildMatch</p><div><p>GrandChild</p></div></div><p data-k="v">Attr</p>"#,
        ".c { font-weight: bold } #i { font-style: italic } div p { font-size: 20px } section p:first-child { font-weight: bold }
         div.x > p { font-style: italic } p[data-k=v] { font-weight: bold }",
    );
    let l = layout(&d, 0);
    assert!(run(&l, "Klass").face.bold);
    assert!(run(&l, "Ident").face.italic);
    assert_eq!(run(&l, "Desc").px, 40.0, "20 CSS px at 32px/16 = 2 device px per CSS px");
    assert!(run(&l, "FirstKid").face.bold && !run(&l, "SecondKid").face.bold);
    assert!(run(&l, "ChildMatch").face.italic && !run(&l, "GrandChild").face.italic);
    assert!(run(&l, "Attr").face.bold);
}

#[test]
fn cascade_specificity_importance_inheritance() {
    let d = doc(
        r#"<p class="a" id="b">Spec</p><p class="imp" id="imp">Imp</p><div class="it"><p>Inherit <span>Child</span></p></div><p style="font-weight:bold" class="n">Inline</p>"#,
        "#b { font-weight: bold } p.a { font-weight: normal } .imp { font-style: italic !important } #imp { font-style: normal }
         .it { font-style: italic } .n { font-weight: normal }",
    );
    let l = layout(&d, 0);
    assert!(run(&l, "Spec").face.bold, "id beats class");
    assert!(run(&l, "Imp").face.italic, "!important beats id");
    assert!(run(&l, "Child").face.italic, "inherited through p and span");
    assert!(run(&l, "Inline").face.bold, "style attribute beats class");
}

#[test]
fn font_properties() {
    let d = doc(
        r#"<p><span class="big">Big</span> <span class="pt">Pt</span> <b>Bold</b> <i>Ital</i> <b><i>BoldIt</i></b> <code>Mono</code> <span class="f">Short</span> <span class="kw">Kw</span></p>"#,
        ".big { font-size: 1.5em } .pt { font-size: 12pt } .f { font: bold italic 10px serif } .kw { font-size: larger }",
    );
    let l = layout(&d, 0);
    assert_eq!(run(&l, "Big").px, 48.0);
    assert_eq!(run(&l, "Pt").px, 32.0, "12pt = 16 CSS px = the base size");
    assert!(run(&l, "Bold").face.bold && !run(&l, "Bold").face.italic);
    assert!(run(&l, "Ital").face.italic);
    let bi = run(&l, "BoldIt");
    assert!(bi.face.bold && bi.face.italic);
    assert_eq!(run(&l, "Mono").face.family, 1, "monospace family");
    let s = run(&l, "Short");
    assert!(s.face.bold && s.face.italic && s.px == 20.0, "font shorthand");
    assert!((run(&l, "Kw").px - 32.0 * 1.2).abs() < 0.01);
}

#[test]
fn text_align_and_indent() {
    let d = doc(
        r#"<p class="l">Left</p><p class="r">Right</p><p class="c">Center</p><p class="i">Indented</p><p class="j">aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp qqq rrr sss ttt uuu vvv www xxx yyy zzz end</p>"#,
        "p { text-indent: 0 } .l { text-align: left } .r { text-align: right } .c { text-align: center } .i { text-indent: 3em } .j { text-align: justify }",
    );
    let l = layout(&d, 0);
    assert!((run(&l, "Left").x - LEFT).abs() < 1.0);
    let r = run(&l, "Right");
    assert!((r.x + r.width - RIGHT).abs() < 1.0, "{} {}", r.x, r.width);
    let c = run(&l, "Center");
    assert!(((c.x + c.width / 2.0) - (LEFT + RIGHT) / 2.0).abs() < 1.0);
    assert!((run(&l, "Indented").x - (LEFT + 96.0)).abs() < 1.0);
    // Justified: the first line ends at the right edge.
    let first_line: Vec<TextRun> = runs(&l).into_iter().filter(|t| t.line_top == run(&l, "aaa").line_top).collect();
    let last = first_line.last().unwrap();
    assert!((last.x + last.width - RIGHT).abs() < 1.5, "{}", last.x + last.width);
}

/// A justified line with an inline image followed by a space still ends at the right
/// edge (the space after the image used to be counted but never added).
#[test]
fn justified_line_with_an_inline_image() {
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"></svg>"#;
    let d = doc(
        &format!("<p>aaa {svg} bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp qqq rrr sss ttt uuu vvv www xxx yyy zzz end</p>"),
        "p { text-indent: 0; text-align: justify }",
    );
    let l = layout(&d, 0);
    let first_line: Vec<TextRun> = runs(&l).into_iter().filter(|t| t.line_top == run(&l, "aaa").line_top).collect();
    let last = first_line.last().unwrap();
    assert!((last.x + last.width - RIGHT).abs() < 1.5, "{}", last.x + last.width);
}

/// The user's paragraph spacing and indent on top of the book's CSS.
#[test]
fn user_paragraph_spacing_and_indent() {
    let body = r#"<p>Alpha</p><p>Beta</p><p class="c">Gamma</p><p>Delta</p>"#;
    let css = "p { text-indent: 1em; margin: 0 } .c { text-align: center }";
    let with = |f: &dyn Fn(&mut LayoutSettings)| {
        let mut s = settings();
        f(&mut s);
        doc_with(&[body], css, &s)
    };
    let top = |d: &HtmlDocument, w: &str| run(&layout(d, 0), w).baseline;
    let x = |d: &HtmlDocument, w: &str| run(&layout(d, 0), w).x;
    let base = with(&|_| {});
    assert!((x(&base, "Alpha") - (LEFT + 32.0)).abs() < 1.0, "the book's 1 em indent");
    let gap0 = top(&base, "Beta") - top(&base, "Alpha");
    // Half an em of extra space after each paragraph: 16 px more between baselines.
    let spaced = with(&|s| s.paragraph_spacing = 0.5);
    let gap1 = top(&spaced, "Beta") - top(&spaced, "Alpha");
    assert!((gap1 - gap0 - 16.0).abs() < 0.5, "{gap0} {gap1}");
    let spaced1 = with(&|s| s.paragraph_spacing = 1.0);
    assert!((top(&spaced1, "Beta") - top(&spaced1, "Alpha") - gap0 - 32.0).abs() < 0.5);
    // Indent override: none, 2 em; the centred paragraph keeps its centring.
    let none = with(&|s| s.paragraph_indent = Some(0.0));
    assert!((x(&none, "Alpha") - LEFT).abs() < 1.0);
    let two = with(&|s| s.paragraph_indent = Some(2.0));
    assert!((x(&two, "Alpha") - (LEFT + 64.0)).abs() < 1.0);
    let (c0, c2) = (run(&layout(&base, 0), "Gamma"), run(&layout(&two, 0), "Gamma"));
    assert!((c0.x - c2.x).abs() < 0.5, "centred paragraph untouched");
    // Without the book's CSS the override still applies.
    let plain = with(&|s| {
        s.publisher_styles = false;
        s.paragraph_indent = Some(1.5);
    });
    assert!((x(&plain, "Alpha") - (LEFT + 48.0)).abs() < 1.0);
}

#[test]
fn margins_padding_borders() {
    let d = doc(
        r#"<p class="a">One</p><p class="b">Two</p><div class="box"><p>Boxed</p></div><p class="auto">Auto</p>"#,
        "p { text-indent: 0; margin: 0 } .a { margin-bottom: 20px } .b { margin-top: 30px }
         .box { border: 4px solid; padding: 10px; margin-left: 15px } .auto { width: 200px; margin: 0 auto }",
    );
    let l = layout(&d, 0);
    let one = run(&l, "One");
    let two = run(&l, "Two");
    // Collapsed margin: max(20, 30) CSS px = 60 device px between the line boxes.
    assert!((two.line_top - one.line_bottom - 60.0).abs() < 0.5, "{} {}", one.line_bottom, two.line_top);
    let boxed = run(&l, "Boxed");
    // margin 15 + border 4 + padding 10 CSS px.
    assert!((boxed.x - (LEFT + 2.0 * 29.0)).abs() < 1.0, "{}", boxed.x);
    let borders: Vec<_> = l.decor.iter().filter(|r| (r.w - 8.0).abs() < 0.1 || (r.h - 8.0).abs() < 0.1).collect();
    assert_eq!(borders.len(), 4, "{:?}", l.decor);
    let auto = run(&l, "Auto");
    assert!((auto.x - ((LEFT + RIGHT) / 2.0 - 200.0)).abs() < 1.0, "{}", auto.x);
}

#[test]
fn line_height() {
    let text = "word ".repeat(60);
    let d = doc(&format!(r#"<p class="n">{text}</p><p class="d">{text}</p>"#), "p { text-indent: 0 } .d { line-height: 2.4 }");
    let l = layout(&d, 0);
    let tops = |cls: usize| {
        let mut v: Vec<f32> = runs(&l).iter().map(|t| t.line_top).collect();
        v.dedup();
        v.windows(2).map(|w| w[1] - w[0]).nth(cls).unwrap()
    };
    // normal: font 32 x user spacing 1.2; 2.4 scaled by 1.2/1.2.
    assert!((tops(0) - 38.4).abs() < 0.5, "{}", tops(0));
    let mut all: Vec<f32> = runs(&l).iter().map(|t| t.line_top).collect();
    all.dedup();
    let last = all.len() - 1;
    assert!((all[last] - all[last - 1] - 76.8).abs() < 0.5);
}

#[test]
fn display_values() {
    let d = doc(
        r#"<p>Before <span class="none">Hidden</span> <span class="blk">Blocky</span> after</p><div class="li">Itemized</div>
           <div class="t"><div class="r"><div class="c">CellA</div><div class="c">CellB</div></div></div><div class="inl">InlA</div><div class="inl">InlB</div>"#,
        ".none { display: none } .blk { display: block } .li { display: list-item; margin-left: 2em } .t { display: table } .r { display: table-row } .c { display: table-cell }
         .inl { display: inline }",
    );
    let l = layout(&d, 0);
    assert!(!runs(&l).iter().any(|t| t.text.contains("Hidden")));
    let b = run(&l, "Before");
    let blk = run(&l, "Blocky");
    assert!(blk.line_top > b.line_top && run(&l, "after").line_top > blk.line_top, "block span on its own line");
    let item = run(&l, "Itemized");
    assert!(runs(&l).iter().any(|t| t.text == "•" && t.line_top == item.line_top && t.x < item.x), "list-item marker");
    let (a, bb) = (run(&l, "CellA"), run(&l, "CellB"));
    assert_eq!(a.baseline, bb.baseline, "table cells side by side");
    assert!(bb.x > a.x + a.width);
    assert_eq!(run(&l, "InlA").line_top, run(&l, "InlB").line_top, "display: inline");
}

#[test]
fn page_breaks() {
    let d = doc_with(
        &[r#"<p>Alpha</p><p class="before">Beta</p><p class="after">Gamma</p><p>Delta</p><h2>Heading</h2><p>Epsilon</p>"#],
        ".before { page-break-before: always } .after { page-break-after: always }",
        &settings(),
    );
    let page_of_word = |w: &str| {
        (0..d.page_count().pages)
            .find(|&p| {
                let (l, span) = d.page_layout(p).unwrap();
                let a = l.slices[span.first as usize].first_item as usize;
                let b = l.slices[span.end as usize - 1].end_item as usize;
                l.items[a..b].iter().any(|i| matches!(i, Draw::Text(t) if t.text.starts_with(w)))
            })
            .unwrap()
    };
    assert_eq!(page_of_word("Alpha"), 0);
    assert_eq!(page_of_word("Beta"), 1);
    assert_eq!(page_of_word("Gamma"), 1);
    assert_eq!(page_of_word("Delta"), 2);
    // A heading is kept with the following text (page-break-after: avoid in the UA sheet).
    let filler = "<p>filler text line</p>".repeat(29);
    let d2 = doc_with(&[&format!("{filler}<h2>KeepMe</h2><p>Following</p>")], "p { margin: 0 }", &settings());
    let pages = d2.page_count().pages;
    let hp = (0..pages).find(|&p| page_runs(&d2, p).iter().any(|t| t.text == "KeepMe")).unwrap();
    let fp = (0..pages).find(|&p| page_runs(&d2, p).iter().any(|t| t.text == "Following")).unwrap();
    assert_eq!(hp, fp);
    // page-break-inside: avoid moves a box that would be split.
    let box_ = "<div class=\"k\"><p>InsideOne</p><p>x</p><p>x</p><p>InsideLast</p></div>";
    let d3 = doc_with(&[&format!("{}{box_}", "<p>f</p>".repeat(30))], "p { margin: 0 } .k { page-break-inside: avoid }", &settings());
    let pages = d3.page_count().pages;
    let p1 = (0..pages).find(|&p| page_runs(&d3, p).iter().any(|t| t.text == "InsideOne")).unwrap();
    let p2 = (0..pages).find(|&p| page_runs(&d3, p).iter().any(|t| t.text == "InsideLast")).unwrap();
    assert_eq!(p1, p2);
}

#[test]
fn white_space() {
    let d = doc(
        "<p class=\"pre\">a  b\nc</p><p class=\"nw\">one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen</p><p>collapse   these    spaces</p>",
        "p { text-indent: 0 } .pre { white-space: pre } .nw { white-space: nowrap }",
    );
    let l = layout(&d, 0);
    let rs = runs(&l);
    assert_eq!(rs[0].text, "a  ", "pre keeps spaces");
    assert_eq!(rs[1].text, "b");
    assert_eq!(rs[0].line_top, rs[1].line_top);
    assert_eq!(rs[2].text, "c");
    assert!(rs[2].line_top > rs[0].line_top, "pre keeps newlines");
    assert!(rs.iter().any(|t| t.text.starts_with("one two") && t.text.ends_with("sixteen")), "nowrap keeps one line");
    let c = run(&l, "collapse");
    let t = run(&l, "these");
    let space = t.x - (c.x + c.width);
    assert!(space > 0.0 && space < 20.0, "collapsed to one space: {space}");
}

#[test]
fn vertical_align_sub_sup() {
    let d = doc("<p>Base<sup>Up</sup> and<sub>Down</sub> <sup><a href=\"#x\">Nested</a></sup></p>", "");
    let l = layout(&d, 0);
    let base = run(&l, "Base").baseline;
    assert!(run(&l, "Up").baseline < base - 5.0);
    assert!(run(&l, "Down").baseline > base + 3.0);
    assert!(run(&l, "Nested").baseline < base - 5.0, "shift applies to descendants of sup");
    assert!(run(&l, "Up").px < 32.0);
}

#[test]
fn text_transform_decoration_spacing() {
    let d = doc(
        r#"<p><span class="u">upper case</span> <span class="c">cap words</span> <u>Under</u> <s>Strike</s> <span class="ls">Spaced</span> <span class="sc">Small</span></p>"#,
        ".u { text-transform: uppercase } .c { text-transform: capitalize } .ls { letter-spacing: 4px } .sc { font-variant: small-caps }",
    );
    let l = layout(&d, 0);
    assert!(runs(&l).iter().any(|t| t.text == "UPPER"));
    assert!(runs(&l).iter().any(|t| t.text == "Cap") && runs(&l).iter().any(|t| t.text == "Words"));
    assert!(run(&l, "Under").underline);
    assert!(run(&l, "Strike").strike);
    assert_eq!(run(&l, "Spaced").letter_spacing, 8.0);
    // Small caps: "S" at full size, "MALL" smaller.
    let s = run(&l, "S");
    let mall = run(&l, "MALL");
    assert_eq!(s.px, 32.0);
    assert!(mall.px < 30.0 && (mall.x - (s.x + s.width)).abs() < 0.5);
}

#[test]
fn user_overrides() {
    let body = format!("<p class=\"c\">Centered</p><p>{}</p>", "justify me please ".repeat(20));
    let css = ".c { text-align: center } p { font-family: 'Book Font', serif; color: #000 }";
    let base = settings();
    let d = doc_with(&[&body], css, &base);
    let l = layout(&d, 0);
    // Size.
    let mut s = base.clone();
    s.font_px = 40.0;
    let big = doc_with(&[&body], css, &s);
    assert_eq!(run(&layout(&big, 0), "Centered").px, 40.0);
    // Spacing.
    let mut s = base.clone();
    s.line_spacing = 2.0;
    let sp = doc_with(&[&body], css, &s);
    let ls = layout(&sp, 0);
    let mut tops: Vec<f32> = runs(&ls).iter().filter(|t| t.text == "justify").map(|t| t.line_top).collect();
    tops.dedup();
    assert!((tops[1] - tops[0] - 64.0).abs() < 0.5, "{tops:?}");
    // Margins.
    let mut s = base.clone();
    s.margin_left = 120.0;
    let m = doc_with(&[&body], css, &s);
    assert!((run(&layout(&m, 0), "justify").x - 120.0 - 32.0 * 1.2).abs() < 1.0);
    // Alignment: justification off.
    let first_line_end = |l: &ChapterLayout| {
        let top = runs(l).iter().find(|t| t.text == "justify").unwrap().line_top;
        let r = runs(l).into_iter().rfind(|t| t.line_top == top).unwrap();
        r.x + r.width
    };
    assert!((first_line_end(&l) - RIGHT).abs() < 1.5);
    let mut s = base.clone();
    s.justify = false;
    let nj = doc_with(&[&body], css, &s);
    assert!(first_line_end(&layout(&nj, 0)) < RIGHT - 5.0);
    assert!((run(&layout(&nj, 0), "Centered").x - run(&l, "Centered").x).abs() < 0.5, "centered stays centered");
    // Publisher styles off: the book's text-align is ignored.
    let mut s = base.clone();
    s.publisher_styles = false;
    let np = doc_with(&[&body], css, &s);
    assert!(run(&layout(&np, 0), "Centered").x < run(&l, "Centered").x - 50.0);
    // Font family override (a system family from a font directory).
    let mut s = base.clone();
    s.font_family = Some("Noto Serif".into());
    let mut f = doc_with(&[&body], css, &s);
    assert!(f.add_system_font("Noto Serif", Some(&manifest_dir().join("../../fonts"))));
    f.set_layout(&s).unwrap();
    assert!(run(&layout(&f, 0), "Centered").face.family >= 2, "the user's family, not the default");
    let mut c = Canvas::new(W as usize, H as usize);
    f.render_page(0, &mut c).unwrap();
    assert!(c.pixels.iter().any(|p| *p < 64));
}

#[test]
fn font_face_embedded_fonts() {
    let d = ink_formats::open(&manifest_dir().join("tests/fixtures/fonts.epub")).unwrap();
    let mut d = d;
    d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
    let l = d.page_layout(0).unwrap().0;
    let rs = runs(&l);
    let serif = rs.iter().find(|t| t.text == "Default").unwrap();
    let sans = rs.iter().find(|t| t.text == "Book").unwrap();
    assert_eq!(serif.face.family, 0);
    assert!(sans.face.family >= 2, "family from @font-face");
    let bold = rs.iter().find(|t| t.text == "bold").unwrap();
    assert!(bold.face.bold && bold.face.family == sans.face.family);
    let broken = rs.iter().find(|t| t.text == "file").unwrap();
    assert_eq!(broken.face.family, 0, "unreadable font falls back");
    // Same text, different glyphs: the embedded face measures differently.
    let mut fb = ink_html::fonts::FontBook::builtin();
    assert_ne!(fb.width(serif.face, sans.px, "Book"), sans.width);
}

#[test]
fn hyphenation_by_language() {
    let path = manifest_dir().join("tests/fixtures/lang.epub");
    let mut d = ink_formats::open(&path).unwrap();
    let mut s = LayoutSettings::for_screen(1272, 1696);
    s.hyphenate = true;
    d.set_layout(&s).unwrap();
    let rs = runs(&d.page_layout(0).unwrap().0);
    let hyphenated: Vec<&str> = rs.iter().filter(|t| t.text.ends_with('-') && t.text.len() > 2).map(|t| t.text.as_str()).collect();
    for want in ["Char-", "Donaudampf-", "Arbejdsløsheds-"] {
        assert!(hyphenated.iter().any(|h| h.starts_with(&want[..want.len() - 1]) && h.ends_with('-')), "{want} in {hyphenated:?}");
    }
    // The hyphens:none paragraph is never hyphenated.
    let none_para: Vec<&TextRun> = rs.iter().skip_while(|t| !t.text.starts_with("none")).take_while(|t| !t.text.starts_with("Soft")).collect();
    assert!(none_para.len() > 5);
    assert!(none_para.iter().all(|t| !t.text.ends_with('-')), "{:?}", none_para.iter().map(|t| &t.text).collect::<Vec<_>>());
    // Hyphenation off: only soft hyphens break words.
    s.hyphenate = false;
    d.set_layout(&s).unwrap();
    let rs = runs(&d.page_layout(0).unwrap().0);
    assert!(!rs.iter().any(|t| t.text.starts_with("Donaudampf") && t.text.ends_with('-')));
    assert!(rs.iter().any(|t| t.text.starts_with("super") && t.text.ends_with('-')) || rs.iter().any(|t| t.text.contains("supercali")));
}
