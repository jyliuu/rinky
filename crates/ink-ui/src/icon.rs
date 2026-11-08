//! Vector icons drawn with [`crate::draw::Path`] on a 24 x 24 design grid, so they need
//! no image files or symbol fonts, and an [`IconView`] widget.

use crate::draw::Path;
use crate::widget::{look_damage, press_or_tap, EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Canvas, Rect, RefreshClass};
use std::f32::consts::PI;

/// The icon set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Chevron pointing left.
    Back,
    /// Chevron pointing right.
    Forward,
    /// Chevron pointing up.
    Up,
    /// Chevron pointing down.
    Down,
    /// X.
    Close,
    /// Three bars.
    Menu,
    /// Magnifying glass.
    Search,
    /// Gear.
    Settings,
    /// House.
    Home,
    /// Ribbon.
    Bookmark,
    /// Two letters A (font settings, the stock "Aa").
    Font,
    /// Sun (frontlight).
    Light,
    /// Crescent moon (night mode).
    Night,
    /// Circular arrow (rotation).
    Rotate,
    /// Wi-Fi arcs.
    Wifi,
    /// Battery outline.
    Battery,
    /// Check mark.
    Check,
    /// Plus.
    Plus,
    /// Minus.
    Minus,
    /// Shift arrow (outline).
    Shift,
    /// Shift arrow (filled): caps lock.
    ShiftLocked,
    /// Backspace key.
    Backspace,
    /// Return arrow.
    Enter,
    /// Globe (keyboard language).
    Globe,
    /// Keyboard with a down chevron (hide the keyboard).
    KeyboardHide,
    /// Star.
    Star,
    /// Circled i.
    Info,
    /// Trash can.
    Trash,
    /// Open book.
    Book,
    /// Books on a shelf (library).
    Library,
    /// Four squares (grid view).
    Grid,
    /// Rows with bullets (list view).
    ListView,
    /// Bar chart (statistics).
    Chart,
    /// Speech bubble (chat, AI).
    Chat,
    /// Nested list (table of contents).
    Toc,
    /// Lines and a down arrow (sort).
    Sort,
    /// Funnel (filter).
    Filter,
    /// Three vertical dots (more actions).
    More,
    /// Folder.
    Folder,
    /// Arrow into a tray (download).
    Download,
    /// Two circular arrows (sync).
    Sync,
    /// Clock face (reading time).
    Clock,
    /// USB trident (USB network).
    Usb,
    /// Window with a `>_` prompt (SSH, shell).
    Terminal,
    /// Large four-pointed sparkle with two small ones (AI).
    Sparkle,
}

impl Icon {
    /// Every icon (for the gallery).
    pub const ALL: [Icon; 45] = [
        Icon::Back,
        Icon::Forward,
        Icon::Up,
        Icon::Down,
        Icon::Close,
        Icon::Menu,
        Icon::Search,
        Icon::Settings,
        Icon::Home,
        Icon::Bookmark,
        Icon::Font,
        Icon::Light,
        Icon::Night,
        Icon::Rotate,
        Icon::Wifi,
        Icon::Battery,
        Icon::Check,
        Icon::Plus,
        Icon::Minus,
        Icon::Shift,
        Icon::ShiftLocked,
        Icon::Backspace,
        Icon::Enter,
        Icon::Globe,
        Icon::KeyboardHide,
        Icon::Star,
        Icon::Info,
        Icon::Trash,
        Icon::Book,
        Icon::Library,
        Icon::Grid,
        Icon::ListView,
        Icon::Chart,
        Icon::Chat,
        Icon::Toc,
        Icon::Sort,
        Icon::Filter,
        Icon::More,
        Icon::Folder,
        Icon::Download,
        Icon::Sync,
        Icon::Clock,
        Icon::Usb,
        Icon::Terminal,
        Icon::Sparkle,
    ];

