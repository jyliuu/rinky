//! Golden tests for the page-number footer and checks of the status bars (top and
//! bottom margins).
//!
//! `crates/ink-html/tests/golden.rs` covers page content only. This file covers what
//! the reader adds on top, without depending on the HTML engine:
//! - `footer_band_goldens`: `screens::reader::draw_footer` (page numbers) on a blank
//!   1272 x 1696 canvas for fixed (page, total) pairs and two font sizes; the bottom band
//!   must match `tests/golden/footer-*.png` exactly, and nothing above it may change.
//! - `render_page_adds_only_the_bars`: `screens::reader::render_page` on real books
//!   equals the content-only `Document::render_page` between the margins, and the margins
//!   hold exactly the default status bars for that page.
//! - `bars_keep_their_rows_and_slots_apart`: long texts in every slot stay in the bar's
//!   rows, the centre never overlaps the sides, the top bar keeps off the bookmark corner.
//!
//! `INK_GOLDEN_UPDATE=1` rewrites the goldens (use `scripts/golden-update.sh`), and
//! `INK_GOLDEN_OUT` (default `target/golden-out`) receives `*.actual.png` and
//! `*.diff.png` (changed pixels black on white) for every mismatch.

use ink_app::screens::reader::footer::{self, DeviceStatus, Edge, FooterInfo, Part};
use ink_app::screens::reader::{draw_footer, layout_for, render_page, DOG_EAR};
use ink_device::{Canvas, Rect};
use ink_doc::LayoutSettings;
use ink_store::Settings;
use ink_text::Fonts;
use std::path::{Path, PathBuf};

const W: u32 = 1272;
const H: u32 = 1696;

/// (golden name, 0-based page index, total pages, body font px).
const CASES: &[(&str, usize, usize, f32)] = &[
    ("footer-1-of-1", 0, 1, 40.0),
    ("footer-3-of-208", 2, 208, 40.0),
    ("footer-208-of-208", 207, 208, 40.0),
    ("footer-999-of-1000", 998, 1000, 40.0),
    ("footer-100000-of-100000", 99_999, 100_000, 40.0),
    ("footer-3-of-208-font56", 2, 208, 56.0),
];

/// Draws one golden case.
type DrawFn = dyn Fn(&mut Canvas, &mut Fonts, &LayoutSettings);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn out_dir() -> PathBuf {
    std::env::var_os("INK_GOLDEN_OUT").map(PathBuf::from).unwrap_or_else(|| root().join("target/golden-out"))
}

fn layout(font_px: f32) -> LayoutSettings {
    let mut settings = Settings::default();
    settings.reader.font_px = font_px;
    layout_for(&settings, W, H)
}

/// The bottom margin: where the footer is drawn.
fn band(layout: &LayoutSettings) -> Rect {
    let top = (layout.height as f32 - layout.margin_bottom).floor() as i32;
    Rect::new(0, top, layout.width, layout.height - top as u32)
}

fn diff_count(a: &Canvas, b: &Canvas) -> Option<usize> {
    ((a.width, a.height) == (b.width, b.height)).then(|| a.pixels.iter().zip(&b.pixels).filter(|(x, y)| x != y).count())
}

fn diff_image(a: &Canvas, b: &Canvas) -> Canvas {
    let mut d = Canvas::new(a.width, a.height);
    for (i, px) in d.pixels.iter_mut().enumerate() {
        if a.pixels.get(i) != b.pixels.get(i) {
            *px = 0;
        }
    }
    d
}

