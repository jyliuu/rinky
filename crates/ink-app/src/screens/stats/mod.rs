//! Reading statistics and vocabulary screens (phase 10, c-stats).
//!
//! - `Route::Statistics` ([`StatsScreen`]): summary cards (today, this week, the last
//!   30 days, streaks, pages per hour, total) and a bar chart of the last 7 or 30 days
//!   (Overview), a month calendar with darker days for more reading (Calendar), and
//!   one card per book with a progress bar (Books). Reached from the library header
//!   ("Stats").
//! - `Route::Vocabulary` ([`vocab::VocabScreen`]): looked-up words and flashcards.
//!   Reached from the Statistics header ("Words").
//!
//! The data comes from `ink_stats` (`report::load` over `paths.statistics()`,
//! `VocabDb` over `paths.root/vocabulary.sqlite3`); the `stats` plugin writes the
//! statistics while a book is open.

pub mod vocab;

#[cfg(test)]
mod tests;

use crate::router::{AppCtx, AppScreen, Route, ScreenRegistry, Transition};
use ink_device::{Key, Point, Rect, RefreshClass, Size};
use ink_stats::report::{self, format_duration, Day, Overview};
use ink_text::Style;
use ink_ui::bars::APP_BAR_H;
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use ink_ui::theme::space;
use ink_ui::{paint_button, AppBar, BarAction, ButtonKind, Damage, Gesture, Icon, PaintCx, ProgressBar, Segmented, UiEvent, Widget};

/// Registers both screens.
pub fn register(reg: &mut ScreenRegistry) {
    reg.register("statistics", |_| Ok(Box::new(StatsScreen::default())));
    reg.register("vocabulary", |_| Ok(Box::new(vocab::VocabScreen::default())));
}

/// Side margin.
pub(crate) const MARGIN: i32 = space(4);
/// Header height (the ink-ui app bar).
pub(crate) const HEADER_H: i32 = APP_BAR_H as i32;
/// Top of the tab switch under the header.
const TABS_Y: i32 = HEADER_H + space(3);
/// Height of the tab switch.
const TABS_H: u32 = 104;
/// Top of the tab content.
const CONTENT_Y: i32 = TABS_Y + TABS_H as i32 + space(4);
/// Height of one book card plus the gap under it.
const BOOK_ROW_H: i32 = 216 + space(2);
/// Height of the pager row at the bottom.
pub(crate) const PAGER_H: i32 = 104;

/// Seconds since the epoch and the local UTC offset for `cx`'s clock.
pub(crate) fn now_and_offset(cx: &AppCtx) -> (u64, i64) {
    let now = cx.services.clock.now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let offset = ink_device::clock::TimeZone::local().offset_at(now as i64) as i64;
    (now, offset)
}

/// A pill button (design system); `primary` is filled.
pub(crate) fn button(p: &mut PaintCx, r: Rect, label: &str, primary: bool) {
    paint_button(p, r, label, None, if primary { ButtonKind::Filled } else { ButtonKind::Outlined }, true, false);
}

/// A round outlined icon button.
pub(crate) fn icon_button(p: &mut PaintCx, r: Rect, icon: Icon) {
    paint_button(p, r, "", Some(icon), ButtonKind::Outlined, true, false);
}

/// The ink-ui app bar with a back button, a title and an optional text action; records
/// the back button as `back` and the action as `right` in `hits`.
pub(crate) fn header<H: Clone>(p: &mut PaintCx, title: &str, right: Option<(&str, H)>, back: H, hits: &mut Vec<(Rect, H)>) {
    let b = Rect::new(0, 0, p.canvas.width as u32, APP_BAR_H);
    let mut bar = AppBar::new(title).with_back();
    if let Some((label, _)) = &right {
        bar = bar.action(BarAction::text(*label, "right"));
    }
    bar.paint(p, b);
    hits.push((AppBar::back_rect(b), back));
    if let Some((_, hit)) = right {
        if let Some(r) = bar.action_rects(b).first().filter(|r| r.w > 0) {
            hits.push((*r, hit));
        }
    }
}

