//! Page text with character boxes: search, selection and highlight rectangles.
//!
//! The text of a page is the characters MuPDF's structured-text device found, in its
//! reading order, each with a bounding box in page space. For search and copying, lines
//! are joined with one space, runs of white space collapse to one space, and a hyphen at
//! the end of a line followed by a lower-case letter is dropped (`hy-` + `phen` matches
//! "hyphen").

use crate::geom::FRect;
use crate::raw::RawChar;
use std::ops::Range;

/// One character of page text.
pub type TextChar = RawChar;

/// The text of one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageText {
    /// 0-based page index.
    pub page: usize,
    /// Characters in reading order.
    pub chars: Vec<TextChar>,
}

/// One entry of the normalized stream used for matching.
#[derive(Clone, Copy, Debug)]
struct Norm {
    /// Lower-cased character used for matching.
    lower: char,
    /// The character as shown (`None` for the 2nd.. chars of a lower-case expansion).
    orig: Option<char>,
    /// Index into `chars`, `None` for a space inserted between lines.
    src: Option<usize>,
}

/// A match of a search on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct TextMatch {
    /// Character range in [`PageText::chars`].
    pub chars: Range<usize>,
    /// Text around the match.
    pub excerpt: String,
    /// Byte range of the match inside `excerpt`.
    pub matched: Range<usize>,
}

impl PageText {
    /// Builds the text of `page` from raw characters.
    pub fn new(page: usize, chars: Vec<TextChar>) -> Self {
        PageText { page, chars }
    }

    /// Approximate heap size (for the cache).
    pub fn bytes(&self) -> usize {
        self.chars.len() * std::mem::size_of::<TextChar>() + 64
    }

    fn normalized(&self) -> Vec<Norm> {
        let mut out: Vec<Norm> = Vec::with_capacity(self.chars.len() + 16);
        for (i, ch) in self.chars.iter().enumerate() {
            // The first character of the next line, when this one ends a line.
            let next_line = self.chars.get(i + 1).filter(|n| n.line != ch.line);
            // A soft hyphen never shows; a hyphen at a line end before a lower-case letter
            // is dropped and the word joined.
            if ch.c == '\u{ad}' || ch.c == '-' && next_line.is_some_and(|n| n.c.is_lowercase()) {
                continue;
            }
            if ch.c.is_whitespace() {
                if out.last().is_some_and(|l| l.lower != ' ') {
                    out.push(Norm { lower: ' ', orig: Some(' '), src: Some(i) });
                }
            } else {
                for (k, lc) in ch.c.to_lowercase().enumerate() {
                    out.push(Norm { lower: lc, orig: (k == 0).then_some(ch.c), src: Some(i) });
                }
            }
            if next_line.is_some() && out.last().is_some_and(|l| l.lower != ' ') {
                out.push(Norm { lower: ' ', orig: Some(' '), src: None });
            }
        }
        while out.last().is_some_and(|l| l.lower == ' ') {
            out.pop();
        }
        out
    }

    /// The page text as one string (lines joined, white space collapsed).
    pub fn plain(&self) -> String {
        self.normalized().iter().filter_map(|n| n.orig).collect()
    }

    /// The text of a character range, normalized like [`PageText::plain`].
    pub fn text_of(&self, range: Range<usize>) -> String {
        text_of(&self.normalized(), range)
    }

    /// The text under each of `rects` ([`PageText::range_in`] of each, then
    /// [`PageText::text_of`]; empty where no character is), normalizing the page once.
    pub fn texts_under(&self, rects: &[FRect]) -> Vec<String> {
        let norm = self.normalized();
        rects.iter().map(|r| self.range_in(std::slice::from_ref(r)).map(|range| text_of(&norm, range)).unwrap_or_default()).collect()
    }

