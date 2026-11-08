//! Generated test documents: a small PDF writer (base-14 fonts, outline, links, metadata)
//! and a CBZ writer (stored zip of PNG pages). Used by the tests, by
//! `examples/mupdf-fixtures.rs` and by selftests on the device. Everything here is
//! self-generated and public domain.

use std::fmt::Write as _;

/// A base-14 font (not embedded; MuPDF uses its URW clones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    /// Helvetica.
    Helvetica,
    /// Helvetica-Bold.
    HelveticaBold,
    /// Times-Roman.
    Times,
}

impl Font {
    fn resource(self) -> &'static str {
        match self {
            Font::Helvetica => "F1",
            Font::HelveticaBold => "F2",
            Font::Times => "F3",
        }
    }
}

/// Where a link or outline entry goes.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfDest {
    /// Page (0-based) and the y of the target's top edge (top-left coordinates).
    Page(usize, f32),
    /// A URL.
    Uri(String),
}

/// One page under construction. Coordinates are points from the top-left corner.
#[derive(Clone, Debug)]
pub struct PdfPage {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
    content: String,
    links: Vec<([f32; 4], PdfDest)>,
}

fn escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            c if c.is_ascii() && !c.is_ascii_control() => o.push(c),
            _ => o.push('?'),
        }
    }
    o
}

impl PdfPage {
    /// An empty page.
    pub fn new(width: f32, height: f32) -> Self {
        PdfPage { width, height, content: String::new(), links: Vec::new() }
    }

    /// US Letter (612 x 792 points).
    pub fn letter() -> Self {
        Self::new(612.0, 792.0)
    }

    /// Text with its top-left corner near `(x, top)` (baseline at `top + 0.8 * size`).
    pub fn text(&mut self, x: f32, top: f32, size: f32, font: Font, s: &str) {
        let y = self.height - top - size * 0.8;
        let _ = writeln!(self.content, "BT /{} {size} Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET", font.resource(), escape(s));
    }

    /// A filled rectangle (`gray` 0 = black, 1 = white).
    pub fn rect(&mut self, x: f32, top: f32, w: f32, h: f32, gray: f32) {
        let y = self.height - top - h;
        let _ = writeln!(self.content, "{gray} g {x} {y} {w} {h} re f 0 g");
    }

    /// A link over `[x0, top, x1, bottom]`.
    pub fn link(&mut self, area: [f32; 4], dest: PdfDest) {
        self.links.push((area, dest));
    }
}

/// One outline entry (flat list with levels, depth first).
#[derive(Clone, Debug)]
pub struct PdfOutline {
    /// Title.
    pub title: String,
    /// Depth, 0 = top.
    pub level: u8,
    /// Target page (0-based).
    pub page: usize,
    /// Target top (top-left coordinates).
    pub top: f32,
}

