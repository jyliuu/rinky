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

/// An open StarDict dictionary.
pub struct Dictionary {
    /// Folder or file stem used as the short name (the `<name>` folder in the library).
    pub name: String,
    /// The `.ifo` header.
    pub info: IfoInfo,
    ifo: PathBuf,
    idx: Vec<u8>,
    /// Per entry: (start of the word in `idx`, word length, offset, size).
    entries: Vec<(u32, u32, u64, u32)>,
    syn: Vec<u8>,
    /// Per synonym: (start, length, entry index).
    syns: Vec<(u32, u32, u32)>,
    dict: DictData,
    /// The dictionary's style sheet (`<stem>.css` next to the `.ifo`), for the full view.
    pub style: crate::markdown::Style,
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary").field("name", &self.name).field("ifo", &self.ifo).field("words", &self.entries.len()).field("synonyms", &self.syns.len()).finish()
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

impl Dictionary {
    /// Opens the dictionary whose header is `ifo` (`.../NAME.ifo`).
    pub fn open(ifo: &Path) -> Result<Dictionary> {
        let text = std::fs::read(ifo).map_err(|e| io_err(ifo, e))?;
        let info = IfoInfo::parse(&String::from_utf8_lossy(&text)).map_err(|e| DictError::Format(format!("{}: {e}", ifo.display())))?;
        let stem = ifo.with_extension("");
        let name = stem.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (idx_path, gz) = [(with_ext(&stem, ".idx"), false), (with_ext(&stem, ".idx.gz"), true)].into_iter().find(|(p, _)| p.is_file()).ok_or_else(|| DictError::Format(format!("{}: no .idx or .idx.gz next to it", ifo.display())))?;
        let idx = read_limited(&idx_path, gz)?;
        let entries = parse_idx(&idx, info.idxoffsetbits).map_err(|e| DictError::Format(format!("{}: {e}", idx_path.display())))?;
        let dict_path = [with_ext(&stem, ".dict"), with_ext(&stem, ".dict.dz")].into_iter().find(|p| p.is_file()).ok_or_else(|| DictError::Format(format!("{}: no .dict or .dict.dz next to it", ifo.display())))?;
        let dict = DictData::open(&dict_path)?;
        let syn_path = with_ext(&stem, ".syn");
        let (syn, syns) = if syn_path.is_file() {
            let syn = read_limited(&syn_path, false)?;
            let syns = parse_syn(&syn, entries.len()).map_err(|e| DictError::Format(format!("{}: {e}", syn_path.display())))?;
            (syn, syns)
        } else {
            (Vec::new(), Vec::new())
        };
        let style = std::fs::read_to_string(stem.with_extension("css")).map(|css| crate::markdown::Style::parse(&css)).unwrap_or_default();
        Ok(Dictionary { name, info, ifo: ifo.to_path_buf(), idx, entries, syn, syns, dict, style })
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
        self.entries.len()
    }

    /// True without headwords.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Headword of entry `i`.
    pub fn headword(&self, i: usize) -> &str {
        let (s, l, _, _) = self.entries[i];
        std::str::from_utf8(&self.idx[s as usize..(s + l) as usize]).unwrap_or("")
    }

    fn word_bytes(&self, i: usize) -> &[u8] {
        let (s, l, _, _) = self.entries[i];
        &self.idx[s as usize..(s + l) as usize]
    }

    fn syn_bytes(&self, i: usize) -> &[u8] {
        let (s, l, _) = self.syns[i];
        &self.syn[s as usize..(s + l) as usize]
    }

    /// Entries whose headword or synonym equals `word` ignoring ASCII case. Entries that
    /// match byte for byte come first, then the others in index order; no duplicates.
    pub fn find(&self, word: &str) -> Vec<usize> {
        let w = word.as_bytes();
        let mut exact = Vec::new();
        let mut other = Vec::new();
        let mut add = |i: usize, same: bool| {
            if !exact.contains(&i) && !other.contains(&i) {
                if same {
                    exact.push(i)
                } else {
                    other.push(i)
                }
            }
        };
        let mut i = lower_bound(self.entries.len(), |i| ascii_casecmp(self.word_bytes(i), w) == Ordering::Less);
        while i < self.entries.len() && ascii_casecmp(self.word_bytes(i), w) == Ordering::Equal {
            add(i, self.word_bytes(i) == w);
            i += 1;
        }
        let mut j = lower_bound(self.syns.len(), |j| ascii_casecmp(self.syn_bytes(j), w) == Ordering::Less);
        while j < self.syns.len() && ascii_casecmp(self.syn_bytes(j), w) == Ordering::Equal {
            add(self.syns[j].2 as usize, self.syn_bytes(j) == w);
            j += 1;
        }
        exact.extend(other);
        exact
    }

