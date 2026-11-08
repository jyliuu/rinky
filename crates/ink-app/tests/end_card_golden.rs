//! Golden images of the reader's end card (`screens::reader::end`), the card shown when
//! the reader turns past the last page. Each case is drawn on a blank 1272 x 1696
//! canvas and must match `tests/golden/end-card-*.png` exactly.
//!
//! `INK_GOLDEN_UPDATE=1` rewrites the goldens (use `scripts/golden-update.sh`), and
//! `INK_GOLDEN_OUT` (default `target/golden-out`) receives `*.actual.png` and
//! `*.diff.png` (changed pixels black on white) for every mismatch.

use ink_app::screens::reader::end::{build, EndInfo, NextBook};
use ink_device::{Canvas, Size};
use ink_text::Fonts;
use ink_ui::{PaintCx, Theme};
use std::path::{Path, PathBuf};

const SIZE: Size = Size { w: 1272, h: 1696 };

type Case = (&'static str, fn() -> EndInfo);

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn out_dir() -> PathBuf {
    std::env::var_os("INK_GOLDEN_OUT").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/golden-out"))
}

/// Not finished, three stars, the next book of the series suggested.
fn reading() -> EndInfo {
    EndInfo {
        title: vec!["The Wind in the Willows".into()],
        author: "Kenneth Grahame".into(),
        finished: false,
        rating: Some(3),
        next: Some(NextBook {
            path: "/mnt/us/documents/willows-2.epub".into(),
            title: "Return to the Willows".into(),
            why: "Next in Willows · book 2".into(),
            progress: None,
        }),
    }
}

/// Finished, five stars, a two-line title, a book in progress suggested.
fn finished() -> EndInfo {
    EndInfo {
        title: vec!["Twenty Thousand Leagues".into(), "Under the Seas".into()],
        author: "Jules Verne".into(),
        finished: true,
        rating: Some(5),
        next: Some(NextBook { path: "/mnt/us/documents/odyssey.epub".into(), title: "The Odyssey".into(), why: "Continue reading · 40%".into(), progress: Some(0.4) }),
    }
}

/// Nothing to suggest, no rating.
fn alone() -> EndInfo {
    EndInfo { title: vec!["Romeo and Juliet".into()], author: "William Shakespeare".into(), finished: false, rating: None, next: None }
}

#[test]
fn end_card_goldens() {
    let update = std::env::var("INK_GOLDEN_UPDATE").as_deref() == Ok("1");
    let mut fonts = Fonts::load();
    let theme = Theme::default();
    let mut failures = Vec::new();
    let cases: [Case; 3] = [("end-card", reading), ("end-card-finished", finished), ("end-card-alone", alone)];
    for (name, info) in cases {
        let mut canvas = Canvas::new(SIZE.w as usize, SIZE.h as usize);
        canvas.clear(theme.bg);
        let mut o = build(&info(), SIZE);
        o.tree.paint(&mut PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme });
        assert!(canvas.pixels.iter().filter(|&&p| p < 128).count() > 2000, "{name}: nothing drawn");
        let file = golden_dir().join(format!("{name}.png"));
        let expected = Canvas::load_png(&file).ok();
        let same = expected.as_ref().is_some_and(|e| (e.width, e.height) == (canvas.width, canvas.height) && e.pixels == canvas.pixels);
        if same {
            continue;
        }
        let out = out_dir();
        std::fs::create_dir_all(&out).unwrap();
        canvas.save_png(&out.join(format!("{name}.actual.png"))).unwrap();
        if let Some(e) = expected.as_ref().filter(|e| (e.width, e.height) == (canvas.width, canvas.height)) {
            let mut d = Canvas::new(e.width, e.height);
            for (i, px) in d.pixels.iter_mut().enumerate() {
                if e.pixels[i] != canvas.pixels[i] {
                    *px = 0;
                }
            }
            d.save_png(&out.join(format!("{name}.diff.png"))).unwrap();
        }
        if update {
            std::fs::create_dir_all(golden_dir()).unwrap();
            canvas.save_png(&file).unwrap();
            println!("updated {}", file.display());
        } else {
            failures.push(format!("{name}: differs from {} (see {})", file.display(), out.display()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}\nRun scripts/golden-update.sh after an intended change.");
}
