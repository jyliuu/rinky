//! Writes small StarDict dictionaries: for tests, the simulator and examples.
//!
//! The output is what StarDict tools write: entries sorted by [`stardict_cmp`],
//! `.dict` or dictzip `.dict.dz` (raw deflate chunks, each ended by a full flush, the last
//! one by a finish), `.idx` with 32- or 64-bit offsets, optional `.syn`.

use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::{Compress, Compression, FlushCompress};

use crate::stardict::stardict_cmp;
use crate::{io_err, Result};

/// One entry: headword and its fields `(type, data)`.
pub type Entry = (String, Vec<(char, Vec<u8>)>);

/// What to write.
#[derive(Debug, Clone)]
pub struct Spec {
    /// File stem (`NAME.ifo`, ...).
    pub stem: String,
    /// `bookname`.
    pub bookname: String,
    /// `sametypesequence`; empty to store a type char per field.
    pub sametypesequence: String,
    /// `idxoffsetbits=64` (version 3.0.0).
    pub offset64: bool,
    /// Write `.dict.dz` instead of `.dict`.
    pub dictzip: bool,
    /// dictzip chunk length (StarDict uses 58315).
    pub chunk_len: u16,
    /// Write `.idx.gz` instead of `.idx`.
    pub gzip_idx: bool,
    /// Entries: headword and its fields `(type, data)`.
    pub entries: Vec<Entry>,
    /// Synonyms: (synonym, headword it points to).
    pub synonyms: Vec<(String, String)>,
}

impl Spec {
    /// A plain-text (`m`) dictionary from (headword, definition) pairs.
    pub fn plain(stem: &str, bookname: &str, entries: &[(&str, &str)]) -> Spec {
        Spec {
            stem: stem.into(),
            bookname: bookname.into(),
            sametypesequence: "m".into(),
            offset64: false,
            dictzip: false,
            chunk_len: 58315,
            gzip_idx: false,
            entries: entries.iter().map(|(w, d)| (w.to_string(), vec![('m', d.as_bytes().to_vec())])).collect(),
            synonyms: Vec::new(),
        }
    }
}

fn encode_entry(fields: &[(char, Vec<u8>)], seq: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let n = fields.len();
    for (k, (kind, data)) in fields.iter().enumerate() {
        let last = k + 1 == n;
        if seq.is_empty() {
            out.push(*kind as u8);
        }
        if kind.is_ascii_lowercase() {
            out.extend_from_slice(data);
            if !last || seq.is_empty() {
                out.push(0);
            }
        } else {
            if !last || seq.is_empty() {
                out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            out.extend_from_slice(data);
        }
    }
    out
}

/// dictzip-compresses `data` with chunks of `chunk_len` bytes.
pub fn dictzip(data: &[u8], chunk_len: u16) -> Vec<u8> {
    let cl = chunk_len.max(1) as usize;
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(cl).collect() };
    let mut comp = Compress::new(Compression::best(), false);
    let mut body = Vec::new();
    let mut sizes = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let flush = if i + 1 == chunks.len() { FlushCompress::Finish } else { FlushCompress::Full };
        let start = comp.total_out();
        let mut input = *c;
        loop {
            body.reserve(cl + 1024);
            let before = comp.total_in();
            let st = comp.compress_vec(input, &mut body, flush).expect("deflate");
            input = &input[(comp.total_in() - before) as usize..];
            let done = if matches!(flush, FlushCompress::Finish) { matches!(st, flate2::Status::StreamEnd) } else { input.is_empty() && body.len() < body.capacity() };
            if done {
                break;
            }
        }
        sizes.push((comp.total_out() - start) as u16);
    }
    let mut out = vec![0x1f, 0x8b, 8, 4 | 8, 0, 0, 0, 0, 2, 3];
    let ra_len = 6 + 2 * sizes.len();
    out.extend_from_slice(&((ra_len + 4) as u16).to_le_bytes());
    out.extend_from_slice(b"RA");
    out.extend_from_slice(&(ra_len as u16).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&chunk_len.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    for s in &sizes {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.extend_from_slice(b"test.dict\0");
    out.extend_from_slice(&body);
    let mut crc = flate2::Crc::new();
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Writes the dictionary into `dir` and returns the `.ifo` path.
pub fn write(dir: &Path, spec: &Spec) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let mut entries = spec.entries.clone();
    entries.sort_by(|a, b| stardict_cmp(a.0.as_bytes(), b.0.as_bytes()));
    let mut dict = Vec::new();
    let mut idx = Vec::new();
    for (w, fields) in &entries {
        let e = encode_entry(fields, &spec.sametypesequence);
        idx.extend_from_slice(w.as_bytes());
        idx.push(0);
        if spec.offset64 {
            idx.extend_from_slice(&(dict.len() as u64).to_be_bytes());
        } else {
            idx.extend_from_slice(&(dict.len() as u32).to_be_bytes());
        }
        idx.extend_from_slice(&(e.len() as u32).to_be_bytes());
        dict.extend_from_slice(&e);
    }
    let p = |ext: &str| dir.join(format!("{}{ext}", spec.stem));
    let w = |path: PathBuf, data: &[u8]| std::fs::write(&path, data).map_err(|e| io_err(&path, e));
    if spec.dictzip {
        w(p(".dict.dz"), &dictzip(&dict, spec.chunk_len))?;
    } else {
        w(p(".dict"), &dict)?;
    }
    if spec.gzip_idx {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&idx).map_err(|e| io_err(&p(".idx.gz"), e))?;
        w(p(".idx.gz"), &gz.finish().map_err(|e| io_err(&p(".idx.gz"), e))?)?;
    } else {
        w(p(".idx"), &idx)?;
    }
    let mut syn_count = 0;
    if !spec.synonyms.is_empty() {
        let mut syns: Vec<(String, u32)> = spec.synonyms.iter().filter_map(|(s, head)| entries.iter().position(|(w, _)| w == head).map(|i| (s.clone(), i as u32))).collect();
        syns.sort_by(|a, b| stardict_cmp(a.0.as_bytes(), b.0.as_bytes()));
        syn_count = syns.len();
        let mut syn = Vec::new();
        for (s, i) in syns {
            syn.extend_from_slice(s.as_bytes());
            syn.push(0);
            syn.extend_from_slice(&i.to_be_bytes());
        }
        w(p(".syn"), &syn)?;
    }
    let mut ifo = format!(
        "StarDict's dict ifo file\nversion={}\nbookname={}\nwordcount={}\nidxfilesize={}\n",
        if spec.offset64 { "3.0.0" } else { "2.4.2" },
        spec.bookname,
        entries.len(),
        idx.len()
    );
    if syn_count > 0 {
        ifo.push_str(&format!("synwordcount={syn_count}\n"));
    }
    if spec.offset64 {
        ifo.push_str("idxoffsetbits=64\n");
    }
    if !spec.sametypesequence.is_empty() {
        ifo.push_str(&format!("sametypesequence={}\n", spec.sametypesequence));
    }
    w(p(".ifo"), ifo.as_bytes())?;
    Ok(p(".ifo"))
}