/// Pager at the bottom: round previous/next buttons and "Page n of m" between them.
pub(crate) fn pager<H: Clone>(p: &mut PaintCx, page: usize, pages: usize, extra: &str, prev: H, next: H, hits: &mut Vec<(Rect, H)>) {
    let (w, h) = (p.canvas.width as i32, p.canvas.height as i32);
    let y = h - PAGER_H - space(3);
    let pr = Rect::new(MARGIN, y, PAGER_H as u32, PAGER_H as u32);
    let nr = Rect::new(w - MARGIN - PAGER_H, y, PAGER_H as u32, PAGER_H as u32);
    if page > 0 {
        icon_button(p, pr, Icon::Back);
        hits.push((pr, prev));
    }
    if page + 1 < pages {
        icon_button(p, nr, Icon::Forward);
        hits.push((nr, next));
    }
    let label = format!("Page {} of {pages}{extra}", page + 1);
    let (cap, muted) = (p.theme.caption, p.theme.muted);
    p.text_centered(Rect::new(pr.right() + space(2), y, (nr.x - pr.right() - space(4)) as u32, PAGER_H as u32), &label, cap.px, cap.style, muted);
}

/// The position of a tap or long press.
pub(crate) fn tap_pos(ev: &UiEvent) -> Option<Point> {
    match ev {
        UiEvent::Gesture(Gesture::Tap { pos } | Gesture::LongPress { pos, .. }) => Some(*pos),
        _ => None,
    }
}

/// +1 for a swipe left or up (next), -1 for right or down.
pub(crate) fn swipe_step(ev: &UiEvent) -> Option<i32> {
    match ev {
        UiEvent::Gesture(g @ Gesture::Swipe { .. }) => {
            let (dx, dy) = g.delta();
            let d = if dx.abs() > dy.abs() { dx } else { dy };
            Some(if d < 0 { 1 } else { -1 })
        }
        _ => None,
    }
}

/// Calendar fill for a day's reading: `(fill, text)` palette values, `None` under a
/// minute (the minutes label goes with every fill, so a `light` fill never carries the
/// information alone at night). Four steps: under 15 min, under an hour, under two
/// hours, more.
pub(crate) fn day_shade(secs: u64, t: &ink_ui::Theme) -> Option<(u8, u8)> {
    match secs {
        0..60 => None,
        s if s < 15 * 60 => Some((t.track(), t.fg)),
        s if s < 3600 => Some((t.subtle, t.fg)),
        s if s < 2 * 3600 => Some((t.muted, t.bg)),
        _ => Some((t.fg, t.bg)),
    }
}

/// Reading of the `n` days ending today (oldest first).
pub(crate) fn last_days(days: &std::collections::BTreeMap<i64, Day>, today: i64, n: usize) -> Vec<(i64, Day)> {
    (0..n as i64).rev().map(|k| today - k).map(|d| (d, days.get(&d).cloned().unwrap_or_default())).collect()
}

/// Short duration for chart labels: `45m`, `1h05`.
fn short_minutes(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m}m")
    } else {
        format!("{}h{:02}", m / 60, m % 60)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Back,
    Words,
    Tab(Tab),
    Range(usize),
    PrevMonth,
    NextMonth,
    PrevPage,
    NextPage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Overview,
    Calendar,
    Books,
}

const TABS: [(Tab, &str); 3] = [(Tab::Overview, "Overview"), (Tab::Calendar, "Calendar"), (Tab::Books, "Books")];

/// The Statistics screen.
#[derive(Default)]
pub struct StatsScreen {
    data: Overview,
    /// Local UTC offset (seconds) used for the days.
    offset: i64,
    error: Option<String>,
    tab: Tab,
    /// Chart range: false = 7 days, true = 30 days.
    month_chart: bool,
    /// Calendar month (year, month).
    month: (i32, u32),
    /// Books page.
    page: usize,
    hits: Vec<(Rect, Hit)>,
    dirty: Option<RefreshClass>,
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

impl StatsScreen {
    fn load(&mut self, cx: &AppCtx) {
        let (now, offset) = now_and_offset(cx);
        let path = cx.paths.statistics();
        let (data, error) = if !path.exists() {
            (report::overview(&[], Vec::new(), now, offset), None)
        } else {
            match ink_store::StatsDb::open(&path).and_then(|db| report::load(&db, now, offset)) {
                Ok(d) => (d, None),
                Err(e) => {
                    log::warn!("statistics: {}: {e}", path.display());
                    (report::overview(&[], Vec::new(), now, offset), Some(format!("Cannot read the statistics: {e}")))
                }
            }
        };
        let (y, m, _) = report::civil_from_days(data.today);
        self.month = (y, m);
        self.data = data;
        self.offset = offset;
        self.error = error;
    }

