//! The forms tried when a selected word is looked up.
//!
//! Order: the cleaned selection as it is, then lowercase; for a multi-word selection the
//! same for its first word; then the word without a possessive (`'s`, `s'`); then simple
//! English inflections of the lowercase word (`-s`, `-es`, `-ies`, `-ed`, `-ied`, `-ing`
//! with or without a final `e` and with a doubled consonant, `-er`/`-est`, `-ly`, `-ves`,
//! `-men`). Lookup takes the first form any dictionary knows.

/// Characters dropped everywhere: soft hyphen, zero-width space/joiners, BOM, word joiner.
fn invisible(c: char) -> bool {
    matches!(c, '\u{00ad}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}')
}

/// Cleans a selection: invisible characters removed, curly apostrophes made straight,
/// whitespace collapsed, leading and trailing punctuation removed.
pub fn clean(sel: &str) -> String {
    let s: String = sel.chars().filter(|c| !invisible(*c)).map(|c| if matches!(c, '\u{2019}' | '\u{2018}' | '\u{02bc}' | '`' | '\u{00b4}') { '\'' } else { c }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.trim_matches(|c: char| !c.is_alphanumeric()).to_string()
}

fn push(out: &mut Vec<String>, s: &str) {
    if !s.is_empty() && !out.iter().any(|o| o == s) {
        out.push(s.to_string());
    }
}

/// The forms to try for `sel`, most likely first, no duplicates. Empty when the
/// selection has no letters or digits.
pub fn candidates(sel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let phrase = clean(sel);
    if phrase.is_empty() {
        return out;
    }
    push(&mut out, &phrase);
    push(&mut out, &phrase.to_lowercase());
    let word = clean(phrase.split(' ').next().unwrap_or(""));
    push(&mut out, &word);
    let lower = word.to_lowercase();
    push(&mut out, &lower);
    let base = strip_possessive(&lower);
    push(&mut out, base);
    if base != lower {
        // "John's" -> "John" for names.
        push(&mut out, strip_possessive(&word));
    }
    for f in inflections(base) {
        push(&mut out, &f);
    }
    out
}

fn strip_possessive(w: &str) -> &str {
    if let Some(s) = w.strip_suffix("'s").or_else(|| w.strip_suffix("'S")) {
        return s;
    }
    if w.len() > 2 && (w.ends_with("s'") || w.ends_with("S'")) {
        return &w[..w.len() - 1];
    }
    w
}

fn vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// "stopp" -> "stop" when the stem ends in a doubled consonant (not "ll", "ss", "ff", "zz").
fn undouble(stem: &str) -> Option<String> {
    let c: Vec<char> = stem.chars().collect();
    let n = c.len();
    if n >= 3 && c[n - 1] == c[n - 2] && !vowel(c[n - 1]) && !matches!(c[n - 1], 'l' | 's' | 'f' | 'z') && c[n - 1].is_alphabetic() {
        Some(c[..n - 1].iter().collect())
    } else {
        None
    }
}

/// Base forms of an inflected lowercase English word, most likely first.
pub fn inflections(w: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let n = w.chars().count();
    if n < 3 || !w.chars().all(|c| c.is_alphabetic() || c == '-' || c == '\'') {
        return out;
    }
    let mut add = |s: String| {
        if s.chars().count() >= 2 && s != w && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(s) = w.strip_suffix("ies") {
        add(format!("{s}y"));
    }
    if let Some(s) = w.strip_suffix("ves") {
        add(format!("{s}f"));
        add(format!("{s}fe"));
    }
    if let Some(s) = w.strip_suffix("men") {
        add(format!("{s}man"));
    }
    if let Some(s) = w.strip_suffix("es") {
        if s.ends_with("sh") || s.ends_with("ch") || s.ends_with('x') || s.ends_with('s') || s.ends_with('z') || s.ends_with('o') {
            add(s.to_string());
        }
    }
    if let Some(s) = w.strip_suffix('s') {
        if !s.ends_with('s') && !s.ends_with('u') && !s.ends_with('i') {
            add(s.to_string());
        }
    }
    if let Some(s) = w.strip_suffix("ied") {
        add(format!("{s}y"));
    }
    if let Some(s) = w.strip_suffix("ed") {
        add(format!("{s}e"));
        add(s.to_string());
        if let Some(u) = undouble(s) {
            add(u);
        }
    }
    if let Some(s) = w.strip_suffix("ing") {
        if s.chars().count() >= 2 {
            add(s.to_string());
            add(format!("{s}e"));
            if let Some(u) = undouble(s) {
                add(u);
            }
            if let Some(y) = s.strip_suffix('y') {
                // "dying" -> "die"
                add(format!("{y}ie"));
            }
        }
    }
    for suf in ["est", "er"] {
        if let Some(s) = w.strip_suffix(suf) {
            if let Some(i) = s.strip_suffix('i') {
                add(format!("{i}y"));
            }
            add(s.to_string());
            add(format!("{s}e"));
            if let Some(u) = undouble(s) {
                add(u);
            }
        }
    }
    if let Some(s) = w.strip_suffix("ily") {
        add(format!("{s}y"));
    }
    if let Some(s) = w.strip_suffix("ly") {
        add(s.to_string());
        add(format!("{s}le"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_punctuation_and_invisibles() {
        assert_eq!(clean("  “Hello,”  "), "Hello");
        assert_eq!(clean("dog\u{2019}s."), "dog's");
        assert_eq!(clean("hy\u{00ad}phen\u{200b}ated"), "hyphenated");
        assert_eq!(clean("(well-known)"), "well-known");
        assert_eq!(clean("— !? —"), "");
        assert_eq!(clean("in\n  spite   of"), "in spite of");
    }

    #[test]
    fn candidate_order() {
        assert_eq!(candidates("Running,"), vec!["Running", "running", "runn", "runne", "run", "runnie"].into_iter().filter(|s| *s != "runnie").map(String::from).collect::<Vec<_>>());
        let c = candidates("“In spite of”");
        assert_eq!(&c[..4], &["In spite of", "in spite of", "In", "in"]);
        let c = candidates("Juliet’s");
        assert_eq!(&c[..4], &["Juliet's", "juliet's", "juliet", "Juliet"]);
        assert!(candidates("...").is_empty());
    }

    #[test]
    fn inflection_forms() {
        let has = |w: &str, base: &str| assert!(inflections(w).iter().any(|f| f == base), "{w} -> {base}: {:?}", inflections(w));
        has("cities", "city");
        has("boxes", "box");
        has("dogs", "dog");
        has("carried", "carry");
        has("walked", "walk");
        has("loved", "love");
        has("stopped", "stop");
        has("making", "make");
        has("walking", "walk");
        has("running", "run");
        has("dying", "die");
        has("bigger", "big");
        has("happiest", "happy");
        has("later", "late");
        has("quickly", "quick");
        has("happily", "happy");
        has("wolves", "wolf");
        has("knives", "knife");
        has("women", "woman");
        assert!(inflections("glass").iter().all(|f| f != "glas"));
        assert!(inflections("is").is_empty());
        assert!(inflections("x2y").is_empty());
    }
}
