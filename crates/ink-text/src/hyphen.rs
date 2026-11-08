//! Hyphenation.
//!
//! The layout engine asks a [`Hyphenator`] for break points inside a word.
//!
//! - [`PatternHyphenator`]: Knuth-Liang patterns from the `hyphenation` crate (all 82
//!   dictionaries embedded, +3.0 MB in the ARM binary), chosen by [`Language`] (BCP 47:
//!   `en`, `en-GB`, `da`, `de`, `fr`, ...). Soft hyphens (U+00AD) in a word take
//!   precedence over patterns, as in EPUBs that hyphenate themselves.
//! - [`NoHyphenation`]: never hyphenates.
//! - [`hyphenator_for`]: the right one for a language tag.

use hyphenation::{Hyphenator as _, Load, Standard};
use unicode_segmentation::UnicodeSegmentation;

/// A BCP 47 language tag as found in OPF `dc:language` or `lang` attributes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Language(pub String);

impl Language {
    /// Builds a tag (lowercased, `_` turned into `-`).
    pub fn new(tag: &str) -> Self {
        Language(tag.trim().replace('_', "-").to_ascii_lowercase())
    }

    /// The primary subtag (`en` for `en-US`).
    pub fn primary(&self) -> &str {
        self.0.split('-').next().unwrap_or("")
    }

    /// The pattern dictionary for this tag, if one exists.
    pub fn patterns(&self) -> Option<hyphenation::Language> {
        use hyphenation::Language as L;
        if let Some(l) = L::try_from_code(&self.0) {
            return Some(l);
        }
        let tag = self.0.as_str();
        Some(match self.primary() {
            "en" if tag.contains("gb") || tag.contains("uk") || tag.contains("au") || tag.contains("nz") => L::EnglishGB,
            "en" => L::EnglishUS,
            "de" if tag.contains("ch") => L::GermanSwiss,
            "de" if tag.contains("1901") => L::German1901,
            "de" => L::German1996,
            "el" => L::GreekMono,
            "no" | "nb" => L::NorwegianBokmal,
            "nn" => L::NorwegianNynorsk,
            "mn" => L::Mongolian,
            "sr" => L::SerbianCyrillic,
            "sh" => L::SerbocroatianLatin,
            "zh" => return None,
            p => return L::try_from_code(p),
        })
    }
}

/// Finds hyphenation points.
pub trait Hyphenator {
    /// Byte offsets inside `word` where a hyphen may be inserted, increasing, strictly
    /// between 0 and `word.len()`. Each is a char boundary and an extended grapheme
    /// boundary (never between a letter and its combining marks), so slicing `word` at
    /// any of them is safe.
    fn hyphenate(&self, word: &str) -> Vec<usize>;

    /// Language served.
    fn language(&self) -> Option<&Language> {
        None
    }
}

/// Never hyphenates.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHyphenation;

impl Hyphenator for NoHyphenation {
    fn hyphenate(&self, _word: &str) -> Vec<usize> {
        Vec::new()
    }
}

/// Break points after each soft hyphen (U+00AD) in `word` (UAX #14 breaks after it).
/// Layout draws no glyph for a soft hyphen and adds a visible hyphen at a line end.
pub fn soft_hyphens(word: &str) -> Vec<usize> {
    word.match_indices('\u{ad}').map(|(i, s)| i + s.len()).filter(|i| *i < word.len()).collect()
}

/// Knuth-Liang hyphenation from the `hyphenation` crate's embedded dictionaries.
#[derive(Debug)]
pub struct PatternHyphenator {
    dict: Standard,
    lang: Language,
    /// Words shorter than this many letters are not hyphenated (default 5).
    pub min_word: usize,
}

impl PatternHyphenator {
    /// The embedded dictionary for `lang`; `None` when there is none.
    pub fn new(lang: &Language) -> Option<Self> {
        let code = lang.patterns()?;
        match Standard::from_embedded(code) {
            Ok(dict) => Some(PatternHyphenator { dict, lang: lang.clone(), min_word: 5 }),
            Err(e) => {
                log::warn!("hyphenation dictionary {code:?}: {e}");
                None
            }
        }
    }
}