    fn rows_per_page(h: i32) -> usize {
        ((h - CONTENT_Y - PAGER_H - 2 * space(3)) / BOOK_ROW_H).max(1) as usize
    }

    fn pages(&self, h: i32) -> usize {
        self.data.books.len().div_ceil(Self::rows_per_page(h)).max(1)
    }

    fn shift_month(&mut self, d: i32) {
        let (y, m) = self.month;
        let i = y * 12 + m as i32 - 1 + d;
        self.month = (i.div_euclid(12), i.rem_euclid(12) as u32 + 1);
    }

    fn act(&mut self, hit: Hit, size: Size) -> Transition {
        match hit {
            Hit::Back => return Transition::Pop,
            Hit::Words => return Transition::Push(Route::Vocabulary),
            Hit::Tab(t) => self.tab = t,
            Hit::Range(n) => self.month_chart = n == 30,
            Hit::PrevMonth => self.shift_month(-1),
            Hit::NextMonth => self.shift_month(1),
            Hit::PrevPage => self.page = self.page.saturating_sub(1),
            Hit::NextPage => self.page = (self.page + 1).min(self.pages(size.h as i32) - 1),
        }
        self.dirty = Some(RefreshClass::Gl16);
        Transition::None
    }

    fn draw(&mut self, p: &mut PaintCx) {
        let (w, h) = (p.canvas.width as i32, p.canvas.height as i32);
        self.hits.clear();
        p.canvas.clear(p.theme.bg);
        header(p, "Reading statistics", Some(("Words", Hit::Words)), Hit::Back, &mut self.hits);
        let tabs_r = Rect::new(MARGIN, TABS_Y, (w - 2 * MARGIN) as u32, TABS_H);
        let sel = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        let mut seg = Segmented::new(&TABS.map(|(_, l)| l), "tab").with_selected(sel);
        seg.paint(p, tabs_r);
        for (r, (t, _)) in seg.cells(tabs_r).into_iter().zip(TABS) {
            self.hits.push((r, Hit::Tab(t)));
        }
        let mut top = CONTENT_Y;
        if let Some(e) = self.error.clone() {
            let (cap, fg) = (p.theme.caption, p.theme.fg);
            p.text_left(Rect::new(MARGIN, top, (w - 2 * MARGIN) as u32, 48), &e, cap.px, cap.style, fg);
            top += 56;
        }
        match self.tab {
            Tab::Overview => self.draw_overview(p, w, top),
            Tab::Calendar => self.draw_calendar(p, w, top),
            Tab::Books => self.draw_books(p, w, h, top),
        }
    }

    fn draw_overview(&mut self, p: &mut PaintCx, w: i32, top: i32) {
        let d = &self.data;
        let pph = d.pages_per_hour().map(|v| format!("{v:.0}")).unwrap_or_else(|| "–".into());
        let days = |n: u32| if n == 1 { "1 day".to_string() } else { format!("{n} days") };
        let pages = |n: u64| if n == 1 { "1 page".to_string() } else { format!("{n} pages") };
        let tiles: [(&str, String, String); 6] = [
            ("Today", format_duration(d.today_read.seconds), pages(d.today_read.pages)),
            ("This week", format_duration(d.week.seconds), pages(d.week.pages)),
            ("Last 30 days", format_duration(d.month.seconds), pages(d.month.pages)),
            ("Streak", days(d.streak), format!("best {}", days(d.best_streak))),
            ("Pages per hour", pph, "all books".into()),
            ("Total", format_duration(d.total_seconds), format!("{} books", d.books.len())),
        ];
        let t = p.theme.clone();
        let gap = space(2);
        let tw = (w - 2 * MARGIN - 2 * gap) / 3;
        let th = 176;
        for (i, (label, value, sub)) in tiles.iter().enumerate() {
            let r = Rect::new(MARGIN + (i as i32 % 3) * (tw + gap), top + (i as i32 / 3) * (th + gap), tw as u32, th as u32);
            // Today's card is filled (light) to lead the row; its text stays black.
            if i == 0 {
                fill_round_rect(p.canvas, r, t.radius_lg, t.light);
            }
            stroke_round_rect(p.canvas, r, t.radius_lg, t.border_px as f32, t.fg);
            let inner = |y: i32, hh: i32| Rect::new(r.x + space(3), r.y + y, (tw - 2 * space(3)) as u32, hh as u32);
            let value = p.ellipsize(value, 48.0, Style::Bold, (tw - 2 * space(3)) as f32);
            p.text_left(inner(16, 40), label, t.caption.px, t.caption.style, t.muted);
            p.text_left(inner(58, 68), &value, 48.0, Style::Bold, t.fg);
            p.text_left(inner(124, 40), sub, t.caption.px, t.caption.style, t.muted);
        }
        self.draw_chart(p, w, top + 2 * th + gap + space(4));
    }

