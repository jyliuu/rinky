//! Reading byte ranges of a `.dict` (plain) or `.dict.dz` (dictzip) file.
//!
//! dictzip is gzip with an `RA` subfield in the FEXTRA header: version (1), chunk length,
//! chunk count, then the compressed size of every chunk (all u16 little-endian). Every
//! chunk is a raw deflate block sequence ended by a full flush, so a chunk inflates on its
//! own. A `.dz` without `RA` is gunzipped whole into memory (only small files do that in
//! practice).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use flate2::{Decompress, FlushDecompress};

use crate::{io_err, DictError, Result};

/// Upper limit for an in-memory `.dz` without random access, and for one entry.
const MAX_WHOLE: u64 = 256 << 20;
const MAX_ENTRY: u32 = 16 << 20;

pub(crate) enum DictData {
    Plain { path: PathBuf, file: Mutex<File>, len: u64 },
    Dz(Box<Dz>),
    Mem(Vec<u8>),
}

pub(crate) struct Dz {
    path: PathBuf,
    file: Mutex<File>,
    chunk_len: u32,
    /// File offset of every chunk, plus the end of the last one.
    offsets: Vec<u64>,
    /// The last two chunks inflated: (index, bytes).
    cache: Mutex<Vec<(usize, Vec<u8>)>>,
}

impl DictData {
    pub(crate) fn open(path: &Path) -> Result<DictData> {
        let mut file = File::open(path).map_err(|e| io_err(path, e))?;
        let len = file.metadata().map_err(|e| io_err(path, e))?.len();
        if !path.to_string_lossy().ends_with(".dz") {
            return Ok(DictData::Plain { path: path.to_path_buf(), file: Mutex::new(file), len });
        }
        let mut head = vec![0u8; len.min(1 << 20) as usize];
        file.read_exact(&mut head).map_err(|e| io_err(path, e))?;
        match parse_header(&head) {
            Some((data_start, chunk_len, sizes)) => {
                let mut offsets = Vec::with_capacity(sizes.len() + 1);
                let mut at = data_start as u64;
                offsets.push(at);
                for s in sizes {
                    at += s as u64;
                    offsets.push(at);
                }
                if at > len {
                    return Err(DictError::Format(format!("{}: dictzip chunks end at {at}, file has {len} bytes", path.display())));
                }
                Ok(DictData::Dz(Box::new(Dz { path: path.to_path_buf(), file: Mutex::new(file), chunk_len, offsets, cache: Mutex::new(Vec::new()) })))
            }
            None => {
                if head.len() < 2 || head[0] != 0x1f || head[1] != 0x8b {
                    return Err(DictError::Format(format!("{}: not a gzip file", path.display())));
                }
                file.seek(SeekFrom::Start(0)).map_err(|e| io_err(path, e))?;
                let mut out = Vec::new();
                flate2::read::MultiGzDecoder::new(file).take(MAX_WHOLE + 1).read_to_end(&mut out).map_err(|e| io_err(path, e))?;
                if out.len() as u64 > MAX_WHOLE {
                    return Err(DictError::Format(format!("{}: over {} MiB without dictzip chunks", path.display(), MAX_WHOLE >> 20)));
                }
                Ok(DictData::Mem(out))
            }
        }
    }

    /// `size` bytes at uncompressed offset `off`.
    pub(crate) fn read(&self, off: u64, size: u32) -> Result<Vec<u8>> {
        if size > MAX_ENTRY {
            return Err(DictError::Format(format!("entry of {size} bytes is too big")));
        }
        match self {
            DictData::Mem(v) => {
                let end = off.checked_add(size as u64).filter(|e| *e <= v.len() as u64).ok_or_else(|| DictError::Format(format!("entry {off}+{size} is past the end ({})", v.len())))?;
                Ok(v[off as usize..end as usize].to_vec())
            }
            DictData::Plain { path, file, len } => {
                if off.checked_add(size as u64).is_none_or(|e| e > *len) {
                    return Err(DictError::Format(format!("{}: entry {off}+{size} is past the end ({len})", path.display())));
                }
                let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
                f.seek(SeekFrom::Start(off)).map_err(|e| io_err(path, e))?;
                let mut buf = vec![0u8; size as usize];
                f.read_exact(&mut buf).map_err(|e| io_err(path, e))?;
                Ok(buf)
            }
            DictData::Dz(dz) => dz.read(off, size),
        }
    }
}

