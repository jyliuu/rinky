//! The "Book cover" screensaver: the cover of the book being read, at panel size.
//!
//! The device draws the screensaver itself when the power key is pressed, before the
//! app hears of it, so the picture must be ready beforehand. When the reader opens a
//! book, its cover is decoded on a thread and written to `cache/sleep-cover.png`; the
//! device is given [`Screensaver::Image`] of that file and reads it at sleep time. A
//! book without a cover removes the file, and the device then keeps the current page.

use ink_device::screensaver::Screensaver;
use ink_device::{Canvas, Size};
use ink_doc::Provider as _;
use ink_store::Paths;
use std::path::{Path, PathBuf};

/// The panel-size cover the device shows while asleep.
pub fn path(paths: &Paths) -> PathBuf {
    paths.cache().join("sleep-cover.png")
}

/// Which book [`path`] was made from (`<book path>\n<mtime secs>`).
fn source_path(paths: &Paths) -> PathBuf {
    paths.cache().join("sleep-cover.src")
}

/// The device screensaver for a setting value (`page`, `cover`, `blank`,
/// `image:PATH`): `cover` is the file [`prepare`] writes.
pub fn screensaver(setting: &str, paths: &Paths) -> Screensaver {
    match setting.trim() {
        "cover" => Screensaver::Image(path(paths)),
        s => Screensaver::from_setting(s),
    }
}

/// Writes the cover of `book`, fitted to `panel` (portrait panel size), to [`path`] on a
/// thread. Does nothing when the file already shows that book.
pub fn prepare(paths: &Paths, book: &Path, panel: Size) {
    let mtime = std::fs::metadata(book).and_then(|m| m.modified()).ok();
    let secs = mtime.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let source = format!("{}\n{secs}\n{}x{}", book.display(), panel.w, panel.h);
    let (out, src) = (path(paths), source_path(paths));
    if std::fs::read_to_string(&src).is_ok_and(|s| s == source) && out.exists() {
        return;
    }
    let book = book.to_path_buf();
    let spawned = std::thread::Builder::new().name("sleep-cover".into()).spawn(move || {
        let t = std::time::Instant::now();
        let _ = std::fs::remove_file(&src);
        match render(&book, panel.w, panel.h) {
            Some(c) => {
                let tmp = out.with_extension("png.tmp");
                if let Some(d) = out.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                match c.save_png(&tmp).and_then(|()| std::fs::rename(&tmp, &out)) {
                    Ok(()) => {
                        let _ = std::fs::write(&src, &source);
                        log::info!("sleep cover: {} ({}x{}) in {} ms", book.display(), c.width, c.height, t.elapsed().as_millis());
                    }
                    Err(e) => log::warn!("sleep cover: cannot write {}: {e}", out.display()),
                }
            }
            None => {
                // The previous book's cover would be wrong: the device keeps the page.
                let _ = std::fs::remove_file(&out);
                let _ = std::fs::write(&src, &source);
                log::info!("sleep cover: {} has no cover; the page stays while asleep", book.display());
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("sleep cover: no thread: {e}");
    }
}

/// The cover of `book` fitted to `w` x `h` (white around it), or `None` without one.
fn render(book: &Path, w: u32, h: u32) -> Option<Canvas> {
    let img = if ink_mupdf::MupdfProvider.can_open(book) {
        let doc = ink_mupdf::MupdfDocument::open(book, ink_mupdf::FixedOptions::default()).ok()?;
        doc.thumbnail(w, h).ok()?
    } else {
        let bytes = ink_formats::read_info(book).ok()?.cover?.bytes;
        let (iw, ih) = ink_html::images::intrinsic_size(&bytes)?;
        let s = (w as f32 / iw as f32).min(h as f32 / ih as f32);
        let (sw, sh) = (((iw as f32 * s).round() as u32).clamp(1, w), ((ih as f32 * s).round() as u32).clamp(1, h));
        let img = ink_html::images::decode_scaled(&bytes, sw, sh, None)?;
        let (iw, ih) = (img.width() as usize, img.height() as usize);
        let pixels = img.into_raw();
        if pixels.len() != iw * ih {
            return None;
        }
        Canvas::from_pixels(iw, ih, pixels)
    };
    Some(img.fit_into(w as usize, h as usize, 255))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_setting_points_the_device_at_the_cache_file() {
        let paths = Paths::kindle();
        assert_eq!(screensaver("cover", &paths), Screensaver::Image(path(&paths)));
        assert_eq!(screensaver("page", &paths), Screensaver::CurrentPage);
        assert!(matches!(screensaver("blank", &paths), Screensaver::Message(_)));
    }

    #[test]
    fn covers_fill_the_panel_and_books_without_one_give_none() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let found = ["juliet.epub", "synthetic.epub", "formats/epub2-ncx.epub"]
            .iter()
            .filter_map(|b| render(&dir.join(b), 126, 168))
            .collect::<Vec<_>>();
        assert_eq!(found.len(), 2, "juliet and epub2-ncx have covers");
        assert!(found.iter().all(|c| (c.width, c.height) == (126, 168)));
        assert!(render(&dir.join("no-such-book.epub"), 126, 168).is_none());
    }
}
