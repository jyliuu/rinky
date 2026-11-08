//! Margin crop and column detection on a small gray rendering of a page.
//!
//! Both work on pixels, not on PDF drawing commands, so they also work for scanned pages
//! and comics. A pixel is "ink" when it is darker than [`INK`].

/// Gray level below which a pixel counts as ink.
pub const INK: u8 = 200;

/// A small 8-bit gray image (stride = width).
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    /// Width.
    pub w: usize,
    /// Height.
    pub h: usize,
    /// Pixels.
    pub px: Vec<u8>,
}

/// Pixel box `[x0, x1) x [y0, y1)`.
pub type PxBox = (usize, usize, usize, usize);

impl Gray {
    /// Rows `[y0, y1)` of the image (none when it has no width).
    fn rows(&self, y0: usize, y1: usize) -> std::slice::ChunksExact<'_, u8> {
        self.px[y0 * self.w..y1 * self.w].chunks_exact(self.w.max(1))
    }

    /// Ink pixels per row inside columns `[x0, x1)`.
    fn row_counts(&self, x0: usize, x1: usize) -> Vec<usize> {
        self.rows(0, self.h).map(|row| row[x0..x1].iter().filter(|&&p| p < INK).count()).collect()
    }

    /// Ink pixels per column inside rows `[y0, y1)`, summed row by row (memory order).
    fn col_counts(&self, y0: usize, y1: usize) -> Vec<usize> {
        let mut out = vec![0; self.w];
        for row in self.rows(y0, y1) {
            for (c, &p) in out.iter_mut().zip(row) {
                *c += usize::from(p < INK);
            }
        }
        out
    }
}

/// First and one-past-last index whose count is at least `min`.
fn span(counts: &[usize], min: usize) -> Option<(usize, usize)> {
    let a = counts.iter().position(|&c| c >= min)?;
    let b = counts.iter().rposition(|&c| c >= min)?;
    Some((a, b + 1))
}

/// Bounding box of the ink, ignoring rows and columns with fewer than 2 ink pixels
/// (specks). `None` for a blank page.
pub fn content_box(img: &Gray) -> Option<PxBox> {
    if img.w == 0 || img.h == 0 {
        return None;
    }
    let (y0, y1) = span(&img.row_counts(0, img.w), 2)?;
    let (x0, x1) = span(&img.col_counts(y0, y1), 2)?;
    // Rows again inside the found columns (a speck column outside does not widen it).
    let (y0b, y1b) = span(&img.row_counts(x0, x1), 1).unwrap_or((y0, y1));
    Some((x0, y0.max(y0b), x1, y1.min(y1b).max(y0.max(y0b) + 1)))
}

/// Cut positions (x, in pixels) that split `bx` into `n` columns. Each cut is the middle
/// of the widest gap (columns with at most 1 % of the box height in ink) within 15 % of
/// the box width around the even split; without a gap, the even split itself.
///
/// The columns span `span` (`[x0, x1)` in pixels: the crop box, or the whole page without
/// crop; it holds `bx`). The cuts increase strictly and every column is at least half an
/// even column of the span wide ([`min_column_px`]): with `n = 3` on a two-column page,
/// the first cut takes the gutter and the second falls back to its even split, instead of
/// both taking the gutter (which would leave a column 0 px wide). Empty when the span is
/// too narrow for `n` columns or `bx` does not fit the image.
pub fn column_cuts(img: &Gray, bx: PxBox, span: (usize, usize), n: usize) -> Vec<usize> {
    let (x0, y0, x1, y1) = bx;
    let (sx0, sx1) = span;
    if n < 2 || x1 > img.w || y1 > img.h || y1 <= y0 || x1 <= x0 || sx1 < sx0 + 2 * n {
        return Vec::new();
    }
    let counts = img.col_counts(y0, y1);
    let tol = ((y1 - y0) / 100).max(1);
    let width = x1 - x0;
    let reach = (width * 15 / 100).max(1);
    let min_w = min_column_px(sx1 - sx0, n);
    let mut cuts = Vec::new();
    let mut prev = sx0;
    for k in 1..n {
        let target = x0 + width * k / n;
        // Room for this column and for the n - k columns after the cut.
        let (lo_lim, hi_lim) = (prev + min_w, sx1 - min_w * (n - k));
        let lo = target.saturating_sub(reach).max(lo_lim).max(x0 + 1);
        let hi = (target + reach).min(hi_lim).min(x1.saturating_sub(1));
        // Widest run of low-ink columns in [lo, hi]; ties go to the one nearest target.
        let mut best: Option<(usize, usize)> = None; // (start, len)
        let mut x = lo;
        while x <= hi {
            if counts[x] <= tol {
                // The gap may reach past the search window: measure all of it.
                let mut s = x;
                while s > x0 + 1 && counts[s - 1] <= tol {
                    s -= 1;
                }
                let run_start = x;
                while x < x1 - 1 && counts[x] <= tol {
                    x += 1;
                }
                let len = x - s;
                // The scan above stops at x1 - 1 without moving when that column is low
                // ink; hi can be x1 - 1, so step past it or this loop never ends.
                let next = x.max(run_start + 1);
                let better = match best {
                    None => true,
                    Some((bs, bl)) => {
                        len > bl || (len == bl && (s + len / 2).abs_diff(target) < (bs + bl / 2).abs_diff(target))
                    }
                };
                if better {
                    best = Some((s, len));
                }
                x = next;
            } else {
                x += 1;
            }
        }
        let cut = match best {
            Some((s, len)) if len >= 2 => s + len / 2,
            _ => target,
        };
        let cut = cut.clamp(lo_lim, hi_lim);
        cuts.push(cut);
        prev = cut;
    }
    cuts
}