    /// Bar chart of the last 7 or 30 days.
    fn draw_chart(&mut self, p: &mut PaintCx, w: i32, top: i32) {
        let t = p.theme.clone();
        let h = p.canvas.height as i32;
        let n = if self.month_chart { 30 } else { 7 };
        let seg_r = Rect::new(w - MARGIN - 420, top, 420, TABS_H);
        let mut seg = Segmented::new(&["7 days", "30 days"], "range").with_selected(self.month_chart as usize);
        seg.paint(p, seg_r);
        for (r, k) in seg.cells(seg_r).into_iter().zip([7, 30]) {
            self.hits.push((r, Hit::Range(k)));
        }
        p.text_left(Rect::new(MARGIN, top, (seg_r.x - MARGIN - space(2)) as u32, TABS_H), "Reading time", t.heading.px, t.heading.style, t.fg);
        let days = last_days(&self.data.days, self.data.today, n);
        let total: u64 = days.iter().map(|(_, d)| d.seconds).sum();
        let read_days = days.iter().filter(|(_, d)| d.seconds > 0).count();
        let summary = format!("{} in {n} days · {read_days} reading days · {} a day", format_duration(total), format_duration(total / n as u64));
        p.text_left(Rect::new(MARGIN, top + TABS_H as i32 + space(1), (w - 2 * MARGIN) as u32, 44), &summary, t.caption.px, t.caption.style, t.muted);
        // Plot area: bars stand on a black base line, labels under it.
        let plot_top = top + TABS_H as i32 + 72;
        let base = h - 120;
        let max = days.iter().map(|(_, d)| d.seconds).max().unwrap_or(0).max(60);
        let cw = (w - 2 * MARGIN) / n as i32;
        let bw = if n == 7 { cw * 3 / 5 } else { (cw - space(1)).max(6) };
        let value_h = if n == 7 { 44 } else { 0 };
        let room = (base - plot_top - value_h).max(10);
        for (i, (day, d)) in days.iter().enumerate() {
            let cx0 = MARGIN + i as i32 * cw;
            let x = cx0 + (cw - bw) / 2;
            if d.seconds > 0 {
                let bh = ((d.seconds as f64 / max as f64) * room as f64).round().max(8.0) as i32;
                let r = Rect::new(x, base - bh, bw as u32, (bh + 8) as u32);
                fill_round_rect(p.canvas, r, (bw as f32 / 2.0).min(12.0), t.fg);
                if value_h > 0 {
                    let vr = Rect::new(cx0, base - bh - value_h, cw as u32, value_h as u32);
                    p.text_centered(vr, &short_minutes(d.seconds), t.caption.px, Style::Bold, t.fg);
                }
            }
            // Labels: weekday initials for 7 days, every fifth day of the month for 30.
            let (_, mm, dd) = report::civil_from_days(*day);
            let label = if n == 7 {
                ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][report::weekday(*day) as usize % 7].to_string()
            } else if (n - 1 - i) % 5 == 0 {
                format!("{dd}/{mm}")
            } else {
                String::new()
            };
            if !label.is_empty() {
                let today = *day == self.data.today;
                let lr = Rect::new(cx0 - if n == 7 { 0 } else { 40 }, base + space(2), (cw + if n == 7 { 0 } else { 80 }) as u32, 44);
                let (style, ink) = if today { (Style::Bold, t.fg) } else { (t.caption.style, t.muted) };
                p.text_centered(lr, &label, t.caption.px, style, ink);
            }
        }
        p.canvas.fill_rect(MARGIN, base, w - 2 * MARGIN, t.border_px, t.fg);
    }