/// A small English test dictionary (HTML definitions, dictzip, synonyms) used by the
/// plugin tests and the simulator screenshots.
pub fn sample_english() -> Spec {
    let e = |w: &str, html: &str| (w.to_string(), vec![('h', html.as_bytes().to_vec())]);
    Spec {
        sametypesequence: "h".into(),
        dictzip: true,
        chunk_len: 64,
        entries: vec![
            e("household", "<b>household</b> <i>noun</i><br>a house and its occupants regarded as a unit: <i>the head of the household</i>.<br><b>adjective</b> relating to the running of a home: <i>household chores</i>."),
            e("dignity", "<b>dignity</b> <i>noun</i><ol><li>the state or quality of being worthy of honour or respect.</li><li>a composed or serious manner or style: <i>he bowed with great dignity</i>.</li><li>a sense of pride in oneself; self-respect.</li></ol>"),
            e("alike", "<b>alike</b> <i>adjective</i><br>(of two or more subjects) similar to each other.<br><b>adverb</b> in the same or a similar way."),
            e("fair", "<b>fair</b><sup>1</sup> <i>adjective</i><ol><li>in accordance with the rules or standards; legitimate.</li><li>(of hair or complexion) light; blonde.</li><li><i>archaic</i> beautiful; attractive: <i>the fairest of her daughters</i>.</li></ol>"),
            e("grudge", "<b>grudge</b> <i>noun</i><br>a persistent feeling of ill will or resentment resulting from a past insult or injury: <i>an ancient grudge</i>."),
            e("mutiny", "<b>mutiny</b> <i>noun</i> (<i>plural</i> mutinies)<br>an open rebellion against the proper authorities, especially by soldiers or sailors against their officers."),
            e("break", "<b>break</b> <i>verb</i> (past broke; past participle broken)<br>separate or cause to separate into pieces as a result of a blow, shock, or strain."),
            e("Verona", "<b>Verona</b><br>a city in NE Italy, on the River Adige; population 264,000. It is the setting of Shakespeare's <i>Romeo and Juliet</i>."),
            e("scene", "<b>scene</b> <i>noun</i><br>the place where an incident in real life or fiction occurs or occurred."),
            e("lie", "<b>lie</b><sup>1</sup> <i>verb</i> (lying; past lay; past participle lain)<br>(of a person or animal) be in or assume a horizontal or resting position on a supporting surface."),
            e("civil", "<b>civil</b> <i>adjective</i><br>relating to ordinary citizens and their concerns, as distinct from military or ecclesiastical matters: <i>civil blood</i>."),
        ],
        synonyms: vec![("broke".into(), "break".into()), ("lay".into(), "lie".into()), ("households".into(), "household".into())],
        ..Spec::plain("sample-en", "Sample English Dictionary", &[])
    }
}
