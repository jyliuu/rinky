//! Writes the synthetic feature fixtures in `crates/ink-html/tests/fixtures/*.epub`.
//!
//! Run: `cargo run -p ink-html --example make_fixtures [OUT_DIR]`. Every fixture is
//! generated here (text, CSS, images), except the embedded Noto Sans font of
//! `fonts.epub`, which is read from `crates/ink-html/tests/fixtures/src/` (SIL OFL).
//! The output is deterministic: zip timestamps are fixed.

use image::{DynamicImage, GrayImage, ImageFormat, Luma, Rgb, RgbImage};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// One EPUB to write.
struct Epub {
    name: &'static str,
    title: &'static str,
    lang: &'static str,
    /// (path inside OEBPS/, bytes, media type, properties)
    files: Vec<(String, Vec<u8>, &'static str)>,
    /// Manifest ids of the spine, in order; ids not in the manifest are written as is.
    spine: Vec<String>,
    /// (title, href relative to OEBPS/, level 0..)
    toc: Vec<(String, String, u8)>,
    /// (label, href)
    page_list: Vec<(String, String)>,
}

impl Epub {
    fn new(name: &'static str, title: &'static str, lang: &'static str) -> Self {
        Epub { name, title, lang, files: Vec::new(), spine: Vec::new(), toc: Vec::new(), page_list: Vec::new() }
    }

    fn chapter(&mut self, path: &str, body: &str, head_extra: &str) {
        let html = xhtml(self.title, self.lang, head_extra, body);
        self.files.push((path.to_string(), html.into_bytes(), "application/xhtml+xml"));
        self.spine.push(id_of(path));
    }

    fn raw_chapter(&mut self, path: &str, html: &str) {
        self.files.push((path.to_string(), html.as_bytes().to_vec(), "application/xhtml+xml"));
        self.spine.push(id_of(path));
    }

    fn file(&mut self, path: &str, bytes: Vec<u8>, media: &'static str) {
        self.files.push((path.to_string(), bytes, media));
    }

    fn toc(&mut self, title: &str, href: &str, level: u8) {
        self.toc.push((title.to_string(), href.to_string(), level));
    }
}

fn id_of(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn xhtml(title: &str, lang: &str, head_extra: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{lang}\" lang=\"{lang}\">\n<head>\n<title>{title}</title>\n\
         <link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n{head_extra}</head>\n<body>\n{body}\n</body>\n</html>\n"
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn write_epub(e: &Epub, dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{}.epub", e.name));
    let mut buf = Cursor::new(Vec::new());
    {
        let mut z = ZipWriter::new(&mut buf);
        let t = DateTime::from_date_and_time(2026, 10, 24, 12, 0, 0).unwrap();
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored).last_modified_time(t);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).last_modified_time(t);
        z.start_file("mimetype", stored)?;
        z.write_all(b"application/epub+zip")?;
        z.start_file("META-INF/container.xml", deflated)?;
        z.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"#,
        )?;
        // OPF
        let mut manifest = String::new();
        for (p, _, media) in &e.files {
            manifest.push_str(&format!("    <item id=\"{}\" href=\"{}\" media-type=\"{}\"/>\n", id_of(p), esc(p), media));
        }
        manifest.push_str("    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
        manifest.push_str("    <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n");
        let spine: String = e.spine.iter().map(|id| format!("    <itemref idref=\"{id}\"/>\n")).collect();
        let opf = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"uid\">\n\
             <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n    <dc:identifier id=\"uid\">urn:uuid:rinky-fixture-{name}</dc:identifier>\n\
             <dc:title>{title}</dc:title>\n    <dc:creator>rinky fixtures</dc:creator>\n    <dc:language>{lang}</dc:language>\n\
             <meta property=\"dcterms:modified\">2026-10-24T12:00:00Z</meta>\n</metadata>\n<manifest>\n{manifest}</manifest>\n\
             <spine toc=\"ncx\">\n{spine}</spine>\n</package>\n",
            name = e.name,
            title = esc(e.title),
            lang = e.lang
        );
        z.start_file("OEBPS/content.opf", deflated)?;
        z.write_all(opf.as_bytes())?;
        // nav.xhtml (EPUB 3)
        let mut nav = String::from("<nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1>\n");
        nav.push_str(&nav_list(&e.toc));
        nav.push_str("</nav>\n");
        if !e.page_list.is_empty() {
            nav.push_str("<nav epub:type=\"page-list\" hidden=\"\"><ol>\n");
            for (label, href) in &e.page_list {
                nav.push_str(&format!("<li><a href=\"{}\">{}</a></li>\n", esc(href), esc(label)));
            }
            nav.push_str("</ol></nav>\n");
        }
        z.start_file("OEBPS/nav.xhtml", deflated)?;
        z.write_all(xhtml("Contents", e.lang, "", &nav).replace("<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n", "").as_bytes())?;
        // toc.ncx (EPUB 2)
        let mut ncx = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
             <head><meta name=\"dtb:uid\" content=\"urn:uuid:rinky-fixture-{}\"/></head>\n<docTitle><text>{}</text></docTitle>\n<navMap>\n",
            e.name,
            esc(e.title)
        );
        let mut order = 0;
        ncx.push_str(&ncx_points(&e.toc, &mut 0, 0, &mut order));
        ncx.push_str("</navMap>\n");
        if !e.page_list.is_empty() {
            ncx.push_str("<pageList>\n");
            for (i, (label, href)) in e.page_list.iter().enumerate() {
                ncx.push_str(&format!(
                    "<pageTarget id=\"p{i}\" type=\"normal\" value=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"{}\"/></pageTarget>\n",
                    i + 1,
                    esc(label),
                    esc(href)
                ));
            }
            ncx.push_str("</pageList>\n");
        }
        ncx.push_str("</ncx>\n");
        z.start_file("OEBPS/toc.ncx", deflated)?;
        z.write_all(ncx.as_bytes())?;
        for (p, bytes, media) in &e.files {
            let opts = if media.starts_with("image/") && !media.contains("svg") { stored } else { deflated };
            z.start_file(format!("OEBPS/{p}"), opts)?;
            z.write_all(bytes)?;
        }
        z.finish()?;
    }
    std::fs::write(&path, buf.into_inner())?;
    Ok(path)
}

