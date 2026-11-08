//! Line breaking.
//!
//! - [`break_opportunities`]: UAX #14 break positions in a string (`unicode-linebreak`).
//! - The box/glue/penalty model ([`Item`]) of Knuth and Plass with two breakers on the
//!   same input: [`knuth_plass`] (total fit: the least "demerits" over the whole
//!   paragraph, so lines are evenly loose and hyphens are rare) and [`greedy_items`]
//!   (first fit). [`break_items`] runs total fit and falls back to first fit when no
//!   feasible set of breaks exists (a word wider than the line).
//! - Hyphenation points enter as flagged [`Item::Penalty`]s with the hyphen's width
//!   ([`push_word`] does it with any [`Hyphenator`]).
//! - [`wrap`]: plain text to lines (UI text, dialogs), with optional hyphenation.
//!
//! The layout engine (`ink-html`) builds items from its inline runs, breaks them here,
//! and maps the returned item positions back to its own text.

use crate::hyphen::{split_at_points, Hyphenator};
use std::ops::Range;

/// Kind of break allowed after a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Break {
    /// A line may break here.
    Allowed,
    /// A line must break here (newline, paragraph separator).
    Mandatory,
}

/// Byte offsets where a line may or must break (UAX #14). The end of the text is
/// always reported as mandatory.
pub fn break_opportunities(text: &str) -> Vec<(usize, Break)> {
    unicode_linebreak::linebreaks(text)
        .map(|(i, b)| {
            let kind = match b {
                unicode_linebreak::BreakOpportunity::Mandatory => Break::Mandatory,
                unicode_linebreak::BreakOpportunity::Allowed => Break::Allowed,
            };
            (i, kind)
        })
        .collect()
}

/// A penalty at or below this forces a break.
pub const FORCED: f32 = -10_000.0;
/// A penalty at or above this forbids a break.
pub const FORBIDDEN: f32 = 10_000.0;
/// Stretch of the glue that fills the last line of a paragraph.
const FILL: f32 = 100_000.0;

/// One element of a paragraph in the box/glue/penalty model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Item {
    /// Unbreakable content of fixed width: a word, a word piece, an inline image.
    Box {
        /// Width in pixels.
        width: f32,
    },
    /// Space between boxes. A line may break at glue that follows a box; glue at the
    /// start of a line is dropped.
    Glue {
        /// Natural width.
        width: f32,
        /// How much it may grow.
        stretch: f32,
        /// How much it may shrink.
        shrink: f32,
    },
    /// A possible break with a cost.
    Penalty {
        /// Added to the line only when it breaks here (the hyphen).
        width: f32,
        /// Cost; [`FORCED`] forces the break, [`FORBIDDEN`] forbids it.
        penalty: f32,
        /// Hyphen breaks: two flagged breaks in a row cost extra.
        flagged: bool,
    },
}

impl Item {
    /// A box.
    pub fn word(width: f32) -> Item {
        Item::Box { width }
    }

    /// An inter-word space for justified text: stretches by 1/2 and shrinks by 1/3.
    pub fn space(width: f32) -> Item {
        Item::Glue { width, stretch: width / 2.0, shrink: width / 3.0 }
    }

    /// A hyphenation point with a visible hyphen `width` wide.
    pub fn hyphen(width: f32) -> Item {
        Item::Penalty { width, penalty: 50.0, flagged: true }
    }

    /// A forced line break (`<br>`).
    pub fn forced_break() -> Item {
        Item::Penalty { width: 0.0, penalty: FORCED, flagged: false }
    }

    /// (width, stretch, shrink) the item adds to a line it is inside of.
    fn dims(&self) -> (f32, f32, f32) {
        match *self {
            Item::Box { width } => (width, 0.0, 0.0),
            Item::Glue { width, stretch, shrink } => (width, stretch, shrink),
            Item::Penalty { .. } => (0.0, 0.0, 0.0),
        }
    }
}