#[test]
fn footer_band_goldens() {
    let update = std::env::var("INK_GOLDEN_UPDATE").as_deref() == Ok("1");
    let mut fonts = Fonts::load();
    let mut failures = Vec::new();
    let mut cases: Vec<(&str, f32, Box<DrawFn>)> = Vec::new();
    for &(name, index, total, font_px) in CASES {
        cases.push((name, font_px, Box::new(move |c, f, l| draw_footer(c, f, l, index, total))));
    }
    for (name, font_px, draw) in cases {
        let layout = layout(font_px);
        let band = band(&layout);
        let mut canvas = Canvas::new(W as usize, H as usize);
        draw(&mut canvas, &mut fonts, &layout);

        // Nothing above the bottom margin changes.
        let above = canvas.crop(Rect::new(0, 0, W, band.y as u32));
        assert!(above.pixels.iter().all(|&p| p == 255), "{name}: footer drew above the bottom margin");

        let actual = canvas.crop(band);
        assert!(actual.pixels.iter().filter(|&&p| p < 128).count() > 50, "{name}: footer band is empty");
        let file = golden_dir().join(format!("{name}.png"));
        let expected = Canvas::load_png(&file).ok();
        let diff = expected.as_ref().and_then(|e| diff_count(e, &actual));
        if diff == Some(0) {
            continue;
        }
        let out = out_dir();
        std::fs::create_dir_all(&out).unwrap();
        actual.save_png(&out.join(format!("{name}.actual.png"))).unwrap();
        if let Some(e) = expected.as_ref().filter(|e| (e.width, e.height) == (actual.width, actual.height)) {
            diff_image(e, &actual).save_png(&out.join(format!("{name}.diff.png"))).unwrap();
        }
        if update {
            std::fs::create_dir_all(golden_dir()).unwrap();
            actual.save_png(&file).unwrap();
            println!("updated {} ({})", file.display(), diff.map_or("new or resized".to_string(), |n| format!("{n} pixels changed")));
        } else {
            failures.push(match diff {
                Some(n) => format!("{name}: {n} pixels differ"),
                None if expected.is_none() => format!("{name}: golden missing (run scripts/golden-update.sh)"),
                None => format!("{name}: size differs"),
            });
        }
    }
    assert!(failures.is_empty(), "footer golden mismatches (actual/diff images in {}):\n{}", out_dir().display(), failures.join("\n"));
}

#[test]
fn footer_is_centred_and_grows_with_the_number() {
    let mut fonts = Fonts::load();
    let layout = layout(40.0);
    let ink = |index: usize, total: usize, fonts: &mut Fonts| {
        let mut c = Canvas::new(W as usize, H as usize);
        draw_footer(&mut c, fonts, &layout, index, total);
        let band = c.crop(band(&layout));
        let cols: Vec<usize> = (0..band.width).filter(|&x| (0..band.height).any(|y| band.pixels[y * band.width + x] < 128)).collect();
        (*cols.first().unwrap(), *cols.last().unwrap())
    };
    let (l1, r1) = ink(0, 1, &mut fonts);
    let (l2, r2) = ink(99_999, 100_000, &mut fonts);
    let centre = W as usize / 2;
    for (l, r) in [(l1, r1), (l2, r2)] {
        let mid = (l + r) / 2;
        assert!(mid.abs_diff(centre) <= 3, "footer centred at {mid}, panel centre {centre}");
    }
    assert!(r2 - l2 > r1 - l1, "longer numbers are wider");
}

#[test]
fn render_page_adds_only_the_bars() {
    let settings = Settings::default();
    let mut fonts = Fonts::load();
    let providers = ink_app::app::providers();
    for (book, pages) in [("testdata/juliet.epub", [0usize, 2, 50]), ("testdata/synthetic.epub", [0, 1, 3])] {
        let path = root().join(book);
        let mut doc = ink_app::screens::reader::open_book(&providers, &settings, &path, W, H).unwrap();
        assert!(doc.wait_paginated(std::time::Duration::from_secs(120)), "{book}: pagination did not complete");
        let total = doc.page_count().pages;
        let layout = doc.layout().clone();
        let band = band(&layout);
        let body = Rect::new(0, layout.margin_top as i32, W, (band.y - layout.margin_top as i32) as u32);
        for page in pages.into_iter().filter(|&p| p < total) {
            let mut content = Canvas::new(W as usize, H as usize);
            doc.render_page(page, &mut content).unwrap();
            let mut reader = Canvas::new(W as usize, H as usize);
            render_page(doc.as_mut(), page, &mut reader, &mut fonts).unwrap();
            assert_eq!(diff_count(&content.crop(body), &reader.crop(body)), Some(0), "{book} page {}: text area changed", page + 1);
            let mut expected = content.clone();
            footer::draw_default_bars(doc.as_ref(), page, &mut expected, &mut fonts);
            assert_eq!(diff_count(&expected, &reader), Some(0), "{book} page {}: bars", page + 1);
            assert_ne!(diff_count(&content.crop(band), &reader.crop(band)), Some(0), "{book} page {}: no bottom bar drawn", page + 1);
        }
    }
}

