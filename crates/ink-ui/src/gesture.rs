//! Gesture recognition from touch frames.
//!
//! [`GestureRecognizer`] maps panel coordinates to logical (rotated) coordinates with a
//! [`ScreenSpace`], tracks every multi-touch slot, and produces two kinds of events:
//!
//! - [`Touch`] notifications per contact ([`TouchKind::Down`], `Press`, `Move`, `Hold`,
//!   `Up`): for pressed-state feedback, dragging, key preview, long-press popups and
//!   repeat on hold. `Press` comes from a timer once the finger has rested within the tap
//!   slop for [`GestureRecognizer::press_delay`]; `Hold` comes first after
//!   [`GestureRecognizer::long_press`], then every [`GestureRecognizer::hold_repeat`].
//!
//! Press feedback (inverted buttons, rows, tiles) waits for `Press`, not `Down`: until
//! then the touch may still become a swipe, and a list scrolled by a swipe that started
//! on a row must not flash that row. [`Touch::moved`] tells widgets the contact left the
//! tap slop (it can no longer be a tap), and [`Touch::press_target`] is the shared rule.
//! A quick tap (lifted before `Press`) shows no feedback; its action repaints instead.
//! The on-screen keyboard keeps its feedback on `Down`, since typing needs it.
//! - [`Gesture`]s when a contact or a pair of contacts ends: tap, double tap, long press,
//!   swipe, two-finger tap, two-finger swipe, pinch/spread, plus [`Gesture::CornerHold`] when a hold in a
//!   screen corner reaches the long-press time.
//!
//! A contact that goes down and up in one `SYN` frame ([`TouchPhase::DownUp`], the PW12
//! chip's very short taps) is a tap; two such contacts in one frame are a two-finger tap
//! (all lifts of fingers already down come first in a frame, then all touch-downs, then
//! moves, then the lifts of `DownUp` contacts).
//!
//! Lost events: a touch-down in a slot that is still down (its release was lost) ends
//! the old contact with [`TouchKind::Cancel`] and no gesture. A contact left over from a
//! finished pair never pairs again, so a lost release of a second finger does not turn
//! later single touches into two-finger touches. [`GestureRecognizer::cancel_all`] ends
//! every contact the same way (the pump calls it when the screen rotates).
//!
//! Classification of a single contact keeps the
//! phase 0 rule: a lift within [`GestureRecognizer::tap_slop`] of the start is a tap (or
//! a long press when held long enough), anything else a swipe. One exception: a contact
//! that moves away after its first `Hold` fired (a drag after a hold: key alternates,
//! text selection) ends without any gesture; screens follow it through the touch
//! notifications. A [`Gesture::LongPress`] is sent when the hold time passes, while the
//! finger is still down (the lift sends no gesture). A [`Gesture::CornerHold`] is
//! followed by a [`Gesture::LongPress`] the same way, so a screen that acts on
//! corner holds ignores the long press that follows it.
//!
//! Timers: [`GestureRecognizer::feed`] first fires every timer due before the frame's
//! time, so scripted input (headless runs) gives the same events as real time. While
//! idle, the event loop asks [`GestureRecognizer::timeout`] how long it may sleep and then
//! calls [`GestureRecognizer::poll`] ([`crate::pump::next_events`] does both).

use crate::UiEvent;
use ink_device::{Point, Rotation, Size, Timestamp, TouchFrame, TouchPhase};
use std::time::{Duration, Instant};

/// Maps panel pixels to logical screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSpace {
    /// Panel size in its native orientation.
    pub panel: Size,
    /// Current rotation.
    pub rotation: Rotation,
}

impl ScreenSpace {
    /// Logical point of a panel point.
    pub fn to_logical(&self, p: Point) -> Point {
        self.rotation.from_panel(p, self.panel)
    }

    /// Logical screen size.
    pub fn size(&self) -> Size {
        self.rotation.logical_size(self.panel)
    }
}

/// Direction of a swipe in logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Finger moved left (next page in left-to-right books).
    Left,
    /// Finger moved right.
    Right,
    /// Finger moved up.
    Up,
    /// Finger moved down.
    Down,
}

/// A screen corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Corner {
    /// Top left.
    TopLeft,
    /// Top right.
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right.
    BottomRight,
}

/// A recognised gesture, in logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// Short touch.
    Tap {
        /// Where the finger lifted.
        pos: Point,
    },
    /// Two taps close in time and place. With [`DoubleTapMode::After`] (default) it
    /// follows the second [`Gesture::Tap`]; with [`DoubleTapMode::Delayed`] it replaces
    /// both taps.
    DoubleTap {
        /// Position of the second tap.
        pos: Point,
    },
    /// Touch held without moving, reported when the hold time passes (the finger is still
    /// down, right after the first [`TouchKind::Hold`]); the lift gives no gesture, and a
    /// finger that moves away after the hold gives none either.
    LongPress {
        /// Position.
        pos: Point,
        /// How long it was held.
        held: Duration,
    },
    /// Movement beyond the tap threshold.
    Swipe {
        /// Start.
        from: Point,
        /// End.
        to: Point,
        /// Dominant direction.
        dir: Direction,
    },
    /// Two fingers tapped together.
    TwoFingerTap {
        /// Midpoint.
        pos: Point,
    },
    /// Two fingers moved together in one direction (their distance changed less than a
    /// pinch needs). `from` and `to` are the midpoints of the two fingers.
    TwoFingerSwipe {
        /// Midpoint at the start.
        from: Point,
        /// Midpoint at the end.
        to: Point,
        /// Dominant direction of the midpoint's movement.
        dir: Direction,
    },
    /// Two fingers moved apart (`scale_milli > 1000`: spread) or together (pinch).
    Pinch {
        /// Midpoint at the end.
        center: Point,
        /// Distance ratio end/start, in thousandths.
        scale_milli: u32,
    },
    /// A still hold in a screen corner reached the long-press time (the finger is still
    /// down; a [`Gesture::LongPress`] follows at once, which a screen that handles corner
    /// holds ignores).
    CornerHold {
        /// Which corner.
        corner: Corner,
        /// Position.
        pos: Point,
    },
}

impl Gesture {
    /// Displacement of a swipe `(dx, dy)`; zero for other gestures.
    pub fn delta(&self) -> (i32, i32) {
        match *self {
            Gesture::Swipe { from, to, .. } | Gesture::TwoFingerSwipe { from, to, .. } => (to.x - from.x, to.y - from.y),
            _ => (0, 0),
        }
    }

    /// Position used for hit testing.
    pub fn pos(&self) -> Point {
        match *self {
            Gesture::Tap { pos }
            | Gesture::DoubleTap { pos }
            | Gesture::LongPress { pos, .. }
            | Gesture::TwoFingerTap { pos }
            | Gesture::CornerHold { pos, .. } => pos,
            Gesture::Swipe { from, .. } | Gesture::TwoFingerSwipe { from, .. } => from,
            Gesture::Pinch { center, .. } => center,
        }
    }
}

/// What happened to one contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TouchKind {
    /// The finger touched.
    Down,
    /// The finger rested within the tap slop for [`GestureRecognizer::press_delay`]
    /// (timer, at most once per contact, never for a two-finger pair): widgets show
    /// pressed feedback from now on.
    Press,
    /// It moved at least [`GestureRecognizer::move_step`] pixels since the last report.
    Move,
    /// It rests (timer): after the long-press time, then every repeat interval.
    Hold,
    /// It lifted.
    Up,
    /// The contact ended without a lift: the screen rotated, or a new touch-down came in
    /// the same slot (the release was lost). Widgets drop pressed state and do nothing
    /// else (no key typed, no value reported). No gesture comes from this contact.
    Cancel,
}