/// Ends a paragraph: drops trailing glue, adds glue that fills the last line and a forced
/// break.
pub fn finish_paragraph(items: &mut Vec<Item>) {
    while matches!(items.last(), Some(Item::Glue { .. })) {
        items.pop();
    }
    items.push(Item::Penalty { width: 0.0, penalty: FORBIDDEN, flagged: false });
    items.push(Item::Glue { width: 0.0, stretch: FILL, shrink: 0.0 });
    items.push(Item::Penalty { width: 0.0, penalty: FORCED, flagged: true });
}

/// Appends `word` as boxes split at its hyphenation points with hyphen penalties
/// between them; `measure` gives the width of a piece. Returns the pieces in order (one
/// per pushed box).
pub fn push_word<'w>(
    items: &mut Vec<Item>,
    word: &'w str,
    hyphenator: &dyn Hyphenator,
    hyphen_width: f32,
    measure: &mut dyn FnMut(&str) -> f32,
) -> Vec<&'w str> {
    let pieces = split_at_points(word, &hyphenator.hyphenate(word));
    for (i, p) in pieces.iter().enumerate() {
        if i > 0 {
            // A piece ending in an explicit hyphen breaks without adding another one.
            let w = if pieces[i - 1].ends_with('-') { 0.0 } else { hyphen_width };
            items.push(Item::hyphen(w));
        }
        items.push(Item::word(measure(p)));
    }
    pieces
}

/// A chosen break.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineBreak {
    /// Item where the line ends (a glue or a penalty; the line holds the items after the
    /// previous break up to this one, the item itself excluded).
    pub position: usize,
    /// Adjustment ratio of the line: 0 = natural spacing, 1 = glue fully stretched,
    /// -1 = fully shrunk. Overfull first-fit lines are below -1.
    pub ratio: f32,
    /// The line ends with an added hyphen.
    pub hyphen: bool,
}

/// Costs of the total-fit breaker (TeX's names in brackets).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BreakParams {
    /// Largest adjustment ratio a line may have (TeX `\tolerance` 200 is about 1.26).
    pub tolerance: f32,
    /// Added to every line's badness (`\linepenalty`).
    pub line_penalty: f32,
    /// Two hyphenated lines in a row (`\doublehyphendemerits`).
    pub flagged_demerits: f32,
    /// Adjacent lines of very different tightness (`\adjdemerits`).
    pub fitness_demerits: f32,
}

impl Default for BreakParams {
    fn default() -> Self {
        // Narrow e-reader lines need more tolerance than TeX's print defaults.
        BreakParams { tolerance: 2.0, line_penalty: 10.0, flagged_demerits: 3000.0, fitness_demerits: 100.0 }
    }
}

struct Sums {
    w: Vec<f32>,
    y: Vec<f32>,
    z: Vec<f32>,
}

impl Sums {
    /// Prefix sums of widths, stretch and shrink over `items[..i]`.
    fn new(items: &[Item]) -> Self {
        let n = items.len();
        let (mut w, mut y, mut z) = (vec![0.0; n + 1], vec![0.0; n + 1], vec![0.0; n + 1]);
        for (i, it) in items.iter().enumerate() {
            let (dw, dy, dz) = it.dims();
            w[i + 1] = w[i] + dw;
            y[i + 1] = y[i] + dy;
            z[i + 1] = z[i] + dz;
        }
        Sums { w, y, z }
    }

    /// Totals at the start of the line after a break at `b`: glue and penalties right
    /// after the break are dropped.
    fn after_break(&self, items: &[Item], b: usize) -> (f32, f32, f32) {
        let mut k = b + usize::from(matches!(items.get(b), Some(Item::Penalty { .. } | Item::Glue { .. })));
        while k < items.len() {
            match items[k] {
                Item::Box { .. } => break,
                Item::Penalty { penalty, .. } if penalty <= FORCED => break,
                _ => k += 1,
            }
        }
        (self.w[k], self.y[k], self.z[k])
    }
}

