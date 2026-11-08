//! Status bars page of the settings screen: a live
//! preview of both bars at the top, a Top / Bottom switch, the text size slider of the
//! chosen bar, then its rows: on/off switch, the widgets of the left, centre and right
//! slots (a tap opens Move / Remove; "Add widget" opens a picker), the progress bar style,
//! the separator and "Reset both bars".
//!
//! Actions (all start with `sbar-`): `sbar-tab:<0|1>`, `sbar-on:<edge>:on|off`,
//! `sbar-size:<px>` (the slider, for the bar shown), `sbar-add:<edge>:<slot>`,
//! `sbar-put:<edge>:<slot>:<widget>`, `sbar-item:<edge>:<slot>:<i>`,
//! `sbar-move:<edge>:<slot>:<i>:<up|down|left|center|right>`, `sbar-del:<edge>:<slot>:<i>`,
//! `sbar-prog:<edge>`, `sbar-progset:<edge>:<style>`, `sbar-sep:<edge>`,
//! `sbar-sepset:<edge>:<i>`, `sbar-reset`. `<edge>` is `top` or `bottom`, `<slot>` 0
//! (left), 1 (centre) or 2 (right). [`action`] is pure; the screen glue
//! ([`SettingsScreen::status_bars_action`], [`SettingsScreen::build_status_bars`]) reads
//! and writes [`AppCtx::settings_update`].

use super::rows::{Row, Trail};
use super::{Page, SettingsScreen, LIGHT_SLIDER_H};
use crate::router::AppCtx;
use crate::screens::reader::footer::{self, DeviceStatus, Edge, FooterInfo, Part, MAX_FONT_PX, MIN_FONT_PX};
use ink_device::{Rect, RefreshClass};
use ink_doc::LayoutSettings;
use ink_text::Style;
use ink_store::prefs::{label, StatusBar, StatusBars, PROGRESS_BARS, STATUS_WIDGETS};
use ink_ui::theme::space;
use ink_ui::{Icon, PaintCx, Segmented, Slider, Widget};

/// Separator choices: (text, label).
pub const SEPARATORS: &[(&str, &str)] = &[(" · ", "Dot"), (" | ", "Vertical bar"), (" – ", "Dash"), (" / ", "Slash"), ("   ", "Space")];
/// Slot names, by index.
const SLOTS: [&str; 3] = ["Left", "Centre", "Right"];
/// Slot names in `sbar-move` actions, by index.
const SLOT_KEYS: [&str; 3] = ["left", "center", "right"];
/// Height of the preview.
pub const PREVIEW_H: u32 = 300;

/// A view of the status bars page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// The settings of one bar.
    Bar(Edge),
    /// The widget picker for a slot.
    Add(Edge, usize),
    /// Move or remove the `i`-th widget of a slot.
    Item(Edge, usize, usize),
    /// Progress bar style.
    Progress(Edge),
    /// Separator.
    Separator(Edge),
}

impl Default for View {
    fn default() -> Self {
        View::Bar(Edge::Top)
    }
}

impl View {
    /// The bar the view edits.
    pub fn edge(&self) -> Edge {
        match *self {
            View::Bar(e) | View::Add(e, _) | View::Item(e, _, _) | View::Progress(e) | View::Separator(e) => e,
        }
    }
}

/// What an action asks the settings screen to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Show this view (nothing changed).
    Open(View),
    /// The bars changed: show this view with the status line.
    Saved(View, String),
}

fn edge_name(e: Edge) -> &'static str {
    match e {
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    }
}

fn edge_of(s: &str) -> Option<Edge> {
    match s {
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        _ => None,
    }
}

fn bar_mut(bars: &mut StatusBars, e: Edge) -> &mut StatusBar {
    match e {
        Edge::Top => &mut bars.top,
        Edge::Bottom => &mut bars.bottom,
    }
}

fn bar_ref(bars: &StatusBars, e: Edge) -> &StatusBar {
    match e {
        Edge::Top => &bars.top,
        Edge::Bottom => &bars.bottom,
    }
}

fn slot_mut(bar: &mut StatusBar, slot: usize) -> Option<&mut Vec<String>> {
    match slot {
        0 => Some(&mut bar.left),
        1 => Some(&mut bar.center),
        2 => Some(&mut bar.right),
        _ => None,
    }
}

fn slot_ref(bar: &StatusBar, slot: usize) -> &[String] {
    match slot {
        0 => &bar.left,
        1 => &bar.center,
        _ => &bar.right,
    }
}