fn nav_list(toc: &[(String, String, u8)]) -> String {
    // Nested <ol> from levels.
    let mut out = String::from("<ol>\n");
    let mut level = 0u8;
    let mut open_li = false;
    for (title, href, l) in toc {
        while level < *l {
            out.push_str("<ol>\n");
            level += 1;
            open_li = false;
        }
        while level > *l {
            out.push_str("</li></ol>\n");
            level -= 1;
            open_li = true;
        }
        if open_li {
            out.push_str("</li>\n");
        }
        out.push_str(&format!("<li><a href=\"{}\">{}</a>", esc(href), esc(title)));
        open_li = true;
    }
    while level > 0 {
        out.push_str("</li></ol>\n");
        level -= 1;
    }
    if open_li {
        out.push_str("</li>\n");
    }
    out.push_str("</ol>\n");
    out
}

fn ncx_points(toc: &[(String, String, u8)], i: &mut usize, level: u8, order: &mut usize) -> String {
    let mut out = String::new();
    while *i < toc.len() {
        let (title, href, l) = &toc[*i];
        if *l < level {
            break;
        }
        *order += 1;
        *i += 1;
        out.push_str(&format!(
            "<navPoint id=\"np{0}\" playOrder=\"{0}\"><navLabel><text>{1}</text></navLabel><content src=\"{2}\"/>\n",
            order,
            esc(title),
            esc(href)
        ));
        if *i < toc.len() && toc[*i].2 > *l {
            out.push_str(&ncx_points(toc, i, *l + 1, order));
        }
        out.push_str("</navPoint>\n");
    }
    out
}

const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore \
et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo \
consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.";