#[test]
fn bars_keep_their_rows_and_slots_apart() {
    let mut fonts = Fonts::load();
    let layout = layout(40.0);
    let long = "A very long chapter title that goes on and on ".repeat(4);
    let info = FooterInfo {
        page: 86,
        total: 208,
        progress: 0.42,
        marks: vec![0.1, 0.3, 0.6],
        pages_left_chapter: Some(7),
        minutes_left_chapter: Some(12),
        book_title: long.clone(),
        chapter_title: Some(long.clone()),
        device: DeviceStatus { clock: "12:34".into(), battery: Some((57, false)), wifi: Some(ink_device::NetStatus::Connected { essid: "Home".into(), signal: Some(4) }), light: Some(12) },
        ..FooterInfo::default()
    };
    let mut bars = ink_store::prefs::StatusBars::default();
    for b in [&mut bars.top, &mut bars.bottom] {
        b.left = vec!["book_title".into(), "clock".into()];
        b.center = vec!["chapter_title".into()];
        b.right = vec!["battery_icon".into(), "percent".into(), "wifi".into()];
        b.progress_bar = "chapters".into();
    }
    let mut c = Canvas::new(W as usize, H as usize);
    let drawn = footer::draw_bars(&mut c, &mut fonts, &layout, &bars, &info, Part::All);
    assert!(drawn.len() == 4, "text row and progress box of both bars: {drawn:?}");
    // No ink outside the drawn rows.
    let mut outside = c.clone();
    for r in &drawn {
        outside.fill_rect(r.x, r.y, r.w as i32, r.h as i32, 255);
    }
    assert!(outside.pixels.iter().all(|&p| p == 255), "ink outside the bar rows");
    // The bookmark mark covers x >= W - DOG_EAR + y on row y: no ink in it or within 8 px.
    for y in 0..layout.margin_top as i32 {
        for x in W as i32 - DOG_EAR - 8 + y..W as i32 {
            assert_eq!(c.get(x, y), Some(255), "top bar ink at {x},{y}, in or next to the bookmark corner");
        }
    }
    // Left, centre and right slots lie inside the row and do not overlap.
    for edge in [Edge::Top, Edge::Bottom] {
        let row = footer::rows_for(&layout, &bars, edge, &info).text.unwrap();
        let [l, m, r] = footer::slot_spans(&mut c, &mut fonts, &layout, &bars, edge, &info).unwrap();
        assert!(m.1 > 0.0 && l.1 > 0.0 && r.1 > 0.0, "{edge:?}: every slot drawn: {l:?} {m:?} {r:?}");
        assert!(l.0 >= row.x as f32 && l.0 + l.1 < m.0 && m.0 + m.1 < r.0 && r.0 + r.1 <= row.right() as f32 + 0.5, "{edge:?}: {l:?} {m:?} {r:?} in {row:?}");
    }
}