/// "Top bar" / "Bottom bar".
fn bar_title(e: Edge) -> &'static str {
    match e {
        Edge::Top => "Top bar",
        Edge::Bottom => "Bottom bar",
    }
}

/// The page title of `view`.
pub fn title(view: &View) -> String {
    match view {
        View::Bar(_) => "Status bars".into(),
        View::Add(e, s) => format!("Add to {} {}", bar_title(*e).to_lowercase(), SLOTS[(*s).min(2)].to_lowercase()),
        View::Item(..) => "Move or remove".into(),
        View::Progress(_) => "Progress bar".into(),
        View::Separator(_) => "Separator".into(),
    }
}

/// The page a back tap shows: `None` = the main settings page.
pub fn parent(view: &View) -> Option<View> {
    match view {
        View::Bar(_) => None,
        v => Some(View::Bar(v.edge())),
    }
}

/// Value of the "Status bars" row on the main page.
pub fn summary(bars: &StatusBars) -> &'static str {
    match (bars.top.enabled, bars.bottom.enabled) {
        (true, true) => "Top and bottom",
        (true, false) => "Top only",
        (false, true) => "Bottom only",
        (false, false) => "Off",
    }
}

/// The label of a separator text.
fn separator_label(sep: &str) -> String {
    match SEPARATORS.iter().find(|(s, _)| *s == sep) {
        Some((s, l)) if s.trim().is_empty() => l.to_string(),
        Some((s, l)) => format!("{l}  ({})", s.trim()),
        None => format!("\"{sep}\""),
    }
}

/// The rows of `view` for `bars`.
pub fn rows(bars: &StatusBars, view: &View) -> Vec<Row> {
    match *view {
        View::Bar(e) => {
            let bar = bar_ref(bars, e);
            let en = edge_name(e);
            let mut rows = vec![Row::toggle(format!("Show the {}", bar_title(e).to_lowercase()), bar.enabled, format!("sbar-on:{en}"))];
            for (s, name) in SLOTS.iter().enumerate() {
                rows.push(Row::header(*name));
                for (i, w) in slot_ref(bar, s).iter().enumerate() {
                    rows.push(Row::item(label(STATUS_WIDGETS, w), format!("sbar-item:{en}:{s}:{i}")).trail(Trail::Chevron));
                }
                rows.push(Row::item("Add widget", format!("sbar-add:{en}:{s}")).icon(Icon::Plus));
            }
            rows.push(Row::header("Style"));
            rows.push(Row::nav(Icon::Chart, "Progress bar", format!("sbar-prog:{en}")).value(label(PROGRESS_BARS, &bar.progress_bar)));
            rows.push(Row::nav(Icon::More, "Separator", format!("sbar-sep:{en}")).value(separator_label(&bar.separator)));
            rows.push(Row::header("Defaults"));
            rows.push(Row::item("Reset both bars", "sbar-reset").icon(Icon::Sync));
            rows
        }
        View::Add(e, s) => {
            let have = slot_ref(bar_ref(bars, e), s);
            let mut rows = vec![Row::header("Widget")];
            for (n, l) in STATUS_WIDGETS {
                let row = Row::item(*l, format!("sbar-put:{}:{s}:{n}", edge_name(e)));
                rows.push(if have.iter().any(|h| h == n) { row.value("in this slot") } else { row });
            }
            rows
        }
        View::Item(e, s, i) => {
            let en = edge_name(e);
            let name = slot_ref(bar_ref(bars, e), s).get(i).map(|w| label(STATUS_WIDGETS, w)).unwrap_or("?");
            let mut rows = vec![Row::header(format!("{name} ({})", SLOTS[s.min(2)].to_lowercase()))];
            let mv = |dir: &str, text: &str| Row::item(text.to_string(), format!("sbar-move:{en}:{s}:{i}:{dir}"));
            // Earlier / later only when there is a neighbour to swap with.
            let len = slot_ref(bar_ref(bars, e), s).len();
            if i > 0 {
                rows.push(mv("up", "Move earlier").icon(Icon::Back));
            }
            if i + 1 < len {
                rows.push(mv("down", "Move later").icon(Icon::Forward));
            }
            for (k, slot) in SLOT_KEYS.iter().enumerate() {
                if k != s {
                    rows.push(mv(slot, &format!("Move to the {} slot", SLOTS[k].to_lowercase())));
                }
            }
            rows.push(Row::item("Remove", format!("sbar-del:{en}:{s}:{i}")).icon(Icon::Trash));
            rows
        }
        View::Progress(e) => {
            let now = &bar_ref(bars, e).progress_bar;
            let mut rows = vec![Row::header("Progress bar")];
            rows.extend(PROGRESS_BARS.iter().map(|(n, l)| Row::choice(*l, n == now, format!("sbar-progset:{}:{n}", edge_name(e)))));
            rows
        }
        View::Separator(e) => {
            let now = &bar_ref(bars, e).separator;
            let mut rows = vec![Row::header("Between two widgets in a slot")];
            rows.extend(SEPARATORS.iter().enumerate().map(|(i, (s, _))| Row::choice(separator_label(s), s == now, format!("sbar-sepset:{}:{i}", edge_name(e)))));
            rows
        }
    }
}