/// Whitespace, entities, comments, self-closing tags, tables without tbody, inline SVG,
/// long text nodes: the cases where xpointers depend on exact DOM building rules.
fn ws() -> Epub {
    let mut e = Epub::new("ws", "Whitespace and xpointers", "en");
    e.file("style.css", b"p { margin: 0.5em 0; }\n.pre { white-space: pre; }\nspan.blk { display: block; }\n".to_vec(), "text/css");
    let long: String = (0..60).map(|i| format!("Sentence number {i} of a very long paragraph that goes on. ")).collect::<String>().repeat(3);
    let body = format!(
        r#"  <h1 id="top">Whitespace   and   offsets</h1>
  <p>Plain    text with    runs of
     spaces and newlines.</p>
  <p>Mixed <i>italic</i> and <b>bold <i>nested</i> text</b> tail.</p>
  <div id="div1">
    <p>Para in div one.</p>
    Loose text between paragraphs.
    <p>Para in div two.</p>
    <span>inline span after block</span> more loose
  </div>
  <div> <span>first child span after space</span> text</div>
  <div><p>A</p>
  <p>B</p>
  <p>C</p>
  after three paragraphs</div>
  <p>Line one<br/>
  line two<br />line three</p>
  <pre>
  pre line one
	tab line    with spaces
end</pre>
  <div class="pre">  css pre   kept
  second</div>
  <p>Soft&shy;hyphen and &amp; entity &#169; &#x2014; &nbsp;nbsp &unknownentity; done.</p>
  <p>Before<!-- a comment -->after comment.</p>
  <p><a id="anchor1"/>Text after self-closing anchor.</p>
  <div/>
  <p>After empty div.</p>
  <table id="t1"><tr><td>Cell one</td><td>Cell two</td></tr><tr><td id="c3">Cell three</td><td>Cell four</td></tr></table>
  <table><tbody><tr><td>Body cell</td></tr></tbody></table>
  <ul>
    <li>Item one</li>
    <li id="li2">Item <em>two</em> words</li>
  </ul>
  <p>Inline svg: <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20"/><text x="1" y="15">T</text></svg> after svg.</p>
  <p>Prefixed svg: <svg:svg xmlns:svg="http://www.w3.org/2000/svg" width="10" height="10"><svg:rect width="10" height="10"/></svg:svg> after prefixed.</p>
  <p><span class="blk">Block span</span> text after block span.</p>
  <p>Outer start <span>inner <b>deep</b></span><p>nested paragraph</p> outer tail</p>
  <blockquote><p>Quote para.</p></blockquote>
  <p id="long">{long}</p>
  <p>End of chapter one.</p>"#
    );
    e.chapter("c1.xhtml", &body, "<style>\n.x { color: black; }\n</style>\n");
    e.spine.push("missing_item".into());
    e.chapter("c2.xhtml", "<h2 id=\"c2top\">Second fragment</h2>\n<p>Second chapter text.</p>\n<p>More <a href=\"c1.xhtml#anchor1\">link</a> text.</p>", "");
    e.toc("Whitespace", "c1.xhtml", 0);
    e.toc("Top heading", "c1.xhtml#top", 1);
    e.toc("Anchor", "c1.xhtml#anchor1", 1);
    e.toc("Div one", "c1.xhtml#div1", 1);
    e.toc("Cell three", "c1.xhtml#c3", 1);
    e.toc("Item two", "c1.xhtml#li2", 1);
    e.toc("Long", "c1.xhtml#long", 1);
    e.toc("Second", "c2.xhtml", 0);
    e.toc("Second top", "c2.xhtml#c2top", 1);
    e
}

fn gray_png(w: u32, h: u32) -> Vec<u8> {
    let img = GrayImage::from_fn(w, h, |x, _| Luma([(x * 255 / w.max(1)) as u8]));
    encode(DynamicImage::ImageLuma8(img), ImageFormat::Png)
}

fn rgb_image(w: u32, h: u32, seed: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
        let cx = x as i32 - w as i32 / 2;
        let cy = y as i32 - h as i32 / 2;
        let ring = (((cx * cx + cy * cy) as f32).sqrt() as u32 / 12 + seed).is_multiple_of(2);
        let r = ((x * 255) / w.max(1)) as u8;
        let g = ((y * 255) / h.max(1)) as u8;
        if ring {
            Rgb([r, g, 200])
        } else {
            Rgb([255 - r, 60, g])
        }
    }))
}

fn encode(img: DynamicImage, fmt: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let img = if fmt == ImageFormat::Jpeg { DynamicImage::ImageRgb8(img.to_rgb8()) } else { img };
    img.write_to(&mut out, fmt).expect("encode image");
    out.into_inner()
}

const CSS_COMMON: &str = "body { margin: 0 } h1, h2 { text-align: center } .label { font-size: 70%; color: #555; }\n";

