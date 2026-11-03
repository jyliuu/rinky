//! Hyphenation by language, implementing `ink_text::Hyphenator` with the
//! `hyphenation` crate's Knuth-Liang patterns (82 languages embedded).
//!
//! The language comes from the element's `lang`/`xml:lang`, then the chapter's
//! `<html lang>`, then the OPF `dc:language`. Tags map to dictionaries by trying the full
//! tag (`en-gb`), then known aliases (`en` -> `en-us`, `de` -> `de-1996`, `no` -> `nb`,
//! `el` -> `el-monoton`, `sr` -> `sr-cyrl`, ...), then the primary subtag.

use hyphenation::{Hyphenator as _, Language as HLang, Load, Standard};
use ink_text::{Hyphenator, Language};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// A pattern hyphenator for one language.
pub struct PatternHyphenator {
    lang: Language,
    dict: Standard,
}

impl Hyphenator for PatternHyphenator {
    fn hyphenate(&self, word: &str) -> Vec<usize> {
        // Words with digits or of 4 chars or fewer are left alone.
        if word.chars().count() < 5 || word.chars().any(|c| c.is_ascii_digit()) {
            return Vec::new();
        }
        let lower = word.to_lowercase();
        if lower.len() != word.len() {
            return Vec::new();
        }
        self.dict.hyphenate(&lower).breaks
    }

    fn language(&self) -> Option<&Language> {
        Some(&self.lang)
    }
}

fn dictionary_code(tag: &str) -> Option<HLang> {
    let t = tag.trim().replace('_', "-").to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    if let Some(l) = HLang::try_from_code(&t) {
        return Some(l);
    }
    let primary = t.split('-').next().unwrap_or("");
    let alias = match primary {
        "en" => "en-us",
        "de" => {
            if t.contains("ch") {
                "de-ch-1901"
            } else if t.contains("1901") {
                "de-1901"
            } else {
                "de-1996"
            }
        }
        "no" => "nb",
        "el" => "el-monoton",
        "sr" => "sr-cyrl",
        "sh" => "sh-latn",
        "mn" => "mn-cyrl",
        "la" => "la",
        "fi" => "fi",
        "zh" => "zh-latn-pinyin",
        p => p,
    };
    HLang::try_from_code(alias)
}

fn cache() -> &'static Mutex<HashMap<String, Option<Arc<PatternHyphenator>>>> {
    static C: OnceLock<Mutex<HashMap<String, Option<Arc<PatternHyphenator>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The hyphenator for a language tag (cached; `None` when no dictionary exists).
pub fn for_lang(tag: &str) -> Option<Arc<PatternHyphenator>> {
    let key = tag.trim().to_ascii_lowercase();
    if let Some(h) = cache().lock().unwrap().get(&key) {
        return h.clone();
    }
    let h = dictionary_code(&key).and_then(|code| {
        Standard::from_embedded(code).ok().map(|dict| Arc::new(PatternHyphenator { lang: Language::new(code.code()), dict }))
    });
    cache().lock().unwrap().insert(key, h.clone());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_switch_dictionaries() {
        let en = for_lang("en").unwrap();
        assert_eq!(en.language().unwrap().0, "en-us");
        let br = en.hyphenate("hyphenation");
        assert!(!br.is_empty(), "{br:?}");
        let de = for_lang("de-DE").unwrap();
        assert_eq!(de.language().unwrap().0, "de-1996");
        let w = "Donaudampfschifffahrt";
        assert!(!de.hyphenate(w).is_empty());
        let da = for_lang("da").unwrap();
        assert_eq!(da.language().unwrap().0, "da");
        assert!(!da.hyphenate("arbejdsløshedsforsikring").is_empty());
        // Different languages give different break points for the same word.
        assert_ne!(for_lang("en-gb").unwrap().hyphenate("characteristically"), for_lang("fr").unwrap().hyphenate("characteristically"));
        assert!(for_lang("xx").is_none());
        assert!(en.hyphenate("abc").is_empty());
        assert!(Arc::ptr_eq(&en, &for_lang("EN").unwrap()));
    }
}