    /// The icon's path scaled into the square centred in `r`.
    pub fn path(self, r: Rect) -> Path {
        let side = r.w.min(r.h) as f32;
        let s = side / 24.0;
        let ox = r.x as f32 + (r.w as f32 - side) / 2.0;
        let oy = r.y as f32 + (r.h as f32 - side) / 2.0;
        let p = |x: f32, y: f32| (ox + x * s, oy + y * s);
        let w = 2.0 * s; // stroke width
        let mut path = Path::new();
        let poly = |path: &mut Path, pts: &[(f32, f32)]| {
            let v: Vec<(f32, f32)> = pts.iter().map(|(x, y)| p(*x, *y)).collect();
            path.polyline(&v, w);
        };
        let fill = |path: &mut Path, pts: &[(f32, f32)]| {
            let v: Vec<(f32, f32)> = pts.iter().map(|(x, y)| p(*x, *y)).collect();
            path.polygon(&v);
        };
        // A filled axis-aligned box from corner (x0, y0) to (x1, y1).
        let bar = |path: &mut Path, x0: f32, y0: f32, x1: f32, y1: f32| fill(path, &[(x0, y0), (x1, y0), (x1, y1), (x0, y1)]);
        // Eight rays around (cx, cy) in pixels, from radius r0 to r1 (grid units).
        let rays = |path: &mut Path, (cx, cy): (f32, f32), r0: f32, r1: f32, width: f32| {
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                let (c, si) = (a.cos(), a.sin());
                path.line(cx + c * r0 * s, cy + si * r0 * s, cx + c * r1 * s, cy + si * r1 * s, width);
            }
        };
        // A star of `n` points around (cx, cy) in pixels, tips at `tip` and the waist
        // between them at `waist` (pixels), the first tip up.
        let star = |path: &mut Path, (cx, cy): (f32, f32), n: usize, tip: f32, waist: f32| {
            let pts: Vec<(f32, f32)> = (0..2 * n)
                .map(|i| {
                    let r = if i % 2 == 0 { tip } else { waist };
                    let a = -PI / 2.0 + i as f32 * PI / n as f32;
                    (cx + r * a.cos(), cy + r * a.sin())
                })
                .collect();
            path.polygon(&pts);
        };
        match self {
            Icon::Back => poly(&mut path, &[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)]),
            Icon::Forward => poly(&mut path, &[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)]),
            Icon::Up => poly(&mut path, &[(5.0, 15.0), (12.0, 8.0), (19.0, 15.0)]),
            Icon::Down => poly(&mut path, &[(5.0, 9.0), (12.0, 16.0), (19.0, 9.0)]),
            Icon::Close => {
                poly(&mut path, &[(6.0, 6.0), (18.0, 18.0)]);
                poly(&mut path, &[(18.0, 6.0), (6.0, 18.0)]);
            }
            Icon::Menu => {
                for y in [6.0, 12.0, 18.0] {
                    poly(&mut path, &[(4.0, y), (20.0, y)]);
                }
            }
            Icon::Search => {
                let (cx, cy) = p(10.0, 10.0);
                path.ring(cx, cy, 6.5 * s, w);
                poly(&mut path, &[(14.5, 14.5), (20.0, 20.0)]);
            }
            Icon::Settings => {
                let (cx, cy) = p(12.0, 12.0);
                path.ring(cx, cy, 7.0 * s, 2.2 * s);
                path.ring(cx, cy, 3.0 * s, w);
                rays(&mut path, (cx, cy), 6.0, 10.0, 2.6 * s);
            }
            Icon::Home => {
                poly(&mut path, &[(3.0, 12.0), (12.0, 4.0), (21.0, 12.0)]);
                poly(&mut path, &[(6.0, 10.0), (6.0, 20.0), (18.0, 20.0), (18.0, 10.0)]);
                poly(&mut path, &[(10.0, 20.0), (10.0, 14.0), (14.0, 14.0), (14.0, 20.0)]);
            }
            Icon::Bookmark => poly(&mut path, &[(6.0, 3.0), (18.0, 3.0), (18.0, 21.0), (12.0, 16.0), (6.0, 21.0), (6.0, 3.0)]),
            Icon::Font => {
                poly(&mut path, &[(2.0, 20.0), (8.0, 4.0), (14.0, 20.0)]);
                poly(&mut path, &[(4.5, 14.0), (11.5, 14.0)]);
                poly(&mut path, &[(14.0, 20.0), (18.0, 10.0), (22.0, 20.0)]);
                poly(&mut path, &[(15.5, 16.5), (20.5, 16.5)]);
            }
            Icon::Light => {
                let (cx, cy) = p(12.0, 12.0);
                path.ring(cx, cy, 4.5 * s, w);
                rays(&mut path, (cx, cy), 7.0, 10.0, w);
            }
            Icon::Night => {
                let (cx, cy) = p(12.0, 12.0);
                path.circle(cx, cy, 8.5 * s);
                let (hx, hy) = p(16.0, 8.5);
                path.hole(&(0..48)
                    .map(|i| {
                        let a = i as f32 / 48.0 * 2.0 * PI;
                        (hx + 7.5 * s * a.cos(), hy + 7.5 * s * a.sin())
                    })
                    .collect::<Vec<_>>());
            }
            Icon::Rotate => {
                let (cx, cy) = p(12.0, 12.0);
                path.arc(cx, cy, 7.5 * s, -PI * 0.35, PI * 1.35, w);
                // Arrow head at the start of the arc.
                let a = -PI * 0.35;
                let (ex, ey) = (cx + 7.5 * s * a.cos(), cy + 7.5 * s * a.sin());
                path.polygon(&[(ex - 4.0 * s, ey - 3.5 * s), (ex + 3.0 * s, ey - 2.5 * s), (ex - 0.5 * s, ey + 3.5 * s)]);
            }
            Icon::Wifi => {
                let (cx, cy) = p(12.0, 19.0);
                for r in [5.0, 10.0, 15.0] {
                    path.arc(cx, cy, r * s, -PI * 0.75, -PI * 0.25, w);
                }
                path.circle(cx, cy, 1.8 * s);
            }
            Icon::Battery => {
                let (x, y) = p(2.0, 7.0);
                path.round_rect_outline(x, y, 18.0 * s, 10.0 * s, 1.5 * s, w);
                bar(&mut path, 20.5, 10.0, 22.0, 14.0);
                bar(&mut path, 5.0, 10.0, 13.0, 14.0);
            }
            Icon::Check => poly(&mut path, &[(4.0, 12.5), (9.5, 18.0), (20.0, 6.5)]),
            Icon::Plus => {
                poly(&mut path, &[(12.0, 5.0), (12.0, 19.0)]);
                poly(&mut path, &[(5.0, 12.0), (19.0, 12.0)]);
            }
            Icon::Minus => poly(&mut path, &[(5.0, 12.0), (19.0, 12.0)]),
            Icon::Shift | Icon::ShiftLocked => {
                let outline = [(12.0, 3.0), (21.0, 12.0), (16.0, 12.0), (16.0, 20.0), (8.0, 20.0), (8.0, 12.0), (3.0, 12.0), (12.0, 3.0)];
                if self == Icon::ShiftLocked {
                    fill(&mut path, &outline[..7]);
                    bar(&mut path, 8.0, 22.0, 16.0, 23.5);
                } else {
                    poly(&mut path, &outline);
                }
            }
            Icon::Backspace => {
                poly(&mut path, &[(8.0, 5.0), (21.0, 5.0), (21.0, 19.0), (8.0, 19.0), (2.5, 12.0), (8.0, 5.0)]);
                poly(&mut path, &[(11.0, 9.0), (17.0, 15.0)]);
                poly(&mut path, &[(17.0, 9.0), (11.0, 15.0)]);
            }
            Icon::Enter => {
                poly(&mut path, &[(19.0, 5.0), (19.0, 14.0), (5.0, 14.0)]);
                poly(&mut path, &[(9.0, 10.0), (5.0, 14.0), (9.0, 18.0)]);
            }
            Icon::Globe => {
                let (cx, cy) = p(12.0, 12.0);
                path.ring(cx, cy, 9.5 * s, w);
                poly(&mut path, &[(3.0, 12.0), (21.0, 12.0)]);
                // Meridian: an ellipse 4.5 wide and 9.5 tall.
                let n = 40;
                let pts: Vec<(f32, f32)> = (0..=n)
                    .map(|i| {
                        let a = i as f32 / n as f32 * 2.0 * PI;
                        (cx + 4.5 * s * a.cos(), cy + 9.5 * s * a.sin())
                    })
                    .collect();
                path.polyline(&pts, w * 0.9);
            }
            Icon::KeyboardHide => {
                let (x, y) = p(2.0, 3.0);
                path.round_rect_outline(x, y, 20.0 * s, 12.0 * s, 1.5 * s, w * 0.8);
                for (kx, ky) in [(5.0, 6.0), (9.0, 6.0), (13.0, 6.0), (17.0, 6.0), (5.0, 9.5), (17.0, 9.5)] {
                    bar(&mut path, kx, ky, kx + 2.0, ky + 2.0);
                }
                bar(&mut path, 8.0, 11.5, 16.0, 13.0);
                poly(&mut path, &[(8.0, 18.0), (12.0, 21.5), (16.0, 18.0)]);
            }
            Icon::Star => star(&mut path, p(12.0, 12.5), 5, 9.5 * s, 4.0 * s),
            Icon::Info => {
                let (cx, cy) = p(12.0, 12.0);
                path.ring(cx, cy, 9.5 * s, w);
                poly(&mut path, &[(12.0, 11.0), (12.0, 17.0)]);
                let (dx, dy) = p(12.0, 7.5);
                path.circle(dx, dy, 1.4 * s);
            }
            Icon::Trash => {
                poly(&mut path, &[(4.0, 6.0), (20.0, 6.0)]);
                poly(&mut path, &[(9.0, 6.0), (9.0, 3.5), (15.0, 3.5), (15.0, 6.0)]);
                poly(&mut path, &[(6.0, 6.0), (7.0, 21.0), (17.0, 21.0), (18.0, 6.0)]);
                poly(&mut path, &[(10.0, 10.0), (10.0, 17.0)]);
                poly(&mut path, &[(14.0, 10.0), (14.0, 17.0)]);
            }
            Icon::Book => {
                poly(&mut path, &[(12.0, 6.5), (8.0, 4.5), (2.5, 4.5), (2.5, 18.5), (8.0, 18.5), (12.0, 20.5)]);
                poly(&mut path, &[(12.0, 6.5), (16.0, 4.5), (21.5, 4.5), (21.5, 18.5), (16.0, 18.5), (12.0, 20.5)]);
                poly(&mut path, &[(12.0, 6.5), (12.0, 20.5)]);
            }
            Icon::Library => {
                poly(&mut path, &[(3.5, 4.0), (7.5, 4.0), (7.5, 20.0), (3.5, 20.0), (3.5, 4.0)]);
                poly(&mut path, &[(9.5, 7.0), (13.5, 7.0), (13.5, 20.0), (9.5, 20.0), (9.5, 7.0)]);
                poly(&mut path, &[(15.0, 6.8), (18.8, 5.8), (22.0, 19.0), (18.2, 20.0), (15.0, 6.8)]);
            }
            Icon::Grid => {
                for (x, y) in [(3.0, 3.0), (13.0, 3.0), (3.0, 13.0), (13.0, 13.0)] {
                    let (px, py) = p(x, y);
                    path.round_rect_outline(px, py, 8.0 * s, 8.0 * s, 1.5 * s, w);
                }
            }
            Icon::ListView => {
                for y in [6.0, 12.0, 18.0] {
                    bar(&mut path, 3.0, y - 1.75, 6.5, y + 1.75);
                    poly(&mut path, &[(10.0, y), (21.0, y)]);
                }
            }
            Icon::Chart => {
                bar(&mut path, 4.0, 13.0, 8.0, 19.5);
                bar(&mut path, 10.0, 8.0, 14.0, 19.5);
                bar(&mut path, 16.0, 4.0, 20.0, 19.5);
                poly(&mut path, &[(2.5, 21.0), (21.5, 21.0)]);
            }
            Icon::Chat => {
                let (x, y) = p(2.5, 3.5);
                path.round_rect_outline(x, y, 19.0 * s, 13.5 * s, 3.5 * s, w);
                fill(&mut path, &[(6.0, 16.0), (11.0, 16.0), (6.5, 21.5)]);
                poly(&mut path, &[(7.0, 8.5), (17.0, 8.5)]);
                poly(&mut path, &[(7.0, 12.0), (13.5, 12.0)]);
            }
            Icon::Toc => {
                for (x, y, end) in [(4.5, 6.0, 21.0), (4.5, 12.0, 21.0), (8.5, 18.0, 21.0)] {
                    let (cx, cy) = p(x, y);
                    path.circle(cx, cy, 1.6 * s);
                    poly(&mut path, &[(x + 4.0, y), (end, y)]);
                }
            }
            Icon::Sort => {
                poly(&mut path, &[(3.0, 6.0), (13.0, 6.0)]);
                poly(&mut path, &[(3.0, 12.0), (10.0, 12.0)]);
                poly(&mut path, &[(3.0, 18.0), (7.0, 18.0)]);
                poly(&mut path, &[(18.0, 4.0), (18.0, 20.0)]);
                poly(&mut path, &[(14.5, 16.5), (18.0, 20.0), (21.5, 16.5)]);
            }
            Icon::Filter => poly(&mut path, &[(3.0, 5.0), (21.0, 5.0), (14.0, 13.0), (14.0, 20.0), (10.0, 18.0), (10.0, 13.0), (3.0, 5.0)]),
            Icon::More => {
                for y in [5.0, 12.0, 19.0] {
                    let (cx, cy) = p(12.0, y);
                    path.circle(cx, cy, 2.2 * s);
                }
            }
            Icon::Folder => poly(&mut path, &[(3.0, 5.0), (9.5, 5.0), (11.5, 8.0), (21.0, 8.0), (21.0, 19.0), (3.0, 19.0), (3.0, 5.0)]),
            Icon::Download => {
                poly(&mut path, &[(12.0, 3.0), (12.0, 15.0)]);
                poly(&mut path, &[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
                poly(&mut path, &[(4.0, 16.0), (4.0, 20.0), (20.0, 20.0), (20.0, 16.0)]);
            }
            Icon::Sync => {
                let (cx, cy) = p(12.0, 12.0);
                let r = 7.5 * s;
                path.arc(cx, cy, r, -PI * 0.95, -PI * 0.2, w);
                path.arc(cx, cy, r, PI * 0.05, PI * 0.8, w);
                // Arrow heads at the clockwise ends of both arcs.
                for a in [-PI * 0.2, PI * 0.8] {
                    let (ex, ey) = (cx + r * a.cos(), cy + r * a.sin());
                    // Tangent (clockwise) and normal.
                    let (tx, ty) = (-a.sin(), a.cos());
                    let (nx, ny) = (a.cos(), a.sin());
                    let k = 3.2 * s;
                    path.polygon(&[(ex + tx * k, ey + ty * k), (ex - tx * k * 0.6 + nx * k, ey - ty * k * 0.6 + ny * k), (ex - tx * k * 0.6 - nx * k, ey - ty * k * 0.6 - ny * k)]);
                }
            }
            Icon::Clock => {
                let (cx, cy) = p(12.0, 12.0);
                path.ring(cx, cy, 9.5 * s, w);
                poly(&mut path, &[(12.0, 6.5), (12.0, 12.0), (16.0, 14.5)]);
            }
            Icon::Usb => {
                poly(&mut path, &[(12.0, 5.0), (12.0, 17.0)]);
                fill(&mut path, &[(9.0, 6.5), (12.0, 2.0), (15.0, 6.5)]);
                let (bx, by) = p(12.0, 19.5);
                path.circle(bx, by, 2.5 * s);
                poly(&mut path, &[(12.0, 15.0), (6.5, 12.0), (6.5, 9.5)]);
                let (lx, ly) = p(6.5, 8.0);
                path.ring(lx, ly, 1.8 * s, 1.5 * s);
                poly(&mut path, &[(12.0, 13.0), (17.5, 10.0), (17.5, 8.0)]);
                bar(&mut path, 15.8, 4.6, 19.2, 8.0);
            }
            Icon::Sparkle => {
                // Four-pointed stars with concave sides: (centre x, y, tip, waist).
                for (x, y, tip, waist) in [(10.5, 13.0, 9.5, 2.3), (19.0, 5.5, 4.2, 1.1), (19.5, 19.0, 3.0, 0.8)] {
                    star(&mut path, p(x, y), 4, tip * s, waist * s);
                }
            }
            Icon::Terminal => {
                poly(&mut path, &[(3.0, 5.0), (21.0, 5.0), (21.0, 19.0), (3.0, 19.0), (3.0, 5.0)]);
                poly(&mut path, &[(7.0, 9.0), (10.5, 12.0), (7.0, 15.0)]);
                poly(&mut path, &[(12.5, 15.0), (17.0, 15.0)]);
            }
        }
        path
    }

    /// Draws the icon into `r` with gray `value`.
    pub fn draw(self, canvas: &mut Canvas, r: Rect, value: u8) {
        self.path(r).fill(canvas, value);
    }
}

