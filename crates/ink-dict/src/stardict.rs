//! One StarDict dictionary.
//!
//! Files (same stem, any folder): `NAME.ifo` (text header), `NAME.idx` or `NAME.idx.gz`
//! (entries: UTF-8 word, NUL, offset u32 BE or u64 BE with `idxoffsetbits=64`, size u32
//! BE), `NAME.dict` or `NAME.dict.dz` (definition data), optional `NAME.syn` (word, NUL,
//! u32 BE index into the idx entries). Words in `.idx` and `.syn` are sorted by
//! [`stardict_cmp`]: ASCII case-insensitive, ties broken by bytes.

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::dictzip::DictData;
use crate::{io_err, DictError, Result};

/// Largest `.idx`/`.syn` file read into memory.
const MAX_INDEX: u64 = 512 << 20;

/// `g_ascii_strcasecmp`: bytes compared after ASCII lowercasing.
pub fn ascii_casecmp(a: &[u8], b: &[u8]) -> Ordering {
    a.iter().map(u8::to_ascii_lowercase).cmp(b.iter().map(u8::to_ascii_lowercase))
}

/// StarDict's sort order (`stardict_strcmp`): [`ascii_casecmp`], then plain bytes.
pub fn stardict_cmp(a: &[u8], b: &[u8]) -> Ordering {
    ascii_casecmp(a, b).then_with(|| a.cmp(b))
}

/// The parsed `.ifo` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IfoInfo {
    /// `version` (2.4.2 or 3.0.0).
    pub version: String,
    /// `bookname`: the dictionary's display name.
    pub bookname: String,
    /// `wordcount`.
    pub wordcount: usize,
    /// `synwordcount` (0 without `.syn`).
    pub synwordcount: usize,
    /// `idxoffsetbits` (32 or 64).
    pub idxoffsetbits: u32,
    /// `sametypesequence`, empty when every field carries its type.
    pub sametypesequence: String,
    /// `author`, `description` and other keys are not needed for lookup.
    pub description: String,
}

impl IfoInfo {
    /// Parses the text of an `.ifo` file.
    pub fn parse(text: &str) -> Result<IfoInfo> {
        let mut lines = text.lines();
        let magic = lines.next().unwrap_or("").trim_start_matches('\u{feff}').trim();
        if magic != "StarDict's dict ifo file" {
            return Err(DictError::Format(format!("not a StarDict .ifo (first line {magic:?})")));
        }
        let mut info = IfoInfo { idxoffsetbits: 32, ..IfoInfo::default() };
        for l in lines {
            let Some((k, v)) = l.split_once('=') else { continue };
            let v = v.trim_end_matches('\r');
            match k.trim() {
                "version" => info.version = v.to_string(),
                "bookname" => info.bookname = v.to_string(),
                "wordcount" => info.wordcount = v.trim().parse().unwrap_or(0),
                "synwordcount" => info.synwordcount = v.trim().parse().unwrap_or(0),
                "idxoffsetbits" => info.idxoffsetbits = v.trim().parse().unwrap_or(32),
                "sametypesequence" => info.sametypesequence = v.trim().to_string(),
                "description" => info.description = v.to_string(),
                _ => {}
            }
        }
        if info.idxoffsetbits != 32 && info.idxoffsetbits != 64 {
            return Err(DictError::Format(format!("idxoffsetbits={} (32 or 64)", info.idxoffsetbits)));
        }
        Ok(info)
    }
}

/// One field of an entry: its StarDict type char and its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Type: `m` plain text, `h` HTML, `x` XDXF, `g` Pango, `t` phonetic, `W`/`P` binary...
    pub kind: char,
    /// Raw data (UTF-8 for the lowercase types).
    pub data: Vec<u8>,
}

impl Field {
    /// The field as plain text, `None` for binary or resource fields.
    pub fn text(&self) -> Option<String> {
        let s = String::from_utf8_lossy(&self.data);
        match self.kind {
            'm' | 'l' | 'y' => Some(s.trim().to_string()),
            't' => Some(format!("/{}/", s.trim())),
            'h' => Some(crate::text::html_to_text(&s)),
            'x' | 'g' | 'k' => Some(crate::text::markup_to_text(&s)),
            _ => None,
        }
    }