impl Hyphenator for PatternHyphenator {
    /// Points inside the alphabetic core of `word`: leading and trailing punctuation
    /// (quotes, commas) is skipped, words with digits or fewer than `min_word` letters
    /// are left whole, and soft hyphens win over patterns.
    fn hyphenate(&self, word: &str) -> Vec<usize> {
        let shy = soft_hyphens(word);
        if !shy.is_empty() {
            return shy;
        }
        let start = word.char_indices().find(|(_, c)| c.is_alphabetic()).map(|(i, _)| i);
        let Some(start) = start else { return Vec::new() };
        let end = word.char_indices().rev().find(|(_, c)| c.is_alphabetic()).map(|(i, c)| i + c.len_utf8()).unwrap_or(start);
        let core = &word[start..end];
        if core.chars().count() < self.min_word || core.chars().any(|c| c.is_numeric()) {
            return Vec::new();
        }
        let graphemes: Vec<usize> = core.grapheme_indices(true).map(|(i, _)| i).collect();
        let points: Vec<usize> = if core.contains('-') {
            // Explicit hyphens: break only after them (the dictionary would not).
            core.match_indices('-').map(|(i, _)| i + 1).collect()
        } else {
            // The dictionary works on lower case. The `hyphenation` crate's own folding
            // only corrects offsets for 'İ'; letters whose lower case has another byte
            // length (ẞ is 3 bytes, ß 2) would shift every later point. Fold here, keep a
            // byte map, and map each point back to the original word.
            let (lower, map) = fold_lowercase(core);
            let back = |b: usize| map.binary_search_by_key(&b, |(lo, _)| *lo).ok().map(|k| map[k].1);
            self.dict.opportunities(&lower).into_iter().filter_map(back).collect()
        };
        // Never between a letter and its combining marks (NFD text).
        points.into_iter().filter(|b| *b > 0 && *b < core.len() && graphemes.binary_search(b).is_ok()).map(|b| start + b).collect()
    }

    fn language(&self) -> Option<&Language> {
        Some(&self.lang)
    }
}

/// Lower case of `word` for dictionary lookup, and for every char of `word` the pair
/// (byte offset in the lower case, byte offset in `word`), plus the pair for the end.
/// 'İ' becomes plain "i" (as the `hyphenation` crate refolds it); a final capital sigma
/// becomes 'ς' (as `str::to_lowercase` does). A point inside the lower case of one char
/// (a char that lowers to several) has no pair and is dropped by the caller.
fn fold_lowercase(word: &str) -> (String, Vec<(usize, usize)>) {
    let mut lower = String::with_capacity(word.len() + 4);
    let mut map = Vec::with_capacity(word.len() + 1);
    let mut prev_letter = false;
    let mut chars = word.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        map.push((lower.len(), i));
        let next_letter = chars.peek().is_some_and(|(_, n)| n.is_alphabetic());
        match c {
            'İ' => lower.push('i'),
            'Σ' if prev_letter && !next_letter => lower.push('ς'),
            c => lower.extend(c.to_lowercase()),
        }
        prev_letter = c.is_alphabetic();
    }
    map.push((lower.len(), word.len()));
    (lower, map)
}

/// The hyphenator for a language tag: patterns when the language has them, else none.
pub fn hyphenator_for(tag: &str) -> Box<dyn Hyphenator + Send + Sync> {
    match PatternHyphenator::new(&Language::new(tag)) {
        Some(h) => Box::new(h),
        None => Box::new(NoHyphenation),
    }
}