/// Legal breakpoint at `b`: (penalty width, penalty, flagged).
fn breakpoint(items: &[Item], b: usize) -> Option<(f32, f32, bool)> {
    match items[b] {
        Item::Penalty { width, penalty, flagged } if penalty < FORBIDDEN => Some((width, penalty, flagged)),
        Item::Glue { .. } if b > 0 && matches!(items[b - 1], Item::Box { .. }) => Some((0.0, 0.0, false)),
        _ => None,
    }
}

fn ratio(sums: &Sums, start: (f32, f32, f32), b: usize, pen_w: f32, target: f32) -> f32 {
    let len = sums.w[b] - start.0 + pen_w;
    if len < target {
        let y = sums.y[b] - start.1;
        if y > 0.0 {
            (target - len) / y
        } else {
            f32::INFINITY
        }
    } else if len > target {
        let z = sums.z[b] - start.2;
        if z > 0.0 {
            (target - len) / z
        } else {
            f32::NEG_INFINITY
        }
    } else {
        0.0
    }
}

fn fitness(r: f32) -> u8 {
    if r < -0.5 {
        0
    } else if r <= 0.5 {
        1
    } else if r <= 1.0 {
        2
    } else {
        3
    }
}

#[derive(Clone, Copy, Debug)]
struct Node {
    position: usize,
    line: usize,
    fitness: u8,
    totals: (f32, f32, f32),
    demerits: f32,
    ratio: f32,
    prev: Option<usize>,
}

/// Total-fit line breaking. `line_width(i)` is the width of line `i` (0-based), so
/// indents and shaped paragraphs work. The paragraph must end with a forced break
/// ([`finish_paragraph`]). Returns `None` when no set of breaks keeps every line within
/// `params.tolerance` without overfull lines.
pub fn knuth_plass(items: &[Item], line_width: &dyn Fn(usize) -> f32, params: &BreakParams) -> Option<Vec<LineBreak>> {
    if items.is_empty() {
        return Some(Vec::new());
    }
    let sums = Sums::new(items);
    let mut nodes = vec![Node {
        position: 0,
        line: 0,
        fitness: 1,
        // The paragraph start is not a break: nothing is dropped before item 0.
        totals: (0.0, 0.0, 0.0),
        demerits: 0.0,
        ratio: 0.0,
        prev: None,
    }];
    let mut active: Vec<usize> = vec![0];
    let mut last_forced_ok = false;
    for b in 0..items.len() {
        let Some((pen_w, pen, flagged)) = breakpoint(items, b) else { continue };
        let mut best: [Option<(f32, usize, f32)>; 4] = [None; 4];
        let mut i = 0;
        while i < active.len() {
            let a = nodes[active[i]];
            let r = ratio(&sums, a.totals, b, pen_w, line_width(a.line));
            if (-1.0..=params.tolerance).contains(&r) {
                let badness = 100.0 * r.abs().powi(3);
                let base = (params.line_penalty + badness).powi(2);
                let mut d = if pen >= 0.0 {
                    base + pen * pen
                } else if pen > FORCED {
                    base - pen * pen
                } else {
                    base
                };
                let prev_flagged =
                    a.prev.is_some() && matches!(items[a.position], Item::Penalty { flagged: true, .. });
                if flagged && prev_flagged {
                    d += params.flagged_demerits;
                }
                let f = fitness(r);
                if a.prev.is_some() && (f as i32 - a.fitness as i32).abs() > 1 {
                    d += params.fitness_demerits;
                }
                let total = a.demerits + d;
                if best[f as usize].is_none_or(|(t, _, _)| total < t) {
                    best[f as usize] = Some((total, active[i], r));
                }
            }
            if r < -1.0 || pen <= FORCED {
                active.remove(i);
            } else {
                i += 1;
            }
        }
        let totals = sums.after_break(items, b);
        for (f, cand) in best.iter().enumerate() {
            if let Some((demerits, from, r)) = *cand {
                let line = nodes[from].line + 1;
                nodes.push(Node { position: b, line, fitness: f as u8, totals, demerits, ratio: r, prev: Some(from) });
                active.push(nodes.len() - 1);
                if pen <= FORCED {
                    last_forced_ok = b == items.len() - 1;
                }
            }
        }
        if active.is_empty() {
            return None;
        }
    }
    if !last_forced_ok {
        return None;
    }
    let end = items.len() - 1;
    let best = active
        .iter()
        .copied()
        .filter(|i| nodes[*i].position == end && nodes[*i].prev.is_some())
        .min_by(|a, b| nodes[*a].demerits.total_cmp(&nodes[*b].demerits))?;
    let mut out = Vec::new();
    let mut cur = Some(best);
    while let Some(i) = cur {
        let n = nodes[i];
        if n.prev.is_none() {
            break;
        }
        let hyphen = matches!(items[n.position], Item::Penalty { width, flagged: true, .. } if width > 0.0);
        out.push(LineBreak { position: n.position, ratio: n.ratio, hyphen });
        cur = n.prev;
    }
    out.reverse();
    Some(out)
}