/// A per-contact notification (logical coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    /// Multi-touch slot.
    pub slot: u8,
    /// What happened.
    pub kind: TouchKind,
    /// Current position.
    pub pos: Point,
    /// Where the contact started.
    pub start: Point,
    /// Time since the contact went down.
    pub held: Duration,
    /// The contact has left the tap slop (it is a swipe or a drag, no tap any more).
    pub moved: bool,
}

impl Touch {
    /// The shared press-feedback rule for rows, buttons and tiles: which item (from
    /// `current`, the item shown pressed now, and `under`, the item under the finger)
    /// shows pressed after this notification.
    ///
    /// - `Press`: the item under the finger.
    /// - `Move`/`Hold` within the tap slop: `current`, while the finger stays on it.
    /// - Anything else (`Down`, a move past the slop, `Up`, `Cancel`): none. A touch-down
    ///   shows nothing, since the touch may still become a swipe.
    pub fn press_target<T: PartialEq>(&self, current: Option<T>, under: Option<T>) -> Option<T> {
        match self.kind {
            TouchKind::Press => under,
            TouchKind::Move | TouchKind::Hold if !self.moved => current.filter(|c| under.as_ref() == Some(c)),
            _ => None,
        }
    }

    /// [`Touch::press_target`] for a single item: pressed after this notification?
    /// `was`: pressed now; `inside`: the finger is on the item.
    pub fn press_on(&self, was: bool, inside: bool) -> bool {
        self.press_target(was.then_some(()), inside.then_some(())).is_some()
    }
}

/// When double taps are reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DoubleTapMode {
    /// Never: every tap is a [`Gesture::Tap`].
    Off,
    /// Taps are reported at once; a second tap close in time and place is followed by a
    /// [`Gesture::DoubleTap`]. Screens that ignore double taps lose nothing.
    #[default]
    After,
    /// A tap waits for the double-tap interval; two taps give only a
    /// [`Gesture::DoubleTap`]. For screens where tap and double tap do different things.
    Delayed,
}

#[derive(Clone, Copy, Debug)]
struct Contact {
    slot: u8,
    start: Point,
    pos: Point,
    reported: Point,
    down: Timestamp,
    moved: bool,
    holds: u32,
    next_hold: Timestamp,
    /// When [`TouchKind::Press`] is due; `None` once sent (or never: moved, paired).
    press_at: Option<Timestamp>,
    paired: bool,
    consumed: bool,
    /// The [`Gesture::LongPress`] was sent when the hold time passed (finger still down);
    /// the lift then sends no tap and no second long press.
    long_fired: bool,
}

#[derive(Clone, Copy, Debug)]
struct Pair {
    start_dist: f32,
}

fn dist(a: Point, b: Point) -> f32 {
    (((a.x - b.x) as f32).powi(2) + ((a.y - b.y) as f32).powi(2)).sqrt()
}

fn mid(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) / 2, (a.y + b.y) / 2)
}

/// True when `a` and `b` are `k` pixels or more apart on either axis (the slop rule of
/// taps, double taps, moves and two-finger taps).
fn beyond(a: Point, b: Point, k: i32) -> bool {
    (a.x - b.x).abs() >= k || (a.y - b.y).abs() >= k
}

/// Dominant direction of a movement `(dx, dy)` (horizontal on a tie).
pub fn direction(dx: i32, dy: i32) -> Direction {
    if dx.abs() >= dy.abs() {
        if dx < 0 {
            Direction::Left
        } else {
            Direction::Right
        }
    } else if dy < 0 {
        Direction::Up
    } else {
        Direction::Down
    }
}

/// How long a still finger rests before a long press: the one hold time of the whole
/// app (and its plugins). Every hold-to-act interaction (word selection, a book's menu,
/// control-center tile pickers, keyboard alternates and the start of key repeat, plugin
/// `long_press`/`corner_hold` gestures) waits for it, through
/// [`GestureRecognizer::long_press`]'s first [`TouchKind::Hold`] and
/// [`Gesture::LongPress`]. 400 ms: Android's long-press timeout (KOReader holds 500 ms).
pub const LONG_PRESS: Duration = Duration::from_millis(400);

/// Longest wait for a second tap when a screen asks for [`DoubleTapMode::Delayed`]
/// through [`GestureRecognizer::set_double_tap`]: single taps are never held back longer.
pub const DELAYED_DOUBLE_TAP: Duration = Duration::from_millis(250);

/// Turns touch frames into gestures and touch notifications.
#[derive(Clone, Debug)]
pub struct GestureRecognizer {
    /// Coordinate mapping.
    pub space: ScreenSpace,
    /// Movement (in pixels, each axis) below which a touch is a tap.
    pub tap_slop: i32,
    /// Rest within the tap slop before [`TouchKind::Press`] (pressed-state feedback).
    pub press_delay: Duration,
    /// Minimum hold for a long press (and the first [`TouchKind::Hold`]): [`LONG_PRESS`].
    /// Only tests change it (to run real-time holds quickly).
    pub long_press: Duration,
    /// Interval of further [`TouchKind::Hold`] notifications while the finger rests.
    pub hold_repeat: Duration,
    /// Double-tap reporting.
    pub double_tap: DoubleTapMode,
    /// Longest time between the two lifts of a double tap.
    pub double_tap_interval: Duration,
    /// Largest distance (each axis) between the two taps of a double tap.
    pub double_tap_slop: i32,
    /// Longest contact of a two-finger tap.
    pub two_finger_duration: Duration,
    /// Largest movement (each axis) of each finger in a two-finger tap.
    pub two_finger_slop: i32,
    /// Smallest change of finger distance for a pinch or spread.
    pub pinch_min: i32,
    /// Smallest change of the distance ratio for a pinch or spread, in thousandths
    /// (250: the distance grows to 1.25x or shrinks to 0.8x). The distance must also
    /// change more than the fingers' midpoint travels; otherwise it is a two-finger swipe.
    pub pinch_ratio_milli: u32,
    /// Smallest travel of the midpoint for a [`Gesture::TwoFingerSwipe`]; each finger
    /// must also move at least [`GestureRecognizer::two_finger_slop`] the same way.
    pub two_finger_swipe_min: i32,
    /// Size of the corner squares for [`Gesture::CornerHold`].
    pub corner_px: i32,
    /// Movement between [`TouchKind::Move`] notifications.
    pub move_step: i32,
    /// Overlapping contacts are independent taps (no two-finger gestures), as on a
    /// keyboard typed with two thumbs.
    pub concurrent_taps: bool,
    contacts: Vec<Contact>,
    pair: Option<Pair>,
    last_tap: Option<(Point, Timestamp)>,
    pending_tap: Option<(Point, Timestamp)>,
    clock: Option<(Timestamp, Instant)>,
}

impl GestureRecognizer {
    /// A recogniser with the prototype's thresholds (80 px tap slop), press feedback after
    /// 150 ms, a [`LONG_PRESS`] long press and KOReader's double-tap and two-finger timings.
    pub fn new(space: ScreenSpace) -> Self {
        GestureRecognizer {
            space,
            tap_slop: 80,
            press_delay: Duration::from_millis(150),
            long_press: LONG_PRESS,
            hold_repeat: Duration::from_millis(150),
            double_tap: DoubleTapMode::After,
            double_tap_interval: Duration::from_millis(300),
            double_tap_slop: 100,
            two_finger_duration: Duration::from_millis(300),
            two_finger_slop: 40,
            pinch_min: 60,
            pinch_ratio_milli: 250,
            two_finger_swipe_min: 100,
            corner_px: 160,
            move_step: 8,
            concurrent_taps: false,
            contacts: Vec::new(),
            pair: None,
            last_tap: None,
            pending_tap: None,
            clock: None,
        }
    }