#[test]
fn bars_ignore_the_margins() {
    let info = FooterInfo { page: 3, total: 208, progress: 0.4, device: DeviceStatus { clock: "12:34".into(), ..DeviceStatus::default() }, ..FooterInfo::default() };
    let bars = ink_store::prefs::StatusBars::default();
    let rows = |l: &LayoutSettings| [Edge::Top, Edge::Bottom].map(|e| footer::rows_for(l, &bars, e, &info));
    let mut settings = Settings::default();
    let base = rows(&layout_for(&settings, W, H));
    // The bars span the screen less BAR_INSET (the top bar stops before the bookmark
    // corner) and sit BAR_EDGE from their screen edges.
    let top = base[0].text.unwrap();
    assert_eq!((top.x, top.y), (footer::BAR_INSET, footer::BAR_EDGE));
    assert!(top.right() >= W as i32 - (DOG_EAR - footer::BAR_EDGE) - 16, "{top:?}");
    let bottom = base[1].progress.unwrap().union(&base[1].text.unwrap());
    assert_eq!((bottom.x, bottom.right(), bottom.bottom()), (footer::BAR_INSET, W as i32 - footer::BAR_INSET, H as i32 - footer::BAR_EDGE));
    assert_eq!(base[0].centre, W as f32 / 2.0);
    for (x, t, b) in [(0.0, 0.0, 0.0), (450.0, 450.0, 450.0), (450.0, 0.0, 450.0), (0.0, 450.0, 0.0)] {
        (settings.reader.margin_x, settings.reader.margin_top, settings.reader.margin_bottom) = (x, t, b);
        let l = layout_for(&settings, W, H);
        assert_eq!(rows(&l), base, "margins {x}/{t}/{b} moved a bar");
        // The text area is between the bars, the margins measured from their inner edges.
        assert_eq!(l.margin_top, top.bottom() as f32 + t);
        assert_eq!(l.margin_bottom, (H as i32 - bottom.y) as f32 + b);
        assert_eq!(l.margin_left, x);
    }
    // With a bar off, its margin is measured from the screen edge.
    settings.status_bars.top.enabled = false;
    settings.reader.margin_top = 0.0;
    assert_eq!(layout_for(&settings, W, H).margin_top, 0.0);
}

/// `battery_icon_percent` puts the percent right after the icon: narrower than the icon
/// and `battery` as two widgets (which the bar's separator joins), and the icon is
/// centred on the digits.
#[test]
fn battery_icon_and_percent_sit_together() {
    let mut fonts = Fonts::load();
    let layout = layout(40.0);
    let info = FooterInfo {
        page: 3,
        total: 208,
        device: DeviceStatus { clock: "12:34".into(), battery: Some((57, true)), wifi: None, light: None },
        ..FooterInfo::default()
    };
    let draw = |right: &[&str], fonts: &mut Fonts| {
        let mut bars = ink_store::prefs::StatusBars::default();
        bars.top.right = right.iter().map(|s| s.to_string()).collect();
        let mut c = Canvas::new(W as usize, H as usize);
        footer::draw_bars(&mut c, fonts, &layout, &bars, &info, Part::All);
        let span = footer::slot_spans(&mut c, fonts, &layout, &bars, Edge::Top, &info).unwrap()[2];
        (c, span)
    };
    let (one, (x_one, w_one)) = draw(&["battery_icon_percent"], &mut fonts);
    let (_, (_, w_two)) = draw(&["battery_icon", "battery"], &mut fonts);
    assert!(w_one + 20.0 < w_two, "one widget {w_one} px, icon and percent {w_two} px");
    // The icon's middle row and the digits' middle row match.
    let rows = footer::rows_for(&layout, &ink_store::prefs::StatusBars::default(), Edge::Top, &info);
    let row = rows.text.unwrap();
    let x0 = x_one.round() as usize;
    let icon_w = (rows.px * 1.5).round() as usize;
    let mid = |xs: std::ops::Range<usize>| {
        let rows: Vec<usize> = (row.y as usize..row.bottom() as usize).filter(|&y| xs.clone().any(|x| one.pixels[y * one.width + x] < 128)).collect();
        (rows[0] + rows[rows.len() - 1]) as f32 / 2.0
    };
    let (icon, digits) = (mid(x0..x0 + icon_w), mid(x0 + icon_w + 4..(x_one + w_one).ceil() as usize));
    assert!((icon - digits).abs() <= 1.0, "icon middle {icon}, digits middle {digits}");
    if let Some(dir) = std::env::var_os("INK_BATTERY_OUT") {
        one.crop(Rect::new(W as i32 - 400, row.y, 400, row.h)).save_png(&PathBuf::from(dir).join("bar-battery.png")).unwrap();
    }
}
