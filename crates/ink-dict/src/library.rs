//! All dictionaries in one directory: `<dir>/<name>/*.ifo`.

use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::normalize::candidates;
use crate::stardict::Dictionary;

/// One definition found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Dictionary display name (`bookname`).
    pub dict: String,
    /// The headword as stored in the dictionary.
    pub headword: String,
    /// Definition as plain text (lines separated by `\n`).
    pub text: String,
    /// The definition parsed into senses ([`crate::entry::parse`]) when its markup is a
    /// known one.
    pub entry: Option<Entry>,
    /// The definition as Markdown with the dictionary's formatting
    /// ([`crate::markdown`]).
    pub markdown: String,
}

/// Result of [`Library::lookup`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LookupResult {
    /// The cleaned selection ([`crate::normalize::clean`]).
    pub query: String,
    /// The form that was found (empty when nothing was found).
    pub form: String,
    /// Definitions, dictionaries in library order.
    pub hits: Vec<Hit>,
}

/// The dictionaries of one directory, in folder-name order.
#[derive(Debug, Default)]
pub struct Library {
    /// Directory scanned.
    pub dir: PathBuf,
    /// Opened dictionaries.
    pub dicts: Vec<Dictionary>,
    /// Dictionaries that failed to open: (path, error).
    pub errors: Vec<(PathBuf, String)>,
}

impl Library {
    /// Opens every `*.ifo` found in `dir/*/` (and in `dir` itself). A missing directory
    /// gives an empty library; broken dictionaries are listed in `errors`.
    pub fn open_dir(dir: &Path) -> Library {
        let mut lib = Library { dir: dir.to_path_buf(), ..Library::default() };
        let mut folders = vec![dir.to_path_buf()];
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut subs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
            subs.sort();
            folders.extend(subs);
        }
        for f in folders {
            let Ok(rd) = std::fs::read_dir(&f) else { continue };
            let mut ifos: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ifo") && p.is_file()).collect();
            ifos.sort();
            for ifo in ifos {
                match Dictionary::open(&ifo) {
                    Ok(d) => lib.dicts.push(d),
                    Err(e) => lib.errors.push((ifo, e.to_string())),
                }
            }
        }
        lib
    }

    /// Looks up `sel`. Each dictionary takes the first form of [`candidates`] it has
    /// (so a form one dictionary lists, such as a plural, does not hide another
    /// dictionary's entry for the base form); hits keep library order, each dictionary's
    /// entries exact case first. `form` is the form of the first hit.
    pub fn lookup(&self, sel: &str) -> LookupResult {
        let mut res = LookupResult { query: crate::normalize::clean(sel), ..LookupResult::default() };
        let forms = candidates(sel);
        for d in &self.dicts {
            for form in &forms {
                let hits = Self::dict_hits(d, form);
                if !hits.is_empty() {
                    if res.form.is_empty() {
                        res.form = form.clone();
                    }
                    res.hits.extend(hits);
                    break;
                }
            }
        }
        res
    }

    fn dict_hits(d: &Dictionary, word: &str) -> Vec<Hit> {
        d.find(word)
            .into_iter()
            .map(|i| {
                let (text, entry, markdown) = match d.fields(i) {
                    Ok(fields) => {
                        let text = fields.iter().filter_map(|f| f.text()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
                        let entry = fields.iter().find_map(|f| crate::entry::parse(f.kind, &String::from_utf8_lossy(&f.data)));
                        let markdown = fields.iter().filter_map(|f| f.markdown(&d.style)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
                        (text, entry, markdown)
                    }
                    Err(e) => {
                        let text = format!("(cannot read this entry: {e})");
                        let markdown = crate::markdown::plain(&text);
                        (text, None, markdown)
                    }
                };
                Hit { dict: d.title().to_string(), headword: d.headword(i).to_string(), text, entry, markdown }
            })
            .collect()
    }

    /// Entries for exactly `word` (ASCII case-insensitive, synonyms included).
    pub fn lookup_exact(&self, word: &str) -> Vec<Hit> {
        self.dicts.iter().flat_map(|d| Self::dict_hits(d, word)).collect()
    }
}
