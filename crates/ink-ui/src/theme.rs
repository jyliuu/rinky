//! Colors, sizes and the design tokens (`docs/DESIGN.md`).
//!
//! Night mode is a theme flag plus the device's night flag: [`Theme::night`] keeps the
//! black-on-white palette and asks the device to invert the panel output
//! ([`ink_device::Screen::set_night_mode`]; the Kindle backend uses the MTK night
//! waveforms where it can, the simulator and headless backends invert in software).
//! Screens call [`Theme::apply`] when the theme changes and repaint with a GC16 flash
//! when it returns true. There is one palette: every gray is chosen so that it also
//! reads after the inversion ([`gray`] lists what each one becomes).
//!
//! Tokens:
//! - type scale: [`Theme::title`], [`Theme::heading`], [`Theme::body`], [`Theme::label`],
//!   [`Theme::caption`] ([`TextRole`]: size and weight);
//! - spacing: an 8 px grid, [`space`] and [`Theme::space`];
//! - shape: [`Theme::radius`] (controls), [`Theme::radius_lg`] (cards, sheets, dialogs),
//!   [`Theme::border_px`] (outlines), [`Theme::rule_px`] (dividers), [`Theme::focus_px`]
//!   (selected outlines);
//! - palette: [`Theme::fg`], [`Theme::muted`], [`Theme::subtle`], [`Theme::light`],
//!   [`Theme::bg`] (see [`gray`]);
//! - motion: [`Theme::anim_frames`], [`Theme::anim_frame_ms`].

use ink_device::Screen;
use ink_text::Style;

/// The gray levels of the palette. All are exact steps of the 16-level panel
/// (multiples of 0x11), so they never dither. Night mode shows `255 - value`.
pub mod gray {
    /// Text, icons, outlines, filled buttons (night: white).
    pub const INK: u8 = 0x00;
    /// Secondary text (captions, authors, details); still dark enough for small text
    /// (night: 0xAA).
    pub const MUTED: u8 = 0x55;
    /// Dividers, disabled controls, grab handles, unread ticks; never thinner than
    /// [`super::Theme::rule_px`] (night: 0x55).
    pub const SUBTLE: u8 = 0xAA;
    /// Fills: progress tracks, chips, key faces, cards (night: 0x22).
    pub const LIGHT: u8 = 0xDD;
    /// Background (night: black).
    pub const PAPER: u8 = 0xFF;
}

/// Spacing on the 8 px grid: `space(n)` = `8 * n` pixels.
pub const fn space(n: i32) -> i32 {
    8 * n
}

/// One step of the type scale: size in pixels and weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRole {
    /// Size in pixels.
    pub px: f32,
    /// Weight / style.
    pub style: Style,
}

impl TextRole {
    /// A role.
    pub const fn new(px: f32, style: Style) -> Self {
        TextRole { px, style }
    }
}

/// UI theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Text and line color.
    pub fg: u8,
    /// Background.
    pub bg: u8,
    /// Secondary text.
    pub muted: u8,
    /// Frame thickness (outlined buttons, cards, dialogs).
    pub border_px: i32,
    /// UI text size: the body role.
    pub font_px: f32,
    /// Small text size (status bar, captions): the caption role.
    pub small_px: f32,
    /// Minimum touch target in pixels (about 9 mm on the PW12's 300 dpi).
    pub touch_px: u32,
    /// Ask the device for night mode.
    pub night: bool,
    /// Light fill (key faces, tracks): a gray that survives the 16-level panel.
    pub light: u8,
    /// Corner radius of buttons, keys and controls.
    pub radius: f32,
    /// Dividers, disabled outlines, grab handles (a 16-level step, see [`gray`]).
    pub subtle: u8,
    /// Corner radius of cards, sheets, dialogs and popups.
    pub radius_lg: f32,
    /// Divider thickness (gray rules are never thinner: thin gray lines ghost).
    pub rule_px: i32,
    /// Outline of a selected or focused element.
    pub focus_px: i32,
    /// Screen titles (app bar of a top-level screen).
    pub title: TextRole,
    /// Section headings, dialog titles, list titles.
    pub heading: TextRole,
    /// Running UI text: list rows, menus, settings.
    pub body: TextRole,
    /// Button and tab captions.
    pub label: TextRole,
    /// Secondary lines, details, status text.
    pub caption: TextRole,
    /// Frames of an animation (each a DU update), before the settle update.
    pub anim_frames: u32,
    /// Time per animation frame in milliseconds.
    pub anim_frame_ms: u32,
}