fn selectors() -> Epub {
    let mut e = Epub::new("css-selectors", "CSS selectors and cascade", "en");
    let css = r#"
p { margin: 0.3em 0; text-indent: 0 }
.bold { font-weight: bold }
#special { font-style: italic }
div.box p { text-align: right }
div.box > p.direct { text-align: center }
section p:first-child { text-decoration: underline }
p[data-x] { font-variant: small-caps; font-weight: bold }
p[lang|="de"] { font-style: italic }
a[href$=".pdf"] { font-weight: bold }
span[title~="key"] { text-decoration: underline }
p.low { font-weight: bold }
p.low { font-weight: normal }
p.spec.spec { font-style: italic }
p.spec { font-style: normal }
.imp { font-weight: bold !important }
#imp2 { font-weight: normal }
.inherit-parent { font-style: italic; text-align: center }
h2 + p { text-transform: uppercase }
h2 ~ p.sib { text-decoration: line-through }
p:not(.x) em { font-weight: bold }
"#;
    e.file("style.css", format!("{CSS_COMMON}{css}").into_bytes(), "text/css");
    e.chapter(
        "sel.xhtml",
        r#"<h1>Selectors</h1>
<p class="bold">Class selector: this line is bold.</p>
<p id="special">ID selector: this line is italic.</p>
<div class="box"><p>Descendant selector: right aligned.</p><p class="direct">Child selector with class: centered.</p><div><p class="direct">Nested deeper: right aligned (child selector does not match).</p></div></div>
<section><p>First child: underlined.</p><p>Second child: plain.</p></section>
<p data-x="1">Attribute presence: bold small caps.</p>
<p lang="de-AT">Attribute dash match lang|=de: italic.</p>
<p>Link with suffix match: <a href="file.pdf">bold link</a> and <a href="x.html">plain link</a>.</p>
<p>Word match: <span title="a key word">underlined span</span>, <span title="keys">plain span</span>.</p>
<h2>Cascade</h2>
<p>Adjacent sibling: this paragraph is upper case.</p>
<p class="low">Same specificity, later rule wins: not bold.</p>
<p class="spec">Higher specificity wins over order: italic.</p>
<p class="imp" id="imp2">!important beats an id rule: bold.</p>
<p class="sib">General sibling: struck through.</p>
<div class="inherit-parent"><p>Inherited from the parent div: italic and centered.</p></div>
<p>Negation: <em>bold emphasis</em> through :not(.x).</p>
<p style="font-weight: bold; text-align: right">Style attribute: bold and right aligned.</p>"#,
        "",
    );
    e.toc("Selectors", "sel.xhtml", 0);
    e
}