    /// Sets the double-tap mode. [`DoubleTapMode::Delayed`] waits at most
    /// [`DELAYED_DOUBLE_TAP`] for the second tap; the other modes use KOReader's 300 ms.
    /// Cheap when nothing changes (the event loop calls it before every wait).
    pub fn set_double_tap(&mut self, mode: DoubleTapMode) {
        if self.double_tap == mode {
            return;
        }
        self.double_tap = mode;
        self.double_tap_interval = if mode == DoubleTapMode::Delayed { DELAYED_DOUBLE_TAP } else { Duration::from_millis(300) };
        log::info!("gesture: double tap mode {mode:?}");
    }

    /// Feeds one frame; returns the gestures it completes (touch notifications dropped).
    pub fn feed(&mut self, frame: &TouchFrame) -> Vec<Gesture> {
        self.feed_events(frame)
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Gesture(g) => Some(g),
                _ => None,
            })
            .collect()
    }

    /// Feeds one frame; returns timer events that were due before it, then touch
    /// notifications and gestures in order.
    ///
    /// Contacts of one frame are handled in this order, whatever their order in the
    /// frame: lifts of fingers already down, then touch-downs (also the down half of
    /// [`TouchPhase::DownUp`]), then moves, then the up half of `DownUp`. So two fingers
    /// that touch and lift in one frame are on the glass together (a two-finger tap), and
    /// a finger that lifts in the frame where another lands is not paired with it.
    pub fn feed_events(&mut self, frame: &TouchFrame) -> Vec<UiEvent> {
        let mut out = Vec::new();
        self.tick_into(frame.time, &mut out);
        self.clock = Some((frame.time, Instant::now()));
        let space = self.space;
        let at = move |c: &ink_device::TouchContact| space.to_logical(Point::new(c.x, c.y));
        // Positions first: a pair lifting (or one finger lifting while the other moves)
        // is measured at the final positions.
        for c in frame.contacts.iter().filter(|c| matches!(c.phase, TouchPhase::Up | TouchPhase::Move)) {
            let p = at(c);
            if let Some(k) = self.contacts.iter_mut().find(|k| k.slot == c.slot) {
                k.pos = p;
            }
        }
        let phase = |want: &'static [TouchPhase]| frame.contacts.iter().filter(move |c| want.contains(&c.phase));
        for c in phase(&[TouchPhase::Up]) {
            self.up(c.slot, at(c), frame.time, &mut out);
        }
        for c in phase(&[TouchPhase::Down, TouchPhase::DownUp]) {
            self.down(c.slot, at(c), frame.time, &mut out);
        }
        for c in phase(&[TouchPhase::Move]) {
            self.moved(c.slot, at(c), &mut out);
        }
        for c in phase(&[TouchPhase::DownUp]) {
            self.up(c.slot, at(c), frame.time, &mut out);
        }
        out
    }

    /// Fires timers due at input time `now` (holds, a delayed tap).
    pub fn tick(&mut self, now: Timestamp) -> Vec<UiEvent> {
        let mut out = Vec::new();
        self.tick_into(now, &mut out);
        out
    }

    /// Input-clock time now, estimated from the last frame and the wall clock.
    fn now(&self) -> Option<Timestamp> {
        self.clock.map(|(t, at)| Timestamp(t.0 + at.elapsed()))
    }

    /// Input time of the next timer, if any.
    pub fn next_deadline(&self) -> Option<Timestamp> {
        let holds = self.contacts.iter().filter(|c| self.holds_allowed(c)).map(|c| c.next_hold);
        let presses = self.contacts.iter().filter(|c| self.holds_allowed(c)).filter_map(|c| c.press_at);
        let pending = self.pending_tap.map(|(_, t)| Timestamp(t.0 + self.double_tap_interval));
        holds.chain(presses).chain(pending).min()
    }

    /// How long the event loop may wait before calling [`GestureRecognizer::poll`].
    pub fn timeout(&self) -> Option<Duration> {
        let deadline = self.next_deadline()?;
        let now = self.now()?;
        Some(deadline.since(now))
    }

    /// Fires the timers that are due now (wall clock bridged to input time).
    pub fn poll(&mut self) -> Vec<UiEvent> {
        match self.now() {
            Some(now) => self.tick(now),
            None => Vec::new(),
        }
    }

    /// Forgets touches in progress (e.g. after a screen change).
    pub fn reset(&mut self) {
        self.contacts.clear();
        self.pair = None;
        self.last_tap = None;
        self.pending_tap = None;
    }

    /// Ends every touch in progress with a [`TouchKind::Cancel`] notification (so widgets
    /// that captured a contact drop their pressed state), then [`GestureRecognizer::reset`].
    /// [`crate::pump::next_events`] calls it when the screen rotates.
    pub fn cancel_all(&mut self) -> Vec<UiEvent> {
        let now = self.clock.map(|(t, _)| t);
        let out = self.contacts.iter().map(|c| self.touch(c, TouchKind::Cancel, now.unwrap_or(c.down).max(c.down))).collect();
        self.reset();
        out
    }

    /// Number of fingers down.
    pub fn active_contacts(&self) -> usize {
        self.contacts.len()
    }

    fn holds_allowed(&self, c: &Contact) -> bool {
        !c.moved && !c.paired && !c.consumed
    }

    fn touch(&self, c: &Contact, kind: TouchKind, now: Timestamp) -> UiEvent {
        UiEvent::Touch(Touch { slot: c.slot, kind, pos: c.pos, start: c.start, held: now.since(c.down), moved: c.moved })
    }

    fn corner(&self, p: Point) -> Option<Corner> {
        let s = self.space.size();
        let (w, h, k) = (s.w as i32, s.h as i32, self.corner_px);
        let left = p.x < k;
        let right = p.x >= w - k;
        let top = p.y < k;
        let bottom = p.y >= h - k;
        match (left, right, top, bottom) {
            (true, _, true, _) => Some(Corner::TopLeft),
            (_, true, true, _) => Some(Corner::TopRight),
            (true, _, _, true) => Some(Corner::BottomLeft),
            (_, true, _, true) => Some(Corner::BottomRight),
            _ => None,
        }
    }

    fn tick_into(&mut self, now: Timestamp, out: &mut Vec<UiEvent>) {
        enum Due {
            Press(usize),
            Hold(usize),
            Tap(Point),
        }
        loop {
            // The earliest due timer first, so events come in time order (on a tie: press,
            // then hold, then the delayed tap).
            let mut due: Option<(Timestamp, Due)> = None;
            let mut offer = |at: Timestamp, d: Due| {
                if at <= now && due.as_ref().is_none_or(|(t, _)| at < *t) {
                    due = Some((at, d));
                }
            };
            for (i, c) in self.contacts.iter().enumerate().filter(|(_, c)| self.holds_allowed(c)) {
                if let Some(at) = c.press_at {
                    offer(at, Due::Press(i));
                }
            }
            for (i, c) in self.contacts.iter().enumerate().filter(|(_, c)| self.holds_allowed(c)) {
                offer(c.next_hold, Due::Hold(i));
            }
            if let Some((p, t)) = self.pending_tap {
                offer(Timestamp(t.0 + self.double_tap_interval), Due::Tap(p));
            }
            match due {
                Some((at, Due::Press(i))) => {
                    self.contacts[i].press_at = None;
                    let c = self.contacts[i];
                    out.push(self.touch(&c, TouchKind::Press, at));
                }
                Some((at, Due::Hold(i))) => {
                    let c = self.contacts[i];
                    out.push(self.touch(&c, TouchKind::Hold, at));
                    if c.holds == 0 {
                        if let Some(corner) = self.corner(c.pos) {
                            out.push(UiEvent::Gesture(Gesture::CornerHold { corner, pos: c.pos }));
                        }
                        // The long press fires now, while the finger is down: the screen
                        // can open its popup at once. The lift sends nothing more.
                        out.push(UiEvent::Gesture(Gesture::LongPress { pos: c.pos, held: at.since(c.down) }));
                        self.contacts[i].long_fired = true;
                    }
                    let c = &mut self.contacts[i];
                    c.holds += 1;
                    c.next_hold = Timestamp(at.0 + self.hold_repeat.max(Duration::from_millis(10)));
                }
                Some((_, Due::Tap(p))) => {
                    self.pending_tap = None;
                    out.push(UiEvent::Gesture(Gesture::Tap { pos: p }));
                }
                None => break,
            }
        }
    }

    /// Ends contact `i` without a gesture (its release was lost): a `Cancel` notification,
    /// and a pair it belonged to is over (the partner gives no gesture either).
    fn cancel(&mut self, i: usize, t: Timestamp, out: &mut Vec<UiEvent>) {
        let c = self.contacts.remove(i);
        out.push(self.touch(&c, TouchKind::Cancel, t.max(c.down)));
        if c.paired && !c.consumed && self.pair.take().is_some() {
            for o in self.contacts.iter_mut().filter(|o| o.paired && !o.consumed) {
                o.consumed = true;
            }
        }
    }

    fn down(&mut self, slot: u8, p: Point, t: Timestamp, out: &mut Vec<UiEvent>) {
        // A new touch-down in a slot that is still down: the old release was lost.
        if let Some(i) = self.contacts.iter().position(|c| c.slot == slot) {
            self.cancel(i, t, out);
        }
        if let Some((pp, _)) = self.pending_tap {
            if beyond(pp, p, self.double_tap_slop) {
                self.pending_tap = None;
                out.push(UiEvent::Gesture(Gesture::Tap { pos: pp }));
            }
        }
        let c = Contact {
            slot,
            start: p,
            pos: p,
            reported: p,
            down: t,
            moved: false,
            holds: 0,
            next_hold: Timestamp(t.0 + self.long_press),
            press_at: Some(Timestamp(t.0 + self.press_delay)),
            paired: false,
            consumed: false,
            long_fired: false,
        };
        out.push(self.touch(&c, TouchKind::Down, t));
        self.contacts.push(c);
        // Two live fingers make a pair. A contact consumed by a finished pair (its partner
        // lifted, or its own release was lost) never pairs again: it only waits for its
        // lift, so later single touches stay single.
        let live: Vec<usize> = (0..self.contacts.len()).filter(|i| !self.contacts[*i].consumed).collect();
        if !self.concurrent_taps && self.pair.is_none() && live.len() == 2 {
            let (a, b) = (self.contacts[live[0]].pos, self.contacts[live[1]].pos);
            self.pair = Some(Pair { start_dist: dist(a, b).max(1.0) });
            for i in live {
                self.contacts[i].paired = true;
            }
        }
    }

    fn moved(&mut self, slot: u8, p: Point, out: &mut Vec<UiEvent>) {
        let (slop, step) = (self.tap_slop, self.move_step);
        let Some(c) = self.contacts.iter_mut().find(|c| c.slot == slot) else { return };
        c.pos = p;
        if beyond(p, c.start, slop) {
            c.moved = true;
        }
        if beyond(p, c.reported, step) {
            c.reported = p;
            let c = *c;
            // A move has no timestamp of its own; `held` is measured at the last frame.
            let now = self.clock.map(|(t, _)| t).unwrap_or(c.down);
            out.push(self.touch(&c, TouchKind::Move, now));
        }
    }

    fn up(&mut self, slot: u8, p: Point, t: Timestamp, out: &mut Vec<UiEvent>) {
        let Some(i) = self.contacts.iter().position(|c| c.slot == slot) else { return };
        let mut c = self.contacts.remove(i);
        c.pos = p;
        out.push(self.touch(&c, TouchKind::Up, t));
        // A consumed contact (left over from an earlier pair) never ends the current pair.
        if c.paired && !c.consumed && self.pair.is_some() {
            let pair = self.pair.take().expect("checked");
            let Some(other) = self.contacts.iter_mut().find(|o| o.paired && !o.consumed) else { return };
            other.consumed = true;
            let o = *other;
            let still = |x: &Contact| !beyond(x.pos, x.start, self.two_finger_slop);
            let short = t.since(c.down) < self.two_finger_duration && t.since(o.down) < self.two_finger_duration;
            let d = dist(c.pos, o.pos);
            let change = (d - pair.start_dist).abs();
            let ratio = d / pair.start_dist;
            let r = 1.0 + self.pinch_ratio_milli as f32 / 1000.0;
            let (m0, m1) = (mid(c.start, o.start), mid(c.pos, o.pos));
            let travel = dist(m0, m1);
            let dir = direction(m1.x - m0.x, m1.y - m0.y);
            // Each finger moved at least the slop, in the midpoint's direction.
            let along = |x: &Contact| beyond(x.pos, x.start, self.two_finger_slop) && direction(x.pos.x - x.start.x, x.pos.y - x.start.y) == dir;
            let g = if short && still(&c) && still(&o) {
                Some(Gesture::TwoFingerTap { pos: m1 })
            } else if change >= self.pinch_min as f32 && (ratio >= r || ratio <= 1.0 / r) && change > travel {
                Some(Gesture::Pinch { center: m1, scale_milli: (ratio * 1000.0).round() as u32 })
            } else if travel >= self.two_finger_swipe_min as f32 && along(&c) && along(&o) {
                Some(Gesture::TwoFingerSwipe { from: m0, to: m1, dir })
            } else {
                None
            };
            if let Some(g) = g {
                out.push(UiEvent::Gesture(g));
            }
            return;
        }
        if c.consumed || c.paired {
            return;
        }
        let still = !beyond(p, c.start, self.tap_slop);
        if c.holds > 0 && (c.moved || !still) {
            // Dragged after a hold fired (key alternates, text selection): the drag is
            // reported by the touch notifications only, never as a swipe.
            return;
        }
        if c.long_fired {
            // The long press was sent when the hold time passed; this lift is its end.
            return;
        }
        if still {
            let held = t.since(c.down);
            if held >= self.long_press {
                out.push(UiEvent::Gesture(Gesture::LongPress { pos: p, held }));
            } else {
                self.tap(p, t, out);
            }
        } else {
            let dir = direction(p.x - c.start.x, p.y - c.start.y);
            out.push(UiEvent::Gesture(Gesture::Swipe { from: c.start, to: p, dir }));
        }
    }

    fn tap(&mut self, p: Point, t: Timestamp, out: &mut Vec<UiEvent>) {
        let near = |q: Point| !beyond(q, p, self.double_tap_slop);
        let soon = |at: Timestamp| t.since(at) < self.double_tap_interval;
        match self.double_tap {
            DoubleTapMode::Off => out.push(UiEvent::Gesture(Gesture::Tap { pos: p })),
            DoubleTapMode::After => {
                out.push(UiEvent::Gesture(Gesture::Tap { pos: p }));
                match self.last_tap {
                    Some((q, at)) if near(q) && soon(at) => {
                        out.push(UiEvent::Gesture(Gesture::DoubleTap { pos: p }));
                        self.last_tap = None;
                    }
                    _ => self.last_tap = Some((p, t)),
                }
            }
            DoubleTapMode::Delayed => match self.pending_tap.take() {
                Some((q, at)) if near(q) && soon(at) => out.push(UiEvent::Gesture(Gesture::DoubleTap { pos: p })),
                Some((q, _)) => {
                    out.push(UiEvent::Gesture(Gesture::Tap { pos: q }));
                    self.pending_tap = Some((p, t));
                }
                None => self.pending_tap = Some((p, t)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_device::TouchContact;

    fn frame(ms: u64, x: i32, y: i32, phase: TouchPhase) -> TouchFrame {
        TouchFrame { time: Timestamp::from_millis(ms), contacts: vec![TouchContact { slot: 0, x, y, phase }] }
    }

    fn frame2(ms: u64, a: (i32, i32, TouchPhase), b: (i32, i32, TouchPhase)) -> TouchFrame {
        TouchFrame {
            time: Timestamp::from_millis(ms),
            contacts: vec![
                TouchContact { slot: 0, x: a.0, y: a.1, phase: a.2 },
                TouchContact { slot: 1, x: b.0, y: b.1, phase: b.2 },
            ],
        }
    }

    fn rec(rotation: Rotation) -> GestureRecognizer {
        GestureRecognizer::new(ScreenSpace { panel: Size::new(1272, 1696), rotation })
    }

    fn touches(ev: &[UiEvent]) -> Vec<TouchKind> {
        ev.iter()
            .filter_map(|e| match e {
                UiEvent::Touch(t) => Some(t.kind),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tap_swipe_longpress_quicktap() {
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Off;
        assert!(r.feed(&frame(0, 100, 100, TouchPhase::Down)).is_empty());
        assert_eq!(r.feed(&frame(50, 120, 110, TouchPhase::Up)), vec![Gesture::Tap { pos: Point::new(120, 110) }]);
        r.feed(&frame(100, 1000, 800, TouchPhase::Down));
        r.feed(&frame(150, 600, 810, TouchPhase::Move));
        let g = r.feed(&frame(200, 200, 820, TouchPhase::Up));
        assert!(matches!(g[0], Gesture::Swipe { dir: Direction::Left, .. }));
        assert_eq!(g[0].delta(), (-800, 20));
        r.feed(&frame(300, 50, 500, TouchPhase::Down));
        // The long press comes with the hold timer (frame 1000 fires it first); the lift adds none.
        assert!(matches!(r.feed(&frame(1000, 52, 500, TouchPhase::Up))[..], [Gesture::LongPress { .. }]));
        assert_eq!(r.feed(&frame(1100, 7, 9, TouchPhase::DownUp)), vec![Gesture::Tap { pos: Point::new(7, 9) }]);
        // An Up without a Down is ignored.
        assert!(r.feed(&frame(1200, 7, 9, TouchPhase::Up)).is_empty());
    }

    #[test]
    fn rotation_maps_taps_and_swipe_directions() {
        let mut r = rec(Rotation::Clockwise);
        // Panel (1271, 0) is logical (0, 0) when rotated clockwise.
        assert_eq!(r.feed(&frame(0, 1271, 0, TouchPhase::DownUp)), vec![Gesture::Tap { pos: Point::new(0, 0) }]);
        // Moving down the panel (y grows) is moving right on the rotated screen.
        r.feed(&frame(1000, 600, 100, TouchPhase::Down));
        let g = r.feed(&frame(1050, 600, 900, TouchPhase::Up));
        assert!(matches!(g[0], Gesture::Swipe { dir: Direction::Right, .. }), "{g:?}");
    }

    #[test]
    fn touch_notifications_and_hold_timers() {
        let mut r = rec(Rotation::Portrait);
        let ev = r.feed_events(&frame(0, 500, 500, TouchPhase::Down));
        assert_eq!(touches(&ev), vec![TouchKind::Down]);
        assert_eq!(r.next_deadline(), Some(Timestamp::from_millis(150)));
        // Small moves below the step are not reported.
        assert!(r.feed_events(&frame(100, 503, 500, TouchPhase::Move)).is_empty());
        // The press timer (150 ms) fires before the frame's move.
        assert_eq!(touches(&r.feed_events(&frame(150, 512, 500, TouchPhase::Move))), vec![TouchKind::Press, TouchKind::Move]);
        assert_eq!(r.next_deadline(), Some(Timestamp::from_millis(400)));
        // Timers fire at 400, 550, 700, 850 ms.
        let ev = r.tick(Timestamp::from_millis(910));
        assert_eq!(touches(&ev), vec![TouchKind::Hold; 4]);
        let UiEvent::Touch(first) = ev[0] else { panic!() };
        assert_eq!(first.held, LONG_PRESS);
        // The long press came with the first hold, while the finger was down.
        let LongPressAt(held) = long_presses(&ev);
        assert_eq!(held, vec![LONG_PRESS]);
        // The lift fires the remaining due holds first, then Up; no second long press.
        let ev = r.feed_events(&frame(1100, 512, 500, TouchPhase::Up));
        assert_eq!(touches(&ev), vec![TouchKind::Hold, TouchKind::Up]);
        assert!(ev.iter().all(|e| !matches!(e, UiEvent::Gesture(_))), "{ev:?}");
        assert_eq!(r.next_deadline(), None);
        // A DownUp contact gives Down and Up notifications too.
        let ev = r.feed_events(&frame(2000, 10, 10, TouchPhase::DownUp));
        assert_eq!(touches(&ev), vec![TouchKind::Down, TouchKind::Up]);
    }

    #[test]
    fn press_comes_after_a_still_rest_only() {
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Off;
        // Still finger: Press at 150 ms (before the 400 ms hold), once.
        r.feed_events(&frame(0, 500, 500, TouchPhase::Down));
        assert_eq!(r.next_deadline(), Some(Timestamp::from_millis(150)));
        assert!(r.tick(Timestamp::from_millis(149)).is_empty());
        let ev = r.tick(Timestamp::from_millis(160));
        assert_eq!(touches(&ev), vec![TouchKind::Press]);
        let UiEvent::Touch(p) = ev[0] else { panic!() };
        assert_eq!((p.held, p.moved), (Duration::from_millis(150), false));
        assert_eq!(r.next_deadline(), Some(Timestamp::from_millis(400)));
        // Hold and LongPress timings are unchanged.
        let ev = r.tick(Timestamp::from_millis(410));
        assert_eq!(touches(&ev), vec![TouchKind::Hold]);
        assert!(matches!(ev.last(), Some(UiEvent::Gesture(Gesture::LongPress { .. }))), "fires while down");
        let ev = r.feed_events(&frame(700, 505, 500, TouchPhase::Up));
        assert!(ev.iter().all(|e| !matches!(e, UiEvent::Gesture(_))), "the lift adds no gesture: {ev:?}");
        // A quick tap: no Press at all.
        let ev = r.feed_events(&frame(1000, 500, 500, TouchPhase::Down));
        assert_eq!(touches(&ev), vec![TouchKind::Down]);
        let ev = r.feed_events(&frame(1080, 500, 500, TouchPhase::Up));
        assert_eq!(touches(&ev), vec![TouchKind::Up]);
        assert!(matches!(ev.last(), Some(UiEvent::Gesture(Gesture::Tap { .. }))));
        // A swipe that leaves the slop before 150 ms: no Press, the Move says `moved`.
        r.feed_events(&frame(2000, 500, 1200, TouchPhase::Down));
        let ev = r.feed_events(&frame(2050, 500, 1000, TouchPhase::Move));
        assert!(matches!(ev[..], [UiEvent::Touch(Touch { kind: TouchKind::Move, moved: true, .. })]), "{ev:?}");
        assert!(r.tick(Timestamp::from_millis(2300)).is_empty(), "no Press or Hold after the slop");
        let ev = r.feed_events(&frame(2350, 500, 600, TouchPhase::Up));
        assert!(matches!(ev.last(), Some(UiEvent::Gesture(Gesture::Swipe { dir: Direction::Up, .. }))));
        // A slow swipe (Press first, then away): still a Swipe, never a tap.
        r.feed_events(&frame(3000, 500, 1200, TouchPhase::Down));
        assert_eq!(touches(&r.tick(Timestamp::from_millis(3200))), vec![TouchKind::Press]);
        let ev = r.feed_events(&frame(3300, 500, 900, TouchPhase::Move));
        assert!(matches!(ev[..], [UiEvent::Touch(Touch { kind: TouchKind::Move, moved: true, .. })]), "{ev:?}");
        let ev = r.feed_events(&frame(3400, 500, 600, TouchPhase::Up));
        assert!(matches!(ev.last(), Some(UiEvent::Gesture(Gesture::Swipe { dir: Direction::Up, .. }))), "{ev:?}");
        assert!(!ev.iter().any(|e| matches!(e, UiEvent::Gesture(Gesture::Tap { .. }))));
    }

    #[test]
    fn press_target_rule() {
        let t = |kind, moved| Touch { slot: 0, kind, pos: Point::new(0, 0), start: Point::new(0, 0), held: Duration::ZERO, moved };
        assert_eq!(t(TouchKind::Down, false).press_target(None, Some(2)), None);
        assert_eq!(t(TouchKind::Press, false).press_target(None, Some(2)), Some(2));
        assert_eq!(t(TouchKind::Move, false).press_target(Some(2), Some(2)), Some(2));
        assert_eq!(t(TouchKind::Move, false).press_target(Some(2), Some(3)), None, "left the row");
        assert_eq!(t(TouchKind::Move, true).press_target(Some(2), Some(2)), None, "left the slop");
        assert_eq!(t(TouchKind::Move, false).press_target(None, Some(2)), None, "no press before Press");
        assert_eq!(t(TouchKind::Hold, false).press_target(Some(2), Some(2)), Some(2));
        assert_eq!(t(TouchKind::Up, false).press_target(Some(2), Some(2)), None);
        assert!(!t(TouchKind::Cancel, false).press_on(true, true));
    }

    #[test]
    fn drag_after_a_hold_is_not_a_swipe() {
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Off;
        // Hold, then slide away (Move frames) and lift: touch notifications only.
        r.feed_events(&frame(0, 500, 500, TouchPhase::Down));
        assert_eq!(touches(&r.tick(Timestamp::from_millis(450))), vec![TouchKind::Press, TouchKind::Hold]);
        let ev = r.feed_events(&frame(500, 700, 300, TouchPhase::Move));
        assert_eq!(touches(&ev), vec![TouchKind::Move]);
        let ev = r.feed_events(&frame(600, 700, 300, TouchPhase::Up));
        assert_eq!(touches(&ev), vec![TouchKind::Up]);
        assert!(ev.iter().all(|e| !matches!(e, UiEvent::Gesture(_))), "{ev:?}");
        // Dragged away and back before lifting: still a drag, not a long press.
        r.feed_events(&frame(1000, 500, 500, TouchPhase::Down));
        r.tick(Timestamp::from_millis(1650));
        r.feed_events(&frame(1700, 800, 500, TouchPhase::Move));
        r.feed_events(&frame(1800, 505, 500, TouchPhase::Move));
        assert!(r.feed(&frame(1900, 505, 500, TouchPhase::Up)).is_empty());
        // The lift frame alone carries the new position.
        r.feed_events(&frame(2000, 500, 500, TouchPhase::Down));
        r.tick(Timestamp::from_millis(2650));
        assert!(r.feed(&frame(2700, 900, 500, TouchPhase::Up)).is_empty());
        // Moving before the hold time is a swipe as before; holding still is a long press.
        r.feed(&frame(3000, 500, 500, TouchPhase::Down));
        r.feed(&frame(3100, 800, 500, TouchPhase::Move));
        assert!(matches!(r.feed(&frame(3900, 900, 500, TouchPhase::Up))[..], [Gesture::Swipe { dir: Direction::Right, .. }]));
        r.feed(&frame(4000, 500, 500, TouchPhase::Down));
        assert!(matches!(r.feed(&frame(4700, 510, 505, TouchPhase::Up))[..], [Gesture::LongPress { .. }]));
    }

    #[test]
    fn moving_cancels_holds() {
        let mut r = rec(Rotation::Portrait);
        r.feed_events(&frame(0, 500, 500, TouchPhase::Down));
        r.feed_events(&frame(100, 700, 500, TouchPhase::Move));
        assert_eq!(r.next_deadline(), None);
        assert!(touches(&r.tick(Timestamp::from_millis(2000))).is_empty());
    }

    #[test]
    fn double_tap_modes() {
        let taps = |r: &mut GestureRecognizer| {
            let mut g = r.feed(&frame(0, 300, 300, TouchPhase::DownUp));
            g.extend(r.feed(&frame(150, 310, 305, TouchPhase::DownUp)));
            g.extend(r.tick(Timestamp::from_millis(2000)).into_iter().filter_map(|e| match e {
                UiEvent::Gesture(g) => Some(g),
                _ => None,
            }));
            g
        };
        let mut r = rec(Rotation::Portrait);
        let p1 = Point::new(300, 300);
        let p2 = Point::new(310, 305);
        assert_eq!(taps(&mut r), vec![Gesture::Tap { pos: p1 }, Gesture::Tap { pos: p2 }, Gesture::DoubleTap { pos: p2 }]);
        r.double_tap = DoubleTapMode::Delayed;
        assert_eq!(taps(&mut r), vec![Gesture::DoubleTap { pos: p2 }]);
        r.double_tap = DoubleTapMode::Off;
        assert_eq!(taps(&mut r), vec![Gesture::Tap { pos: p1 }, Gesture::Tap { pos: p2 }]);
        // Delayed: a single tap comes out when the interval ends (timer) ...
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Delayed;
        assert!(r.feed(&frame(0, 300, 300, TouchPhase::DownUp)).is_empty());
        assert_eq!(r.next_deadline(), Some(Timestamp::from_millis(300)));
        assert_eq!(r.tick(Timestamp::from_millis(300)), vec![UiEvent::Gesture(Gesture::Tap { pos: p1 })]);
        // ... or as soon as a touch starts somewhere else.
        r.feed(&frame(1000, 300, 300, TouchPhase::DownUp));
        let g = r.feed(&frame(1100, 900, 900, TouchPhase::Down));
        assert_eq!(g, vec![Gesture::Tap { pos: p1 }]);
        // Taps far apart or slow are not double taps.
        let mut r = rec(Rotation::Portrait);
        r.feed(&frame(0, 300, 300, TouchPhase::DownUp));
        assert_eq!(r.feed(&frame(100, 600, 300, TouchPhase::DownUp)).len(), 1);
        assert_eq!(r.feed(&frame(1000, 600, 300, TouchPhase::DownUp)).len(), 1);
    }

    #[test]
    fn two_finger_tap_and_pinch() {
        let mut r = rec(Rotation::Portrait);
        r.feed(&frame2(0, (400, 800, TouchPhase::Down), (800, 800, TouchPhase::Down)));
        let g = r.feed(&frame2(120, (402, 801, TouchPhase::Up), (799, 802, TouchPhase::Up)));
        assert_eq!(g, vec![Gesture::TwoFingerTap { pos: Point::new(600, 801) }]);
        // Spread: fingers move apart.
        r.feed(&frame2(1000, (500, 800, TouchPhase::Down), (700, 800, TouchPhase::Down)));
        r.feed(&frame2(1100, (400, 800, TouchPhase::Move), (800, 800, TouchPhase::Move)));
        r.feed(&frame2(1200, (300, 800, TouchPhase::Move), (900, 800, TouchPhase::Move)));
        let g = r.feed(&frame2(1300, (300, 800, TouchPhase::Up), (900, 800, TouchPhase::Up)));
        assert_eq!(g, vec![Gesture::Pinch { center: Point::new(600, 800), scale_milli: 3000 }]);
        // Pinch: fingers move together; the second finger lifts later and adds nothing.
        r.feed(&frame2(2000, (200, 400, TouchPhase::Down), (1000, 1200, TouchPhase::Down)));
        r.feed(&frame2(2100, (500, 700, TouchPhase::Move), (700, 900, TouchPhase::Move)));
        let g = r.feed(&TouchFrame {
            time: Timestamp::from_millis(2200),
            contacts: vec![TouchContact { slot: 0, x: 500, y: 700, phase: TouchPhase::Up }],
        });
        assert!(matches!(g[0], Gesture::Pinch { scale_milli, .. } if scale_milli < 300), "{g:?}");
        let g = r.feed(&TouchFrame {
            time: Timestamp::from_millis(2300),
            contacts: vec![TouchContact { slot: 1, x: 700, y: 900, phase: TouchPhase::Up }],
        });
        assert!(g.is_empty());
        // Two fingers held still and long: neither tap nor pinch.
        r.feed(&frame2(3000, (400, 800, TouchPhase::Down), (800, 800, TouchPhase::Down)));
        assert!(r.feed(&frame2(4000, (400, 800, TouchPhase::Up), (800, 800, TouchPhase::Up))).is_empty());
        // Two fingers moving up together (distance nearly the same): a two-finger swipe.
        r.feed(&frame2(4500, (400, 1200, TouchPhase::Down), (800, 1200, TouchPhase::Down)));
        r.feed(&frame2(4600, (410, 900, TouchPhase::Move), (820, 880, TouchPhase::Move)));
        let g = r.feed(&frame2(4700, (410, 700, TouchPhase::Up), (830, 690, TouchPhase::Up)));
        assert!(matches!(g[..], [Gesture::TwoFingerSwipe { dir: Direction::Up, from, .. }] if from == Point::new(600, 1200)), "{g:?}");
        // Concurrent taps: each finger is its own tap.
        r.concurrent_taps = true;
        r.double_tap = DoubleTapMode::Off;
        r.feed(&frame2(5000, (400, 800, TouchPhase::Down), (800, 800, TouchPhase::Down)));
        let g = r.feed(&frame2(5100, (400, 800, TouchPhase::Up), (800, 800, TouchPhase::Up)));
        assert_eq!(g, vec![Gesture::Tap { pos: Point::new(400, 800) }, Gesture::Tap { pos: Point::new(800, 800) }]);
    }

    fn frame_of(ms: u64, contacts: &[(u8, i32, i32, TouchPhase)]) -> TouchFrame {
        TouchFrame {
            time: Timestamp::from_millis(ms),
            contacts: contacts.iter().map(|&(slot, x, y, phase)| TouchContact { slot, x, y, phase }).collect(),
        }
    }

    #[test]
    fn frame_order_lifts_then_downs_then_moves() {
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Off;
        // Two fingers down and up in one frame: one two-finger tap.
        let g = r.feed(&frame_of(0, &[(0, 400, 800, TouchPhase::DownUp), (1, 800, 800, TouchPhase::DownUp)]));
        assert_eq!(g, vec![Gesture::TwoFingerTap { pos: Point::new(600, 800) }]);
        assert_eq!(r.active_contacts(), 0);
        // Slot 1 lifts in the frame where slot 0 lands (listed first): two single taps,
        // not a pair.
        r.feed(&frame_of(1000, &[(1, 900, 300, TouchPhase::Down)]));
        let g = r.feed(&frame_of(1050, &[(0, 200, 300, TouchPhase::Down), (1, 900, 300, TouchPhase::Up)]));
        assert_eq!(g, vec![Gesture::Tap { pos: Point::new(900, 300) }]);
        assert_eq!(r.feed(&frame_of(1100, &[(0, 200, 300, TouchPhase::Up)])), vec![Gesture::Tap { pos: Point::new(200, 300) }]);
        // A pinch where one finger lifts in the frame where the other moves is measured at
        // the moved position.
        r.feed(&frame_of(2000, &[(0, 500, 800, TouchPhase::Down), (1, 700, 800, TouchPhase::Down)]));
        let g = r.feed(&frame_of(2100, &[(0, 300, 800, TouchPhase::Up), (1, 900, 800, TouchPhase::Move)]));
        assert_eq!(g, vec![Gesture::Pinch { center: Point::new(600, 800), scale_milli: 3000 }]);
        assert!(r.feed(&frame_of(2200, &[(1, 900, 800, TouchPhase::Up)])).is_empty());
        // In concurrent mode two same-frame fingers are two taps.
        r.concurrent_taps = true;
        let g = r.feed(&frame_of(3000, &[(0, 400, 800, TouchPhase::DownUp), (1, 800, 800, TouchPhase::DownUp)]));
        assert_eq!(g, vec![Gesture::Tap { pos: Point::new(400, 800) }, Gesture::Tap { pos: Point::new(800, 800) }]);
    }

    #[test]
    fn lost_releases_cancel_and_never_pair_again() {
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Off;
        // Pair, then slot 1's release is lost while slot 0 lifts.
        r.feed(&frame_of(0, &[(0, 400, 800, TouchPhase::Down), (1, 800, 800, TouchPhase::Down)]));
        r.feed(&frame_of(100, &[(0, 400, 800, TouchPhase::Up)]));
        assert_eq!(r.active_contacts(), 1);
        // Single taps work; a new pair (slots 0 and 2) still gives a two-finger tap.
        assert_eq!(r.feed(&frame_of(1000, &[(0, 300, 300, TouchPhase::DownUp)])), vec![Gesture::Tap { pos: Point::new(300, 300) }]);
        r.feed(&frame_of(2000, &[(0, 400, 1200, TouchPhase::Down), (2, 800, 1200, TouchPhase::Down)]));
        assert_eq!(r.feed(&frame_of(2100, &[(0, 400, 1200, TouchPhase::Up), (2, 800, 1200, TouchPhase::Up)])), vec![Gesture::TwoFingerTap { pos: Point::new(600, 1200) }]);
        // The stale slot-1 contact's late lift neither ends a pair nor gives a gesture.
        r.feed(&frame_of(3000, &[(0, 400, 1200, TouchPhase::Down), (2, 800, 1200, TouchPhase::Down)]));
        assert!(r.feed(&frame_of(3050, &[(1, 800, 800, TouchPhase::Up)])).is_empty());
        assert_eq!(r.feed(&frame_of(3100, &[(0, 400, 1200, TouchPhase::Up), (2, 800, 1200, TouchPhase::Up)])), vec![Gesture::TwoFingerTap { pos: Point::new(600, 1200) }]);
        assert_eq!(r.active_contacts(), 0);
        // A new touch-down in a slot that is still down cancels the old contact first.
        r.feed(&frame_of(4000, &[(0, 100, 100, TouchPhase::Down)]));
        let ev = r.feed_events(&frame_of(5000, &[(0, 500, 500, TouchPhase::Down)]));
        // Press at 4150, holds at 4400, 4550, 4700, 4850 and 5000 ms first, then the stale contact's Cancel.
        assert_eq!(touches(&ev), vec![TouchKind::Press, TouchKind::Hold, TouchKind::Hold, TouchKind::Hold, TouchKind::Hold, TouchKind::Hold, TouchKind::Cancel, TouchKind::Down]);
        assert!(ev.iter().all(|e| !matches!(e, UiEvent::Gesture(Gesture::Tap { .. }))));
        assert_eq!(r.feed(&frame_of(5050, &[(0, 500, 500, TouchPhase::Up)])), vec![Gesture::Tap { pos: Point::new(500, 500) }]);
        // A lost release inside an active pair ends the pair: the partner gives nothing.
        r.feed(&frame_of(6000, &[(0, 400, 800, TouchPhase::Down), (1, 800, 800, TouchPhase::Down)]));
        let ev = r.feed_events(&frame_of(6050, &[(1, 820, 800, TouchPhase::Down)]));
        assert_eq!(touches(&ev), vec![TouchKind::Cancel, TouchKind::Down]);
        assert!(r.feed(&frame_of(6100, &[(0, 400, 800, TouchPhase::Up)])).is_empty());
        assert_eq!(r.feed(&frame_of(6150, &[(1, 820, 800, TouchPhase::Up)])), vec![Gesture::Tap { pos: Point::new(820, 800) }]);
    }

    #[test]
    fn cancel_all_ends_every_contact() {
        let mut r = rec(Rotation::Portrait);
        assert!(r.cancel_all().is_empty());
        r.feed(&frame_of(0, &[(0, 400, 800, TouchPhase::Down), (3, 800, 800, TouchPhase::Down)]));
        let ev = r.cancel_all();
        assert_eq!(touches(&ev), vec![TouchKind::Cancel, TouchKind::Cancel]);
        assert_eq!((r.active_contacts(), r.next_deadline()), (0, None));
        // Their lifts arrive later and are ignored.
        assert!(r.feed_events(&frame_of(100, &[(0, 400, 800, TouchPhase::Up), (3, 800, 800, TouchPhase::Up)])).is_empty());
    }

    struct LongPressAt(Vec<Duration>);

    fn long_presses(ev: &[UiEvent]) -> LongPressAt {
        LongPressAt(ev.iter().filter_map(|e| if let UiEvent::Gesture(Gesture::LongPress { held, .. }) = e { Some(*held) } else { None }).collect())
    }

    #[test]
    fn default_long_press_fires_at_exactly_the_global_hold_time() {
        assert_eq!(LONG_PRESS, Duration::from_millis(400));
        let ms = LONG_PRESS.as_millis() as u64;
        let mut r = rec(Rotation::Portrait);
        r.double_tap = DoubleTapMode::Off;
        assert_eq!(r.long_press, LONG_PRESS);
        r.feed(&frame(1000, 500, 500, TouchPhase::Down));
        r.tick(Timestamp::from_millis(1150)); // the pressed-state feedback
        assert_eq!(r.next_deadline(), Some(Timestamp::from_millis(1000 + ms)));
        assert!(r.tick(Timestamp::from_millis(1000 + ms - 1)).iter().all(|e| !matches!(e, UiEvent::Gesture(_) | UiEvent::Touch(Touch { kind: TouchKind::Hold, .. }))));
        let ev = r.tick(Timestamp::from_millis(1000 + ms));
        assert_eq!(touches(&ev), vec![TouchKind::Hold]);
        assert_eq!(long_presses(&ev).0, vec![LONG_PRESS]);
        r.feed(&frame(1000 + ms + 10, 500, 500, TouchPhase::Up));
        // A lift 1 ms before the hold time is a tap, not a long press.
        r.feed(&frame(5000, 500, 500, TouchPhase::Down));
        assert_eq!(r.feed(&frame(5000 + ms - 1, 500, 500, TouchPhase::Up)), vec![Gesture::Tap { pos: Point::new(500, 500) }]);
    }

    #[test]
    fn long_press_fires_while_the_finger_is_down_and_the_lift_is_silent() {
        let mut r = rec(Rotation::Portrait);
        r.long_press = Duration::from_millis(300);
        r.feed(&frame(0, 500, 500, TouchPhase::Down));
        assert!(r.tick(Timestamp::from_millis(299)).iter().all(|e| !matches!(e, UiEvent::Gesture(_))));
        let ev = r.tick(Timestamp::from_millis(300));
        assert_eq!(touches(&ev), vec![TouchKind::Hold]);
        let LongPressAt(held) = long_presses(&ev);
        assert_eq!(held, vec![Duration::from_millis(300)]);
        // More holds repeat, but only one long press; the lift sends no tap or long press.
        let ev = r.tick(Timestamp::from_millis(700));
        assert!(long_presses(&ev).0.is_empty());
        assert!(r.feed(&frame(800, 502, 500, TouchPhase::Up)).is_empty());
        // Moving away after the long press: no swipe, no second long press.
        r.feed(&frame(1000, 500, 500, TouchPhase::Down));
        assert_eq!(long_presses(&r.tick(Timestamp::from_millis(1300))).0.len(), 1);
        r.feed(&frame(1400, 500, 900, TouchPhase::Move));
        assert!(r.feed(&frame(1500, 500, 900, TouchPhase::Up)).is_empty());
        // A hold that moved before the time never becomes a long press (it is a swipe).
        r.feed(&frame(2000, 500, 500, TouchPhase::Down));
        r.feed(&frame(2100, 500, 900, TouchPhase::Move));
        assert!(long_presses(&r.tick(Timestamp::from_millis(2500))).0.is_empty());
        assert!(matches!(r.feed(&frame(2600, 500, 900, TouchPhase::Up))[..], [Gesture::Swipe { .. }]));
    }

    #[test]
    fn corner_holds() {
        let mut r = rec(Rotation::Portrait);
        r.feed(&frame(0, 1250, 1680, TouchPhase::Down));
        let ev = r.tick(Timestamp::from_millis(650));
        assert!(ev.contains(&UiEvent::Gesture(Gesture::CornerHold { corner: Corner::BottomRight, pos: Point::new(1250, 1680) })));
        assert!(ev.iter().any(|e| matches!(e, UiEvent::Gesture(Gesture::LongPress { .. }))), "long press follows the corner hold at once");
        // Only once per hold.
        let ev = r.tick(Timestamp::from_millis(1000));
        assert!(ev.iter().all(|e| !matches!(e, UiEvent::Gesture(_))));
        let g = r.feed(&frame(1100, 1250, 1680, TouchPhase::Up));
        assert!(g.is_empty(), "the lift adds nothing: {g:?}");
        // Not in the middle; corners follow the rotation.
        let mut r = rec(Rotation::Clockwise);
        // Panel (1271, 0) is logical (0, 0): top left.
        r.feed(&frame(0, 1271, 0, TouchPhase::Down));
        let ev = r.tick(Timestamp::from_millis(700));
        assert!(ev.iter().any(|e| matches!(e, UiEvent::Gesture(Gesture::CornerHold { corner: Corner::TopLeft, .. }))));
        let mut r = rec(Rotation::Portrait);
        r.feed(&frame(0, 600, 800, TouchPhase::Down));
        assert!(r.tick(Timestamp::from_millis(700)).iter().all(|e| !matches!(e, UiEvent::Gesture(Gesture::CornerHold { .. }))));
    }

    #[test]
    fn wall_clock_timeout_and_poll() {
        let mut r = rec(Rotation::Portrait);
        r.long_press = Duration::from_millis(30);
        assert_eq!(r.timeout(), None);
        r.feed(&frame(5000, 500, 500, TouchPhase::Down));
        let t = r.timeout().unwrap();
        assert!(t <= Duration::from_millis(30));
        std::thread::sleep(t + Duration::from_millis(5));
        assert_eq!(touches(&r.poll()), vec![TouchKind::Hold]);
        r.reset();
        assert_eq!(r.active_contacts(), 0);
        assert_eq!(r.timeout(), None);
    }
}