/// Runs a status-bar action on `bars` (`view` is the view shown). `None`: not a status
/// bar action, or one with an unknown edge, slot, index or name (ignored).
pub fn action(a: &str, bars: &mut StatusBars, view: &View) -> Option<Step> {
    let (verb, arg) = a.split_once(':').unwrap_or((a, ""));
    let parts: Vec<&str> = arg.split(':').collect();
    let edge = || parts.first().and_then(|p| edge_of(p));
    let slot = || parts.get(1).and_then(|p| p.parse::<usize>().ok()).filter(|s| *s < 3);
    let index = || parts.get(2).and_then(|p| p.parse::<usize>().ok());
    match verb {
        "sbar-tab" => Some(Step::Open(View::Bar(if arg == "1" { Edge::Bottom } else { Edge::Top }))),
        "sbar-reset" => {
            *bars = StatusBars::default();
            Some(Step::Saved(View::Bar(view.edge()), "Both bars reset to the defaults".into()))
        }
        "sbar-on" => {
            let e = edge()?;
            let on = *parts.get(1)? == "on";
            bar_mut(bars, e).enabled = on;
            Some(Step::Saved(View::Bar(e), format!("{} {}", bar_title(e), if on { "on" } else { "off" })))
        }
        "sbar-size" => {
            let px: f32 = arg.parse().ok().filter(|v: &f32| v.is_finite())?;
            let e = view.edge();
            bar_mut(bars, e).font_px = px.clamp(MIN_FONT_PX, MAX_FONT_PX);
            Some(Step::Saved(View::Bar(e), format!("{} text size {}", bar_title(e), px.round())))
        }
        "sbar-add" => Some(Step::Open(View::Add(edge()?, slot()?))),
        "sbar-item" => {
            let (e, s, i) = (edge()?, slot()?, index()?);
            (i < slot_ref(bar_ref(bars, e), s).len()).then_some(Step::Open(View::Item(e, s, i)))
        }
        "sbar-put" => {
            let (e, s) = (edge()?, slot()?);
            let w = *parts.get(2)?;
            STATUS_WIDGETS.iter().find(|(n, _)| *n == w)?;
            let bar = bar_mut(bars, e);
            slot_mut(bar, s)?.push(w.to_string());
            // A widget added to a bar that is off would not show: turn the bar on.
            let was_off = !bar.enabled;
            bar.enabled = true;
            let on = if was_off { format!(", {} on", bar_title(e).to_lowercase()) } else { String::new() };
            Some(Step::Saved(View::Bar(e), format!("{} added ({}){on}", label(STATUS_WIDGETS, w), SLOTS[s].to_lowercase())))
        }
        "sbar-del" => {
            let (e, s, i) = (edge()?, slot()?, index()?);
            let list = slot_mut(bar_mut(bars, e), s)?;
            (i < list.len()).then_some(())?;
            let w = list.remove(i);
            Some(Step::Saved(View::Bar(e), format!("{} removed", label(STATUS_WIDGETS, &w))))
        }
        "sbar-move" => {
            let (e, s, i) = (edge()?, slot()?, index()?);
            let dir = *parts.get(3)?;
            let bar = bar_mut(bars, e);
            let list = slot_mut(bar, s)?;
            (i < list.len()).then_some(())?;
            let target = match dir {
                "up" | "down" => {
                    let j = if dir == "up" { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < list.len()) };
                    if let Some(j) = j {
                        list.swap(i, j);
                    }
                    let to = j.unwrap_or(i);
                    return Some(Step::Saved(View::Item(e, s, to), format!("{} moved", label(STATUS_WIDGETS, &list[to]))));
                }
                key => SLOT_KEYS.iter().position(|k| *k == key)?,
            };
            if target == s {
                return Some(Step::Open(View::Item(e, s, i)));
            }
            let w = list.remove(i);
            let text = format!("{} moved to the {} slot", label(STATUS_WIDGETS, &w), SLOTS[target].to_lowercase());
            slot_mut(bar, target)?.push(w);
            Some(Step::Saved(View::Bar(e), text))
        }
        "sbar-prog" => Some(Step::Open(View::Progress(edge()?))),
        "sbar-progset" => {
            let e = edge()?;
            let p = *parts.get(1)?;
            PROGRESS_BARS.iter().find(|(n, _)| *n == p)?;
            bar_mut(bars, e).progress_bar = p.to_string();
            Some(Step::Saved(View::Bar(e), format!("{} progress bar: {}", bar_title(e), label(PROGRESS_BARS, p))))
        }
        "sbar-sep" => Some(Step::Open(View::Separator(edge()?))),
        "sbar-sepset" => {
            let e = edge()?;
            let (sep, l) = SEPARATORS.get(parts.get(1)?.parse::<usize>().ok()?)?;
            bar_mut(bars, e).separator = sep.to_string();
            Some(Step::Saved(View::Bar(e), format!("{} separator: {l}", bar_title(e))))
        }
        _ => None,
    }
}