fn text_props() -> Epub {
    let mut e = Epub::new("css-text", "CSS text properties", "en");
    let css = r#"
p { margin: 0.4em 0 }
.serif { font-family: serif } .mono { font-family: "Courier New", monospace }
.big { font-size: 1.5em } .small { font-size: 0.75em } .px { font-size: 12px } .pt { font-size: 9pt } .kw { font-size: x-large }
.w7 { font-weight: 700 } .it { font-style: italic } .bi { font-weight: bold; font-style: italic }
.left { text-align: left } .right { text-align: right } .center { text-align: center } .justify { text-align: justify }
.indent { text-indent: 3em } .neg { text-indent: -2em; margin-left: 2em } .pct { text-indent: 20% }
.lh1 { line-height: 1 } .lh2 { line-height: 2 } .lhem { line-height: 2.5em }
.pre { white-space: pre } .prewrap { white-space: pre-wrap } .nowrap { white-space: nowrap } .preline { white-space: pre-line }
.upper { text-transform: uppercase } .cap { text-transform: capitalize } .ls { letter-spacing: 0.3em }
.u { text-decoration: underline } .s { text-decoration: line-through } .gray { color: #777 }
"#;
    e.file("style.css", format!("{CSS_COMMON}{css}").into_bytes(), "text/css");
    e.chapter(
        "fonts.xhtml",
        r#"<h1>Font properties</h1>
<p>Default text. <span class="big">Bigger 1.5em.</span> <span class="small">Smaller 0.75em.</span> <span class="px">Twelve px.</span> <span class="pt">Nine pt.</span> <span class="kw">x-large.</span></p>
<p><span class="w7">Weight 700.</span> <span class="it">Italic style.</span> <span class="bi">Bold italic.</span> <b>b element</b> <i>i element</i> <strong><em>strong em</em></strong></p>
<p class="mono">Monospace family: iiii MMMM 0123.</p>
<p>H<sub>2</sub>O and E = mc<sup>2</sup>; footnote mark<sup>12</sup> and x<sub>i</sub><sup>2</sup>.</p>
<p class="upper">uppercase transform.</p><p class="cap">capitalize every word here.</p><p class="ls">Letter spacing.</p>
<p><span class="u">Underlined</span>, <span class="s">struck through</span>, <span class="gray">gray text</span>.</p>"#,
        "",
    );
    let lorem = LOREM;
    e.chapter(
        "align.xhtml",
        &format!(
            r#"<h1>Alignment and indent</h1>
<p class="left">Left: {lorem}</p>
<p class="right">Right: {lorem}</p>
<p class="center">Center: {lorem}</p>
<p class="justify">Justify: {lorem}</p>
<p class="indent">Indent 3em: {lorem}</p>
<p class="neg">Hanging indent -2em: {lorem}</p>
<p class="pct">Indent 20%: {lorem}</p>"#
        ),
        "",
    );
    e.chapter(
        "space.xhtml",
        &format!(
            r#"<h1>Line height and white space</h1>
<p class="lh1">Line height 1: {lorem}</p>
<p class="lh2">Line height 2: {lorem}</p>
<p class="lhem">Line height 2.5em: Ut enim ad minim veniam, quis nostrud exercitation.</p>
<p class="pre">pre:    keeps   spaces
and newlines.</p>
<p class="prewrap">pre-wrap:    keeps   spaces and wraps long lines when they do not fit the width of the page at all.</p>
<p class="nowrap">nowrap: this line does not wrap even when long.</p>
<p class="preline">pre-line:    collapses   spaces
but keeps newlines.</p>"#
        ),
        "",
    );
    e.toc("Fonts", "fonts.xhtml", 0);
    e.toc("Alignment", "align.xhtml", 0);
    e.toc("White space", "space.xhtml", 0);
    e
}

fn box_props() -> Epub {
    let mut e = Epub::new("css-box", "CSS boxes and page breaks", "en");
    let css = r#"
p { margin: 0; text-indent: 0 }
.m { margin: 1em 0 } .m2 { margin-top: 2em; margin-bottom: 0.5em }
.pad { padding: 1em; border: 2px solid black } .dot { border: 1px dotted black; padding: 0.3em }
.thick { border-top: 6px solid; border-bottom: 2px solid; padding: 0.5em 0 }
.left-border { border-left: 4px solid black; padding-left: 0.8em; margin-left: 1em }
.centered { width: 50%; margin: 1em auto; border: 1px solid; padding: 0.5em; text-align: center }
.maxw { max-width: 300px; border: 1px solid; padding: 4px }
.hidden { display: none } .inline-p { display: inline } .item { display: list-item; margin-left: 2em; list-style-type: square }
.blk { display: block; margin: 0.5em 2em; font-weight: bold }
.break { page-break-before: always } .after { page-break-after: always } .keep { page-break-inside: avoid; border: 1px solid; padding: 0.5em }
.spacer { height: 3em }
"#;
    e.file("style.css", format!("{CSS_COMMON}{css}").into_bytes(), "text/css");
    e.chapter(
        "boxes.xhtml",
        &format!(
            r#"<h1>Margins, padding, borders</h1>
<p class="m">Margin 1em above and below (collapsed with neighbours).</p>
<p class="m2">Margin top 2em, bottom 0.5em.</p>
<p class="pad">Padding 1em inside a 2px border.</p>
<p class="dot">Dotted 1px border (drawn solid).</p>
<p class="thick">Thick top border, thin bottom border.</p>
<p class="left-border">Left border like a block quote. {LOREM}</p>
<div class="centered">Width 50%, margin auto: centered box.</div>
<div class="maxw">max-width 300px box with text that wraps inside the narrow box.</div>
<div class="spacer"></div>
<p>Display: <span class="blk">span shown as block</span> <span class="hidden">HIDDEN TEXT</span>none hides text.</p>
<p class="inline-p">Paragraph shown inline,</p> <p class="inline-p">followed by another inline paragraph.</p>
<p class="item">Paragraph shown as a list item.</p>
<hr/>
<p>Text after a horizontal rule.</p>"#
        ),
        "",
    );
    let many: String = (1..=14).map(|i| format!("<p class=\"m\">Filler paragraph {i}. {LOREM}</p>\n")).collect();
    e.chapter(
        "breaks.xhtml",
        &format!(
            r#"<h1>Page breaks</h1>
<p>The next paragraph starts a new page (page-break-before: always).</p>
<p class="break">Started on a new page.</p>
<p class="after">This paragraph ends its page (page-break-after: always).</p>
<p>First paragraph of a new page.</p>
{many}<div class="keep"><p>A box with page-break-inside: avoid stays on one page.</p><p>{LOREM}</p><p>{LOREM}</p></div>
<h2>A heading is kept with the next lines</h2>
<p>{LOREM}</p>"#
        ),
        "",
    );
    e.toc("Boxes", "boxes.xhtml", 0);
    e.toc("Breaks", "breaks.xhtml", 0);
    e
}

fn fonts() -> Epub {
    let mut e = Epub::new("fonts", "Embedded fonts", "en");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/src");
    e.file("fonts/NotoSans-Regular.ttf", std::fs::read(dir.join("NotoSans-Regular.ttf")).unwrap(), "font/ttf");
    e.file("fonts/NotoSans-Bold.ttf", std::fs::read(dir.join("NotoSans-Bold.ttf")).unwrap(), "font/ttf");
    e.file("fonts/broken.ttf", b"this is not a font".to_vec(), "font/ttf");
    let css = r#"
@font-face { font-family: "Book Sans"; src: url(fonts/NotoSans-Regular.ttf) format("truetype"); }
@font-face { font-family: "Book Sans"; font-weight: bold; src: url("fonts/NotoSans-Bold.ttf"); }
@font-face { font-family: Broken; src: url(fonts/broken.ttf); }
.sans { font-family: "Book Sans", sans-serif }
.broken { font-family: Broken, serif }
p { text-indent: 0; margin: 0.5em 0 }
"#;
    e.file("style.css", format!("{CSS_COMMON}{css}").into_bytes(), "text/css");
    e.chapter(
        "fonts.xhtml",
        &format!(
            r#"<h1>Embedded fonts</h1>
<p>Default serif face: {LOREM}</p>
<p class="sans">Embedded Book Sans (Noto Sans from the EPUB): {LOREM}</p>
<p class="sans"><b>Embedded bold face.</b> <i>Italic falls back to the regular face.</i></p>
<p class="broken">A font file that does not parse falls back to serif.</p>"#
        ),
        "",
    );
    e.toc("Fonts", "fonts.xhtml", 0);
    e
}

fn images() -> Epub {
    let mut e = Epub::new("images", "Images", "en");
    e.file("img/photo.jpg", encode(rgb_image(900, 600, 0), ImageFormat::Jpeg), "image/jpeg");
    e.file("img/rings.png", encode(rgb_image(400, 400, 1), ImageFormat::Png), "image/png");
    e.file("img/anim.gif", encode(rgb_image(200, 120, 0), ImageFormat::Gif), "image/gif");
    e.file("img/pic.webp", encode(rgb_image(300, 200, 1), ImageFormat::WebP), "image/webp");
    e.file("img/gradient.png", gray_png(1000, 80), "image/png");
    e.file("img/icon.png", encode(rgb_image(24, 24, 0), ImageFormat::Png), "image/png");
    e.file("img/tall.png", encode(rgb_image(300, 3000, 1), ImageFormat::Png), "image/png");
    let svg = r##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="400" height="200" viewBox="0 0 400 200">
<rect x="10" y="10" width="380" height="180" fill="none" stroke="black" stroke-width="6"/>
<circle cx="100" cy="100" r="60" fill="#888"/><polygon points="220,170 300,30 380,170" fill="black"/></svg>"##;
    e.file("img/shapes.svg", svg.as_bytes().to_vec(), "image/svg+xml");
    e.file("img/cover.jpg", encode(rgb_image(600, 800, 1), ImageFormat::Jpeg), "image/jpeg");
    let css = ".center { text-align: center } .half { width: 50% } figure { margin: 1em 0; text-align: center } figcaption { font-style: italic; font-size: 80% } p { text-indent: 0 }\n";
    e.file("style.css", format!("{CSS_COMMON}{css}").into_bytes(), "text/css");
    e.chapter(
        "cover.xhtml",
        r#"<div><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.1" width="100%" height="100%" viewBox="0 0 600 800" preserveAspectRatio="xMidYMid meet"><image width="600" height="800" xlink:href="img/cover.jpg"/></svg></div>"#,
        "",
    );
    e.chapter(
        "formats.xhtml",
        r#"<h1>Image formats</h1>
<figure><img src="img/photo.jpg" alt="JPEG"/><figcaption>JPEG 900 x 600, scaled to the page width</figcaption></figure>
<figure><img src="img/rings.png" alt="PNG"/><figcaption>PNG 400 x 400</figcaption></figure>
<figure><img src="img/anim.gif" alt="GIF"/> <img src="img/pic.webp" alt="WebP"/><figcaption>GIF and WebP side by side</figcaption></figure>
<figure><img src="img/shapes.svg" alt="SVG"/><figcaption>SVG file rendered with resvg</figcaption></figure>
<p>Inline SVG element: <svg xmlns="http://www.w3.org/2000/svg" width="60" height="30" viewBox="0 0 60 30"><rect width="60" height="30" fill="black"/><circle cx="45" cy="15" r="10" fill="white"/></svg> inside text.</p>
<p>An icon <img src="img/icon.png" alt="icon"/> inline with the text, and the gray gradient dithered to 16 levels:</p>
<p class="center"><img src="img/gradient.png" alt="gradient"/></p>
<p class="center"><img class="half" src="img/rings.png" alt="half"/></p>
<p>CSS width 50% above. A very tall image follows; it is scaled to fit one page:</p>
<p class="center"><img src="img/tall.png" alt="tall"/></p>
<p>A missing image: <img src="img/missing.png" alt="missing"/> (skipped).</p>"#,
        "",
    );
    e.toc("Cover", "cover.xhtml", 0);
    e.toc("Formats", "formats.xhtml", 0);
    e
}

fn tables_lists() -> Epub {
    let mut e = Epub::new("tables-lists", "Tables, lists, quotes", "en");
    let css = "table.grid { border-collapse: collapse; width: 100% } table.grid td, table.grid th { border: 1px solid black; padding: 4px } p { text-indent: 0 } pre { border: 1px solid; padding: 0.5em }\n";
    e.file("style.css", format!("{CSS_COMMON}{css}").into_bytes(), "text/css");
    let rows: String = (1..=40).map(|i| format!("<tr><td>{i}</td><td>Row {i} name</td><td>{} units</td></tr>\n", i * 7)).collect();
    e.chapter(
        "tables.xhtml",
        &format!(
            r#"<h1>Tables</h1>
<table class="grid"><caption>Table 1: a simple grid</caption>
<thead><tr><th>Name</th><th>Born</th><th>Notes</th></tr></thead>
<tbody><tr><td>Romeo</td><td>Verona</td><td>A Montague who falls in love.</td></tr>
<tr><td>Juliet</td><td>Verona</td><td>A Capulet, thirteen years old.</td></tr>
<tr><td colspan="2">Spanning two columns</td><td>Last cell</td></tr></tbody></table>
<p>A table with the border attribute and no tbody:</p>
<table border="1"><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d with more text in it</td></tr></table>
<p>Nested table:</p>
<table class="grid"><tr><td>Outer cell</td><td><table class="grid"><tr><td>in 1</td><td>in 2</td></tr></table></td></tr></table>
<p>A long table that continues on the next page:</p>
<table class="grid"><tr><th>#</th><th>Name</th><th>Value</th></tr>
{rows}</table>"#
        ),
        "",
    );
    e.chapter(
        "lists.xhtml",
        r#"<h1>Lists</h1>
<ul><li>Disc item one</li><li>Disc item two<ul><li>Circle nested</li><li>Circle nested two<ul><li>Square third level</li></ul></li></ul></li></ul>
<ol><li>Decimal one</li><li>Decimal two</li><li value="7">Value attribute seven</li><li>Eight</li></ol>
<ol start="4" style="list-style-type: upper-roman"><li>Roman four</li><li>Roman five</li></ol>
<ol style="list-style-type: lower-alpha"><li>alpha a</li><li>alpha b</li></ol>
<ul style="list-style: none"><li>No marker</li></ul>
<ul style="list-style-position: inside"><li>Inside marker with a longer text that wraps to show where the second line starts.</li></ul>
<dl><dt>Term</dt><dd>Definition of the term.</dd><dt>Second term</dt><dd>Second definition.</dd></dl>
<h2>Quotes and code</h2>
<blockquote><p>A block quote is indented on both sides. Two households, both alike in dignity, in fair Verona, where we lay our scene.</p></blockquote>
<pre>fn main() {
    println!("pre keeps    spacing");
}</pre>"#,
        "",
    );
    e.toc("Tables", "tables.xhtml", 0);
    e.toc("Lists", "lists.xhtml", 0);
    e
}

fn notes() -> Epub {
    let mut e = Epub::new("notes", "Footnotes and endnotes", "en");
    e.file("style.css", format!("{CSS_COMMON}aside {{ font-size: 80%; border-top: 1px solid; margin-top: 1em }}\n").into_bytes(), "text/css");
    e.chapter(
        "text.xhtml",
        &format!(
            r##"<h1>Chapter with notes</h1>
<p>An EPUB 3 footnote reference<a epub:type="noteref" href="#fn1" id="r1">1</a> in the text, a classic numbered link<sup><a href="endnotes.xhtml#en2" id="r2">2</a></sup>, a bracketed one <a href="endnotes.xhtml#en3" id="r3">[3]</a>, and an ordinary internal link to <a href="endnotes.xhtml">the notes chapter</a>.</p>
<p>An external link: <a href="https://standardebooks.org/">Standard Ebooks</a>. {LOREM}</p>
<aside epub:type="footnote" id="fn1"><p>1. The footnote text, shown in a popup.</p></aside>"##
        ),
        "",
    );
    e.chapter(
        "endnotes.xhtml",
        r#"<section epub:type="endnotes" role="doc-endnotes"><h1>Notes</h1>
<ol><li id="en2" epub:type="endnote"><p>The second note, an endnote. <a href="text.xhtml#r2">Back</a></p></li>
<li id="en3"><p>The third note, found by its short link text. <a href="text.xhtml#r3">Back</a></p></li></ol></section>"#,
        "",
    );
    e.toc("Text", "text.xhtml", 0);
    e.toc("Notes", "endnotes.xhtml", 0);
    e
}

fn lang() -> Epub {
    let mut e = Epub::new("lang", "Hyphenation by language", "en");
    e.file("style.css", format!("{CSS_COMMON}p {{ text-align: justify; hyphens: auto; margin: 0.5em 0 }} .narrow {{ margin: 0 30% 0 0 }}\n").into_bytes(), "text/css");
    let en = "Characteristically, international organizations unquestionably acknowledge incomprehensibilities, notwithstanding counterrevolutionary misunderstandings and representational responsibilities.";
    let de = "Die Donaudampfschifffahrtsgesellschaft beschäftigte Rechtsschutzversicherungsgesellschaften und Kraftfahrzeughaftpflichtversicherungen.";
    let da = "Arbejdsløshedsforsikringsordningen og sundhedsministeriets kvalitetssikringsprogrammer blev gennemgået.";
    let fr = "Anticonstitutionnellement, les intergouvernementalisations internationales furent extraordinairement compliquées.";
    e.chapter(
        "lang.xhtml",
        &format!(
            r#"<h1>Hyphenation</h1>
<div class="narrow">
<p>English (from the OPF language): {en}</p>
<p lang="de">Deutsch (lang="de"): {de}</p>
<p lang="da">Dansk (lang="da"): {da}</p>
<p xml:lang="fr">Français (xml:lang="fr"): {fr}</p>
<p style="hyphens: none">No hyphens (hyphens: none): {en}</p>
<p>Soft hyphens are always honoured: super&#173;cali&#173;fragi&#173;listic&#173;expi&#173;ali&#173;docious super&#173;cali&#173;fragi&#173;listic&#173;expi&#173;ali&#173;docious.</p>
</div>"#
        ),
        "",
    );
    e.toc("Hyphenation", "lang.xhtml", 0);
    e
}

fn pagelist() -> Epub {
    let mut e = Epub::new("pagelist", "Printed page labels", "en");
    e.file("style.css", CSS_COMMON.as_bytes().to_vec(), "text/css");
    for (ci, (name, first)) in [("front.xhtml", 0usize), ("body.xhtml", 3)].iter().enumerate() {
        let mut body = String::from(if ci == 0 { "<h1>Front matter</h1>\n" } else { "<h1>Chapter One</h1>\n" });
        for k in 0..3 {
            let label = if ci == 0 { ["i", "ii", "iii"][k].to_string() } else { (k + 1).to_string() };
            let id = format!("page{}", first + k);
            body.push_str(&format!("<p><span epub:type=\"pagebreak\" id=\"{id}\" title=\"{label}\"/>Printed page {label} starts here. {LOREM} {LOREM}</p>\n"));
            e.page_list.push((label, format!("{name}#{id}")));
        }
        e.chapter(name, &body, "");
        e.toc(if ci == 0 { "Front" } else { "One" }, name, 0);
    }
    e
}

fn malformed() -> Epub {
    let mut e = Epub::new("malformed", "Broken markup", "en");
    e.file("style.css", CSS_COMMON.as_bytes().to_vec(), "text/css");
    // Not well-formed XML: parsed with html5ever.
    e.raw_chapter(
        "html5.xhtml",
        "<!DOCTYPE html><html><head><title>t</title><link rel=stylesheet href=style.css></head><body>\n<h1>Tag soup</h1>\n<p>Unclosed paragraph with <b>bold <i>both</b> italic</i> text<br>after a void br\n<p>Second paragraph &amp; a stray & ampersand, &copy; &mdash; entities.\n<div>Stray end tags </span></div></div>\n<ul><li>one<li>two</ul>\n<table><tr><td>cell<td>cell 2</table>\n<p><a id=x/>Self-closing anchor in HTML syntax.</p>\n</body></html>",
    );
    // No body element at all.
    e.raw_chapter("nobody.xhtml", "<html><p>Text without a body element, and an unclosed <em>emphasis.</p>");
    // Well-formed but odd: CDATA, comments, processing instructions, unknown elements.
    e.raw_chapter(
        "odd.xhtml",
        "<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><body><p>CDATA: <![CDATA[<not a tag>]]> and a <!-- comment --> and <?pi x?> and <custom-element>custom</custom-element>.</p></body></html>",
    );
    e.toc("Soup", "html5.xhtml", 0);
    e
}

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"));
    std::fs::create_dir_all(&dir)?;
    for e in [ws(), selectors(), text_props(), box_props(), fonts(), images(), tables_lists(), notes(), lang(), pagelist(), malformed()] {
        let p = write_epub(&e, &dir)?;
        println!("{} ({} bytes)", p.display(), std::fs::metadata(&p)?.len());
    }
    Ok(())
}