/// Narrowest column [`column_cuts`] makes: half an even column of a span `width` pixels
/// wide split `n` ways.
pub fn min_column_px(width: usize, n: usize) -> usize {
    (width / (2 * n.max(1))).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize, blocks: &[(usize, usize, usize, usize)]) -> Gray {
        let mut px = vec![255u8; w * h];
        for &(x0, y0, x1, y1) in blocks {
            for y in y0..y1 {
                for x in x0..x1 {
                    px[y * w + x] = 0;
                }
            }
        }
        Gray { w, h, px }
    }

    #[test]
    fn crop_finds_ink_and_ignores_specks() {
        let mut g = img(100, 200, &[(20, 30, 80, 150)]);
        g.px[5 * 100 + 5] = 0; // one speck in the corner
        assert_eq!(content_box(&g), Some((20, 30, 80, 150)));
        assert_eq!(content_box(&img(10, 10, &[])), None);
    }

    #[test]
    fn columns_split_at_the_gutter() {
        // Two text columns 10..45 and 55..90 with a gutter at 45..55.
        let g = img(100, 200, &[(10, 20, 45, 180), (55, 20, 90, 180)]);
        let cuts = column_cuts(&g, (10, 20, 90, 180), (0, 100), 2);
        assert_eq!(cuts, vec![50]);
        // Off-centre gutter at 60..66 is still found.
        let g = img(100, 200, &[(10, 20, 60, 180), (66, 20, 90, 180)]);
        assert_eq!(column_cuts(&g, (10, 20, 90, 180), (0, 100), 2), vec![63]);
        // No gutter: the even split.
        let g = img(100, 200, &[(10, 20, 90, 180)]);
        assert_eq!(column_cuts(&g, (10, 20, 90, 180), (0, 100), 2), vec![50]);
        assert!(column_cuts(&g, (10, 20, 90, 180), (0, 100), 1).is_empty());
    }

    #[test]
    fn more_columns_than_gutters_never_make_empty_columns() {
        // One gutter at 45..55 of a 0..100 box, asked for 3 and 4 columns: the cuts stay
        // strictly increasing and every column keeps at least half an even width.
        let g = img(100, 200, &[(0, 20, 45, 180), (55, 20, 100, 180)]);
        for n in [3, 4] {
            let cuts = column_cuts(&g, (0, 20, 100, 180), (0, 100), n);
            assert_eq!(cuts.len(), n - 1, "{n}: {cuts:?}");
            let mut edges = vec![0];
            edges.extend(&cuts);
            edges.push(100);
            let min = min_column_px(100, n);
            assert!(edges.windows(2).all(|e| e[1] >= e[0] + min), "{n}: {cuts:?}");
            assert!(cuts.contains(&50), "{n}: the gutter is still one of the cuts: {cuts:?}");
        }
        // A span too narrow for the columns: no cuts.
        assert!(column_cuts(&g, (0, 20, 5, 180), (0, 5), 3).is_empty());
        // Narrow ink at the left of a wide span: the columns still split the span, none
        // narrower than half an even column of it.
        let g = img(400, 200, &[(40, 20, 120, 180)]);
        let cuts = column_cuts(&g, (40, 20, 120, 180), (0, 400), 4);
        let mut edges = vec![0];
        edges.extend(&cuts);
        edges.push(400);
        assert!(edges.windows(2).all(|e| e[1] >= e[0] + 50), "{cuts:?}");
    }

    #[test]
    fn column_cuts_returns_when_the_last_box_column_is_gap() {
        // A box 2-8 px wide whose columns are all low ink (blank, or two thin dashes):
        // the search window reaches x1 - 1 and that column is gap. column_cuts used to
        // visit it forever. Run on a thread so a regression fails instead of hanging.
        let cases: Vec<(Gray, PxBox, (usize, usize), usize)> = vec![
            (img(4, 600, &[]), (0, 0, 4, 600), (0, 4), 2),
            (img(8, 600, &[]), (0, 0, 8, 600), (0, 8), 4),
            (img(100, 300, &[(48, 0, 52, 1), (48, 299, 52, 300)]), (48, 0, 52, 300), (0, 100), 2),
            (img(600, 400, &[(300, 0, 302, 1), (300, 399, 302, 400)]), (300, 0, 302, 400), (0, 600), 2),
        ];
        for (i, (g, bx, span, n)) in cases.into_iter().enumerate() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(column_cuts(&g, bx, span, n));
            });
            let cuts = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("case {i}: column_cuts did not return in 5 s"));
            assert_eq!(cuts.len(), n - 1, "case {i}: {cuts:?}");
            assert!(cuts.windows(2).all(|c| c[0] < c[1]), "case {i}: {cuts:?}");
            assert!(cuts.iter().all(|&c| c >= span.0 && c <= span.1), "case {i}: {cuts:?}");
        }
    }
}