/// Writes a PDF 1.4 file with uncompressed streams.
pub fn write_pdf(pages: &[PdfPage], outline: &[PdfOutline], info: &[(&str, &str)]) -> Vec<u8> {
    let n = pages.len();
    let page_obj = |i: usize| 7 + 2 * i;
    let mut next = 7 + 2 * n;
    let mut annot_objs: Vec<Vec<usize>> = Vec::new();
    for p in pages {
        let ids: Vec<usize> = (0..p.links.len()).map(|k| next + k).collect();
        next += p.links.len();
        annot_objs.push(ids);
    }
    let outline_root = if outline.is_empty() { None } else { Some(next) };
    let item_obj = |k: usize| outline_root.unwrap_or(0) + 1 + k;
    let total = if outline.is_empty() { next - 1 } else { next + outline.len() };

    let dest = |page: usize, top: f32| {
        let p = page.min(n.saturating_sub(1));
        let h = pages.get(p).map(|x| x.height).unwrap_or(792.0);
        format!("[{} 0 R /XYZ 0 {} 0]", page_obj(p), h - top)
    };
    let mut objs: Vec<String> = vec![String::new(); total + 1];
    objs[1] = match outline_root {
        Some(r) => format!("<< /Type /Catalog /Pages 2 0 R /Outlines {r} 0 R /PageMode /UseOutlines >>"),
        None => "<< /Type /Catalog /Pages 2 0 R >>".into(),
    };
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", page_obj(i))).collect();
    objs[2] = format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" "));
    objs[3] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into();
    objs[4] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".into();
    objs[5] = "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding /WinAnsiEncoding >>".into();
    let info_entries: Vec<String> = info.iter().map(|(k, v)| format!("/{k} ({})", escape(v))).collect();
    objs[6] = format!("<< {} >>", info_entries.join(" "));
    for (i, p) in pages.iter().enumerate() {
        let annots = if annot_objs[i].is_empty() {
            String::new()
        } else {
            format!(" /Annots [{}]", annot_objs[i].iter().map(|o| format!("{o} 0 R")).collect::<Vec<_>>().join(" "))
        };
        objs[page_obj(i)] = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >> >> /Contents {} 0 R{annots} >>",
            p.width,
            p.height,
            page_obj(i) + 1
        );
        objs[page_obj(i) + 1] = format!("<< /Length {} >>\nstream\n{}endstream", p.content.len(), p.content);
        for (k, (area, d)) in p.links.iter().enumerate() {
            let rect = format!("[{} {} {} {}]", area[0], p.height - area[3], area[2], p.height - area[1]);
            let action = match d {
                PdfDest::Page(pg, top) => format!("/Dest {}", dest(*pg, *top)),
                PdfDest::Uri(u) => format!("/A << /S /URI /URI ({}) >>", escape(u)),
            };
            objs[annot_objs[i][k]] = format!("<< /Type /Annot /Subtype /Link /Rect {rect} /Border [0 0 0] {action} >>");
        }
    }
    if let Some(root) = outline_root {
        // Parent of each item: the nearest earlier item one level up, else the root.
        let m = outline.len();
        let mut parent = vec![None; m];
        for k in 0..m {
            parent[k] = (0..k).rev().find(|&j| outline[j].level < outline[k].level);
        }
        let children = |p: Option<usize>| -> Vec<usize> { (0..m).filter(|&k| parent[k] == p).collect() };
        let descendants = |k: usize| -> usize {
            (k + 1..m).take_while(|&j| outline[j].level > outline[k].level).count()
        };
        let top = children(None);
        objs[root] = format!(
            "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
            item_obj(top[0]),
            item_obj(*top.last().unwrap_or(&top[0])),
            m
        );
        for k in 0..m {
            let sibs = children(parent[k]);
            let pos = sibs.iter().position(|&s| s == k).unwrap_or(0);
            let mut d = format!(
                "<< /Title ({}) /Parent {} 0 R /Dest {}",
                escape(&outline[k].title),
                parent[k].map(item_obj).unwrap_or(root),
                dest(outline[k].page, outline[k].top)
            );
            if pos > 0 {
                let _ = write!(d, " /Prev {} 0 R", item_obj(sibs[pos - 1]));
            }
            if pos + 1 < sibs.len() {
                let _ = write!(d, " /Next {} 0 R", item_obj(sibs[pos + 1]));
            }
            let kids = children(Some(k));
            if !kids.is_empty() {
                let _ = write!(
                    d,
                    " /First {} 0 R /Last {} 0 R /Count {}",
                    item_obj(kids[0]),
                    item_obj(*kids.last().unwrap_or(&kids[0])),
                    descendants(k)
                );
            }
            d.push_str(" >>");
            objs[item_obj(k)] = d;
        }
    }
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = vec![0usize; total + 1];
    for (i, body) in objs.iter().enumerate().skip(1) {
        offsets[i] = out.len();
        out.extend_from_slice(format!("{i} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = out.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", total + 1);
    for off in offsets.iter().skip(1) {
        let _ = writeln!(x, "{off:010} 00000 n ");
    }
    let _ = write!(x, "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n", total + 1);
    out.extend_from_slice(x.as_bytes());
    out
}

/// Words for filler text (public domain: generated, no source text).
const WORDS: &[&str] = &[
    "the", "reader", "turns", "a", "page", "and", "light", "falls", "on", "quiet", "paper", "while", "ink", "settles",
    "into", "letters", "that", "carry", "old", "stories", "across", "long", "winter", "evenings", "by", "the", "window",
];

/// A line of filler words, deterministic in `seed`.
pub fn filler(seed: usize, words: usize) -> String {
    (0..words).map(|k| WORDS[(seed * 7 + k * 3 + k * k) % WORDS.len()]).collect::<Vec<_>>().join(" ")
}

/// The standard fixture PDF (10 Letter pages):
/// - page 0: wide margins (text block from x 150), a black marker 100 x 20 at (150, 100),
///   gray rectangles 60 x 20 at (270, 100) (50 %) and (350, 100) (80 %),
///   an internal link "see chapter 2" to page 2, a URL link to `https://example.org`,
///   a footnote mark "1" linking to page 4;
/// - page 1: two text columns (x 72..288 and 324..540) under a full-width heading;
/// - page 2: "Chapter 2" with "needle" (twice, one capitalised) and "hay-"/"stack"
///   hyphenated across lines;
/// - page 3: appendix; page 4: notes; pages 5..9: "Page N" filler.
///
/// Outline: Chapter 1 (Margins, Two columns), Chapter 2, Appendix (Notes).
/// Info: title "rinky fixture", authors "Ada Lovelace; Charles Babbage".
pub fn fixture_pdf() -> Vec<u8> {
    let mut pages = Vec::new();

    let mut p = PdfPage::letter();
    p.rect(150.0, 100.0, 100.0, 20.0, 0.0);
    p.rect(270.0, 100.0, 60.0, 20.0, 0.5);
    p.rect(350.0, 100.0, 60.0, 20.0, 0.8);
    p.text(150.0, 140.0, 24.0, Font::HelveticaBold, "Chapter 1: Margins");
    for k in 0..16 {
        p.text(150.0, 190.0 + 16.0 * k as f32, 12.0, Font::Times, &filler(k, 8));
    }
    p.text(150.0, 470.0, 12.0, Font::Helvetica, "see chapter 2");
    p.link([150.0, 468.0, 230.0, 484.0], PdfDest::Page(2, 72.0));
    p.text(260.0, 470.0, 12.0, Font::Helvetica, "example.org");
    p.link([260.0, 468.0, 330.0, 484.0], PdfDest::Uri("https://example.org/".into()));
    p.text(340.0, 466.0, 8.0, Font::Helvetica, "1");
    p.link([338.0, 464.0, 346.0, 476.0], PdfDest::Page(4, 72.0));
    pages.push(p);

    let mut p = PdfPage::letter();
    p.text(72.0, 60.0, 20.0, Font::HelveticaBold, "Two columns");
    // Column by column, as typesetters write them.
    for k in 0..40 {
        p.text(72.0, 110.0 + 14.0 * k as f32, 11.0, Font::Times, &format!("Left column line {}", k + 1));
    }
    for k in 0..40 {
        p.text(324.0, 110.0 + 14.0 * k as f32, 11.0, Font::Times, &format!("Right column line {}", k + 1));
    }
    pages.push(p);

    let mut p = PdfPage::letter();
    p.text(72.0, 72.0, 24.0, Font::HelveticaBold, "Chapter 2");
    p.text(72.0, 120.0, 12.0, Font::Times, "Looking for a needle in a hay-");
    p.text(72.0, 136.0, 12.0, Font::Times, "stack takes patience and good light.");
    for k in 0..10 {
        p.text(72.0, 160.0 + 16.0 * k as f32, 12.0, Font::Times, &filler(k + 40, 9));
    }
    p.text(72.0, 340.0, 12.0, Font::Times, "Needle and thread lie on the table.");
    pages.push(p);

    let mut p = PdfPage::letter();
    p.text(72.0, 72.0, 24.0, Font::HelveticaBold, "Appendix");
    for k in 0..20 {
        p.text(72.0, 120.0 + 16.0 * k as f32, 12.0, Font::Times, &filler(k + 80, 10));
    }
    pages.push(p);

    let mut p = PdfPage::letter();
    p.text(72.0, 72.0, 24.0, Font::HelveticaBold, "Notes");
    p.text(72.0, 120.0, 12.0, Font::Times, "1. A footnote about margins.");
    pages.push(p);

    for i in 5..10 {
        let mut p = PdfPage::letter();
        p.text(72.0, 72.0, 24.0, Font::HelveticaBold, &format!("Page {}", i + 1));
        for k in 0..30 {
            p.text(72.0, 120.0 + 18.0 * k as f32, 12.0, Font::Times, &filler(i * 31 + k, 10));
        }
        pages.push(p);
    }

    let outline = vec![
        PdfOutline { title: "Chapter 1".into(), level: 0, page: 0, top: 0.0 },
        PdfOutline { title: "Margins".into(), level: 1, page: 0, top: 140.0 },
        PdfOutline { title: "Two columns".into(), level: 1, page: 1, top: 60.0 },
        PdfOutline { title: "Chapter 2".into(), level: 0, page: 2, top: 72.0 },
        PdfOutline { title: "Appendix".into(), level: 0, page: 3, top: 72.0 },
        PdfOutline { title: "Notes".into(), level: 1, page: 4, top: 72.0 },
    ];
    write_pdf(
        &pages,
        &outline,
        &[("Title", "rinky fixture"), ("Author", "Ada Lovelace; Charles Babbage"), ("Subject", "Generated test PDF")],
    )
}

// --- CBZ ---

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

/// Appends `(value, width in bytes)` fields, little-endian.
fn le(out: &mut Vec<u8>, fields: &[(u32, usize)]) {
    for &(v, n) in fields {
        out.extend_from_slice(&v.to_le_bytes()[..n]);
    }
}

/// A zip archive with stored (uncompressed) entries, in the given order.
pub fn write_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let (len, off, crc, nlen) = (data.len() as u32, out.len() as u32, crc32(data), name.len() as u32);
        // Shared by both headers: version 20, no flags, stored, time 0, date 0x21, CRC,
        // compressed and plain size, name length.
        let common = [(20, 2), (0, 2), (0, 2), (0, 2), (0x21, 2), (crc, 4), (len, 4), (len, 4), (nlen, 2)];
        le(&mut out, &[(0x0403_4b50, 4)]);
        le(&mut out, &common);
        le(&mut out, &[(0, 2)]); // extra length
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        le(&mut central, &[(0x0201_4b50, 4), (20, 2)]); // signature, made by version 20
        le(&mut central, &common);
        // Extra, comment, disk, internal attributes, external attributes, header offset.
        le(&mut central, &[(0, 2), (0, 2), (0, 2), (0, 2), (0, 4), (off, 4)]);
        central.extend_from_slice(name.as_bytes());
    }
    let (count, size, cd_off) = (entries.len() as u32, central.len() as u32, out.len() as u32);
    out.extend_from_slice(&central);
    // End of central directory: signature, disk numbers, entry counts, size, offset, comment.
    le(&mut out, &[(0x0605_4b50, 4), (0, 2), (0, 2), (count, 2), (count, 2), (size, 4), (cd_off, 4), (0, 2)]);
    out
}