/// Sample values the preview shows.
fn sample_info(clock: String) -> FooterInfo {
    FooterInfo {
        page: 86,
        total: 208,
        progress: 0.42,
        marks: vec![0.06, 0.18, 0.31, 0.47, 0.6, 0.74, 0.88],
        pages_left_chapter: Some(7),
        minutes_left_chapter: Some(12),
        minutes_left_book: Some(312),
        book_title: "Dune".into(),
        author: "Frank Herbert".into(),
        chapter_title: Some("Book One: Dune".into()),
        bookmarked: true,
        device: DeviceStatus { clock, battery: Some((76, false)), wifi: Some(true), light: Some(12) },
        ..FooterInfo::default()
    }
}

/// The live preview: both bars drawn by the reader's own painter into a small page, at
/// their fixed places across the page width (the margins do not move them), with sample
/// book values and the current time; the text lines sit inside the reader's margins,
/// measured from the bars. The bar being edited has a dark outline around its band; a
/// bar that is off shows "Top bar off".
pub struct Preview {
    bars: StatusBars,
    margins: (f32, f32, f32),
    info: FooterInfo,
    edit: Edge,
}

impl Preview {
    /// A preview of `bars` with the reader margins of `reader` and the clock text `clock`,
    /// the bar at `edit` marked.
    pub fn new(bars: StatusBars, reader: &ink_store::settings::ReaderSettings, clock: String, edit: Edge) -> Self {
        Preview { bars, margins: (reader.margin_x, reader.margin_top, reader.margin_bottom), info: sample_info(clock), edit }
    }

    /// The band of `edge`'s bar in a preview drawn in `b`: from the page edge to the
    /// bar's inner edge as if the bar were on, plus [`footer::BAR_EDGE`] / 2.
    fn band(b: Rect, bars: &StatusBars, edge: Edge) -> Rect {
        let bar = bar_ref(bars, edge);
        let h = (footer::BAR_EDGE + footer::band_h(bar, true, footer::has_progress(bar)) + footer::BAR_EDGE / 2) as u32;
        match edge {
            Edge::Top => Rect::new(b.x, b.y, b.w, h),
            Edge::Bottom => Rect::new(b.x, b.bottom() - h as i32, b.w, h),
        }
    }

    /// Draws the progress track for night mode (darker, as the reader does).
    pub fn night(mut self, night: bool) -> Self {
        self.info.night = night;
        self
    }
}

