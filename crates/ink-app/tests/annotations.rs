//! A page shown before its annotations are ready is presented again when they arrive,
//! with no input: the annotation thread wakes the app loop (`ink_device::wake`). Its own
//! test binary: the wake is process-wide.

use ink_app::app::App;
use ink_app::plugin_host::PluginHost;
use ink_app::router::Route;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::script::parse;
use ink_store::{Paths, Settings};
use std::path::PathBuf;
use std::sync::Arc;

/// Annotates the first character of every paragraph with a wide reading, slowly.
struct Slow;

impl ink_doc::TextAnnotator for Slow {
    fn id(&self) -> &str {
        "slow"
    }
    fn version(&self) -> u64 {
        1
    }
    fn wants(&self, p: &str, _: Option<&str>) -> bool {
        !p.trim().is_empty()
    }
    fn annotate(&self, p: &str, _: Option<&str>) -> Vec<ink_doc::Annotation> {
        std::thread::sleep(std::time::Duration::from_millis(300));
        p.char_indices().next().map(|(_, c)| ink_doc::Annotation::over(0..c.len_utf8(), "WWWWWW")).into_iter().collect()
    }
}

struct SlowPlugin(ink_plugin::PluginManifest);

impl ink_plugin::Plugin for SlowPlugin {
    fn manifest(&self) -> &ink_plugin::PluginManifest {
        &self.0
    }
    fn on_load(&mut self, _reg: &mut ink_plugin::Registrar) -> ink_plugin::Result<()> {
        Ok(())
    }
    fn text_annotator(&self) -> Option<Arc<dyn ink_doc::TextAnnotator>> {
        Some(Arc::new(Slow))
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-annotations-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Opens a copy of juliet.epub, turns to its fifth page (with the slow annotator when
/// `annotate`), waits `ms` with no input, and returns the probe (the panel as shown).
fn run(name: &str, annotate: bool, ms: u64) -> HeadlessProbe {
    let dir = tmp(name);
    let book = dir.join("juliet.epub");
    std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/juliet.epub"), &book).unwrap();
    // Four pages on (text), then `ms` of real time with no input (`sleep`; a `wait`
    // only moves the script's clock).
    let script = format!("{}sleep {ms}\nshot after\nkey power\n", "key next\n".repeat(4));
    let out = std::env::var_os("INK_ANNOT_OUT").map(|d| PathBuf::from(d).join(name));
    let (device, probe) = headless::open(HeadlessOptions { script: parse(&script).unwrap(), out_dir: out, ..Default::default() });
    let mut app = App::new(device, Settings::default(), Paths { root: dir.join("home") });
    if annotate {
        app.plugins = PluginHost::new(vec![Box::new(SlowPlugin(ink_plugin::PluginManifest::new("slow", "1", &[])))]);
    }
    app.run(Route::Reader(book)).expect("reader run");
    probe
}

#[test]
fn late_annotations_are_shown_without_input() {
    let diff = |a: &HeadlessProbe, b: &HeadlessProbe| a.panel().pixels.iter().zip(&b.panel().pixels).filter(|(x, y)| x != y).count();
    // At once: the page is shown without them (annotate() takes 300 ms each).
    let early = run("early", true, 0);
    // After a wait with no input: the annotations are on the panel.
    let t = std::time::Instant::now();
    let late = run("late", true, 1500);
    println!("run with a 1.5 s wait: {:?}", t.elapsed());
    let differ = diff(&early, &late);
    assert!(differ > 300, "the annotations reached the panel after the wait: {differ} pixels differ");
}
