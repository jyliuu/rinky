//! Google Translate's web endpoint: the request URL, the answer's parser, language codes.
//!
//! Everything here is pure (no network): the plugin sends the request and hands the body
//! to [`parse`]. The endpoint is the one KOReader uses,
//! `https://translate.googleapis.com/translate_a/single?client=gtx&sl=<from>&tl=<to>&dt=...&q=<text>`
//! (no key; `client=gtx` matters, other clients answer 403). The answer is a JSON array
//! whose slots depend on the `dt` parameters:
//!
//! - `[0]` (`dt=t`): segments `[translated, source, ...]`; a last segment with a null
//!   translation carries the romanization and is skipped.
//! - `[1]` (`dt=bd`): dictionary of a single word, `[[part of speech, [terms...], ...], ...]`.
//! - `[2]`: the source language (the detected one with `sl=auto`).
//! - `[5]` (`dt=at`): alternatives per segment, `[[source, _, [[text, ...], ...], ...], ...]`.
//! - `[12]` (`dt=md`): definitions of the source word, `[[part of speech, [[gloss, ...], ...]], ...]`.
//!
//! Every slot but `[0]` may be null or missing.

use ink_net::url::encode_component;
use ink_plugin::serde_json::{self, Value};

/// The endpoint.
pub const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";
/// Characters of the selection sent at most (a GET request; Google refuses very long URLs).
pub const MAX_CHARS: usize = 1500;

/// A parsed answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Translation {
    /// The translated text (segments joined).
    pub text: String,
    /// The source language Google used (detected with `auto`).
    pub source_lang: Option<String>,
    /// Dictionary of a single word: part of speech and terms.
    pub dictionary: Vec<(String, Vec<String>)>,
    /// Other translations of the whole text (only when it is one segment).
    pub alternatives: Vec<String>,
    /// Definitions of the source word, in the source language: part of speech and glosses.
    pub definitions: Vec<(String, Vec<String>)>,
}

/// The request URL translating `text` from `from` (`auto` or a code) into `to`.
pub fn url(from: &str, to: &str, text: &str) -> String {
    format!(
        "{ENDPOINT}?client=gtx&ie=UTF-8&oe=UTF-8&sl={}&tl={}&hl=en&otf=1&ssel=0&tsel=0&dt=t&dt=bd&dt=at&dt=md&q={}",
        encode_component(from),
        encode_component(to),
        encode_component(text)
    )
}

/// The selection as text to translate: soft hyphens removed, white space collapsed, cut
/// at [`MAX_CHARS`] on a word boundary. The flag says whether it was cut.
pub fn clean_text(sel: &str) -> (String, bool) {
    let s = ink_plugin::kit::squeeze(&sel.replace('\u{00AD}', ""));
    if s.chars().count() <= MAX_CHARS {
        return (s, false);
    }
    let cut = ink_plugin::kit::cut_at(&s, MAX_CHARS);
    (cut.trim_end_matches('…').to_string(), true)
}

fn str_at(v: &Value, i: usize) -> Option<&str> {
    v.get(i).and_then(Value::as_str)
}

fn strings(v: Option<&Value>, pick: impl Fn(&Value) -> Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in v.and_then(Value::as_array).into_iter().flatten() {
        if let Some(s) = pick(x).map(str::trim).filter(|s| !s.is_empty()) {
            if !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// Parses an answer. An answer without a translation is an error.
pub fn parse(body: &[u8]) -> Result<Translation, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
    let segments = v.get(0).and_then(Value::as_array).ok_or("no translation in the answer")?;
    let text: String = segments.iter().filter_map(|s| str_at(s, 0)).collect();
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("empty translation".into());
    }
    let source_lang = str_at(&v, 2).map(str::to_string).filter(|s| !s.is_empty());
    let dictionary = v
        .get(1)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let terms = strings(e.get(1), Value::as_str);
            (!terms.is_empty()).then(|| (str_at(e, 0).unwrap_or("").to_string(), terms))
        })
        .collect();
    // Alternatives of the whole text exist only when it is one segment.
    let alts = v.get(5).and_then(Value::as_array).filter(|a| a.len() == 1);
    let alternatives = strings(alts.and_then(|a| a[0].get(2)), |x| str_at(x, 0)).into_iter().filter(|a| *a != text).collect();
    let definitions = v
        .get(12)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let glosses = strings(e.get(1), |g| str_at(g, 0));
            (!glosses.is_empty()).then(|| (str_at(e, 0).unwrap_or("").to_string(), glosses))
        })
        .collect();
    Ok(Translation { text, source_lang, dictionary, alternatives, definitions })
}

/// Whether `s` looks like a language code Google takes: `de`, `haw`, `zh-CN`, `zh-TW`.
pub fn valid_code(s: &str) -> bool {
    let (main, region) = match s.split_once('-') {
        Some((m, r)) => (m, Some(r)),
        None => (s, None),
    };
    (2..=3).contains(&main.len())
        && main.bytes().all(|b| b.is_ascii_lowercase())
        && region.is_none_or(|r| (2..=4).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// A language code as typed (`DE`, ` zh-cn `) in Google's form (`de`, `zh-CN`).
pub fn normalize_code(s: &str) -> String {
    let s = s.trim();
    match s.split_once('-') {
        Some((m, r)) => format!("{}-{}", m.to_lowercase(), r.to_uppercase()),
        None => s.to_lowercase(),
    }
}

/// The book's language (`en-US`, `de`, `zh-Hant`) as a code Google takes; `None` when
/// it is missing or not a two-letter code (`und`, `eng`).
pub fn book_code(lang: &str) -> Option<String> {
    let l = lang.trim().replace('_', "-");
    let (main, rest) = l.split_once('-').unwrap_or((&l, ""));
    let main = main.to_lowercase();
    if main.len() != 2 || !main.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    if main == "zh" {
        let r = rest.to_lowercase();
        return Some(if r.contains("tw") || r.contains("hk") || r.contains("hant") { "zh-TW" } else { "zh-CN" }.into());
    }
    Some(main)
}

/// Languages offered in the settings pickers: code and English name, by name.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arabic"),
    ("bg", "Bulgarian"),
    ("ca", "Catalan"),
    ("zh-CN", "Chinese (Simplified)"),
    ("zh-TW", "Chinese (Traditional)"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("en", "English"),
    ("et", "Estonian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("de", "German"),
    ("el", "Greek"),
    ("iw", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("is", "Icelandic"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("la", "Latin"),
    ("lv", "Latvian"),
    ("lt", "Lithuanian"),
    ("no", "Norwegian"),
    ("fa", "Persian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("es", "Spanish"),
    ("sv", "Swedish"),
    ("th", "Thai"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("vi", "Vietnamese"),
];

/// The English name of `code`, else the code itself.
pub fn name(code: &str) -> String {
    let c = match code {
        "he" => "iw",
        "zh" => "zh-CN",
        c => c,
    };
    LANGUAGES.iter().find(|(k, _)| k.eq_ignore_ascii_case(c)).map(|(_, n)| n.to_string()).unwrap_or_else(|| code.to_string())
}

/// Whether two codes name the same language (`zh-CN` and `zh`, `iw` and `he`).
pub fn same_lang(a: &str, b: &str) -> bool {
    let main = |s: &str| match s.split('-').next().unwrap_or(s).to_lowercase().as_str() {
        "he" => "iw".to_string(),
        m => m.to_string(),
    };
    a.eq_ignore_ascii_case(b) || (!a.contains('-') || !b.contains('-')) && main(a) == main(b)
}