impl Dz {
    fn read(&self, off: u64, size: u32) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size as usize);
        let cl = self.chunk_len as u64;
        let mut pos = off;
        let end = off + size as u64;
        while pos < end {
            let i = (pos / cl) as usize;
            let chunk = self.chunk(i)?;
            let start = (pos - i as u64 * cl) as usize;
            if start >= chunk.len() {
                return Err(DictError::Format(format!("{}: entry {off}+{size} is past the end", self.path.display())));
            }
            let take = ((end - pos) as usize).min(chunk.len() - start);
            out.extend_from_slice(&chunk[start..start + take]);
            pos += take as u64;
        }
        Ok(out)
    }

    fn chunk(&self, i: usize) -> Result<Vec<u8>> {
        if let Some((_, b)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(j, _)| *j == i) {
            return Ok(b.clone());
        }
        if i + 1 >= self.offsets.len() {
            return Err(DictError::Format(format!("{}: chunk {i} does not exist ({} chunks)", self.path.display(), self.offsets.len() - 1)));
        }
        let (a, b) = (self.offsets[i], self.offsets[i + 1]);
        let mut raw = vec![0u8; (b - a) as usize];
        {
            let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
            f.seek(SeekFrom::Start(a)).map_err(|e| io_err(&self.path, e))?;
            f.read_exact(&mut raw).map_err(|e| io_err(&self.path, e))?;
        }
        let mut out = Vec::with_capacity(self.chunk_len as usize);
        let mut d = Decompress::new(false);
        // Inflate until the chunk's input is used up or a full chunk came out.
        loop {
            let before_in = d.total_in();
            let before_out = d.total_out();
            out.reserve(self.chunk_len as usize + 64);
            let st = d.decompress_vec(&raw[d.total_in() as usize..], &mut out, FlushDecompress::Sync).map_err(|e| DictError::Format(format!("{}: chunk {i}: {e}", self.path.display())))?;
            if matches!(st, flate2::Status::StreamEnd) || d.total_in() as usize >= raw.len() || out.len() >= self.chunk_len as usize {
                break;
            }
            if d.total_in() == before_in && d.total_out() == before_out {
                break;
            }
        }
        out.truncate(self.chunk_len as usize);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 2 {
            cache.remove(0);
        }
        cache.push((i, out.clone()));
        Ok(out)
    }
}

/// Returns (start of the deflate data, chunk length, compressed chunk sizes) when `h` is a
/// gzip header with a dictzip `RA` subfield.
fn parse_header(h: &[u8]) -> Option<(usize, u32, Vec<u16>)> {
    if h.len() < 12 || h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
        return None;
    }
    let flags = h[3];
    if flags & 4 == 0 {
        return None;
    }
    let u16le = |p: usize| -> Option<u16> { Some(u16::from_le_bytes([*h.get(p)?, *h.get(p + 1)?])) };
    let xlen = u16le(10)? as usize;
    let extra = h.get(12..12 + xlen)?;
    let mut at = 12 + xlen;
    let mut ra = None;
    let mut p = 0;
    while p + 4 <= extra.len() {
        let len = u16::from_le_bytes([extra[p + 2], extra[p + 3]]) as usize;
        let body = extra.get(p + 4..p + 4 + len)?;
        if &extra[p..p + 2] == b"RA" && body.len() >= 6 {
            let ver = u16::from_le_bytes([body[0], body[1]]);
            let chlen = u16::from_le_bytes([body[2], body[3]]) as u32;
            let count = u16::from_le_bytes([body[4], body[5]]) as usize;
            if ver == 1 && chlen > 0 && body.len() >= 6 + 2 * count {
                let sizes = (0..count).map(|k| u16::from_le_bytes([body[6 + 2 * k], body[7 + 2 * k]])).collect();
                ra = Some((chlen, sizes));
            }
        }
        p += 4 + len;
    }
    let (chlen, sizes) = ra?;
    if flags & 8 != 0 {
        at += h.get(at..)?.iter().position(|b| *b == 0)? + 1;
    }
    if flags & 16 != 0 {
        at += h.get(at..)?.iter().position(|b| *b == 0)? + 1;
    }
    if flags & 2 != 0 {
        at += 2;
    }
    Some((at, chlen, sizes))
}