/// Half the height of a digit above the baseline, in ems (Noto Sans digits are 0.71
/// em): where to centre a battery drawn next to its percent.
pub const DIGIT_MID: f32 = 0.36;

/// A battery filling `r` (body plus the nub on the right), filled from the left to
/// `percent`, with a lightning bolt when `charging`. `bg` is the colour behind it: the
/// bolt gets a `bg` rim so it stays readable over the fill.
pub fn draw_battery(canvas: &mut Canvas, r: Rect, percent: u8, charging: bool, fg: u8, bg: u8) {
    let (x, y, h) = (r.x as f32, r.y as f32, r.h as f32);
    let stroke = (h / 10.0).round().max(2.0);
    let nub = (h * 0.16).round().max(2.0);
    let body = r.w as f32 - nub;
    // Fill: inset by the stroke and a gap of the same width; any charge shows a sliver.
    let inset = 2.0 * stroke;
    let inner = (body - 2.0 * inset).max(0.0);
    let p = f32::from(percent.min(100)) / 100.0;
    let fill = if percent == 0 { 0.0 } else { (inner * p).round().max(stroke.min(inner)) };
    if fill > 0.0 {
        Path::new().rect(x + inset, y + inset, fill, h - 2.0 * inset).fill(canvas, fg);
    }
    if charging {
        // A bolt as tall as the fill, centred on the body.
        let bh = h - 2.0 * inset;
        let bw = bh * 1.1;
        let (bx, by) = (x + (body - bw) / 2.0, y + (h - bh) / 2.0);
        let pts: Vec<(f32, f32)> = [(0.62, 0.0), (0.05, 0.58), (0.45, 0.58), (0.35, 1.0), (0.95, 0.40), (0.55, 0.40)]
            .iter()
            .map(|(u, v)| (bx + u * bw, by + v * bh))
            .collect();
        // Knocked out (`bg`) over the fill, `fg` over the empty part.
        let edge = (x + inset + fill).round() as i32;
        let prev = canvas.clip();
        let limit = |c: Rect| prev.map_or(c, |p| p.intersect(&c));
        let left = Rect::new(r.x, r.y, (edge - r.x).max(0) as u32, r.h);
        let right = Rect::new(edge, r.y, (r.right() - edge).max(0) as u32, r.h);
        for (part, value) in [(left, bg), (right, fg)] {
            canvas.set_clip(Some(limit(part)));
            Path::new().polygon(&pts).fill(canvas, value);
        }
        canvas.set_clip(prev);
    }
    let mut outline = Path::new();
    outline.round_rect_outline(x, y, body, h, stroke * 1.2, stroke);
    outline.rect(x + body, y + h * 0.3, nub, h * 0.4);
    outline.fill(canvas, fg);
}