    /// The field as Markdown with the dictionary's `style` ([`crate::markdown`]), `None`
    /// for binary or resource fields.
    pub fn markdown(&self, style: &crate::markdown::Style) -> Option<String> {
        match self.kind {
            'h' | 'x' | 'g' | 'k' => Some(crate::markdown::to_markdown(self.kind, &String::from_utf8_lossy(&self.data), style)),
            _ => self.text().map(|t| crate::markdown::plain(&t)),
        }
    }
}

/// An index file (`.idx` or `.syn`) in memory: records of a NUL-terminated word and a
/// fixed-size tail, sorted by [`stardict_cmp`]. Beside the file's bytes only the start
/// of each record is kept: 4 bytes per word, not a decoded copy of every record.
#[derive(Default)]
struct Index {
    buf: Vec<u8>,
    starts: Vec<u32>,
    /// Bytes after each word's NUL.
    tail: usize,
}

impl Index {
    /// Splits `buf` into records with `tail` bytes after each word's NUL. `what` names a
    /// record in errors.
    fn parse(buf: Vec<u8>, tail: usize, what: &str) -> std::result::Result<Index, String> {
        if buf.len() > u32::MAX as usize {
            return Err(format!("{what} file too big"));
        }
        let mut starts = Vec::new();
        let mut p = 0usize;
        while p < buf.len() {
            let nul = buf[p..].iter().position(|b| *b == 0).ok_or_else(|| format!("{what} at byte {p} has no NUL"))?;
            let end = p + nul + 1 + tail;
            if end > buf.len() {
                return Err(format!("{what} at byte {p} is cut off"));
            }
            starts.push(p as u32);
            p = end;
        }
        Ok(Index { buf, starts, tail })
    }

    fn len(&self) -> usize {
        self.starts.len()
    }

    /// Record `i`: its word and its tail. Records are contiguous, so a record ends
    /// where the next one starts.
    fn record(&self, i: usize) -> (&[u8], &[u8]) {
        let end = self.starts.get(i + 1).map_or(self.buf.len(), |&s| s as usize);
        let r = &self.buf[self.starts[i] as usize..end];
        let (word, tail) = r.split_at(r.len() - self.tail);
        (&word[..word.len() - 1], tail)
    }

    fn word(&self, i: usize) -> &[u8] {
        self.record(i).0
    }

    /// The records whose word equals `w` ignoring ASCII case, in order.
    fn matching<'a>(&'a self, w: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if ascii_casecmp(self.word(mid), w) == Ordering::Less {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo..self.len()).take_while(move |&i| ascii_casecmp(self.word(i), w) == Ordering::Equal)
    }
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().expect("4 bytes"))
}

/// An open StarDict dictionary.
pub struct Dictionary {
    /// Folder or file stem used as the short name (the `<name>` folder in the library).
    pub name: String,
    /// The `.ifo` header.
    pub info: IfoInfo,
    ifo: PathBuf,
    /// Headwords; each tail is the offset (u32 or u64 BE) and size (u32 BE) in `dict`.
    idx: Index,
    /// Synonyms; each tail is the u32 BE number of an entry of `idx`.
    syn: Index,
    dict: DictData,
    /// The dictionary's style sheet (`<stem>.css` next to the `.ifo`), for the full view.
    pub style: crate::markdown::Style,
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary").field("name", &self.name).field("ifo", &self.ifo).field("words", &self.idx.len()).field("synonyms", &self.syn.len()).finish()
    }
}

fn read_limited(path: &Path, gz: bool) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path).map_err(|e| io_err(path, e))?;
    let mut out = Vec::new();
    if gz {
        flate2::read::MultiGzDecoder::new(f).take(MAX_INDEX + 1).read_to_end(&mut out).map_err(|e| io_err(path, e))?;
    } else {
        f.take(MAX_INDEX + 1).read_to_end(&mut out).map_err(|e| io_err(path, e))?;
    }
    if out.len() as u64 > MAX_INDEX {
        return Err(DictError::Format(format!("{}: index over {} MiB", path.display(), MAX_INDEX >> 20)));
    }
    Ok(out)
}

