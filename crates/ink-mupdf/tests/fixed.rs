//! PDF and CBZ through MuPDF, on generated fixtures (`ink_mupdf::testgen`).
//!
//! Sections follow the phase 6 acceptance checks: rendering (zoom, rotation, fit, pan,
//! crop, columns, contrast/gamma), text (search, selection, highlights), navigation
//! (TOC, links, metadata, thumbnail), CBZ (order, spreads) and the page cache.

use ink_device::{Canvas, Point};
use ink_doc::{DocError, DocKind, Document, LayoutSettings, LinkTarget, PagePosition, Position, Provider};
use ink_mupdf::{testgen, Fit, FixedOptions, MupdfDocument, MupdfProvider, Plan, RenderSpec, Spreads};
use std::path::{Path, PathBuf};

const W: u32 = 1272;
const H: u32 = 1696;

/// Writes `bytes` to a per-process temp file (written to a temporary name and renamed,
/// so parallel tests never read a half-written file).
fn write(name: &str, bytes: &[u8]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("ink-mupdf-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let tmp = dir.join(format!(".{name}.{}", N.fetch_add(1, Ordering::SeqCst)));
    std::fs::write(&tmp, bytes).unwrap();
    std::fs::rename(&tmp, &p).unwrap();
    p
}

fn pdf_path() -> PathBuf {
    static P: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    P.get_or_init(|| write("fixture.pdf", &testgen::fixture_pdf())).clone()
}

fn cbz_path() -> PathBuf {
    static P: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    P.get_or_init(|| write("comic.cbz", &testgen::fixture_cbz(12))).clone()
}

fn open(path: &Path, opts: &str, w: u32, h: u32) -> MupdfDocument {
    let mut d = MupdfDocument::open(path, FixedOptions::parse(opts).unwrap()).unwrap();
    d.set_layout(&LayoutSettings::for_screen(w, h)).unwrap();
    d
}

fn screen(d: &mut MupdfDocument, i: usize) -> Canvas {
    let mut c = Canvas::new(d.layout().width as usize, d.layout().height as usize);
    d.render_page(i, &mut c).unwrap();
    c
}

fn px(c: &Canvas, x: f32, y: f32) -> u8 {
    c.get(x.round() as i32, y.round() as i32).expect("inside the canvas")
}

/// Length of the dark run starting at or after `x` on row `y`.
fn dark_run(c: &Canvas, x: i32, y: i32) -> i32 {
    let mut x = x;
    while c.get(x, y).is_some_and(|v| v >= 100) {
        x += 1;
    }
    let start = x;
    while c.get(x, y).is_some_and(|v| v < 100) {
        x += 1;
    }
    x - start
}

// --- 2. PDF rendering ---------------------------------------------------------------

#[test]
fn fit_page_renders_gray_with_marker_where_expected() {
    let path = pdf_path();
    let mut d = open(&path, "", W, H);
    assert_eq!(d.kind(), DocKind::Fixed);
    assert_eq!(d.page_count().pages, 10);
    assert_eq!(d.plan(), Plan::Tiles { cols: 1, nx: 1, ny: 1 });
    let c = screen(&mut d, 0);
    // Letter page, fit page on 1272 x 1696: scale = 1272 / 612, centred vertically.
    let s = W as f32 / 612.0;
    let top = (H as f32 - 792.0 * s) / 2.0;
    assert_eq!(px(&c, 200.0 * s, top + 110.0 * s), 0, "black marker");
    assert_eq!(px(&c, 140.0 * s, top + 110.0 * s), 255, "left of the marker");
    assert_eq!(px(&c, 200.0 * s, top + 90.0 * s), 255, "above the marker");
    let gray = px(&c, 300.0 * s, top + 110.0 * s);
    assert!((120..=135).contains(&gray), "50 % gray rectangle is {gray}");
    // The piece transform agrees with the pixels.
    let p = d.pieces(0)[0];
    let (mx, my) = p.ctm.apply(200.0, 110.0);
    assert_eq!(px(&c, mx, my), 0);
}

#[test]
fn render_at_zoom_and_rotation() {
    let path = pdf_path();
    let d = open(&path, "", W, H);
    // Marker centre (200, 110) on a 612 x 792 page, zoom 1.
    let expect = [(0u16, (200.0, 110.0)), (90, (792.0 - 110.0, 200.0)), (180, (612.0 - 200.0, 792.0 - 110.0)), (270, (110.0, 612.0 - 200.0))];
    for (rot, (x, y)) in expect {
        let mut c = Canvas::new(800, 800);
        d.render_page_at(0, &RenderSpec { zoom: 1.0, rotate: rot, x: 0.0, y: 0.0, crop: false }, &mut c).unwrap();
        assert_eq!(px(&c, x, y), 0, "rotation {rot}: marker centre");
        // The marker is 100 x 20: long side horizontal at 0/180, vertical at 90/270.
        let horizontal = dark_run(&c, (x - 60.0) as i32, y as i32);
        assert!(if rot % 180 == 0 { horizontal >= 98 } else { horizontal <= 22 }, "rotation {rot}: run {horizontal}");
    }
    // Zoom 2.5: the marker is 250 px wide.
    let mut c = Canvas::new(1000, 600);
    d.render_page_at(0, &RenderSpec { zoom: 2.5, rotate: 0, x: 0.0, y: 0.0, crop: false }, &mut c).unwrap();
    let run = dark_run(&c, 300, (110.0 * 2.5) as i32);
    assert!((248..=252).contains(&run), "zoomed marker width {run}");
}

#[test]
fn free_pan_shifts_pixels_exactly() {
    let path = pdf_path();
    let d = open(&path, "", W, H);
    let mut a = Canvas::new(500, 500);
    let mut b = Canvas::new(500, 500);
    let spec = RenderSpec { zoom: 2.0, rotate: 0, x: 250.0, y: 150.0, crop: false };
    d.render_page_at(0, &spec, &mut a).unwrap();
    d.render_page_at(0, &RenderSpec { x: 350.0, y: 200.0, ..spec }, &mut b).unwrap();
    let mut worst = 0u8;
    for y in 0..450 {
        for x in 0..400 {
            let va = a.get(x + 100, y + 50).unwrap();
            let vb = b.get(x, y).unwrap();
            worst = worst.max(va.abs_diff(vb));
        }
    }
    assert!(worst <= 2, "pan by (100, 50) differs by up to {worst}");
    assert!(a.pixels.contains(&0), "something drawn");
}

#[test]
fn fit_width_tiles_and_positions_round_trip() {
    let path = pdf_path();
    // Landscape screen, fit width: a Letter page is 1.7 screens tall -> 2 tiles.
    let mut d = open(&path, "fit=width", 1696, 1272);
    assert_eq!(d.plan(), Plan::Tiles { cols: 1, nx: 1, ny: 2 });
    assert_eq!(d.page_count().pages, 20);
    let top = screen(&mut d, 0);
    let s = 1696.0 / 612.0;
    assert_eq!(px(&top, 200.0 * s, 110.0 * s), 0, "marker on the first tile");
    let p1 = d.position_of_page(1).unwrap();
    match &p1 {
        Position::Page(pp) => {
            assert_eq!(pp.page, 0);
            assert!(pp.quads[0].y0 > 300.0 && pp.quads[0].y1 > 780.0, "{p1}");
        }
        other => panic!("{other:?}"),
    }
    for v in 0..20 {
        let pos = d.position_of_page(v).unwrap();
        assert_eq!(d.page_of(&pos).unwrap(), v, "{pos}");
        let parsed: Position = pos.to_string().parse().unwrap();
        assert_eq!(d.page_of(&parsed).unwrap(), v);
    }
}

#[test]
fn zoom_tiles_go_row_by_row_and_survive_option_changes() {
    let path = pdf_path();
    let mut d = open(&path, "zoom=2", W, H);
    assert_eq!(d.plan(), Plan::Tiles { cols: 1, nx: 2, ny: 2 });
    assert_eq!(d.page_count().pages, 40);
    let quad = |d: &MupdfDocument, v: usize| match d.position_of_page(v).unwrap() {
        Position::Page(p) => p.quads[0],
        other => panic!("{other:?}"),
    };
    let (q0, q1, q2, q3) = (quad(&d, 0), quad(&d, 1), quad(&d, 2), quad(&d, 3));
    assert!(q0.x0 < 1.0 && q0.y0 < 1.0, "top-left first");
    assert!(q1.x0 > 250.0 && q1.y0 < 1.0, "then top-right");
    assert!(q2.x0 < 1.0 && q2.y0 > 300.0, "then bottom-left");
    assert!(q3.x0 > 250.0 && q3.y0 > 300.0, "then bottom-right");
    for v in 0..40 {
        let pos = d.position_of_page(v).unwrap();
        assert_eq!(d.page_of(&pos).unwrap(), v, "{pos}");
    }
    // Screen 9 = page 2, bottom-right; back to fit page keeps page 2.
    d.set_current_page(9);
    d.set_options(FixedOptions::default());
    assert_eq!(d.page_count().pages, 10);
    assert_eq!(d.current_page(), 2);
    // And a stored whole-page position opens the first tile of that page.
    d.set_options(FixedOptions { zoom: 2.0, ..Default::default() });
    assert_eq!(d.page_of(&"page:3".parse().unwrap()).unwrap(), 12);
}

#[test]
fn crop_margins_finds_the_text_block_and_enlarges_it() {
    let path = pdf_path();
    let mut d = open(&path, "crop=1", W, H);
    let b = d.content_box(0).unwrap();
    assert!((130.0..=150.0).contains(&b.x0), "{b:?}");
    assert!((80.0..=100.0).contains(&b.y0), "{b:?}");
    assert!((400.0..=440.0).contains(&b.x1), "{b:?}");
    assert!((480.0..=510.0).contains(&b.y1), "{b:?}");
    // Free pan from the crop box while the document itself does not crop.
    let plain = open(&path, "", W, H);
    assert_eq!(plain.content_box(0).unwrap().x0, 0.0);
    let mut c = Canvas::new(300, 100);
    plain.render_page_at(0, &RenderSpec { zoom: 1.0, rotate: 0, x: 0.0, y: 0.0, crop: true }, &mut c).unwrap();
    let (ox, oy) = (150.0 - b.x0, 110.0 - b.y0);
    assert_eq!(px(&c, ox + 50.0, oy), 0, "marker relative to the crop box");
    let c = screen(&mut d, 0);
    let p = d.pieces(0)[0];
    let (mx, my) = p.ctm.apply(200.0, 110.0);
    assert_eq!(px(&c, mx, my), 0);
    let (lx, _) = p.ctm.apply(150.0, 110.0);
    let run = dark_run(&c, lx as i32 - 5, my as i32);
    let plain = 100.0 * W as f32 / 612.0;
    assert!(run as f32 > plain * 1.3, "marker {run} px with crop vs {plain} without");
}

#[test]
fn columns_mode_reads_left_column_then_right() {
    let path = pdf_path();
    let d = open(&path, "crop=1,columns=2", W, H);
    assert_eq!(d.plan(), Plan::Tiles { cols: 2, nx: 1, ny: 1 });
    let cols = d.columns(1).unwrap();
    assert_eq!(cols.len(), 2);
    assert!(cols[0].x1 < 300.0 && cols[1].x0 > 290.0, "{cols:?}");
    let text = d.page_text(1).unwrap();
    let first = d.first_screen_of(1);
    let left = d.pieces(first)[0].region;
    let right = d.pieces(first + 1)[0].region;
    let lt = text.text_in(&[left]);
    let rt = text.text_in(&[right]);
    // MuPDF's reading order is column by column for this page.
    assert!(text.plain().contains("Left column line 40 Right column line 1"), "{}", text.plain());
    assert!(lt.contains("Left column line 40") && !lt.contains("Right"), "{lt}");
    assert!(rt.contains("Right column line 1") && !rt.contains("Left"), "{rt}");
    // Fit width on the columns: more screens, each column is as wide as the screen.
    let mut d = open(&path, "crop=1,columns=2,fit=width", W, H);
    let first = d.first_screen_of(1);
    let p = d.pieces(first)[0];
    let r = p.ctm.map_rect(&p.region);
    assert!((r.w() - W as f32).abs() < 2.0, "column width on screen {}", r.w());
    let _ = screen(&mut d, first);
}

#[test]
fn contrast_and_gamma_change_gray_levels() {
    let path = pdf_path();
    let s = W as f32 / 612.0;
    let top = (H as f32 - 792.0 * s) / 2.0;
    let levels = |opts: &str| {
        let mut d = open(&path, opts, W, H);
        let c = screen(&mut d, 0);
        (px(&c, 200.0 * s, top + 110.0 * s), px(&c, 300.0 * s, top + 110.0 * s), px(&c, 380.0 * s, top + 110.0 * s), px(&c, 20.0, 20.0))
    };
    let (black, mid, light, white) = levels("");
    assert_eq!((black, white), (0, 255));
    assert!((120..=135).contains(&mid) && (198..=210).contains(&light), "{mid} {light}");
    let (b2, m2, l2, w2) = levels("gamma=2");
    assert_eq!((b2, w2), (0, 255));
    assert!((58..=72).contains(&m2) && (155..=172).contains(&l2), "gamma 2: {m2} {l2}");
    let (b3, m3, l3, w3) = levels("contrast=2");
    assert_eq!((b3, w3), (0, 255));
    assert!((118..=138).contains(&m3) && l3 == 255, "contrast 2: {m3} {l3}");
}

/// Colour management is off (`fz_disable_icc` in the shim): gray content keeps its exact
/// levels whether it is drawn in DeviceGray or as neutral RGB, and colours become gray
/// by the plain luma weights (0.30 R + 0.59 G + 0.11 B).
#[test]
fn gray_levels_are_exact_and_colours_use_luma_weights() {
    let mut page = testgen::PdfPage::letter();
    page.rect(100.0, 100.0, 100.0, 100.0, 0.5);
    page.rect_rgb(300.0, 100.0, 100.0, 100.0, [0.5, 0.5, 0.5]);
    page.rect_rgb(100.0, 300.0, 100.0, 100.0, [1.0, 0.0, 0.0]);
    page.rect_rgb(300.0, 300.0, 100.0, 100.0, [0.0, 1.0, 0.0]);
    let path = write("colours.pdf", &testgen::write_pdf(&[page], &[], &[]));
    let mut d = open(&path, "", W, H);
    let c = screen(&mut d, 0);
    let s = W as f32 / 612.0;
    let top = (H as f32 - 792.0 * s) / 2.0;
    let at = |x: f32, y: f32| px(&c, x * s, top + y * s);
    let (gray, neutral, red, green) = (at(150.0, 150.0), at(350.0, 150.0), at(150.0, 350.0), at(350.0, 350.0));
    assert!((127..=128).contains(&gray), "DeviceGray 0.5: {gray}");
    assert_eq!(neutral, gray, "neutral RGB draws as the same gray");
    assert!((76..=77).contains(&red), "red: {red}");
    assert!((149..=151).contains(&green), "green: {green}");
}

// --- 3. Text, search, selection, highlights ------------------------------------------

#[test]
fn page_text_has_character_boxes() {
    let path = pdf_path();
    let d = open(&path, "", W, H);
    let t = d.page_text(2).unwrap();
    assert!(t.plain().contains("Looking for a needle in a haystack takes patience"), "{}", t.plain());
    assert!(t.chars.len() > 100);
    for ch in &t.chars {
        assert!(ch.rect.x0 >= 0.0 && ch.rect.x1 <= 612.0 && ch.rect.y0 >= 0.0 && ch.rect.y1 <= 792.0, "{ch:?}");
    }
    let n = t.chars.iter().position(|c| c.c == 'n').unwrap();
    assert!(t.chars[n].rect.y0 > 110.0 && t.chars[n].rect.y1 < 140.0, "{:?}", t.chars[n]);
}

#[test]
fn search_returns_page_and_quads() {
    let path = pdf_path();
    let mut d = open(&path, "", W, H);
    let hits = d.search("needle", 10).unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    for h in &hits {
        assert_eq!(&h.excerpt[h.matched.clone()].to_lowercase(), "needle");
        assert_eq!(h.page, Some(2));
        match &h.position {
            Position::Page(p) => {
                assert_eq!(p.page, 2);
                assert_eq!(p.quads.len(), 1);
                assert!(p.quads[0].x1 - p.quads[0].x0 > 25.0);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(d.search("NEEDLE", 1).unwrap().len(), 1);
    let hay = d.search("haystack", 10).unwrap();
    assert_eq!(hay.len(), 1);
    match &hay[0].position {
        Position::Page(p) => assert_eq!(p.quads.len(), 2, "hyphenated over two lines"),
        other => panic!("{other:?}"),
    }
    assert!(d.search("zebra", 10).unwrap().is_empty());
    assert!(d.search("  ", 10).unwrap().is_empty());
    // In tile mode the hit points at the tile that shows it.
    let mut z = open(&path, "zoom=2", W, H);
    let h = z.search("Needle and thread", 1).unwrap();
    // y 340..352 is above the bottom tiles' top edge (384): the top-left tile.
    assert_eq!(h[0].page, Some(2 * 4), "top-left tile of page 2");
    let h = z.search("Page 6", 1).unwrap();
    assert_eq!(h[0].page, Some(5 * 4));
}

#[test]
fn selection_gives_text_quads_and_highlights_round_trip() {
    let path = pdf_path();
    let mut d = open(&path, "", W, H);
    let hit = d.search("needle", 1).unwrap().remove(0);
    let Position::Page(pp) = hit.position else { panic!() };
    let q = pp.quads[0];
    let ctm = d.pieces(2)[0].ctm;
    let (ax, ay) = ctm.apply(q.x0 + 2.0, (q.y0 + q.y1) / 2.0);
    let (bx, by) = ctm.apply(q.x1 - 2.0, (q.y0 + q.y1) / 2.0);
    let sel = d.select(2, Point::new(ax as i32, ay as i32), Point::new(bx as i32, by as i32)).unwrap().unwrap();
    assert_eq!(sel.text, "needle");
    assert_eq!(sel.page, 2);
    assert_eq!(d.selection().map(|s| s.text.as_str()), Some("needle"));
    // Stored as page + quads; the string form parses back.
    let stored = sel.start.to_string();
    assert!(stored.starts_with("page:2;"), "{stored}");
    let Position::Page(back) = stored.parse::<Position>().unwrap() else { panic!() };
    let rects = d.highlight_rects(2, &back);
    assert_eq!(rects.len(), 1);
    let (a, b) = (rects[0], sel.rects[0]);
    assert!((a.x - b.x).abs() <= 1 && (a.w as i32 - b.w as i32).abs() <= 2, "{a:?} vs {b:?}");
    assert_eq!(d.text_at(&back).unwrap(), "needle");
    // Selection across lines.
    let (cx, cy) = ctm.apply(80.0, 126.0);
    let (ex, ey) = ctm.apply(110.0, 142.0);
    let two = d.select(2, Point::new(cx as i32, cy as i32), Point::new(ex as i32, ey as i32)).unwrap().unwrap();
    assert_eq!(two.rects.len(), 2, "{two:?}");
    assert!(two.text.contains("haystack"), "{}", two.text);
    d.clear_selection();
    assert!(d.selection().is_none());
    // The same highlight at zoom 2 lands on the tile that shows it.
    d.set_options(FixedOptions { zoom: 2.0, ..Default::default() });
    let v = d.page_of(&Position::Page(back.clone())).unwrap();
    assert!((8..12).contains(&v));
    let r = d.highlight_rects(v, &back);
    assert_eq!(r.len(), 1);
    assert!(r[0].w as f32 > b.w as f32 * 1.8, "twice as wide at zoom 2");
    assert!(d.highlight_rects(v + 1, &PagePosition { page: 7, quads: back.quads.clone() }).is_empty());
}

#[test]
fn read_around_returns_text_near_a_position() {
    let path = pdf_path();
    let mut d = open(&path, "", W, H);
    let hit = d.search("thread", 1).unwrap().remove(0);
    let around = d.read_around(&hit.position, 60).unwrap();
    assert!(around.contains("thread"), "{around}");
    assert!(around.chars().count() <= 60);
    let start = d.read_around(&"page:4".parse().unwrap(), 400).unwrap();
    assert!(start.starts_with("Notes 1. A footnote"), "{start}");
    assert!(start.contains("Page 6"), "continues on the next page: {start}");
    assert!(matches!(d.read_around(&"page:99".parse().unwrap(), 10), Err(DocError::PageOutOfRange(99))));
    assert!(matches!(d.read_around(&"flow:1.2.3".parse().unwrap(), 10), Err(DocError::BadPosition(_))));
}

// --- 4. TOC, links, metadata, cover thumbnail ------------------------------------------

#[test]
fn toc_links_metadata_thumbnail() {
    let path = pdf_path();
    let mut d = open(&path, "", W, H);
    let toc = d.toc();
    let got: Vec<(&str, u8, Option<usize>)> = toc.iter().map(|e| (e.title.as_str(), e.level, e.page)).collect();
    assert_eq!(
        got,
        vec![
            ("Chapter 1", 0, Some(0)),
            ("Margins", 1, Some(0)),
            ("Two columns", 1, Some(1)),
            ("Chapter 2", 0, Some(2)),
            ("Appendix", 0, Some(3)),
            ("Notes", 1, Some(4))
        ]
    );
    for e in &toc {
        assert_eq!(d.page_of(&e.position).unwrap(), e.page.unwrap());
    }
    // Zoom 2 (2 x 2 tiles): "Margins" (y 140) is on the first tile of page 0, "Chapter 2"
    // (y 72) on the first tile of page 2; every entry maps back to its own screen.
    let z = open(&path, "zoom=2", W, H);
    let zt = z.toc();
    assert_eq!(zt[1].page, Some(0));
    assert_eq!(zt[3].page, Some(8));
    for e in &zt {
        assert_eq!(z.page_of(&e.position).unwrap(), e.page.unwrap(), "{}", e.title);
    }

    let links = d.links(&"page:0".parse().unwrap()).unwrap();
    assert_eq!(links.len(), 3, "{links:?}");
    let internal = links.iter().find(|l| l.text == "see chapter 2").expect("internal link");
    assert!(matches!(&internal.target, LinkTarget::Internal(Position::Page(p)) if p.page == 2), "{internal:?}");
    assert!(!internal.footnote);
    let rect = internal.rect.expect("on screen");
    let s = W as f32 / 612.0;
    assert!((rect.x as f32 - 150.0 * s).abs() < 4.0, "{rect:?}");
    let ext = links.iter().find(|l| matches!(&l.target, LinkTarget::External(u) if u == "https://example.org/")).expect("URL");
    assert_eq!(ext.text, "example.org");
    let note = links.iter().find(|l| l.text == "1").expect("footnote mark");
    assert!(note.footnote);
    assert!(matches!(&note.target, LinkTarget::Internal(Position::Page(p)) if p.page == 4));
    assert!(d.links(&"page:5".parse().unwrap()).unwrap().is_empty());

    let m = d.metadata();
    assert_eq!(m.title, "rinky fixture");
    assert_eq!(m.authors, vec!["Ada Lovelace".to_string(), "Charles Babbage".to_string()]);
    assert_eq!(m.description.as_deref(), Some("Generated test PDF"));

    let t = d.thumbnail(200, 300).unwrap();
    assert_eq!(t.width, 200);
    assert!((255..=262).contains(&t.height), "{}", t.height);
    assert!(t.pixels.iter().any(|&v| v < 30), "marker visible in the thumbnail");
    let c = ink_mupdf::cover_thumbnail(&path, 120, 120).unwrap();
    assert!(c.width <= 120 && c.height == 120);
}

#[test]
fn provider_opens_pdf_and_cbz_as_documents() {
    let pdf = pdf_path();
    let mut d = MupdfProvider.open(&pdf).unwrap();
    d.set_layout(&LayoutSettings::for_screen(W, H)).unwrap();
    assert_eq!(d.page_count().pages, 10);
    let mut c = Canvas::new(W as usize, H as usize);
    d.render_page(9, &mut c).unwrap();
    assert!(matches!(d.render_page(10, &mut c), Err(DocError::PageOutOfRange(10))));
    assert_eq!(d.current_position().unwrap().to_string(), "page:0");
    d.set_current_page(3);
    assert_eq!(d.current_position().unwrap().to_string(), "page:3");
    let cbz = cbz_path();
    let d = MupdfProvider.open(&cbz).unwrap();
    assert_eq!(d.metadata().title, "comic");
}

// --- 5. CBZ ----------------------------------------------------------------------------

/// Reads the page number from a generated comic page: the top bar is 40 px per page.
fn comic_number(d: &MupdfDocument, page: usize) -> usize {
    let b = d.page_bounds(page).unwrap();
    let zoom = testgen::COMIC_W as f32 / b.w();
    let mut c = Canvas::new(testgen::COMIC_W, testgen::COMIC_H);
    d.render_page_at(page, &RenderSpec { zoom, rotate: 0, x: 0.0, y: 0.0, crop: false }, &mut c).unwrap();
    let run = dark_run(&c, 12, 35);
    ((run as f32) / 40.0).round() as usize
}

#[test]
fn cbz_pages_in_natural_order_without_junk() {
    let path = cbz_path();
    let d = open(&path, "", W, H);
    assert_eq!(d.doc_pages(), 12, "ComicInfo.xml, __MACOSX and dot files are not pages");
    assert_eq!(d.file_kind(), ink_mupdf::FileKind::Cbz);
    let order: Vec<usize> = (0..12).map(|p| comic_number(&d, p)).collect();
    assert_eq!(order, (1..=12).collect::<Vec<_>>());
    assert_eq!(d.page_count().pages, 12, "portrait: one page per screen");
}

#[test]
fn cbz_two_page_spreads_in_landscape() {
    let path = cbz_path();
    let mut d = open(&path, "", 1696, 1272);
    assert_eq!(d.plan(), Plan::Spread { cover_alone: true, rtl: false });
    assert_eq!(d.page_count().pages, 7);
    assert_eq!(d.pages_of_screen(0), vec![0]);
    assert_eq!(d.pages_of_screen(1), vec![1, 2]);
    assert_eq!(d.pages_of_screen(6), vec![11]);
    let p = d.pieces(1);
    assert_eq!((p[0].page, p[0].clip.right()), (1, 848), "page 2 ends at the middle");
    assert_eq!((p[1].page, p[1].clip.x), (2, 848), "page 3 starts at the middle");
    let c = screen(&mut d, 1);
    // Bars: page 2 is 80 px, page 3 is 120 px long in the source images (y 20..50).
    let scale = p[0].ctm.m[0];
    let k = scale * d.page_bounds(1).unwrap().w() / testgen::COMIC_W as f32; // screen px per image px
    let bar = |piece: &ink_mupdf::Piece| dark_run(&c, piece.clip.x + (12.0 * k) as i32, piece.clip.y + (35.0 * k) as i32);
    let (left_bar, right_bar) = (bar(&p[0]), bar(&p[1]));
    assert!(((left_bar as f32 / k) - 80.0).abs() < 4.0, "left bar {left_bar} (k {k})");
    assert!(((right_bar as f32 / k) - 120.0).abs() < 4.0, "right bar {right_bar}");
    // The cover is centred alone; positions map to the screen holding the page.
    let cover = d.pieces(0);
    assert_eq!(cover.len(), 1);
    assert!((cover[0].clip.x - (1696 - cover[0].clip.right())).abs() <= 1);
    assert_eq!(d.page_of(&"page:4".parse().unwrap()).unwrap(), 2);
    assert_eq!(d.position_of_page(2).unwrap().to_string(), "page:3");
    // Right to left: the first page of a pair goes right.
    d.set_options(FixedOptions { rtl: true, ..Default::default() });
    let p = d.pieces(1);
    assert_eq!((p[0].page, p[0].clip.x), (1, 848));
    // Off, and in portrait.
    d.set_options(FixedOptions { spreads: Spreads::Never, ..Default::default() });
    assert_eq!(d.page_count().pages, 12);
    let d = open(&path, "spreads=always,cover=0", W, H);
    assert_eq!(d.page_count().pages, 6);
    assert_eq!(d.pages_of_screen(0), vec![0, 1]);
    // PDFs in landscape stay single pages unless asked.
    let pdf = pdf_path();
    assert!(matches!(open(&pdf, "", 1696, 1272).plan(), Plan::Tiles { .. }));
}

// --- 6. Page cache -----------------------------------------------------------------------

/// `Document::prefetch_page` draws a screen into the cache once; the turn to it then
/// shows the same pixels as a screen drawn on demand.
#[test]
fn prefetch_page_fills_the_cache_once() {
    let path = cbz_path();
    let mut d = open(&path, "", W, H);
    let doc: &mut dyn Document = &mut d;
    assert!(doc.prefetch_page(3), "drawn now");
    assert!(!doc.prefetch_page(3), "already cached");
    assert!(!doc.prefetch_page(12), "past the last screen");
    assert!(d.is_cached(3) && !d.is_cached(2));
    let fresh = screen(&mut open(&path, "", W, H), 3);
    assert_eq!(screen(&mut d, 3).pixels, fresh.pixels);
}

#[test]
fn page_cache_respects_its_memory_cap() {
    let path = pdf_path();
    let mut d = open(&path, "", W, H);
    let one = (W * H) as usize;
    d.set_cache_cap(3 * one + 10);
    let mut first = Vec::new();
    for v in 0..7 {
        let c = screen(&mut d, v);
        if v == 5 {
            first = c.pixels.clone();
        }
        let (used, cap) = d.cache_usage();
        assert!(used <= cap, "{used} > {cap}");
    }
    assert_eq!(d.cache_usage().0, 3 * one);
    assert!(d.cache_evictions() >= 4);
    assert!(d.is_cached(6) && d.is_cached(5) && !d.is_cached(0));
    assert_eq!(screen(&mut d, 5).pixels, first, "cached screen is identical");
    d.prefetch(8).unwrap();
    assert!(d.is_cached(8));
    assert!(d.display_lists() <= ink_mupdf::document::DISPLAY_LISTS);
    d.set_cache_cap(one);
    assert!(d.cache_usage().0 <= one);
    // A cap below one screen: nothing is stored, rendering still works.
    d.set_cache_cap(100);
    let _ = screen(&mut d, 2);
    assert_eq!(d.cache_usage().0, 0);
}

// --- Robustness ---------------------------------------------------------------------------

#[test]
fn damaged_and_odd_files_fail_cleanly() {
    let junk = write("junk.pdf", b"this is not a pdf at all");
    assert!(matches!(MupdfDocument::open(&junk, FixedOptions::default()), Err(DocError::Format(_))));
    let empty = write("empty.pdf", b"");
    assert!(MupdfDocument::open(&empty, FixedOptions::default()).is_err());
    let full = testgen::fixture_pdf();
    let cut = write("cut.pdf", &full[..full.len() * 2 / 3]);
    if let Ok(mut d) = MupdfDocument::open(&cut, FixedOptions::default()) {
        d.set_layout(&LayoutSettings::for_screen(600, 800)).unwrap();
        let mut c = Canvas::new(600, 800);
        for v in 0..d.page_count().pages {
            let _ = d.render_page(v, &mut c);
        }
        assert!(d.warnings() > 0, "repair warnings");
    }
    let no_images = write("text.cbz", &testgen::write_zip(&[("readme.txt", b"hello".to_vec())]));
    if let Ok(mut d) = MupdfDocument::open(&no_images, FixedOptions::default()) {
        assert_eq!(d.page_count().pages, 0);
        d.set_layout(&LayoutSettings::for_screen(600, 800)).unwrap();
        let mut c = Canvas::new(600, 800);
        assert!(d.render_page(0, &mut c).is_err());
        assert!(d.position_of_page(0).is_err());
    }
    let d = open(&pdf_path(), "", W, H);
    assert!(matches!(d.page_of(&"page:10".parse().unwrap()), Err(DocError::PageOutOfRange(10))));
    assert!(matches!(d.page_of(&"/body/DocFragment[1]/body".parse().unwrap()), Err(DocError::BadPosition(_))));
    let mut d = open(&pdf_path(), "", W, H);
    assert!(d.set_layout(&LayoutSettings::for_screen(0, 10)).is_err());
    let _ = Fit::Width;
}

#[test]
fn zoom_step_cuts_pages_into_more_screens_and_back() {
    let mut d = open(&pdf_path(), "", W, H);
    let flat = d.page_count().pages;
    assert_eq!(d.zoom_step(1), Some(1.25));
    assert_eq!(d.zoom_step(1), Some(1.5));
    let zoomed = d.page_count().pages;
    assert!(zoomed > flat, "{flat} -> {zoomed}");
    assert_eq!(d.options().zoom, 1.5);
    assert_eq!(d.zoom_step(-2), Some(1.0));
    assert_eq!(d.page_count().pages, flat);
    for _ in 0..10 {
        d.zoom_step(1);
    }
    assert_eq!(d.zoom_step(1), None, "at the top of the ladder");
    assert_eq!(d.options().zoom, 4.0);
}

/// The trait's view options reach the engine: two columns cut each page into two
/// screens, the current page stays on screen, and the engine's own gamma stays.
#[test]
fn fixed_view_through_the_document_trait() {
    let path = pdf_path();
    let mut d = open(&path, "gamma=1.5", W, H);
    let base = d.fixed_view().expect("a fixed-layout document has view options");
    assert_eq!(base, ink_doc::FixedView::default());
    let before = d.page_count().pages;
    d.set_current_page(d.page_of(&"page:2".parse().unwrap()).unwrap());
    let view = ink_doc::FixedView { columns: 2, ..base };
    d.set_fixed_view(&view).unwrap();
    assert_eq!(d.fixed_view(), Some(view));
    assert_eq!(d.options().gamma, 1.5, "gamma is the engine's, not the view's");
    assert!(d.page_count().pages > before, "two columns: more screens ({} vs {before})", d.page_count().pages);
    let Position::Page(at) = d.position_of_page(d.current_page()).unwrap() else { panic!("a page position") };
    assert_eq!(at.page, 2, "the page shown before stays on screen");
    // Setting the same view again changes nothing (the screen cache stays).
    let cur = d.current_page();
    d.render_page(cur, &mut Canvas::new(W as usize, H as usize)).unwrap();
    d.set_fixed_view(&view).unwrap();
    assert!(d.is_cached(cur));
}
