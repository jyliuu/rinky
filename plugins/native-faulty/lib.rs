//! Test fixture for rinky's native plugin host: a plugin library that misbehaves on
//! request. Never packaged. Used by `crates/ink-plugin/tests/native.rs`.
//!
//! - `on_action("panic")` panics (caught inside the library, reported as a crash).
//! - `on_action("segfault")` writes through a null pointer (a SIGSEGV in rinky).
//! - `on_action("abort")` calls `std::process::abort` (SIGABRT).
//! - `on_action("ui")` uses host services; `claims_lookup` and the annotator panic for
//!   the text `boom`.
//! - The annotator returns one good and two bad ranges for every paragraph.
//! - `on_book_open` segfaults when its data folder holds a file `crash-on-open` (the
//!   emulator scenario `tests/e2e/plugin-native-crash.sh`).

use ink_plugin_native::{export_plugin, json, Annotation, AnnotationPosition, Host, HttpEvent, Plugin, Registrar, Result, TextAnnotator};
use std::sync::Arc;

/// The fixture.
#[derive(Default)]
pub struct Faulty {
    results: Vec<String>,
}

struct Marks;

impl TextAnnotator for Marks {
    fn id(&self) -> &str {
        "faulty-marks"
    }
    fn version(&self) -> u64 {
        7
    }
    fn wants(&self, p: &str, _lang: Option<&str>) -> bool {
        !p.is_empty()
    }
    fn positions(&self) -> &[AnnotationPosition] {
        &[AnnotationPosition::Over, AnnotationPosition::Under]
    }
    fn annotate(&self, p: &str, _lang: Option<&str>) -> Vec<Annotation> {
        assert!(p != "boom", "the annotator panics on purpose");
        let first = p.char_indices().nth(1).map_or(p.len(), |(i, _)| i);
        vec![
            Annotation { range: 0..first, text: "1st".into(), position: AnnotationPosition::Under },
            Annotation { range: 0..p.len() + 5, text: "past the end".into(), position: AnnotationPosition::Over },
            Annotation { range: 3..3, text: "empty".into(), position: AnnotationPosition::Over },
        ]
    }
}

impl Plugin for Faulty {
    const NAME: &'static str = "native-faulty";

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.menu_item("panic", "Panic", 0)?;
        reg.selection_action("lookup", "Look up")?;
        reg.control_tile("t", "Tile", "")?;
        reg.status_widget("w", "left", "ready")?;
        Ok(())
    }

    fn on_book_open(&mut self, host: &mut Host) -> Result<()> {
        if host.data_dir().is_ok_and(|d| d.join("crash-on-open").exists()) {
            host.log("crash-on-open: segfault now");
            // SAFETY: none; the point is the crash.
            unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u32>(), 1) };
        }
        Ok(())
    }

    fn on_action(&mut self, host: &mut Host, id: &str) -> Result<()> {
        match id {
            "panic" => panic!("the fixture panics on purpose"),
            "segfault" => {
                // SAFETY: none; the point is the crash.
                unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u32>(), 1) };
                Ok(())
            }
            "abort" => std::process::abort(),
            "ui" => {
                host.toast("hello from a library")?;
                let id = host.menu("Pick", &[("One", "one"), ("Two", "two")])?;
                host.set_setting("menu", &json!(id))?;
                let back = host.setting("menu")?;
                host.status_text("w", &format!("menu {}", back.unwrap_or_default()))?;
                host.view("Card", "**bold**")?;
                Ok(())
            }
            "page" => {
                let t = host.page_text(None)?;
                host.toast(&format!("page {} chars", t.chars().count()))
            }
            "denied" => host.http_request(&ink_plugin_native::HttpRequest::get("https://example.com/")).map(|_| ()),
            other => Err(format!("unknown action {other}").into()),
        }
    }

    fn on_ui_result(&mut self, host: &mut Host, id: u32, result: &str) -> Result<()> {
        self.results.push(format!("{id}={result}"));
        host.toast(&format!("result {}", self.results.join(",")))
    }

    fn on_http(&mut self, host: &mut Host, id: u32, ev: &HttpEvent) -> Result<()> {
        host.toast(&format!("http {id} {ev:?}"))
    }

    fn claims_lookup(&self, text: &str, _lang: Option<&str>) -> bool {
        assert!(text != "boom", "claims_lookup panics on purpose");
        text.starts_with('z')
    }

    fn word_bounds(&self, text: &str, at: usize, _lang: Option<&str>) -> Option<std::ops::Range<usize>> {
        (text.chars().count() >= 2).then(|| at..at + 1)
    }

    fn text_annotator(&self) -> Option<Arc<dyn TextAnnotator>> {
        Some(Arc::new(Marks))
    }
}

export_plugin!(Faulty);

/// A function that lets a panic unwind out of the library on purpose, bypassing
/// `export_plugin!`'s guards: rinky's test shows that the host cannot catch it (the
/// process aborts with "Rust cannot catch foreign exceptions"), which is why every
/// exported hook must catch panics on the plugin side.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn native_faulty_unwind_across() -> u32 {
    panic!("a panic crossing into the host on purpose")
}