    fn draw_calendar(&mut self, p: &mut PaintCx, w: i32, top: i32) {
        let t = p.theme.clone();
        let (y, m) = self.month;
        let prev = Rect::new(MARGIN, top, 104, 104);
        let next = Rect::new(w - MARGIN - 104, top, 104, 104);
        icon_button(p, prev, Icon::Back);
        icon_button(p, next, Icon::Forward);
        self.hits.push((prev, Hit::PrevMonth));
        self.hits.push((next, Hit::NextMonth));
        let title = format!("{} {y}", MONTHS[(m - 1) as usize]);
        p.text_centered(Rect::new(prev.right() + space(2), top, (next.x - prev.right() - space(4)) as u32, 104), &title, t.heading.px, t.heading.style, t.fg);
        let cw = (w - 2 * MARGIN) / 7;
        let ch = 150;
        let wy = top + 104 + space(3);
        for (i, name) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().enumerate() {
            p.text_centered(Rect::new(MARGIN + i as i32 * cw, wy, cw as u32, 44), name, t.caption.px, t.caption.style, t.muted);
        }
        let grid = report::month_grid(y, m, &self.data.days);
        let gy = wy + 44 + space(2);
        let (mut month_secs, mut month_days) = (0u64, 0usize);
        for (r, row) in grid.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let Some(cell) = cell else { continue };
                let rect = Rect::new(MARGIN + c as i32 * cw + 6, gy + r as i32 * ch + 6, (cw - 12) as u32, (ch - 12) as u32);
                month_secs += cell.read.seconds;
                month_days += (cell.read.seconds > 0) as usize;
                let shade = day_shade(cell.read.seconds, &t);
                let text = shade.map_or(t.fg, |(_, ink)| ink);
                if let Some((fill, _)) = shade {
                    fill_round_rect(p.canvas, rect, t.radius, fill);
                }
                if cell.index == self.data.today {
                    stroke_round_rect(p.canvas, rect, t.radius, t.focus_px as f32, t.fg);
                }
                let num = Rect::new(rect.x, rect.y + 14, rect.w, 52);
                p.text_centered(num, &cell.day.to_string(), t.label.px, Style::Bold, text);
                if cell.read.seconds >= 60 {
                    let mr = Rect::new(rect.x, rect.y + 72, rect.w, 44);
                    p.text_centered(mr, &short_minutes(cell.read.seconds), t.caption.px, t.caption.style, text);
                }
            }
        }
        // Legend and month total.
        let ly = gy + grid.len() as i32 * ch + space(3);
        let mut x = MARGIN;
        p.text_left(Rect::new(x, ly, 90, 56), "Less", t.caption.px, t.caption.style, t.muted);
        x += 90;
        for secs in [0u64, 600, 1800, 5400, 9000] {
            let r = Rect::new(x, ly + 4, 48, 48);
            if let Some((fill, _)) = day_shade(secs, &t) {
                fill_round_rect(p.canvas, r, 8.0, fill);
            }
            // Outlined, so the light step reads in night mode too.
            stroke_round_rect(p.canvas, r, 8.0, t.border_px as f32, t.fg);
            x += 48 + space(1);
        }
        p.text_left(Rect::new(x + space(1), ly, 100, 56), "More", t.caption.px, t.caption.style, t.muted);
        let total = format!("{} · {month_days} {}", format_duration(month_secs), if month_days == 1 { "day" } else { "days" });
        let tw = p.text_width(Style::Bold, t.caption.px, &total);
        p.draw_text(Style::Bold, t.caption.px, w as f32 - MARGIN as f32 - tw, p.baseline_in(Rect::new(0, ly, 10, 56), t.caption.px, Style::Bold), &total, t.fg);
    }

    fn draw_books(&mut self, p: &mut PaintCx, w: i32, h: i32, top: i32) {
        let t = p.theme.clone();
        if self.data.books.is_empty() {
            let r = Rect::new(MARGIN, top + 60, (w - 2 * MARGIN) as u32, 80);
            p.text_left(r, "No reading recorded yet. Open a book and turn some pages.", t.body.px, t.body.style, t.muted);
            return;
        }
        let per = Self::rows_per_page(h);
        let pages = self.pages(h);
        self.page = self.page.min(pages - 1);
        let cw = (w - 2 * MARGIN) as u32;
        let iw = cw - 2 * space(3) as u32;
        for (i, b) in self.data.books.iter().skip(self.page * per).take(per).enumerate() {
            let r = Rect::new(MARGIN, top + i as i32 * BOOK_ROW_H, cw, (BOOK_ROW_H - space(2)) as u32);
            stroke_round_rect(p.canvas, r, t.radius_lg, t.border_px as f32, t.fg);
            let x = r.x + space(3);
            let title = if b.title.is_empty() { b.md5.as_str() } else { b.title.as_str() };
            let title = p.ellipsize(title, t.body.px, Style::Bold, iw as f32);
            p.text_left(Rect::new(x, r.y + 16, iw, 56), &title, t.body.px, Style::Bold, t.fg);
            let pph = report::pages_per_hour(b.pages_read, b.read_time).map(|v| format!(" · {v:.0} pages/h")).unwrap_or_default();
            let last = b.last_read.map(|tm| {
                let (yy, mm, dd) = report::civil_from_days(report::day_index(tm, self.offset));
                format!(" · last {yy}-{mm:02}-{dd:02}")
            });
            let line = format!("{} · {} sessions{pph}{}", format_duration(b.read_time), b.sessions, last.unwrap_or_default());
            let line = p.ellipsize(&line, t.caption.px, t.caption.style, iw as f32);
            p.text_left(Rect::new(x, r.y + 76, iw, 44), &line, t.caption.px, t.caption.style, t.muted);
            // Pages read of the book, as a thin bar with the share on the right.
            let bar_r = Rect::new(x, r.y + 132, iw, 56);
            match b.pages.filter(|n| *n > 0) {
                Some(n) => {
                    let v = (b.pages_read as f32 / n as f32).min(1.0);
                    let caption = format!("{} of {n} pages read", b.pages_read);
                    ProgressBar::thin(v).with_label(format!("{:.0}%", v * 100.0)).with_caption(caption).paint(p, bar_r);
                }
                None => {
                    let caption = format!("{} pages read", b.pages_read);
                    p.text_left(bar_r, &caption, t.caption.px, t.caption.style, t.muted);
                }
            }
        }
        pager(p, self.page, pages, "", Hit::PrevPage, Hit::NextPage, &mut self.hits);
    }
}