impl Widget for Preview {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = std::time::Instant::now();
        let (mx, top, bottom) = self.margins;
        // The page is as wide as the panel's page, scaled to the preview's width.
        let scale = b.w as f32 / 1272.0;
        // The text area as the reader places it (footer::text_layout), margins scaled;
        // the top and bottom are cut together so a few lines stay in the short preview.
        let side = ((1272.0 - footer::MIN_TEXT_PX) / 2.0).max(0.0);
        let mut top = footer::bar_reserve(&self.bars, Edge::Top) + top.max(0.0) * scale;
        let mut bottom = footer::bar_reserve(&self.bars, Edge::Bottom) + bottom.max(0.0) * scale;
        let room = (b.h as f32 - 2.0 * space(2) as f32 - 56.0).max(0.0);
        if top + bottom > room {
            let k = room / (top + bottom);
            (top, bottom) = (top * k, bottom * k);
        }
        let layout = LayoutSettings {
            width: b.w,
            height: b.h,
            margin_left: mx.clamp(0.0, side) * scale,
            margin_right: mx.clamp(0.0, side) * scale,
            margin_top: top,
            margin_bottom: bottom,
            ..LayoutSettings::for_screen(b.w, b.h)
        };
        cx.canvas.fill_rect(b.x, b.y, b.w as i32, b.h as i32, cx.theme.bg);
        // Text lines of the page: light bars between the margins.
        let (x0, x1) = (b.x + layout.margin_left as i32, b.x + (b.w as f32 - layout.margin_right) as i32);
        let mut y = b.y + layout.margin_top as i32 + space(2);
        let end = b.bottom() - layout.margin_bottom as i32 - space(2);
        while y + 16 <= end {
            cx.canvas.fill_rect(x0, y, x1 - x0, 16, cx.theme.light);
            y += 40;
        }
        footer::draw_bars_at(cx.canvas, cx.fonts, &layout, (b.x, b.y), &self.bars, &self.info, Part::All);
        for edge in [Edge::Top, Edge::Bottom] {
            let band = Self::band(b, &self.bars, edge);
            if !bar_ref(&self.bars, edge).enabled {
                let text = format!("{} off", bar_title(edge));
                let px = 24.0_f32.min(band.h as f32 * 0.6);
                let w = cx.text_width(Style::Regular, px, &text);
                let base = cx.baseline_in(band, px, Style::Regular);
                let muted = cx.theme.muted;
                cx.draw_text(Style::Regular, px, b.x as f32 + (b.w as f32 - w) / 2.0, base, &text, muted);
            }
        }
        cx.canvas.stroke_rect(b, cx.theme.border_px, cx.theme.fg);
        // The bar being edited: a second, inner outline around its margin band.
        let band = Self::band(b, &self.bars, self.edit);
        let k = cx.theme.border_px;
        cx.canvas.stroke_rect(Rect::new(band.x + k, band.y + k, band.w.saturating_sub(2 * k as u32), band.h.saturating_sub(2 * k as u32)), k, cx.theme.fg);
        log::debug!("settings status bars: preview drawn in {} ms", t.elapsed().as_millis());
    }
}

impl SettingsScreen {
    /// Builds the status bars page `view` in `content`.
    pub(super) fn build_status_bars(&mut self, cx: &mut AppCtx, content: Rect, view: &View) {
        let s = super::current_settings(cx);
        let bars = s.status_bars.clone();
        let x = content.x + space(4);
        let w = content.w.saturating_sub(2 * space(4) as u32);
        let clock = cx.services.clock.local_now().clock(false);
        let preview = Preview::new(bars.clone(), &s.reader, clock, view.edge()).night(cx.theme.night);
        let mut y = content.y + space(2);
        self.tree.add(None, Box::new(preview), Rect::new(x, y, w, PREVIEW_H));
        y += PREVIEW_H as i32 + space(2);
        if let View::Bar(e) = view {
            let tab = Segmented::new(&["Top bar", "Bottom bar"], "sbar-tab").with_selected(usize::from(*e == Edge::Bottom));
            self.tree.add(None, Box::new(tab), Rect::new(x, y, w, cx.theme.control_h()));
            y += cx.theme.control_h() as i32 + space(2);
            let bar = bar_ref(&bars, *e);
            let slider = Slider::new("Text size", MIN_FONT_PX, MAX_FONT_PX, 2.0, footer::bar_px(bar), "sbar-size");
            self.tree.add(None, Box::new(slider), Rect::new(x, y, w, LIGHT_SLIDER_H));
            y += LIGHT_SLIDER_H as i32 + space(1);
        }
        let list = Rect::new(content.x, y, content.w, (content.bottom() - y).max(240) as u32);
        self.add_rows(rows(&bars, view), list);
    }

