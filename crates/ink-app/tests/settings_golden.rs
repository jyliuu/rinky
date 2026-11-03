//! Golden images of the shared settings components: the settings popup
//! (`screens::settings::popup::SettingsPopup`, every popup of the control center and
//! every plugin menu) and the settings page (`screens::settings::page`, every settings
//! row page, a plugin's settings page too). Each case is drawn on a blank 1272 x 1696
//! canvas, day and night, and must match `tests/golden/settings-*.png` exactly.
//!
//! `INK_GOLDEN_UPDATE=1` rewrites the goldens (use `scripts/golden-update.sh`), and
//! `INK_GOLDEN_OUT` (default `target/golden-out`) receives `*.actual.png` and
//! `*.diff.png` (changed pixels black on white) for every mismatch.

use ink_app::screens::settings::page;
use ink_app::screens::settings::popup::{menu_rows, Anchor, SettingsPopup};
use ink_app::screens::settings::rows::{Lead, Row};
use ink_device::{Canvas, Rect, Size};
use ink_text::Fonts;
use ink_ui::{Icon, PaintCx, Theme, WidgetTree};
use std::path::{Path, PathBuf};

const SIZE: Size = Size { w: 1272, h: 1696 };

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn out_dir() -> PathBuf {
    std::env::var_os("INK_GOLDEN_OUT").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/golden-out"))
}

/// The rows of every kind: header, switch, choice, link with a value, signal and lock,
/// action, slider, note.
fn sample_rows() -> Vec<Row> {
    vec![
        Row::header("Connection"),
        Row::toggle("Wi-Fi", true, "wifi").icon(Icon::Wifi).subtitle("Connected to Home"),
        Row::choice("Home", true, "net:0").lead(Lead::Signal(5)).lock(true).value("Saved"),
        Row::nav(Icon::Bookmark, "Saved networks", "page:saved").value("3"),
        Row::item("Scan again", "scan").icon(Icon::Sync),
        Row::slider("Brightness", 0.0, 24.0, 1.0, 12.0, "bright"),
        Row::note("Swipe down from the top edge for the control center."),
    ]
}

fn draw_popup(canvas: &mut Canvas, fonts: &mut Fonts, theme: &Theme) {
    let items: Vec<(String, String)> = [
        ("On · 192.168.15.244 (usb0)", "status"),
        ("Turn off", "off"),
        ("Turn on when rinky starts\ton", "autostart"),
        ("Server\tauto", "server"),
        ("How to connect a computer", "help"),
    ]
    .iter()
    .map(|(l, a)| (l.to_string(), a.to_string()))
    .collect();
    let mut tree = WidgetTree::new();
    let p = SettingsPopup::new("USB network", menu_rows(&items)).with_note("Plugin settings, the same rows as a page");
    let r = p.frame(Rect::new(0, 120, SIZE.w, 1100), Anchor::Top);
    tree.add(None, Box::new(p), r);
    tree.paint(&mut PaintCx { canvas, fonts, theme });
}

fn draw_page(canvas: &mut Canvas, fonts: &mut Fonts, theme: &Theme) {
    let mut tree = WidgetTree::new();
    page::add_bar(&mut tree, SIZE, "Wi-Fi", vec![ink_ui::BarAction::text("Scan", "scan")]);
    page::add_list(&mut tree, sample_rows(), page::content_rect(SIZE), 0, None);
    page::add_status(&mut tree, SIZE, "Connected to Home");
    tree.paint(&mut PaintCx { canvas, fonts, theme });
}

type DrawFn = fn(&mut Canvas, &mut Fonts, &Theme);

#[test]
fn settings_component_goldens() {
    let update = std::env::var("INK_GOLDEN_UPDATE").as_deref() == Ok("1");
    let mut fonts = Fonts::load();
    let mut failures = Vec::new();
    let cases: [(&str, DrawFn); 2] = [("settings-popup", draw_popup), ("settings-page", draw_page)];
    for (name, draw) in cases {
        for night in [false, true] {
            let name = if night { format!("{name}-night") } else { name.to_string() };
            let theme = if night { Theme::night() } else { Theme::default() };
            let mut canvas = Canvas::new(SIZE.w as usize, SIZE.h as usize);
            canvas.clear(theme.bg);
            draw(&mut canvas, &mut fonts, &theme);
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
    }
    assert!(failures.is_empty(), "{failures:#?}\nRun scripts/golden-update.sh after an intended change.");
}
