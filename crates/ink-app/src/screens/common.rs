//! Small helpers the library, statistics and settings screens share: the paint context
//! of an [`AppCtx`], an enabled pill button, and how a tap or swipe reads.

use crate::router::AppCtx;
use ink_device::{Point, Rect};
use ink_ui::{paint_button, ButtonKind, Gesture, Icon, PaintCx, UiEvent};

/// The paint context of `cx` (its canvas, fonts and theme).
pub(crate) fn paint_cx<'a>(cx: &'a mut AppCtx<'_>) -> PaintCx<'a> {
    PaintCx { canvas: cx.canvas, fonts: cx.fonts, theme: cx.theme }
}

/// An enabled, unpressed design-system button: filled when `primary`, else outlined.
pub(crate) fn pill(p: &mut PaintCx, r: Rect, label: &str, icon: Option<Icon>, primary: bool) {
    paint_button(p, r, label, icon, if primary { ButtonKind::Filled } else { ButtonKind::Outlined }, true, false);
}

/// The position of a tap or long press.
pub(crate) fn tap_pos(ev: &UiEvent) -> Option<Point> {
    match ev {
        UiEvent::Gesture(Gesture::Tap { pos } | Gesture::LongPress { pos, .. }) => Some(*pos),
        _ => None,
    }
}

/// +1 for a swipe left or up (next), -1 for right or down; the longer axis decides.
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

#[cfg(test)]
mod tests {
    use super::*;
    use ink_ui::Direction;

    #[test]
    fn swipes_step_along_their_longer_axis() {
        let swipe = |dx: i32, dy: i32| {
            let (from, to) = (Point::new(600, 800), Point::new(600 + dx, 800 + dy));
            let dir = if dx.abs() > dy.abs() { if dx < 0 { Direction::Left } else { Direction::Right } } else if dy < 0 { Direction::Up } else { Direction::Down };
            UiEvent::Gesture(Gesture::Swipe { from, to, dir })
        };
        assert_eq!(swipe_step(&swipe(0, -400)), Some(1));
        assert_eq!(swipe_step(&swipe(-400, 100)), Some(1));
        assert_eq!(swipe_step(&swipe(0, 400)), Some(-1));
        assert_eq!(swipe_step(&swipe(400, -100)), Some(-1));
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(5, 6) });
        assert_eq!((swipe_step(&tap), tap_pos(&tap)), (None, Some(Point::new(5, 6))));
    }
}