/// First-fit line breaking on the same items: each line takes as much as fits at
/// natural width (shrinking if needed); a box wider than the line gets a line of its
/// own (overfull, ratio below -1).
pub fn greedy_items(items: &[Item], line_width: &dyn Fn(usize) -> f32) -> Vec<LineBreak> {
    let sums = Sums::new(items);
    let mut out: Vec<LineBreak> = Vec::new();
    let mut start = (0.0, 0.0, 0.0);
    let mut feasible: Option<(usize, f32, bool)> = None;
    let mut b = 0;
    while b < items.len() {
        let Some((pen_w, pen, flagged)) = breakpoint(items, b) else {
            b += 1;
            continue;
        };
        let target = line_width(out.len());
        let r = ratio(&sums, start, b, pen_w, target);
        let hyphen = flagged && pen_w > 0.0;
        if r >= -1.0 {
            if pen <= FORCED {
                out.push(LineBreak { position: b, ratio: r, hyphen });
                start = sums.after_break(items, b);
                feasible = None;
            } else {
                feasible = Some((b, r, hyphen));
            }
            b += 1;
        } else if let Some((f, fr, fh)) = feasible.take() {
            out.push(LineBreak { position: f, ratio: fr, hyphen: fh });
            start = sums.after_break(items, f);
            b = f + 1;
        } else {
            // Nothing fits: an overfull line ends here.
            out.push(LineBreak { position: b, ratio: r, hyphen });
            start = sums.after_break(items, b);
            b += 1;
        }
    }
    out
}

/// Total fit when possible, else first fit.
pub fn break_items(items: &[Item], line_width: &dyn Fn(usize) -> f32, params: &BreakParams) -> Vec<LineBreak> {
    knuth_plass(items, line_width, params).unwrap_or_else(|| greedy_items(items, line_width))
}

/// A line of [`wrap`] output.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// Bytes of the source text on this line (spaces at the break excluded).
    pub range: Range<usize>,
    /// Natural width of the line, the added hyphen included.
    pub width: f32,
    /// The line ends inside a word: draw a hyphen after it.
    pub hyphen: bool,
}

