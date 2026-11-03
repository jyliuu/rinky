//! KF8 (AZW3) reconstruction, following KindleUnpack's `mobi_k8proc.py`.
//!
//! - FDST splits the text into flows: flow 0 is the XHTML, later flows are CSS or SVG.
//! - Each skeleton entry (`SKEL`) is a file; its fragments (`FRAG`, keyed by insert
//!   position) are inserted into it in order. An insert position that falls inside a
//!   tag is recomputed from the fragment's `aid` selector.
//! - `kindle:pos:fid:XXXX:off:YYYYYYYYYY` links become `partNNNN.xhtml#id` (the nearest
//!   `id`/`name` before that position, or an `aid`, which then gets an `id="aid-..."`).
//! - `kindle:embed:XXXX` becomes `res/NNNNN.ext`; `kindle:flow:XXXX` becomes
//!   `flows/flowNNNN.ext` (served from memory, with their own `kindle:embed` URLs fixed).
//! - TOC from the NCX index (tag 6 = position as fragment + offset).

use super::header::{decode, MobiHeader};
use super::mobi6::{ncx, nest};
use super::{base32, index, Converted, Mobi, Resources};
use crate::book::Target;
use crate::util::be32;
use crate::xhtml::{self, NoRewrite};
use crate::xml;
use ink_doc::{DocError, Result};
use ink_html::HtmlChapter;
use std::collections::{HashMap, HashSet};

/// One skeleton (file) entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Skeleton {
    /// Number of fragments.
    pub frags: usize,
    /// Start in the text.
    pub start: usize,
    /// Length of the skeleton itself.
    pub len: usize,
}

/// One fragment entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    /// Insert position in the assembled text.
    pub insert: usize,
    /// Selector (`P-//*[@aid='0']`).
    pub selector: String,
    /// File number.
    pub file: usize,
    /// Start of the fragment text (relative to the end of its skeleton chain).
    pub start: usize,
    /// Length.
    pub len: usize,
}

/// A reconstructed file and where it sits in the assembled text.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// XHTML bytes.
    pub text: Vec<u8>,
    /// Start of the file in assembled positions.
    pub start: usize,
    /// End (exclusive).
    pub end: usize,
}

/// Flow boundaries from the FDST record (a single flow when it is missing).
pub fn flows(m: &Mobi, h: &MobiHeader, text_len: usize) -> Vec<(usize, usize)> {
    let rec = h.abs(h.fdst_index).and_then(|r| m.db.record(r));
    if let Some(rec) = rec.filter(|r| r.starts_with(b"FDST")) {
        // Section count from the file: bounded by the 8-byte entries in the record.
        let n = (be32(rec, 8).unwrap_or(0) as usize).min(rec.len().saturating_sub(12) / 8);
        let mut out = Vec::with_capacity(n);
        // Flows follow each other in the text; overlapping entries are cut so that each
        // byte belongs to one flow (every flow is copied into memory).
        let mut prev_end = 0;
        for i in 0..n {
            let (Some(s), Some(e)) = (be32(rec, 12 + 8 * i), be32(rec, 16 + 8 * i)) else { break };
            let s = (s as usize).max(prev_end).min(text_len);
            let e = (e as usize).min(text_len).max(s);
            out.push((s, e));
            prev_end = e;
        }
        if !out.is_empty() {
            return out;
        }
    }
    vec![(0, text_len)]
}