impl AppScreen for StatsScreen {
    fn name(&self) -> &'static str {
        "statistics"
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let keep_month = self.month != (0, 0);
        let month = self.month;
        self.load(cx);
        if keep_month {
            self.month = month;
        }
        self.dirty = Some(RefreshClass::Gc16);
        Ok(())
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let size = cx.screen.size();
        if matches!(ev, UiEvent::Quit) {
            return Ok(Transition::Exit);
        }
        if let UiEvent::Key(k) = ev {
            if !k.pressed {
                return Ok(Transition::None);
            }
            return Ok(match k.key {
                Key::Back => Transition::Pop,
                Key::PageNext if self.tab == Tab::Books => self.act(Hit::NextPage, size),
                Key::PagePrev if self.tab == Tab::Books => self.act(Hit::PrevPage, size),
                Key::PageNext if self.tab == Tab::Calendar => self.act(Hit::NextMonth, size),
                Key::PagePrev if self.tab == Tab::Calendar => self.act(Hit::PrevMonth, size),
                _ => Transition::None,
            });
        }
        if let Some(step) = swipe_step(ev) {
            let hit = match (self.tab, step > 0) {
                (Tab::Books, true) => Hit::NextPage,
                (Tab::Books, false) => Hit::PrevPage,
                (Tab::Calendar, true) => Hit::NextMonth,
                (Tab::Calendar, false) => Hit::PrevMonth,
                (Tab::Overview, more) => Hit::Range(if more { 30 } else { 7 }),
            };
            return Ok(self.act(hit, size));
        }
        if let Some(pos) = tap_pos(ev) {
            if let Some(hit) = self.hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, h)| h.clone()) {
                return Ok(self.act(hit, size));
            }
        }
        Ok(Transition::None)
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        let Some(class) = self.dirty.take() else { return Ok(Vec::new()) };
        let mut p = PaintCx { canvas: cx.canvas, fonts: cx.fonts, theme: cx.theme };
        self.draw(&mut p);
        Ok(vec![Damage { rect: cx.canvas.bounds(), class }])
    }
}
