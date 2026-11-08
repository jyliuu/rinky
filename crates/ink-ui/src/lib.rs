//! UI toolkit for e-ink.
//!
//! - [`widget`]: retained [`WidgetTree`] (layers, touch capture, timed layers, partial
//!   damage), [`Widget`] trait, [`Label`], [`Button`].
//! - [`controls`]: [`Toggle`], [`Slider`], [`ProgressBar`] (thick/thin, chapter marks),
//!   [`RingProgress`], [`Segmented`], [`Chips`], [`Tabs`], [`TextBlock`].
//! - [`bars`]: [`AppBar`] (back, title, actions) and [`NavBar`] (bottom navigation).
//! - [`list`]: [`List`] (paged rows) and [`ScrollView`] (paged children).
//! - [`scrollbar`]: [`ScrollBar`], the thin position bar of lists that move a screen at a
//!   time (track, thumb, tap/drag target).
//! - [`input`]: [`TextInput`] (single-line field with a cursor).
//! - [`dialog`]: [`Dialog`], [`Toast`], [`Menu`], [`BottomSheet`], [`Popup`], [`Scrim`].
//! - [`icon`]: vector [`Icon`]s and [`IconView`]; [`draw`]: anti-aliased shapes.
//! - [`refresh`]: [`Damage`], the page-turn and UI [`RefreshScheduler`].
//! - [`gesture`]: [`GestureRecognizer`] (multi-touch, timers, rotation-aware).
//! - [`pump`]: [`pump::next_events`], the event loop's input step.
//! - [`keyboard`]: on-screen keyboard (layouts en, da, de, fr, symbols; alternates,
//!   key preview, repeat on hold).
//! - [`theme`]: design tokens ([`Theme`]: type scale, 8 px spacing, radii, borders,
//!   the 16-level gray palette, motion), night mode. See `docs/DESIGN.md`.
//! - [`message`]: full-screen message (the prototype's "Opening ..." screen).
//!
//! Screens receive [`UiEvent`]s: gestures and per-contact touch notifications in logical
//! coordinates, plus keys and power events passed through from the device.
//!
//! Owner from wave A: the `ui` agent (with `ink-text`).

#![warn(missing_docs)]

/// Builder methods that each set one field, written inside an `impl` block as
/// `name(arg: Type) => field = value;` after the method's doc comment.
macro_rules! setters {
    ($($(#[$doc:meta])* $name:ident($($arg:ident: $t:ty)?) => $field:ident = $value:expr;)*) => {
        $(
            $(#[$doc])*
            pub fn $name(mut self $(, $arg: $t)?) -> Self {
                self.$field = $value;
                self
            }
        )*
    };
}

pub mod bars;
pub mod controls;
pub mod dialog;
pub mod draw;
pub mod gesture;
pub mod icon;
pub mod input;
pub mod keyboard;
pub mod list;
pub mod message;
pub mod pump;
pub mod refresh;
pub mod scrollbar;
pub mod theme;
pub mod widget;

pub use bars::{AppBar, BarAction, NavBar, NavItem};
pub use controls::{Chip, Chips, ProgressBar, ProgressKind, RingProgress, Segmented, Slider, Tabs, TextBlock, Toggle};
pub use dialog::{BottomSheet, Dialog, Menu, Popup, Scrim, Toast};
pub use gesture::{Corner, Direction, DoubleTapMode, Gesture, GestureRecognizer, ScreenSpace, Touch, TouchKind, LONG_PRESS};
pub use icon::{Icon, IconView};
pub use input::TextInput;
pub use keyboard::{KeyAction, Keyboard, KeyboardLayout};
pub use list::{paint_cover, Card, CardKind, Cover, List, ListItem, ScrollView};
pub use refresh::{Damage, RefreshScheduler};
pub use scrollbar::ScrollBar;
pub use theme::{TextRole, Theme};
pub use widget::{paint_button, Button, ButtonKind, ClipGuard, EventResult, Label, PaintCx, Widget, WidgetId, WidgetTree};

use ink_device::{InputEvent, KeyEvent, PowerEvent};

/// An event as screens see it.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// A recognised touch gesture (logical coordinates).
    Gesture(Gesture),
    /// A per-contact touch notification (logical coordinates): down, move, hold, up.
    Touch(Touch),
    /// A key.
    Key(KeyEvent),
    /// A powerd event.
    Power(PowerEvent),
    /// USB plugged or unplugged.
    Usb(bool),
    /// A timer the screen asked for fired.
    Timer(u32),
    /// The app should stop.
    Quit,
}

/// Turns device input into UI events (touch frames through the recogniser: touch
/// notifications and gestures).
pub fn translate(rec: &mut GestureRecognizer, ev: InputEvent) -> Vec<UiEvent> {
    match ev {
        InputEvent::Touch(frame) => rec.feed_events(&frame),
        InputEvent::Key(k) => vec![UiEvent::Key(k)],
        InputEvent::Power(p) => vec![UiEvent::Power(p)],
        InputEvent::Usb(u) => vec![UiEvent::Usb(u)],
        InputEvent::Quit => vec![UiEvent::Quit],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_device::{Key, Rotation, Size, Timestamp, TouchContact, TouchFrame, TouchPhase};

    #[test]
    fn translate_passes_keys_and_recognises_touch() {
        let mut rec = GestureRecognizer::new(ScreenSpace { panel: Size::new(100, 100), rotation: Rotation::Portrait });
        let k = KeyEvent { key: Key::Power, pressed: true, time: Timestamp::default() };
        assert_eq!(translate(&mut rec, InputEvent::Key(k)), vec![UiEvent::Key(k)]);
        let f = TouchFrame {
            time: Timestamp::default(),
            contacts: vec![TouchContact { slot: 0, x: 1, y: 2, phase: TouchPhase::DownUp }],
        };
        let ev = translate(&mut rec, InputEvent::Touch(f));
        assert_eq!(ev.iter().filter(|e| matches!(e, UiEvent::Gesture(Gesture::Tap { .. }))).count(), 1);
        assert_eq!(ev.iter().filter(|e| matches!(e, UiEvent::Touch(_))).count(), 2);
        assert_eq!(translate(&mut rec, InputEvent::Quit), vec![UiEvent::Quit]);
    }
}