    /// Runs an `sbar-` action: stores a change in [`AppCtx::settings_update`] and shows
    /// the next view (the same view is rebuilt in place, keeping its list page). Returns
    /// false for any other action.
    pub(super) fn status_bars_action(&mut self, a: &str, cx: &mut AppCtx) -> bool {
        if !a.starts_with("sbar-") {
            return false;
        }
        let view = match &self.page {
            Page::StatusBars(v) => v.clone(),
            _ => View::default(),
        };
        let mut s = super::current_settings(cx).clone();
        let (next, text) = match action(a, &mut s.status_bars, &view) {
            None => {
                log::info!("settings status bars: '{a}' ignored");
                return true;
            }
            Some(Step::Open(v)) => (v, String::new()),
            Some(Step::Saved(v, text)) => {
                log::info!("settings status bars: {text} (top {:?}, bottom {:?})", s.status_bars.top, s.status_bars.bottom);
                *cx.settings_update = Some(s);
                (v, text)
            }
        };
        let page = Page::StatusBars(next);
        if page == self.page {
            self.rebuild = Some(RefreshClass::Gl16);
        } else {
            self.go(page, cx);
        }
        self.status_text = text;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_move_remove_and_styles() {
        let mut b = StatusBars::default();
        let v = View::Bar(Edge::Bottom);
        assert_eq!(action("sbar-put:bottom:0:clock", &mut b, &v), Some(Step::Saved(View::Bar(Edge::Bottom), "Clock added (left)".into())));
        assert_eq!(b.bottom.left, vec!["time_left_chapter".to_string(), "clock".to_string()]);
        assert!(matches!(action("sbar-move:bottom:0:1:up", &mut b, &v), Some(Step::Saved(View::Item(Edge::Bottom, 0, 0), _))));
        assert_eq!(b.bottom.left[0], "clock");
        action("sbar-move:bottom:0:0:right", &mut b, &v).unwrap();
        assert_eq!((b.bottom.left.len(), b.bottom.right.last().map(String::as_str)), (1, Some("clock")));
        action("sbar-del:bottom:2:1", &mut b, &v).unwrap();
        assert_eq!(b.bottom.right, vec!["percent".to_string()]);
        action("sbar-progset:top:thick", &mut b, &v).unwrap();
        action("sbar-sepset:top:1", &mut b, &v).unwrap();
        action("sbar-on:top:off", &mut b, &v).unwrap();
        assert_eq!((b.top.progress_bar.as_str(), b.top.separator.as_str(), b.top.enabled), ("thick", " | ", false));
        action("sbar-size:30", &mut b, &v).unwrap();
        assert_eq!(b.bottom.font_px, 30.0);
        for bad in ["sbar-put:bottom:0:sparkles", "sbar-del:bottom:2:9", "sbar-put:middle:0:clock", "sbar-progset:top:x", "gest:tap"] {
            assert_eq!(action(bad, &mut b, &v), None, "{bad}");
        }
        assert_eq!(action("sbar-reset", &mut b, &v), Some(Step::Saved(v.clone(), "Both bars reset to the defaults".into())));
        assert_eq!(b, StatusBars::default());
        assert!(rows(&b, &View::Bar(Edge::Top)).iter().any(|r| r.action == "sbar-add:top:2"));
    }

    #[test]
    fn adding_to_a_bar_that_is_off_turns_it_on_and_lone_widgets_have_no_order_rows() {
        let mut b = StatusBars::default();
        b.top.enabled = false;
        let v = View::Add(Edge::Top, 2);
        let step = action("sbar-put:top:2:battery_icon", &mut b, &v);
        assert_eq!(step, Some(Step::Saved(View::Bar(Edge::Top), "Battery icon added (right), top bar on".into())));
        assert!(b.top.enabled);
        assert_eq!(b.top.right, vec!["battery".to_string(), "battery_icon".to_string()]);
        let acts = |v: &View| rows(&b, v).into_iter().map(|r| r.action).collect::<Vec<_>>();
        // Centre has one widget: neither earlier nor later.
        let one = acts(&View::Item(Edge::Top, 1, 0));
        assert!(!one.iter().any(|a| a.ends_with(":up") || a.ends_with(":down")), "{one:?}");
        let first = acts(&View::Item(Edge::Top, 2, 0));
        assert!(first.contains(&"sbar-move:top:2:0:down".to_string()) && !first.contains(&"sbar-move:top:2:0:up".to_string()));
        let last = acts(&View::Item(Edge::Top, 2, 1));
        assert!(last.contains(&"sbar-move:top:2:1:up".to_string()) && !last.contains(&"sbar-move:top:2:1:down".to_string()));
    }
}