/// A tappable icon (with an optional caption below it).
#[derive(Clone, Debug, PartialEq)]
pub struct IconView {
    /// The icon.
    pub icon: Icon,
    /// Caption under the icon (empty: none).
    pub caption: String,
    /// Action reported on tap (empty: the icon ignores taps).
    pub action: String,
    /// Drawn inverted (selected or pressed).
    pub inverted: bool,
    pressed: bool,
    /// The last event changed the look (pressed state).
    changed: bool,
}

impl IconView {
    /// An icon reporting `action` on tap.
    pub fn new(icon: Icon, action: impl Into<String>) -> Self {
        IconView { icon, caption: String::new(), action: action.into(), inverted: false, pressed: false, changed: false }
    }

    setters! {
        /// With a caption.
        with_caption(caption: impl Into<String>) => caption = caption.into();
    }
}

impl Widget for IconView {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let inv = self.inverted ^ self.pressed;
        let (fg, bg) = if inv { (cx.theme.bg, cx.theme.fg) } else { (cx.theme.fg, cx.theme.bg) };
        if inv {
            crate::draw::fill_round_rect(cx.canvas, b, cx.theme.radius, bg);
        }
        let cap_h = if self.caption.is_empty() { 0 } else { (cx.theme.small_px * 1.4) as u32 };
        let icon_side = b.w.min(b.h.saturating_sub(cap_h)) * 3 / 5;
        let icon_r = Rect::new(
            b.x + (b.w as i32 - icon_side as i32) / 2,
            b.y + (b.h.saturating_sub(cap_h) as i32 - icon_side as i32) / 2,
            icon_side,
            icon_side,
        );
        self.icon.draw(cx.canvas, icon_r, fg);
        if cap_h > 0 {
            let r = Rect::new(b.x, b.bottom() - cap_h as i32 - 4, b.w, cap_h);
            let px = cx.theme.small_px;
            cx.text_centered(r, &self.caption, px, ink_text::Style::Regular, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        if self.action.is_empty() {
            return EventResult::Ignored;
        }
        press_or_tap(ev, b, &mut self.pressed, &mut self.changed, &self.action).unwrap_or(EventResult::Ignored)
    }

    fn refresh_class(&self) -> RefreshClass {
        RefreshClass::Du
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        look_damage(&mut self.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws_inside_its_box() {
        for icon in Icon::ALL {
            let mut c = Canvas::new(120, 120);
            icon.draw(&mut c, Rect::new(10, 10, 96, 96), 0);
            let ink = c.pixels.iter().filter(|v| **v < 128).count();
            assert!(ink > 150, "{icon:?} draws {ink} dark pixels");
            // Nothing far outside the box (strokes may reach 1 px over the edge).
            for y in 0..120 {
                for x in 0..120 {
                    if c.pixels[y * 120 + x] < 128 {
                        assert!((7..110).contains(&x) && (7..110).contains(&y), "{icon:?} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn battery_fill_follows_the_charge_and_shows_a_bolt_when_charging() {
        let render = |p: u8, charging: bool| {
            let mut c = Canvas::new(80, 40);
            draw_battery(&mut c, Rect::new(4, 7, 54, 26), p, charging, 0, 255);
            if let Ok(dir) = std::env::var("INK_BATTERY_OUT") {
                c.save_png(&std::path::Path::new(&dir).join(format!("battery-{p}-{charging}.png"))).unwrap();
            }
            c
        };
        let ink = |c: &Canvas| c.pixels.iter().filter(|v| **v < 128).count();
        let levels: Vec<usize> = [0, 5, 25, 50, 75, 100].iter().map(|p| {
            render(*p, true);
            ink(&render(*p, false))
        }).collect();
        assert!(levels.windows(2).all(|w| w[0] < w[1]), "fill grows with the charge: {levels:?}");
        // Over an empty and a full battery the bolt shows (dark on empty, light rim on full).
        assert!(ink(&render(0, true)) > levels[0] + 40);
        assert!(ink(&render(100, true)) + 40 < levels[5]);
        // The bolt keeps its shape across the fill edge: as many bolt pixels at 50%.
        let bolt = |p: u8| {
            let (on, off) = (render(p, true), render(p, false));
            on.pixels.iter().zip(&off.pixels).filter(|(a, b)| (**a < 128) != (**b < 128)).count()
        };
        let (b0, b50) = (bolt(0), bolt(50));
        assert!(b50.abs_diff(b0) < b0 / 3, "bolt pixels {b0} empty, {b50} at 50%");
        // Nothing outside the box.
        let c = render(100, true);
        for y in 0..40 {
            for x in 0..80 {
                if c.pixels[y * 80 + x] < 128 {
                    assert!((4..58).contains(&x) && (7..33).contains(&y), "ink at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn icons_differ() {
        let render = |i: Icon| {
            let mut c = Canvas::new(48, 48);
            i.draw(&mut c, Rect::new(0, 0, 48, 48), 0);
            c.pixels
        };
        let all: Vec<Vec<u8>> = Icon::ALL.iter().map(|i| render(*i)).collect();
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert_ne!(all[i], all[j], "{:?} == {:?}", Icon::ALL[i], Icon::ALL[j]);
            }
        }
    }
}
