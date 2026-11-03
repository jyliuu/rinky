//! What the panel shows while the device sleeps.
//!
//! The device layer draws it itself (the framework's screensaver module is gone while
//! rinky runs): the current page with a small "asleep" badge, a chosen PNG, or a
//! book cover the app rendered into a [`Canvas`]. The result is in *panel*
//! orientation (portrait), whatever the UI rotation is.

use crate::canvas::Canvas;
use crate::font;
use crate::geom::Size;
use std::path::PathBuf;

/// The screensaver source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Screensaver {
    /// Keep the page that is on the panel, with a small "asleep" badge.
    #[default]
    CurrentPage,
    /// A PNG file, scaled to fit (decoded at sleep time; falls back to a message).
    Image(PathBuf),
    /// A cover (or any picture) the app rendered, scaled to fit.
    Cover(Canvas),
    /// A plain message screen.
    Message(String),
}

impl Screensaver {
    /// From a setting string: `page`, `cover` (needs a canvas later; `page` until
    /// then), `blank`, or `image:PATH` / a path ending in `.png`.
    pub fn from_setting(s: &str) -> Screensaver {
        let s = s.trim();
        if let Some(p) = s.strip_prefix("image:") {
            return Screensaver::Image(PathBuf::from(p));
        }
        if s.to_ascii_lowercase().ends_with(".png") {
            return Screensaver::Image(PathBuf::from(s));
        }
        match s {
            "blank" | "message" => Screensaver::Message("rinky".into()),
            _ => Screensaver::CurrentPage,
        }
    }

    /// Short name for logs.
    pub fn describe(&self) -> String {
        match self {
            Screensaver::CurrentPage => "current page".into(),
            Screensaver::Image(p) => format!("image {}", p.display()),
            Screensaver::Cover(c) => format!("cover {}x{}", c.width, c.height),
            Screensaver::Message(m) => format!("message {m:?}"),
        }
    }
}

fn badge(c: &mut Canvas, text: &str) {
    // A black box with white text in the bottom-right corner.
    let scale = 4;
    let tw = font::text_width(text, scale);
    let (bw, bh) = (tw + 48, font::CELL_H * scale + 32);
    let x = c.width as i32 - bw - 32;
    let y = c.height as i32 - bh - 32;
    c.fill_rect(x - 4, y - 4, bw + 8, bh + 8, 255);
    c.fill_rect(x, y, bw, bh, 0);
    font::draw_text(c, x + 24, y + 18, scale, 255, text);
}

fn message_screen(size: Size, lines: &[&str]) -> Canvas {
    let mut c = Canvas::new(size.w as usize, size.h as usize);
    let (w, h) = (size.w as i32, size.h as i32);
    c.fill_rect(0, 0, w, h, 0);
    c.fill_rect(40, 40, w - 80, h - 80, 255);
    font::draw_lines_centered(&mut c, 8, 0, lines);
    c
}

/// Renders the screensaver for a panel of `size`; `panel_now` is what the panel shows
/// (panel orientation), used by [`Screensaver::CurrentPage`].
pub fn render(source: &Screensaver, panel_now: Option<&Canvas>, size: Size) -> Canvas {
    let (w, h) = (size.w as usize, size.h as usize);
    match source {
        Screensaver::CurrentPage => match panel_now {
            Some(p) if p.width == w && p.height == h => {
                let mut c = p.clone();
                badge(&mut c, "asleep");
                c
            }
            _ => message_screen(size, &["rinky", "asleep", "press the power button to wake"]),
        },
        Screensaver::Image(path) => match Canvas::load_png(path) {
            Ok(img) => img.fit_into(w, h, 255),
            // A missing file is normal for the book cover (a book without one, or no book
            // opened yet): the page stays, as with `CurrentPage`.
            Err(e) => {
                log::info!("screensaver image {}: {e}; showing the current page", path.display());
                render(&Screensaver::CurrentPage, panel_now, size)
            }
        },
        Screensaver::Cover(img) => img.fit_into(w, h, 255),
        Screensaver::Message(m) => message_screen(size, &[m.as_str(), "asleep", "press the power button to wake"]),
    }
}

/// The USB drive-mode lock screen (panel orientation).
pub fn usb_lock_screen(size: Size) -> Canvas {
    message_screen(
        size,
        &["USB drive mode", "Your computer can change files now.", "Eject it on the computer,", "then unplug the cable."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: Size = Size::new(126, 168);

    #[test]
    fn current_page_keeps_the_page_and_adds_a_badge() {
        let size = Size::new(400, 300);
        let mut page = Canvas::new(400, 300);
        page.fill_rect(0, 0, 50, 50, 0);
        let s = render(&Screensaver::CurrentPage, Some(&page), size);
        assert_eq!(s.get(10, 10), Some(0), "page content kept");
        assert_eq!(s.get(399 - 40, 299 - 40), Some(0), "badge in the corner");
        // Without a snapshot (or a wrong size) it falls back to a message.
        let m = render(&Screensaver::CurrentPage, None, PANEL);
        assert_eq!((m.width, m.height), (126, 168));
    }

    #[test]
    fn cover_and_image_fit_the_panel() {
        let mut cover = Canvas::new(60, 90);
        cover.clear(0);
        let s = render(&Screensaver::Cover(cover.clone()), None, PANEL);
        assert_eq!((s.width, s.height), (126, 168));
        assert_eq!(s.get(63, 84), Some(0));
        let dir = crate::sysfs::test_dir("saver");
        let p = dir.join("c.png");
        cover.save_png(&p).unwrap();
        let s2 = render(&Screensaver::Image(p.clone()), None, PANEL);
        assert_eq!(s2, s);
        let missing = render(&Screensaver::Image(dir.join("none.png")), None, PANEL);
        assert_eq!((missing.width, missing.height), (126, 168));
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(Screensaver::from_setting("image:/mnt/us/x.png"), Screensaver::Image("/mnt/us/x.png".into()));
        assert_eq!(Screensaver::from_setting("/mnt/us/y.PNG"), Screensaver::Image("/mnt/us/y.PNG".into()));
        assert_eq!(Screensaver::from_setting("page"), Screensaver::CurrentPage);
        assert!(matches!(Screensaver::from_setting("blank"), Screensaver::Message(_)));
        assert_eq!(usb_lock_screen(PANEL).width, 126);
    }
}