impl Theme {
    /// Black on white.
    pub fn day() -> Self {
        Theme {
            fg: gray::INK,
            bg: gray::PAPER,
            muted: gray::MUTED,
            border_px: 3,
            font_px: 40.0,
            small_px: 28.0,
            touch_px: 108,
            night: false,
            light: gray::LIGHT,
            radius: 12.0,
            subtle: gray::SUBTLE,
            radius_lg: 24.0,
            rule_px: 3,
            focus_px: 6,
            title: TextRole::new(56.0, Style::Bold),
            heading: TextRole::new(46.0, Style::Bold),
            body: TextRole::new(40.0, Style::Regular),
            label: TextRole::new(36.0, Style::Regular),
            caption: TextRole::new(28.0, Style::Regular),
            anim_frames: 3,
            anim_frame_ms: 60,
        }
    }

    /// The same palette with night mode requested from the device.
    pub fn night() -> Self {
        Theme { night: true, ..Theme::day() }
    }

    /// Track of progress bars and sliders: `light` by day; at night `subtle`, which the
    /// inverting panel shows as 0x55 (a `light` track would show as 0x22 on black and
    /// disappear).
    pub fn track(&self) -> u8 {
        if self.night {
            self.subtle
        } else {
            self.light
        }
    }

    /// `n` steps of the 8 px spacing grid.
    pub fn space(&self, n: i32) -> i32 {
        space(n)
    }

    /// Height of a standard control (buttons, chips, segmented controls): the touch
    /// target rounded to the grid (at least 96 px).
    pub fn control_h(&self) -> u32 {
        (self.touch_px.max(96) / 8 * 8).max(96)
    }

    /// Sets the device's night flag from the theme. Returns true when it changed (the
    /// caller repaints the whole screen with a flash).
    pub fn apply(&self, screen: &mut dyn Screen) -> ink_device::Result<bool> {
        if screen.night_mode() == self.night {
            return Ok(false);
        }
        screen.set_night_mode(self.night)?;
        Ok(true)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::day()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_device::headless::{self, HeadlessOptions};
    use ink_device::{Canvas, RefreshClass};

    #[test]
    fn night_only_flips_the_flag() {
        let (d, n) = (Theme::day(), Theme::night());
        assert!(n.night && !d.night);
        assert_eq!((d.fg, d.bg), (n.fg, n.bg));
        assert_eq!(Theme { night: false, ..n }, d);
        assert_eq!(Theme::default(), d);
    }

    #[test]
    fn palette_is_on_16_level_steps_and_tokens_on_the_grid() {
        let t = Theme::day();
        for v in [t.fg, t.bg, t.muted, t.subtle, t.light] {
            assert_eq!(v % 0x11, 0, "{v:#x} is not a 16-level step");
        }
        assert!(t.muted < 128, "secondary text stays dark");
        assert!(t.fg < t.muted && t.muted < t.subtle && t.subtle < t.light && t.light < t.bg);
        assert!(t.rule_px >= 3 && t.border_px >= 3, "no thin lines");
        assert_eq!((space(1), t.space(3)), (8, 24));
        assert!(t.control_h() >= 96 && t.control_h().is_multiple_of(8));
        assert!(t.title.px > t.heading.px && t.heading.px > t.body.px && t.body.px > t.label.px && t.label.px > t.caption.px);
        assert_eq!((t.body.px, t.caption.px), (t.font_px, t.small_px));
        assert!(t.anim_frames <= 4, "few frames");
    }

    #[test]
    fn apply_sets_the_device_flag_and_the_panel_inverts() {
        let (mut dev, probe) = headless::open(HeadlessOptions::default());
        assert!(!Theme::day().apply(dev.screen.as_mut()).unwrap());
        assert!(Theme::night().apply(dev.screen.as_mut()).unwrap());
        assert!(dev.screen.night_mode());
        assert!(!Theme::night().apply(dev.screen.as_mut()).unwrap(), "already on");
        let c = Canvas::new(1272, 1696); // white
        dev.screen.update_all(&c, RefreshClass::Gc16).unwrap();
        assert_eq!(probe.panel().get(10, 10), Some(0), "white UI shows black in night mode");
        assert!(Theme::day().apply(dev.screen.as_mut()).unwrap());
        assert!(!dev.screen.night_mode());
    }
}
