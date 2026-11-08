//! Parsed `fontdue` faces, shared by every [`crate::Fonts`] of the process.
//!
//! `fontdue` reads every glyph of a face when it parses it: 0.3 to 1.3 s and 12 to 15 MB
//! per text face on a Kindle. Each font data is therefore parsed once and the result
//! shared: every `Fonts` built from the same bytes, on any thread, holds the same
//! `Arc<Font>`. A `Font` is plain data that no method changes after parsing, so it is
//! `Send + Sync`; the glyph and metric caches that need `&mut` stay in each `Fonts`.
//!
//! The table holds weak references: a face no `Fonts` uses any more is freed, so a
//! reading font the user tried and left does not stay in memory. Two threads asking for
//! the same face at once parse it once; the second waits for the first.

use crate::TextError;
use fontdue::{Font, FontSettings};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, Weak};

/// Identity of font data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    /// Bytes that live for the whole program (embedded or memory-mapped): their address
    /// and length name them exactly.
    Static(usize, usize),
    /// Other bytes: their length and a 64-bit hash of their contents.
    Owned(usize, u64),
}

impl Key {
    fn of_static(bytes: &'static [u8]) -> Self {
        Key::Static(bytes.as_ptr() as usize, bytes.len())
    }

    fn of_owned(bytes: &[u8]) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        Key::Owned(bytes.len(), h.finish())
    }
}

/// One face: locked while it is parsed, so a second request waits instead of parsing.
type Slot = Arc<Mutex<Weak<Font>>>;

static TABLE: Mutex<Option<HashMap<Key, Slot>>> = Mutex::new(None);

// The faces are shared between threads.
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<Font>();
};

/// The parsed face of `bytes`, which live for the whole program.
pub(crate) fn parse_static(bytes: &'static [u8]) -> Result<Arc<Font>, TextError> {
    parse(Key::of_static(bytes), bytes)
}

/// The parsed face of `bytes` (any lifetime; looked up by contents).
pub(crate) fn parse_owned(bytes: &[u8]) -> Result<Arc<Font>, TextError> {
    parse(Key::of_owned(bytes), bytes)
}

fn parse(key: Key, bytes: &[u8]) -> Result<Arc<Font>, TextError> {
    let slot = {
        let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        let table = table.get_or_insert_with(HashMap::new);
        if !table.contains_key(&key) {
            // Forget faces nobody holds before adding one. A slot being parsed has a
            // second reference (the parsing thread's) and stays.
            table.retain(|_, s| Arc::strong_count(s) > 1 || !matches!(s.try_lock(), Ok(w) if w.strong_count() == 0));
        }
        table.entry(key).or_default().clone()
    };
    let mut weak = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(font) = weak.upgrade() {
        return Ok(font);
    }
    let font = Font::from_bytes(bytes, FontSettings::default()).map_err(|e| TextError::Font(e.to_string()))?;
    let font = Arc::new(font);
    *weak = Arc::downgrade(&font);
    Ok(font)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True when the face of `key` is parsed and in use.
    fn live(key: Key) -> bool {
        let table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        let slot = table.as_ref().and_then(|t| t.get(&key).cloned());
        slot.is_some_and(|s| s.lock().is_ok_and(|w| w.strong_count() > 0))
    }

    #[test]
    fn a_face_is_parsed_once_and_freed_when_unused() {
        // Font data no other test uses: the test font with one byte of padding.
        let mut bytes = std::fs::read(crate::testdirs::data().join("InkTestFallback.ttf")).unwrap();
        bytes.push(0);
        let a = parse_owned(&bytes).unwrap();
        let copy = bytes.clone();
        let b = std::thread::spawn(move || parse_owned(&copy).unwrap()).join().unwrap();
        assert!(Arc::ptr_eq(&a, &b), "another thread and another copy of the bytes share the face");
        drop((a, b));
        assert!(!live(Key::of_owned(&bytes)), "a face nobody holds is freed");
        let c = parse_owned(&bytes).unwrap();
        assert!(live(Key::of_owned(&bytes)));
        assert_eq!(c.metrics('A', 40.0), Font::from_bytes(bytes.as_slice(), FontSettings::default()).unwrap().metrics('A', 40.0));
    }

    #[test]
    fn static_faces_are_shared() {
        let bytes = crate::embedded_font_bytes(crate::Style::Bold);
        let a = parse_static(bytes).unwrap();
        let b = std::thread::spawn(move || parse_static(bytes).unwrap()).join().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(parse_owned(b"not a font").is_err());
    }
}