/// Reads the skeleton and fragment tables.
pub fn tables(m: &Mobi, h: &MobiHeader) -> (Vec<Skeleton>, Vec<Fragment>) {
    let skels = h
        .abs(h.skel_index)
        .map(|r| index::read(&m.db, r))
        .unwrap_or_default()
        .entries
        .iter()
        .map(|e| Skeleton { frags: e.tag(1).unwrap_or(0) as usize, start: e.tag_at(6, 0).unwrap_or(0) as usize, len: e.tag_at(6, 1).unwrap_or(0) as usize })
        .collect();
    let fidx = h.abs(h.frag_index).map(|r| index::read(&m.db, r)).unwrap_or_default();
    let frags = fidx
        .entries
        .iter()
        .map(|e| Fragment {
            insert: e.name_str().trim().parse().unwrap_or(0),
            selector: e.tag(2).and_then(|o| fidx.cncx.get(&o)).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default(),
            file: e.tag(3).unwrap_or(0) as usize,
            start: e.tag_at(6, 0).unwrap_or(0) as usize,
            len: e.tag_at(6, 1).unwrap_or(0) as usize,
        })
        .collect();
    (skels, frags)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn rfind(hay: &[u8], b: u8) -> Option<usize> {
    hay.iter().rposition(|x| *x == b)
}

/// Start and end of the tag that carries `aid="X"` (KindleUnpack `locate_beg_end_of_tag`).
fn tag_with_aid(text: &[u8], aid: &str) -> Option<(usize, usize)> {
    for q in *b"\"'" {
        let pat = [b"aid=".as_slice(), &[q], aid.as_bytes(), &[q]].concat();
        if let Some(p) = find(text, &pat, 0) {
            let b = rfind(&text[..p], b'<')?;
            let e = find(text, b">", p)?;
            return Some((b, e));
        }
    }
    None
}

/// Reassembles the files from skeletons and fragments.
pub fn reconstruct(text: &[u8], skels: &[Skeleton], frags: &[Fragment]) -> Vec<Part> {
    let mut parts = Vec::with_capacity(skels.len());
    let mut fi = 0;
    // Skeletons and fragments partition the text in real books; overlapping entries
    // would copy it again for every skeleton. Stop at twice the text size.
    let budget = text.len().saturating_mul(2).saturating_add(1 << 20);
    let mut total = 0usize;
    for s in skels {
        if total > budget {
            log::warn!("KF8: skeleton table covers more than twice the text; {} files left out", skels.len() - parts.len());
            break;
        }
        let mut base = s.start.saturating_add(s.len);
        let mut skeleton = text.get(s.start..base.min(text.len())).unwrap_or(&[]).to_vec();
        for _ in 0..s.frags {
            let Some(f) = frags.get(fi) else { break };
            fi += 1;
            let part = text.get(base..base.saturating_add(f.len).min(text.len())).unwrap_or(&[]);
            let mut at = f.insert.saturating_sub(s.start).min(skeleton.len());
            let (head, tail) = skeleton.split_at(at);
            let tail_open = find(tail, b">", 0).is_some_and(|g| find(tail, b"<", 0).is_none_or(|l| g < l));
            let head_open = rfind(head, b'<').is_some_and(|l| rfind(head, b'>').is_none_or(|g| g < l));
            if tail_open || head_open {
                // The insert position is inside a tag: find the tag by its aid and insert
                // after it, at the fragment's offset.
                let aid = f.selector.split("@aid='").nth(1).and_then(|r| r.split('\'').next()).unwrap_or("");
                if let Some((_, e)) = tag_with_aid(&skeleton, aid) {
                    at = (e + 1).saturating_add(f.start).min(skeleton.len());
                }
            }
            skeleton.splice(at..at, part.iter().copied());
            base = base.saturating_add(f.len);
        }
        let end = s.start.saturating_add(skeleton.len());
        total = total.saturating_add(skeleton.len());
        parts.push(Part { text: skeleton, start: s.start, end });
    }
    parts
}

/// Attribute `name` of a tag (`<p id="x" ...>`), quotes required.
fn tag_attr(tag: &[u8], name: &[u8]) -> Option<String> {
    let mut i = 0;
    while let Some(p) = find(tag, name, i) {
        i = p + 1;
        if p == 0 || !tag[p - 1].is_ascii_whitespace() {
            continue;
        }
        let mut j = p + name.len();
        while j < tag.len() && tag[j].is_ascii_whitespace() {
            j += 1;
        }
        if tag.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while j < tag.len() && tag[j].is_ascii_whitespace() {
            j += 1;
        }
        let q = *tag.get(j)?;
        if q != b'"' && q != b'\'' {
            continue;
        }
        let end = tag[j + 1..].iter().position(|b| *b == q)? + j + 1;
        return Some(String::from_utf8_lossy(&tag[j + 1..end]).into_owned());
    }
    None
}

/// What a `kindle:pos` target resolves to.
#[derive(Clone, Debug, PartialEq)]
pub enum Anchor {
    /// An `id` or `name` attribute.
    Id(String),
    /// An `aid` attribute (gets `id="aid-X"`).
    Aid(String),
    /// The top of the file.
    Top,
}

/// The file and anchor for an assembled position (KindleUnpack `getIDTag`).
pub fn anchor_at(parts: &[Part], pos: usize) -> Option<(usize, Anchor)> {
    let pn = parts.iter().position(|p| p.start <= pos && pos < p.end).or_else(|| parts.iter().rposition(|p| p.start <= pos))?;
    let text = &parts[pn].text;
    let mut npos = (pos - parts[pn].start).min(text.len());
    let pgt = find(text, b">", npos);
    let plt = find(text, b"<", npos);
    if let Some(g) = pgt {
        if plt == Some(npos) || plt.is_none_or(|l| g < l) {
            npos = g + 1;
        }
    }
    let mut block = &text[..npos];
    // Walk tags backwards.
    while let Some(gt) = rfind(block, b'>') {
        let Some(lt) = rfind(&block[..gt], b'<') else { break };
        let tag = &block[lt..=gt];
        if tag.starts_with(b"<body ") || tag.starts_with(b"<body>") {
            return Some((pn, Anchor::Top));
        }
        if !tag.starts_with(b"<meta ") && !tag.starts_with(b"</") {
            if let Some(id) = tag_attr(tag, b"id").or_else(|| tag_attr(tag, b"name")) {
                return Some((pn, Anchor::Id(id)));
            }
            if let Some(aid) = tag_attr(tag, b"aid") {
                return Some((pn, Anchor::Aid(aid)));
            }
        }
        block = &block[..lt];
    }
    Some((pn, Anchor::Top))
}

fn part_name(i: usize) -> String {
    format!("part{i:04}.xhtml")
}

/// Rewrites `kindle:` URIs in `s`.
fn rewrite_uris(
    s: &str,
    res: &Resources,
    flow_names: &HashMap<usize, String>,
    pos: &mut dyn FnMut(usize, usize) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("kindle:") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let end = after.find(['"', '\'', ')', ' ', '>', '<']).unwrap_or(after.len());
        let uri = &after[..end];
        let replacement = if let Some(r) = uri.strip_prefix("kindle:embed:") {
            let n = r.split('?').next().unwrap_or("");
            base32(n).and_then(|n| res.path(n)).map(str::to_string)
        } else if let Some(r) = uri.strip_prefix("kindle:flow:") {
            let n = r.split('?').next().unwrap_or("");
            base32(n).and_then(|n| flow_names.get(&n)).cloned()
        } else if let Some(r) = uri.strip_prefix("kindle:pos:fid:") {
            let mut it = r.split(':');
            let fid = it.next().and_then(base32);
            let off = if it.next() == Some("off") { it.next().and_then(base32) } else { Some(0) };
            fid.zip(off).and_then(|(f, o)| pos(f, o))
        } else {
            None
        };
        match replacement {
            Some(r) => out.push_str(&r),
            None => out.push_str(uri),
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Adds `id="aid-X"` to the tags carrying the linked aids.
fn add_aid_ids(text: &str, aids: &HashSet<String>) -> String {
    if aids.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + aids.len() * 16);
    let bytes = text.as_bytes();
    let mut last = 0;
    let mut i = 0;
    while let Some(p) = find(bytes, b"aid=", i) {
        i = p + 4;
        let Some(&q) = bytes.get(p + 4) else { break };
        if q != b'"' && q != b'\'' {
            continue;
        }
        let Some(e) = bytes[p + 5..].iter().position(|b| *b == q).map(|x| x + p + 5) else { break };
        let aid = &text[p + 5..e];
        if !aids.contains(aid) {
            continue;
        }
        let lt = rfind(&bytes[..p], b'<').unwrap_or(0);
        let gt = find(bytes, b">", e).unwrap_or(bytes.len());
        if tag_attr(&bytes[lt..gt], b"id").is_some() {
            continue;
        }
        out.push_str(&text[last..=e]);
        out.push_str(&format!(" id=\"aid-{aid}\""));
        last = e + 1;
    }
    out.push_str(&text[last..]);
    out
}

fn flow_extension(data: &[u8], mime: Option<&str>) -> &'static str {
    match mime {
        Some(m) if m.contains("svg") => "svg",
        Some(m) if m.contains("css") => "css",
        _ if data.windows(4).take(256).any(|w| w == b"<svg") => "svg",
        _ => "css",
    }
}

/// Converts the KF8 part described by `h`.
pub fn convert(m: &Mobi, h: &MobiHeader, res: &Resources) -> Result<Converted> {
    let text = m.text(h)?;
    let fl = flows(m, h, text.len());
    let (skels, frags) = tables(m, h);
    let flow0_end = fl.first().map(|f| f.1).unwrap_or(text.len());
    let parts = if skels.is_empty() {
        vec![Part { text: text[..flow0_end].to_vec(), start: 0, end: flow0_end }]
    } else {
        reconstruct(&text, &skels, &frags)
    };
    if parts.is_empty() {
        return Err(DocError::Format("KF8: no files".into()));
    }
    // Flow names from the references (the mime type tells CSS from SVG).
    let all = parts.iter().map(|p| String::from_utf8_lossy(&p.text)).collect::<Vec<_>>().join("");
    // The first `kindle:flow:XXXX` reference of each flow and its `mime=` (one scan; the
    // flow count comes from the file).
    let mut mimes: HashMap<&str, Option<&str>> = HashMap::new();
    let mut at = 0;
    while let Some(p) = all[at..].find("kindle:flow:").map(|p| at + p) {
        at = p + "kindle:flow:".len();
        if let Some(key) = all.get(at..at + 4) {
            let uri = all[p..].split(['"', '\'', ')']).next().unwrap_or("");
            mimes.entry(key).or_insert_with(|| uri.split("mime=").nth(1));
        }
    }
    let mut flow_names = HashMap::new();
    let mut files = HashMap::new();
    for (i, (s, e)) in fl.iter().enumerate().skip(1) {
        let data = text.get(*s..*e).unwrap_or(&[]);
        let mime = mimes.get(to_base32(i, 4).as_str()).copied().flatten();
        let name = format!("flows/flow{i:04}.{}", flow_extension(data, mime));
        flow_names.insert(i, name);
    }
    let resolve = |fid: usize, off: usize| -> Option<(usize, Anchor)> {
        let f = frags.get(fid)?;
        anchor_at(&parts, f.insert.saturating_add(off))
    };
    let mut linked: Vec<HashSet<String>> = vec![HashSet::new(); parts.len()];
    let mut pass1 = Vec::with_capacity(parts.len());
    for p in &parts {
        let s = String::from_utf8_lossy(&p.text).into_owned();
        let mut aids: Vec<(usize, String)> = Vec::new();
        let r = rewrite_uris(&s, res, &flow_names, &mut |fid, off| {
            let (pn, a) = resolve(fid, off)?;
            Some(match a {
                Anchor::Id(id) => format!("{}#{id}", part_name(pn)),
                Anchor::Aid(aid) => {
                    aids.push((pn, aid.clone()));
                    format!("{}#aid-{aid}", part_name(pn))
                }
                Anchor::Top => part_name(pn),
            })
        });
        for (pn, aid) in aids {
            linked[pn].insert(aid);
        }
        pass1.push(r);
    }
    // TOC targets may name aids too.
    let entries = ncx(m, h);
    let mut targets = Vec::with_capacity(entries.len());
    for e in &entries {
        let t = match (e.pos_fid, e.pos) {
            (Some((fid, off)), _) => resolve(fid, off),
            (None, Some(p)) => anchor_at(&parts, p),
            _ => None,
        };
        targets.push(t.map(|(pn, a)| match a {
            Anchor::Id(id) => Target::id(pn, id),
            Anchor::Aid(aid) => {
                linked[pn].insert(aid.clone());
                Target::id(pn, format!("aid-{aid}"))
            }
            Anchor::Top => Target::chapter(pn),
        }));
    }
    let toc = nest(&entries, &targets);
    let mut chapters = Vec::with_capacity(parts.len());
    for (i, s) in pass1.into_iter().enumerate() {
        let s = add_aid_ids(&s, &linked[i]);
        let html = if xml::is_well_formed(&s) { s } else { xhtml::to_xhtml(&s, &mut NoRewrite) };
        chapters.push(HtmlChapter { path: part_name(i), html });
    }
    for (i, (s, e)) in fl.iter().enumerate().skip(1) {
        let data = decode(text.get(*s..*e).unwrap_or(&[]), h.codepage);
        let data = rewrite_uris(&data, res, &flow_names, &mut |_, _| None);
        // Paths in a flow resolve against flows/; resources live at the root.
        let data = data.replace("res/", "../res/");
        files.insert(flow_names[&i].clone(), data.into_bytes());
    }
    Ok((chapters, toc, files))
}

/// Base 32 with `width` digits.
pub fn to_base32(mut v: usize, width: usize) -> String {
    const D: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
    let mut s = Vec::new();
    while v > 0 {
        s.push(D[v % 32]);
        v /= 32;
    }
    while s.len() < width {
        s.push(b'0');
    }
    s.reverse();
    String::from_utf8(s).expect("ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruct_skeleton_with_fragments() {
        // Skeleton: <html><body><div aid="0"></div></body></html> with two fragments
        // inserted inside the div.
        let skel = b"<html><body><div aid=\"0\"></div></body></html>";
        let f1 = b"<p id=\"a\">One</p>";
        let f2 = b"<p aid=\"1\">Two</p>";
        let mut text = skel.to_vec();
        text.extend_from_slice(f1);
        text.extend_from_slice(f2);
        let ins = skel.windows(6).position(|w| w == b"</div>").unwrap();
        let skels = vec![Skeleton { frags: 2, start: 0, len: skel.len() }];
        let frags = vec![
            Fragment { insert: ins, selector: "P-//*[@aid='0']".into(), file: 0, start: 0, len: f1.len() },
            Fragment { insert: ins + f1.len(), selector: "P-//*[@aid='0']".into(), file: 0, start: f1.len(), len: f2.len() },
        ];
        let parts = reconstruct(&text, &skels, &frags);
        assert_eq!(parts.len(), 1);
        assert_eq!(String::from_utf8_lossy(&parts[0].text), "<html><body><div aid=\"0\"><p id=\"a\">One</p><p aid=\"1\">Two</p></div></body></html>");
        // A position inside "Two" resolves to the aid of its paragraph; inside "One" to id a.
        let two = parts[0].text.windows(3).position(|w| w == b"Two").unwrap();
        assert_eq!(anchor_at(&parts, two), Some((0, Anchor::Aid("1".into()))));
        let one = parts[0].text.windows(3).position(|w| w == b"One").unwrap();
        assert_eq!(anchor_at(&parts, one), Some((0, Anchor::Id("a".into()))));
        assert_eq!(anchor_at(&parts, 3), Some((0, Anchor::Top)));
        // A wrong insert position (inside the div tag) is recomputed from the aid.
        let bad = vec![Fragment { insert: 15, selector: "P-//*[@aid='0']".into(), file: 0, start: 0, len: f1.len() }];
        let parts = reconstruct(&text, &[Skeleton { frags: 1, start: 0, len: skel.len() }], &bad);
        assert!(String::from_utf8_lossy(&parts[0].text).contains("<div aid=\"0\"><p id=\"a\">One</p></div>"));
    }

    #[test]
    fn overlapping_tables_are_bounded() {
        // 10 000 skeleton entries that each cover the whole 1 MiB text would copy it
        // 10 000 times (10 GB); reconstruction stops at about twice the text.
        let text = vec![b'a'; 1 << 20];
        let skels: Vec<Skeleton> = (0..10_000).map(|_| Skeleton { frags: 0, start: 0, len: text.len() }).collect();
        let parts = reconstruct(&text, &skels, &[]);
        let total: usize = parts.iter().map(|p| p.text.len()).sum();
        assert!(parts.len() < 10 && total <= 4 * text.len(), "{} parts, {total} bytes", parts.len());
    }

    #[test]
    fn uris_and_aids() {
        let mut res = Resources::default();
        res.by_number.insert(33, super::super::Resource { number: 33, path: "res/00033.png".into(), record: 1, kind: super::super::ResKind::Image, media_type: "image/png".into() });
        let flows: HashMap<usize, String> = [(1, "flows/flow0001.css".to_string())].into();
        let s = r#"<link href="kindle:flow:0001?mime=text/css"/><img src="kindle:embed:0011?mime=image/png"/><a href="kindle:pos:fid:0002:off:000000000A">x</a><a href="kindle:pos:fid:0009:off:0000000000">y</a>"#;
        let out = rewrite_uris(s, &res, &flows, &mut |f, o| (f == 2 && o == 10).then(|| "part0001.xhtml#here".to_string()));
        assert_eq!(out, r#"<link href="flows/flow0001.css"/><img src="res/00033.png"/><a href="part0001.xhtml#here">x</a><a href="kindle:pos:fid:0009:off:0000000000">y</a>"#);
        let aids: HashSet<String> = ["7".to_string()].into();
        assert_eq!(add_aid_ids("<p aid=\"7\">a</p><p id=\"k\" aid=\"7\">b</p>", &aids), "<p aid=\"7\" id=\"aid-7\">a</p><p id=\"k\" aid=\"7\">b</p>");
        assert_eq!(to_base32(33, 4), "0011");
        assert_eq!(tag_attr(b"<p class=\"x\" id='y'>", b"id"), Some("y".into()));
        assert_eq!(tag_attr(b"<p aid=\"y\">", b"id"), None);
    }
}