    /// Every case-insensitive match of `query` (white space in the query matches any run
    /// of white space or a line break), with an excerpt of about `context` characters on
    /// each side.
    pub fn find(&self, query: &str, context: usize) -> Vec<TextMatch> {
        let q: Vec<char> = query.split_whitespace().collect::<Vec<_>>().join(" ").chars().flat_map(|c| c.to_lowercase()).collect();
        if q.is_empty() {
            return Vec::new();
        }
        let norm = self.normalized();
        let mut out = Vec::new();
        let mut i = 0;
        while i + q.len() <= norm.len() {
            if norm[i..i + q.len()].iter().zip(&q).all(|(n, c)| n.lower == *c) {
                let srcs = || norm[i..i + q.len()].iter().filter_map(|n| n.src);
                if let (Some(a), Some(b)) = (srcs().min(), srcs().max()) {
                    let shown = |r: Range<usize>| norm[r].iter().filter_map(|n| n.orig).collect::<String>();
                    let lo = i.saturating_sub(context);
                    let start = shown(lo..i).len();
                    let matched = start..start + shown(i..i + q.len()).len();
                    let excerpt = shown(lo..(i + q.len() + context).min(norm.len()));
                    out.push(TextMatch { chars: a..b + 1, excerpt, matched });
                }
                i += q.len();
            } else {
                i += 1;
            }
        }
        out
    }

    /// One rectangle per line for a character range (page space).
    pub fn rects(&self, range: Range<usize>) -> Vec<FRect> {
        let mut out: Vec<(u32, FRect)> = Vec::new();
        for ch in self.chars.get(range).unwrap_or(&[]) {
            if ch.c.is_whitespace() && out.last().is_none_or(|(l, _)| *l != ch.line) {
                continue; // a line that would start with a space
            }
            match out.last_mut() {
                Some((l, r)) if *l == ch.line => *r = r.union(&ch.rect),
                _ => out.push((ch.line, ch.rect)),
            }
        }
        out.into_iter().map(|(_, r)| r).filter(|r| !r.is_empty()).collect()
    }

    /// The character nearest to `(x, y)` (page space): first the nearest line
    /// (vertically, then horizontally), then the nearest character on that line.
    pub fn nearest(&self, x: f32, y: f32) -> Option<usize> {
        let mut lines: Vec<(u32, FRect)> = Vec::new();
        for ch in self.chars.iter().filter(|c| !c.rect.is_empty()) {
            // Each line id appears once in `lines`; in reading order it is nearly always the
            // last one, so look from the end (linear instead of chars x lines).
            match lines.iter_mut().rev().find(|(l, _)| *l == ch.line) {
                Some((_, r)) => *r = r.union(&ch.rect),
                None => lines.push((ch.line, ch.rect)),
            }
        }
        let dy = |r: &FRect| if y < r.y0 { r.y0 - y } else if y > r.y1 { y - r.y1 } else { 0.0 };
        let dx = |r: &FRect| if x < r.x0 { r.x0 - x } else if x > r.x1 { x - r.x1 } else { 0.0 };
        let line = lines.iter().min_by(|a, b| (dy(&a.1), dx(&a.1)).partial_cmp(&(dy(&b.1), dx(&b.1))).unwrap_or(std::cmp::Ordering::Equal))?.0;
        self.chars
            .iter()
            .enumerate()
            .filter(|(_, c)| c.line == line && !c.rect.is_empty())
            .min_by(|(_, a), (_, b)| dx(&a.rect).total_cmp(&dx(&b.rect)).then(a.rect.distance(x, y, 1.0).total_cmp(&b.rect.distance(x, y, 1.0))))
            .map(|(i, _)| i)
    }

    /// Characters whose centre lies inside any of `rects` (page space), as one range from
    /// the first to the last.
    pub fn range_in(&self, rects: &[FRect]) -> Option<Range<usize>> {
        let first = self.chars.iter().position(|c| centre_in(c, rects))?;
        let last = self.chars.iter().rposition(|c| centre_in(c, rects)).unwrap_or(first);
        Some(first..last + 1)
    }

    /// The text of the characters whose centre lies inside any of `rects` (reading order,
    /// normalized like [`PageText::plain`]).
    pub fn text_in(&self, rects: &[FRect]) -> String {
        let chars = self.chars.iter().filter(|c| centre_in(c, rects)).copied().collect();
        PageText::new(self.page, chars).plain()
    }