/// An 8-bit gray PNG.
pub fn png_gray(w: usize, h: usize, px: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w as u32, h as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().expect("png header");
        wr.write_image_data(px).expect("png data");
        wr.finish().expect("png finish");
    }
    buf
}

fn fill(px: &mut [u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize, v: u8) {
    let h = px.len() / w;
    for y in y0.min(h)..y1.min(h) {
        for x in x0.min(w)..x1.min(w) {
            px[y * w + x] = v;
        }
    }
}

/// Draws a seven-segment digit with its top-left corner at `(x, y)`, `dw` x `dh` pixels.
fn digit(px: &mut [u8], w: usize, x: usize, y: usize, dw: usize, dh: usize, d: u32) {
    let t = dw / 6;
    let half = dh / 2;
    // Segments lit per digit, bit 0 = a .. bit 6 = g (anything above 9 draws a 9).
    const SEGMENTS: [u8; 10] = [0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f];
    let segs = SEGMENTS[(d as usize).min(9)];
    let rects = [
        (x, y, x + dw, y + t),                         // a
        (x + dw - t, y, x + dw, y + half),             // b
        (x + dw - t, y + half, x + dw, y + dh),        // c
        (x, y + dh - t, x + dw, y + dh),               // d
        (x, y + half, x + t, y + dh),                  // e
        (x, y, x + t, y + half),                       // f
        (x, y + half - t / 2, x + dw, y + half + t / 2), // g
    ];
    for (k, r) in rects.into_iter().enumerate() {
        if segs >> k & 1 != 0 {
            fill(px, w, r.0, r.1, r.2, r.3, 0);
        }
    }
}

/// Size of a generated comic page.
pub const COMIC_W: usize = 600;
/// Size of a generated comic page.
pub const COMIC_H: usize = 900;

/// Comic page `n` (1-based): a frame, the number `n` in big seven-segment digits and a
/// bar at the top whose length is `40 * n` pixels (x 20..20+40n, y 20..50), so tests can
/// read the page number from pixels.
pub fn comic_page(n: usize) -> Vec<u8> {
    let (w, h) = (COMIC_W, COMIC_H);
    let mut px = vec![255u8; w * h];
    fill(&mut px, w, 0, 0, w, 8, 0);
    fill(&mut px, w, 0, h - 8, w, h, 0);
    fill(&mut px, w, 0, 0, 8, h, 0);
    fill(&mut px, w, w - 8, 0, w, h, 0);
    fill(&mut px, w, 20, 20, 20 + 40 * n, 50, 0);
    let s = n.to_string();
    let dw = 150;
    let gap = 40;
    let total = s.len() * dw + (s.len() - 1) * gap;
    let mut x = (w - total.min(w)) / 2;
    for ch in s.chars() {
        digit(&mut px, w, x, 300, dw, 300, ch.to_digit(10).unwrap_or(8));
        x += dw + gap;
    }
    // A gray band, so contrast/gamma changes are visible.
    fill(&mut px, w, 40, 700, w - 40, 760, 128);
    png_gray(w, h, &px)
}

/// The standard fixture CBZ: pages 1..=`pages` stored in shuffled order with mixed-case
/// names in a subfolder (`pages/p10.png`, `pages/P1.png`, ...), plus `ComicInfo.xml`,
/// a `__MACOSX/` resource fork and a hidden `.DS_Store`-style file that must be ignored.
pub fn fixture_cbz(pages: usize) -> Vec<u8> {
    let mut order: Vec<usize> = (1..=pages).collect();
    // Deterministic shuffle.
    for i in (1..order.len()).rev() {
        let j = (i * 7 + 3) % (i + 1);
        order.swap(i, j);
    }
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    entries.push(("ComicInfo.xml".into(), b"<ComicInfo><Title>Fixture comic</Title></ComicInfo>".to_vec()));
    for n in order {
        let name = if n == 1 { "pages/P1.png".to_string() } else { format!("pages/p{n}.png") };
        entries.push((name, comic_page(n)));
    }
    entries.push(("__MACOSX/pages/._p1.png".into(), b"\x00\x05\x16\x07 not an image".to_vec()));
    entries.push(("pages/.hidden.png".into(), b"not an image either".to_vec()));
    let refs: Vec<(&str, Vec<u8>)> = entries.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    write_zip(&refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_known_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn pdf_is_well_formed_text() {
        let pdf = fixture_pdf();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.contains("/Outlines") && s.contains("/URI (https://example.org/)"));
        assert!(s.trim_end().ends_with("%%EOF"));
        // xref offsets (bytes) point at "N 0 obj".
        let tail = String::from_utf8_lossy(&pdf[pdf.len() - 40..]).into_owned();
        let xref_at: usize = tail.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(pdf[xref_at..].starts_with(b"xref\n"));
        let entry = std::str::from_utf8(&pdf[xref_at..xref_at + 80]).unwrap().lines().nth(3).unwrap();
        let off: usize = entry[..10].parse().unwrap();
        assert!(pdf[off..].starts_with(b"1 0 obj"));
        assert!(pdf.len() < 200_000);
    }
}