/// Wraps plain text into lines at most `max_width` wide. Paragraphs end at `\n`; words
/// are split at whitespace and, when they do not fit, at the hyphenator's points.
/// `optimal` uses total fit ([`knuth_plass`]), else first fit.
pub fn wrap(
    text: &str,
    max_width: f32,
    measure: &mut dyn FnMut(&str) -> f32,
    hyphenator: &dyn Hyphenator,
    optimal: bool,
) -> Vec<TextLine> {
    let space = measure(" ");
    let hyphen_w = measure("-");
    let mut lines = Vec::new();
    let mut para_start = 0;
    for para in text.split('\n') {
        let base = para_start;
        para_start += para.len() + 1;
        // Items and the byte range of each box.
        let mut items: Vec<Item> = Vec::new();
        let mut spans: Vec<Option<Range<usize>>> = Vec::new();
        let mut pos = 0;
        for word in para.split_whitespace() {
            let off = para[pos..].find(word).map(|i| i + pos).unwrap_or(pos);
            pos = off + word.len();
            if !items.is_empty() {
                // Ragged right: generous stretch, no shrink.
                items.push(Item::Glue { width: space, stretch: max_width / 3.0, shrink: 0.0 });
                spans.push(None);
            }
            let mut start = off;
            let pieces = push_word(&mut items, word, hyphenator, hyphen_w, measure);
            for (i, p) in pieces.iter().enumerate() {
                if i > 0 {
                    spans.push(None); // the hyphen penalty
                }
                spans.push(Some(base + start..base + start + p.len()));
                start += p.len();
            }
        }
        if items.is_empty() {
            lines.push(TextLine { range: base..base, width: 0.0, hyphen: false });
            continue;
        }
        let n = items.len();
        finish_paragraph(&mut items);
        spans.resize(items.len(), None);
        let width = |_: usize| max_width;
        let breaks = if optimal {
            break_items(&items, &width, &BreakParams { tolerance: 10.0, ..BreakParams::default() })
        } else {
            greedy_items(&items, &width)
        };
        let mut first = 0;
        for br in breaks {
            let mut boxes = spans[first..br.position.min(n)].iter().flatten();
            if let Some(a) = boxes.next() {
                let z = boxes.next_back().unwrap_or(a);
                let w: f32 = items[first..br.position]
                    .iter()
                    .skip_while(|i| !matches!(i, Item::Box { .. }))
                    .map(|i| i.dims().0)
                    .sum::<f32>()
                    + if br.hyphen { hyphen_w } else { 0.0 };
                lines.push(TextLine { range: a.start..z.end, width: w, hyphen: br.hyphen });
            }
            first = br.position + 1;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyphen::{Language, NoHyphenation, PatternHyphenator};

    #[test]
    fn opportunities() {
        let b = break_opportunities("two words\nnext");
        assert_eq!(b, vec![(4, Break::Allowed), (10, Break::Mandatory), (14, Break::Mandatory)]);
    }

    /// Monospace measure: 10 px per char.
    fn mono(s: &str) -> f32 {
        s.chars().count() as f32 * 10.0
    }

    fn paragraph(words: &[&str], h: &dyn Hyphenator) -> Vec<Item> {
        let mut items = Vec::new();
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                items.push(Item::space(10.0));
            }
            push_word(&mut items, w, h, 10.0, &mut mono);
        }
        finish_paragraph(&mut items);
        items
    }

    fn line_lengths(items: &[Item], breaks: &[LineBreak]) -> Vec<f32> {
        let mut out = Vec::new();
        let mut first = 0;
        for b in breaks {
            let w: f32 = items[first..b.position].iter().skip_while(|i| !matches!(i, Item::Box { .. })).map(|i| i.dims().0).sum();
            out.push(w + if b.hyphen { 10.0 } else { 0.0 });
            first = b.position + 1;
        }
        out
    }

    #[test]
    fn total_fit_balances_lines_better_than_first_fit() {
        // The classic case: first fit leaves a very short middle line.
        let words = ["aaa", "bb", "cc", "ddddd", "eee", "ff", "gggg", "h", "iiiiii", "jj", "kkk"];
        let items = paragraph(&words, &NoHyphenation);
        let w = |_| 160.0;
        let kp = knuth_plass(&items, &w, &BreakParams { tolerance: 5.0, ..Default::default() }).unwrap();
        let ff = greedy_items(&items, &w);
        let spread = |lens: &[f32]| {
            let body = &lens[..lens.len() - 1];
            body.iter().cloned().fold(f32::MIN, f32::max) - body.iter().cloned().fold(f32::MAX, f32::min)
        };
        let (kl, fl) = (line_lengths(&items, &kp), line_lengths(&items, &ff));
        assert!(kl.iter().chain(&fl).all(|l| *l <= 160.0), "{kl:?} {fl:?}");
        assert!(spread(&kl) <= spread(&fl), "total fit {kl:?} first fit {fl:?}");
        // The last break is the paragraph end.
        assert_eq!(kp.last().unwrap().position, items.len() - 1);
        assert_eq!(ff.last().unwrap().position, items.len() - 1);
    }

    #[test]
    fn hyphenation_points_are_used_when_they_help() {
        let h = PatternHyphenator::new(&Language::new("en-us")).unwrap();
        // "hyphenation" (110 px) does not fit after "the" on a 120 px line; with
        // hyphenation "the hy-" (60 + hyphen) can end the first line.
        let items = paragraph(&["the", "hyphenation", "algorithm"], &h);
        let breaks = break_items(&items, &|_| 120.0, &BreakParams::default());
        assert!(breaks.iter().any(|b| b.hyphen), "{breaks:?}");
        for l in line_lengths(&items, &breaks) {
            assert!(l <= 120.0, "{l}");
        }
        // Without hyphenation there is no hyphen.
        let plain = paragraph(&["the", "hyphenation", "algorithm"], &NoHyphenation);
        assert!(break_items(&plain, &|_| 120.0, &BreakParams::default()).iter().all(|b| !b.hyphen));
    }

    #[test]
    fn overlong_words_fall_back_to_first_fit() {
        let items = paragraph(&["a", "supercalifragilistic", "b"], &NoHyphenation);
        assert!(knuth_plass(&items, &|_| 100.0, &BreakParams::default()).is_none());
        let breaks = break_items(&items, &|_| 100.0, &BreakParams::default());
        assert_eq!(breaks.len(), 3);
        assert!(breaks[1].ratio < -1.0, "the long word is overfull: {breaks:?}");
    }

    #[test]
    fn forced_breaks_and_line_widths() {
        let mut items = vec![Item::word(30.0), Item::space(10.0), Item::word(30.0), Item::forced_break(), Item::word(30.0)];
        finish_paragraph(&mut items);
        // First line indented (narrower).
        let w = |i: usize| if i == 0 { 50.0 } else { 200.0 };
        let kp = break_items(&items, &w, &BreakParams::default());
        assert_eq!(kp.iter().map(|b| b.position).collect::<Vec<_>>(), vec![1, 3, items.len() - 1]);
        let ff = greedy_items(&items, &w);
        assert_eq!(ff.iter().map(|b| b.position).collect::<Vec<_>>(), vec![1, 3, items.len() - 1]);
        assert!(knuth_plass(&[], &w, &BreakParams::default()).unwrap().is_empty());
    }

    #[test]
    fn wrap_plain_text() {
        let text = "The quick brown fox jumps over the lazy dog\n\nEnd";
        let lines = wrap(text, 100.0, &mut mono, &NoHyphenation, false);
        let shown: Vec<&str> = lines.iter().map(|l| &text[l.range.clone()]).collect();
        assert_eq!(shown, vec!["The quick", "brown fox", "jumps over", "the lazy", "dog", "", "End"]);
        assert!(lines.iter().all(|l| l.width <= 100.0));
        let h = PatternHyphenator::new(&Language::new("en")).unwrap();
        let lines = wrap("unbelievably", 80.0, &mut mono, &h, false);
        assert!(lines.len() >= 2 && lines[0].hyphen, "{lines:?}");
        let joined: String = lines.iter().map(|l| &"unbelievably"[l.range.clone()]).collect();
        assert_eq!(joined, "unbelievably");
        let optimal = wrap(text, 100.0, &mut mono, &NoHyphenation, true);
        assert_eq!(optimal.iter().map(|l| &text[l.range.clone()]).collect::<String>().replace(' ', ""), shown.concat().replace(' ', ""));
    }
}