    /// The character range around character `i` with about `chars` characters in total.
    pub fn window(&self, i: usize, chars: usize) -> Range<usize> {
        let n = self.chars.len();
        let half = chars / 2;
        let a = i.min(n).saturating_sub(half);
        let b = (a + chars).min(n);
        a.saturating_sub(chars.saturating_sub(b - a))..b
    }
}

/// True when the centre of `c` lies inside one of `rects` (grown by half a point).
fn centre_in(c: &TextChar, rects: &[FRect]) -> bool {
    let (cx, cy) = c.rect.center();
    rects.iter().any(|r| r.inflate(0.5).contains(cx, cy))
}

/// The text of the character range `range` from the normalized stream `norm`.
fn text_of(norm: &[Norm], range: Range<usize>) -> String {
    let mut out = String::new();
    for (k, n) in norm.iter().enumerate() {
        let inside = match n.src {
            Some(i) => range.contains(&i),
            None => {
                // A space between lines belongs to the range when both neighbours do.
                let prev = norm[..k].iter().rev().find_map(|m| m.src);
                let next = norm[k + 1..].iter().find_map(|m| m.src);
                matches!((prev, next), (Some(a), Some(b)) if range.contains(&a) && range.contains(&b))
            }
        };
        if inside {
            if let Some(c) = n.orig {
                out.push(c);
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays out `lines` with 10 x 12 point character boxes.
    fn page(lines: &[&str]) -> PageText {
        let mut chars = Vec::new();
        for (l, text) in lines.iter().enumerate() {
            for (k, c) in text.chars().enumerate() {
                let x = 10.0 * k as f32;
                let y = 20.0 * l as f32;
                chars.push(RawChar { c, rect: FRect::new(x, y, x + 10.0, y + 12.0), line: l as u32 });
            }
        }
        PageText::new(0, chars)
    }

    #[test]
    fn plain_joins_lines_and_hyphens() {
        let p = page(&["The quick  brown", "fox jumps over hy-", "phenated words"]);
        assert_eq!(p.plain(), "The quick brown fox jumps over hyphenated words");
    }

    #[test]
    fn find_is_case_insensitive_and_spans_lines() {
        let p = page(&["The quick brown", "Fox jumps over hy-", "phenated words"]);
        let m = p.find("brown fox", 4);
        assert_eq!(m.len(), 1);
        let r = &m[0];
        assert_eq!(&r.excerpt[r.matched.clone()], "brown Fox");
        assert_eq!(p.rects(r.chars.clone()).len(), 2);
        let h = p.find("HYPHENATED", 0);
        assert_eq!(h.len(), 1);
        assert_eq!(&h[0].excerpt[h[0].matched.clone()], "hyphenated");
        assert_eq!(p.find("o", 0).len(), 4);
        assert!(p.find("   ", 0).is_empty());
        assert!(p.find("zebra", 0).is_empty());
    }

    #[test]
    fn rects_nearest_and_text_of() {
        let p = page(&["abc def", "ghi"]);
        let rects = p.rects(0..10);
        assert_eq!(rects, vec![FRect::new(0.0, 0.0, 70.0, 12.0), FRect::new(0.0, 20.0, 30.0, 32.0)]);
        assert_eq!(p.nearest(45.0, 5.0), Some(4)); // 'd'
        assert_eq!(p.nearest(500.0, 25.0), Some(9)); // end of line 2
        assert_eq!(p.text_of(4..10), "def ghi");
        assert_eq!(p.range_in(&[FRect::new(0.0, 18.0, 100.0, 40.0)]), Some(7..10));
        assert_eq!(p.text_in(&[FRect::new(35.0, 0.0, 100.0, 40.0)]), "def");
        assert_eq!(
            p.texts_under(&[FRect::new(35.0, 0.0, 100.0, 12.0), FRect::new(0.0, 100.0, 9.0, 120.0), FRect::new(0.0, 18.0, 100.0, 40.0)]),
            vec!["def", "", "ghi"]
        );
        assert_eq!(p.window(5, 4), 3..7);
        assert_eq!(p.window(0, 100), 0..10);
    }
}