    /// The fields of entry `i`.
    pub fn fields(&self, i: usize) -> Result<Vec<Field>> {
        let (_, _, off, size) = self.entries[i];
        let data = self.dict.read(off, size)?;
        Ok(split_fields(&data, &self.info.sametypesequence))
    }

    /// Entry `i` as plain text: the text fields joined by blank lines.
    pub fn definition(&self, i: usize) -> Result<String> {
        let parts: Vec<String> = self.fields(i)?.iter().filter_map(Field::text).filter(|s| !s.is_empty()).collect();
        Ok(parts.join("\n\n"))
    }
}

/// First index in `0..n` for which `less` is false (`less` must be true then false).
fn lower_bound(n: usize, less: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if less(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn parse_idx(idx: &[u8], bits: u32) -> std::result::Result<Vec<(u32, u32, u64, u32)>, String> {
    if idx.len() > u32::MAX as usize {
        return Err("index too big".into());
    }
    let tail = if bits == 64 { 12 } else { 8 };
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < idx.len() {
        let nul = idx[p..].iter().position(|b| *b == 0).ok_or_else(|| format!("word at byte {p} has no NUL"))?;
        let w = p + nul + 1;
        let t = idx.get(w..w + tail).ok_or_else(|| format!("entry at byte {p} is cut off"))?;
        let (off, size) = if bits == 64 {
            (u64::from_be_bytes(t[..8].try_into().unwrap()), u32::from_be_bytes(t[8..12].try_into().unwrap()))
        } else {
            (u32::from_be_bytes(t[..4].try_into().unwrap()) as u64, u32::from_be_bytes(t[4..8].try_into().unwrap()))
        };
        out.push((p as u32, nul as u32, off, size));
        p = w + tail;
    }
    Ok(out)
}

fn parse_syn(syn: &[u8], words: usize) -> std::result::Result<Vec<(u32, u32, u32)>, String> {
    if syn.len() > u32::MAX as usize {
        return Err("synonyms too big".into());
    }
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < syn.len() {
        let nul = syn[p..].iter().position(|b| *b == 0).ok_or_else(|| format!("synonym at byte {p} has no NUL"))?;
        let w = p + nul + 1;
        let t = syn.get(w..w + 4).ok_or_else(|| format!("synonym at byte {p} is cut off"))?;
        let i = u32::from_be_bytes(t.try_into().unwrap());
        // A synonym pointing past the index is skipped, not fatal.
        if (i as usize) < words {
            out.push((p as u32, nul as u32, i));
        }
        p = w + 4;
    }
    Ok(out)
}

/// Splits entry data into fields. With a `sametypesequence` the types are not stored and
/// the last field runs to the end; without one, each field starts with its type char.
/// Lowercase types end at a NUL; uppercase types start with a u32 BE size.
fn split_fields(data: &[u8], seq: &str) -> Vec<Field> {
    let mut out = Vec::new();
    let mut p = 0usize;
    if !seq.is_empty() {
        let kinds: Vec<char> = seq.chars().collect();
        for (k, &kind) in kinds.iter().enumerate() {
            if p > data.len() {
                break;
            }
            let last = k + 1 == kinds.len();
            if last {
                let d = &data[p..];
                let d = if kind.is_ascii_lowercase() { d.strip_suffix(&[0]).unwrap_or(d) } else if d.len() >= 4 && !kind.is_ascii_lowercase() && u32::from_be_bytes(d[..4].try_into().unwrap()) as usize == d.len() - 4 { &d[4..] } else { d };
                out.push(Field { kind, data: d.to_vec() });
                break;
            }
            match take_field(data, p, kind) {
                Some((f, next)) => {
                    out.push(Field { kind, data: f.to_vec() });
                    p = next;
                }
                None => break,
            }
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
        assert!(parse_idx(b"word", 32).is_err());
        assert!(parse_idx(b"word\0\0\0", 32).is_err());
        assert_eq!(parse_idx(b"", 32).unwrap().len(), 0);
    }
}