/// Splits `word` at the hyphenation points `breaks` (as returned by [`Hyphenator`]).
pub fn split_at_points<'a>(word: &'a str, breaks: &[usize]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(breaks.len() + 1);
    let mut last = 0;
    for b in breaks {
        if *b > last && *b < word.len() && word.is_char_boundary(*b) {
            out.push(&word[last..*b]);
            last = *b;
        }
    }
    out.push(&word[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(h: &dyn Hyphenator, w: &str) -> String {
        split_at_points(w, &h.hyphenate(w)).join("-")
    }

    #[test]
    fn language_tags_and_no_hyphenation() {
        let l = Language::new("en_US");
        assert_eq!(l.0, "en-us");
        assert_eq!(l.primary(), "en");
        assert!(NoHyphenation.hyphenate("hyphenation").is_empty());
        assert!(NoHyphenation.language().is_none());
        assert_eq!(Language::new("en-GB").patterns(), Some(hyphenation::Language::EnglishGB));
        assert_eq!(Language::new("en").patterns(), Some(hyphenation::Language::EnglishUS));
        assert_eq!(Language::new("de-DE").patterns(), Some(hyphenation::Language::German1996));
        assert_eq!(Language::new("da-DK").patterns(), Some(hyphenation::Language::Danish));
        assert_eq!(Language::new("fr-CA").patterns(), Some(hyphenation::Language::French));
        assert_eq!(Language::new("nb").patterns(), Some(hyphenation::Language::NorwegianBokmal));
        assert_eq!(Language::new("zh-CN").patterns(), None);
        assert_eq!(Language::new("xx").patterns(), None);
    }

    #[test]
    fn english_patterns() {
        let h = PatternHyphenator::new(&Language::new("en-US")).unwrap();
        assert_eq!(pieces(&h, "hyphenation"), "hy-phen-a-tion");
        assert_eq!(pieces(&h, "Hyphenation,"), "Hy-phen-a-tion,");
        assert_eq!(pieces(&h, "\u{201c}extraordinary\u{201d}"), "\u{201c}ex-tra-or-di-nary\u{201d}");
        assert_eq!(pieces(&h, "cat"), "cat", "short words stay whole");
        assert_eq!(pieces(&h, "2024abc"), "2024abc");
        assert_eq!(pieces(&h, "well-known"), "well-known".replace("well-", "well--"));
        assert_eq!(h.language(), Some(&Language::new("en-us")));
    }

    #[test]
    fn other_languages() {
        let da = hyphenator_for("da");
        assert!(da.hyphenate("rødgrødmedfløde").len() >= 3, "{:?}", da.hyphenate("rødgrødmedfløde"));
        let de = hyphenator_for("de");
        assert!(!de.hyphenate("Silbentrennung").is_empty());
        let fr = hyphenator_for("fr");
        assert!(!fr.hyphenate("anticonstitutionnellement").is_empty());
        // Byte offsets stay on char boundaries with multi-byte letters.
        for b in da.hyphenate("rødgrødmedfløde") {
            assert!("rødgrødmedfløde".is_char_boundary(b));
        }
        assert!(hyphenator_for("zh").hyphenate("hyphenation").is_empty());
    }

    /// Points found on the lower-case copy land at the same letters of the original word,
    /// also when a capital's lower case has another byte length (ẞ 3 bytes, ß 2) and
    /// after 'İ' (2 bytes, folded to 1).
    #[test]
    fn points_map_back_through_case_folding() {
        let char_lens = |w: &str, h: &dyn Hyphenator| -> Vec<usize> { split_at_points(w, &h.hyphenate(w)).iter().map(|p| p.chars().count()).collect() };
        let de = hyphenator_for("de");
        let lower = "großstadtstraßenverkehr";
        assert_eq!(split_at_points(lower, &de.hyphenate(lower)).join("-"), "groß-stadt-stra-ßen-ver-kehr");
        for upper in ["GROẞSTADTSTRAẞENVERKEHR", "Großstadtstraßenverkehr", "GroẞStadtStraẞenVerkehr"] {
            assert_eq!(char_lens(upper, &*de), char_lens(lower, &*de), "{upper}");
        }
        let tr = hyphenator_for("tr");
        let (up, low) = ("İSTANBULLULAŞTIRAMADIKLARIMIZDAN", "istanbullulaştıramadıklarımızdan");
        assert!(!tr.hyphenate(low).is_empty());
        assert_eq!(char_lens(up, &*tr), char_lens(low, &*tr));
        // Points of ASCII and lower-case words are the dictionary's own.
        let en = PatternHyphenator::new(&Language::new("en")).unwrap();
        for w in ["hyphenation", "extraordinary", "Hyphenation", "HYPHENATION", "rødgrødmedfløde"] {
            assert_eq!(en.hyphenate(w), en.dict.hyphenate(w).breaks, "{w}");
        }
    }

    /// Decomposed accents (NFD) stay with their letter.
    #[test]
    fn points_never_split_a_letter_from_its_marks() {
        use unicode_segmentation::UnicodeSegmentation;
        let words = ["e\u{301}le\u{301}phant\u{301}ine\u{301}s", "re\u{301}e\u{301}valuation", "co\u{308}operation"];
        for lang in ["tr", "fr", "en", "de"] {
            let h = hyphenator_for(lang);
            for w in words {
                let g: Vec<usize> = w.grapheme_indices(true).map(|(i, _)| i).collect();
                for p in h.hyphenate(w) {
                    assert!(g.contains(&p), "{lang} {w:?}: {p}");
                }
            }
        }
    }

    #[test]
    fn soft_hyphens_win() {
        let h = PatternHyphenator::new(&Language::new("en")).unwrap();
        let w = "in\u{ad}con\u{ad}ceiv\u{ad}able";
        assert_eq!(h.hyphenate(w), vec![4, 9, 15]);
        assert_eq!(split_at_points(w, &h.hyphenate(w)), vec!["in\u{ad}", "con\u{ad}", "ceiv\u{ad}", "able"]);
        assert_eq!(soft_hyphens("plain"), Vec::<usize>::new());
    }
}