fn with_ext(stem: &Path, ext: &str) -> PathBuf {
    let mut s = stem.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

fn format_err(path: &Path, e: impl std::fmt::Display) -> DictError {
    DictError::Format(format!("{}: {e}", path.display()))
}

impl Dictionary {
    /// Opens the dictionary whose header is `ifo` (`.../NAME.ifo`).
    pub fn open(ifo: &Path) -> Result<Dictionary> {
        let text = std::fs::read(ifo).map_err(|e| io_err(ifo, e))?;
        let info = IfoInfo::parse(&String::from_utf8_lossy(&text)).map_err(|e| format_err(ifo, e))?;
        let stem = ifo.with_extension("");
        let name = stem.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (idx_path, gz) = [(with_ext(&stem, ".idx"), false), (with_ext(&stem, ".idx.gz"), true)].into_iter().find(|(p, _)| p.is_file()).ok_or_else(|| format_err(ifo, "no .idx or .idx.gz next to it"))?;
        let tail = if info.idxoffsetbits == 64 { 12 } else { 8 };
        let idx = Index::parse(read_limited(&idx_path, gz)?, tail, "entry").map_err(|e| format_err(&idx_path, e))?;
        let dict_path = [with_ext(&stem, ".dict"), with_ext(&stem, ".dict.dz")].into_iter().find(|p| p.is_file()).ok_or_else(|| format_err(ifo, "no .dict or .dict.dz next to it"))?;
        let dict = DictData::open(&dict_path)?;
        let syn_path = with_ext(&stem, ".syn");
        let syn = if syn_path.is_file() {
            Index::parse(read_limited(&syn_path, false)?, 4, "synonym").map_err(|e| format_err(&syn_path, e))?
        } else {
            Index::default()
        };
        let style = std::fs::read_to_string(stem.with_extension("css")).map(|css| crate::markdown::Style::parse(&css)).unwrap_or_default();
        Ok(Dictionary { name, info, ifo: ifo.to_path_buf(), idx, syn, dict, style })
    }

    /// Display name: `bookname`, or the file stem when it is empty.
    pub fn title(&self) -> &str {
        if self.info.bookname.trim().is_empty() {
            &self.name
        } else {
            self.info.bookname.trim()
        }
    }

    /// Number of headwords.
    pub fn len(&self) -> usize {
        self.idx.len()
    }

    /// True without headwords.
    pub fn is_empty(&self) -> bool {
        self.idx.len() == 0
    }

    /// Headword of entry `i`.
    pub fn headword(&self, i: usize) -> &str {
        std::str::from_utf8(self.idx.word(i)).unwrap_or("")
    }

    /// Entries whose headword or synonym equals `word` ignoring ASCII case. Entries that
    /// match byte for byte come first, then the others in index order; no duplicates.
    pub fn find(&self, word: &str) -> Vec<usize> {
        let w = word.as_bytes();
        let words = self.idx.matching(w).map(|i| (i, self.idx.word(i) == w));
        let syns = self.syn.matching(w).filter_map(|j| {
            let (syn, tail) = self.syn.record(j);
            // A synonym pointing past the index is skipped, not fatal.
            Some(be32(tail) as usize).filter(|&i| i < self.idx.len()).map(|i| (i, syn == w))
        });
        // An entry's first match decides whether it counts as exact.
        let mut found: Vec<(usize, bool)> = Vec::new();
        for (i, same) in words.chain(syns) {
            if !found.iter().any(|(j, _)| *j == i) {
                found.push((i, same));
            }
        }
        let (exact, other): (Vec<_>, Vec<_>) = found.into_iter().partition(|(_, same)| *same);
        exact.into_iter().chain(other).map(|(i, _)| i).collect()
    }

    /// The fields of entry `i`.
    pub fn fields(&self, i: usize) -> Result<Vec<Field>> {
        let t = self.idx.record(i).1;
        let (off, size) = if self.info.idxoffsetbits == 64 { (u64::from_be_bytes(t[..8].try_into().expect("8 bytes")), be32(&t[8..])) } else { (be32(t) as u64, be32(&t[4..])) };
        let data = self.dict.read(off, size)?;
        Ok(split_fields(&data, &self.info.sametypesequence))
    }

    /// Entry `i` as plain text: the text fields joined by blank lines.
    pub fn definition(&self, i: usize) -> Result<String> {
        Ok(join_text(&self.fields(i)?))
    }
}

/// The plain text of `fields` joined by blank lines.
pub(crate) fn join_text(fields: &[Field]) -> String {
    fields.iter().filter_map(Field::text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
}

/// Splits entry data into fields. With a `sametypesequence` the types are not stored and
/// the last field runs to the end; without one, each field starts with its type char.
/// Lowercase types end at a NUL; uppercase types start with a u32 BE size.
fn split_fields(data: &[u8], seq: &str) -> Vec<Field> {
    let mut out = Vec::new();
    let mut p = 0usize;
    if !seq.is_empty() {
        let last = seq.chars().count() - 1;
        for (k, kind) in seq.chars().enumerate() {
            if k == last {
                // The rest; a NUL or size prefix written anyway is dropped.
                let d = &data[p..];
                let d = if kind.is_ascii_lowercase() { d.strip_suffix(&[0]).unwrap_or(d) } else if d.len() >= 4 && be32(d) as usize == d.len() - 4 { &d[4..] } else { d };
                out.push(Field { kind, data: d.to_vec() });
                break;
            }
            let Some((f, next)) = take_field(data, p, kind) else { break };
            out.push(Field { kind, data: f.to_vec() });
            p = next;
        }
        return out;
    }
    while p < data.len() {
        let kind = data[p] as char;
        p += 1;
        match take_field(data, p, kind) {
            Some((f, next)) => {
                out.push(Field { kind, data: f.to_vec() });
                p = next;
            }
            None => {
                // A last field without its NUL: take the rest.
                if kind.is_ascii_lowercase() {
                    out.push(Field { kind, data: data[p.min(data.len())..].to_vec() });
                }
                break;
            }
        }
    }
    out
}

fn take_field(data: &[u8], p: usize, kind: char) -> Option<(&[u8], usize)> {
    if kind.is_ascii_lowercase() {
        let rest = data.get(p..)?;
        let n = rest.iter().position(|b| *b == 0)?;
        Some((&rest[..n], p + n + 1))
    } else {
        let s = u32::from_be_bytes(data.get(p..p + 4)?.try_into().ok()?) as usize;
        let d = data.get(p + 4..p + 4 + s)?;
        Some((d, p + 4 + s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_order_matches_stardict() {
        let mut v: Vec<&[u8]> = vec![b"apple", b"Apple", b"APPLE", b"a", b"B", b"b", b"_x", b"Zed"];
        v.sort_by(|a, b| stardict_cmp(a, b));
        assert_eq!(v, vec![&b"_x"[..], b"a", b"APPLE", b"Apple", b"apple", b"B", b"b", b"Zed"]);
    }

    #[test]
    fn ifo_parse() {
        let i = IfoInfo::parse("StarDict's dict ifo file\nversion=3.0.0\nbookname=Test = Dict\nwordcount=2\nidxoffsetbits=64\nsametypesequence=h\n").unwrap();
        assert_eq!(i.bookname, "Test = Dict");
        assert_eq!(i.idxoffsetbits, 64);
        assert_eq!(i.sametypesequence, "h");
        assert!(IfoInfo::parse("hello\n").is_err());
        assert!(IfoInfo::parse("StarDict's dict ifo file\nidxoffsetbits=16\n").is_err());
    }

    #[test]
    fn fields_with_and_without_sequence() {
        assert_eq!(split_fields(b"hello", "m"), vec![Field { kind: 'm', data: b"hello".to_vec() }]);
        assert_eq!(split_fields(b"ab\0cd", "tm"), vec![Field { kind: 't', data: b"ab".to_vec() }, Field { kind: 'm', data: b"cd".to_vec() }]);
        let mut d = b"mone\0W".to_vec();
        d.extend_from_slice(&3u32.to_be_bytes());
        d.extend_from_slice(b"xyzhtwo\0");
        let f = split_fields(&d, "");
        assert_eq!(f.len(), 3);
        assert_eq!((f[0].kind, &f[0].data[..]), ('m', &b"one"[..]));
        assert_eq!((f[1].kind, &f[1].data[..]), ('W', &b"xyz"[..]));
        assert_eq!((f[2].kind, &f[2].data[..]), ('h', &b"two"[..]));
        // Cut-off data does not panic.
        assert!(split_fields(b"W\0\0\0\x09ab", "").is_empty());
        let _ = split_fields(b"", "tm");
    }

    #[test]
    fn broken_index_is_an_error() {
        let parse = |b: &[u8]| Index::parse(b.to_vec(), 8, "entry");
        assert!(parse(b"word").is_err());
        assert!(parse(b"word\0\0\0").is_err());
        assert_eq!(parse(b"").unwrap().len(), 0);
    }
}
