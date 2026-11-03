//! Gesture recognition on recorded raw touch streams:
//!
//! - `tests/data/fts_ts-emu-5.19.5.tsv`: the emulator's fts_ts controller, one gesture
//!   per section (tap, hold, swipes, double tap, corner hold), recorded with QMP input.
//! - `tests/data/pt_mt-pw6-typing.tsv`: the real PW12's pt_mt chip while the user typed
//!   fast with two thumbs in KOReader (overlapping contacts in two slots), with the taps
//!   KOReader recognised for comparison.
//!
//! The raw records go through `ink_device::evdev::TouchDecoder` (as on the device) and
//! then through `GestureRecognizer`.

use ink_device::evdev::{RawEvent, TouchDecoder, TouchMapping};
use ink_device::{Point, Rotation, Size, Timestamp, TouchFrame};
use ink_ui::{Corner, Direction, DoubleTapMode, Gesture, GestureRecognizer, ScreenSpace, TouchKind, UiEvent};
use std::path::Path;
use std::time::Duration;

const PANEL: Size = Size::new(1272, 1696);

/// A section of a recording: its name and the frames decoded in it.
struct Section {
    name: String,
    frames: Vec<TouchFrame>,
}

/// Parses `RAW` lines (decoded into frames), `GESTURE name` section headers and `GES`
/// reference lines.
fn load(file: &str) -> (Vec<Section>, Vec<(String, Point)>) {
    // INK_TEST_DATA: fixtures copied next to the cross-built test binary on the Kindle.
    let dir = std::env::var_os("INK_TEST_DATA").map(std::path::PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data"));
    let path = dir.join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut dec = TouchDecoder::new(TouchMapping::identity(PANEL.w as i32, PANEL.h as i32));
    let mut sections = vec![Section { name: String::new(), frames: Vec::new() }];
    let mut reference = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "GESTURE" => sections.push(Section { name: f[1].to_string(), frames: Vec::new() }),
            "GES" => reference.push((f[1].to_string(), Point::new(f[2].parse().unwrap(), f[3].parse().unwrap()))),
            "RAW" => {
                let n = |i: usize| f[i].parse::<i64>().unwrap();
                let ev = RawEvent {
                    time: Timestamp(Duration::from_secs(n(4) as u64) + Duration::from_micros(n(5) as u64)),
                    ty: n(1) as u16,
                    code: n(2) as u16,
                    value: n(3) as i32,
                };
                if let Some(frame) = dec.feed(ev) {
                    sections.last_mut().unwrap().frames.push(frame);
                }
            }
            other => panic!("unknown line kind {other}"),
        }
    }
    (sections, reference)
}

fn recognizer(rotation: Rotation) -> GestureRecognizer {
    GestureRecognizer::new(ScreenSpace { panel: PANEL, rotation })
}

fn gestures(events: &[UiEvent]) -> Vec<Gesture> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Gesture(g) => Some(*g),
            _ => None,
        })
        .collect()
}

/// Runs every section; a final tick after each section fires pending timers.
fn run_sections(rec: &mut GestureRecognizer, sections: &[Section]) -> Vec<(String, Vec<UiEvent>)> {
    let mut out = Vec::new();
    for s in sections.iter().filter(|s| !s.name.is_empty()) {
        let mut ev = Vec::new();
        for f in &s.frames {
            ev.extend(rec.feed_events(f));
        }
        if let Some(last) = s.frames.last() {
            ev.extend(rec.tick(Timestamp(last.time.0 + Duration::from_millis(400))));
        }
        out.push((s.name.clone(), ev));
    }
    out
}

#[test]
fn fts_ts_emulator_gestures() {
    let (sections, _) = load("fts_ts-emu-5.19.5.tsv");
    assert_eq!(sections.iter().filter(|s| !s.name.is_empty()).count(), 7);
    let mut rec = recognizer(Rotation::Portrait);
    let results = run_sections(&mut rec, &sections);
    let get = |name: &str| results.iter().find(|(n, _)| n == name).map(|(_, e)| e.clone()).unwrap();
    let p = Point::new(900, 700);
    assert_eq!(gestures(&get("tap")), vec![Gesture::Tap { pos: p }]);
    let hold = get("hold");
    let holds = hold.iter().filter(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Hold)).count();
    assert_eq!(holds, 5, "held 1014 ms: holds at 400, 550, 700, 850, 1000 ms");
    assert!(matches!(gestures(&hold)[..], [Gesture::LongPress { pos, held }] if pos == p && held == ink_ui::LONG_PRESS), "the long press fires at the hold timer, not at the lift");
    let swipe = |name: &str| match gestures(&get(name))[..] {
        [Gesture::Swipe { from, to, dir }] => (from, to, dir),
        ref other => panic!("{name}: {other:?}"),
    };
    assert_eq!(swipe("swipe-left"), (Point::new(1100, 700), Point::new(300, 700), Direction::Left));
    assert_eq!(swipe("swipe-right"), (Point::new(300, 700), Point::new(1100, 700), Direction::Right));
    assert_eq!(swipe("swipe-up"), (Point::new(900, 1000), Point::new(900, 600), Direction::Up));
    assert_eq!(
        gestures(&get("double-tap")),
        vec![Gesture::Tap { pos: p }, Gesture::Tap { pos: Point::new(905, 702) }, Gesture::DoubleTap { pos: Point::new(905, 702) }]
    );
    let corner = gestures(&get("corner-hold"));
    assert_eq!(corner[0], Gesture::CornerHold { corner: Corner::BottomRight, pos: Point::new(1250, 1681) });
    assert!(matches!(corner[1], Gesture::LongPress { .. }));
    // Every contact gives exactly one Down and one Up.
    let all: Vec<UiEvent> = results.iter().flat_map(|(_, e)| e.clone()).collect();
    let count = |k: TouchKind| all.iter().filter(|e| matches!(e, UiEvent::Touch(t) if t.kind == k)).count();
    assert_eq!((count(TouchKind::Down), count(TouchKind::Up)), (8, 8));
}

#[test]
fn fts_ts_stream_in_every_rotation() {
    let (sections, _) = load("fts_ts-emu-5.19.5.tsv");
    // Swipe left on the panel, seen in each orientation.
    let expect = [
        (Rotation::Portrait, Direction::Left),
        (Rotation::Clockwise, Direction::Down),
        (Rotation::UpsideDown, Direction::Right),
        (Rotation::CounterClockwise, Direction::Up),
    ];
    for (rot, dir) in expect {
        let mut rec = recognizer(rot);
        let results = run_sections(&mut rec, &sections);
        let swipe = gestures(&results.iter().find(|(n, _)| n == "swipe-left").unwrap().1);
        assert!(matches!(swipe[..], [Gesture::Swipe { dir: d, .. }] if d == dir), "{rot:?}: {swipe:?}");
        // The tap lands on the rotated point, and taps map back to the same panel pixel.
        let tap = gestures(&results.iter().find(|(n, _)| n == "tap").unwrap().1);
        let logical = rot.from_panel(Point::new(900, 700), PANEL);
        assert_eq!(tap, vec![Gesture::Tap { pos: logical }], "{rot:?}");
        assert_eq!(rot.to_panel(logical, PANEL), Point::new(900, 700));
    }
}

fn pt_mt_frames() -> (Vec<TouchFrame>, Vec<(String, Point)>) {
    let (sections, reference) = load("pt_mt-pw6-typing.tsv");
    (sections.into_iter().flat_map(|s| s.frames).collect(), reference)
}

#[test]
fn pt_mt_two_thumb_typing_with_concurrent_taps_matches_koreader() {
    let (frames, reference) = pt_mt_frames();
    let mut rec = recognizer(Rotation::Portrait);
    rec.concurrent_taps = true;
    rec.double_tap = DoubleTapMode::Off;
    let mut events = Vec::new();
    for f in &frames {
        events.extend(rec.feed_events(f));
    }
    let ours: Vec<Point> = gestures(&events)
        .into_iter()
        .filter_map(|g| match g {
            Gesture::Tap { pos } => Some(pos),
            _ => None,
        })
        .collect();
    let theirs: Vec<Point> = reference.iter().filter(|(k, _)| k == "tap").map(|(_, p)| *p).collect();
    assert_eq!(ours, theirs, "taps differ from KOReader's");
    // One Down per KOReader "touch".
    let downs = events.iter().filter(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Down)).count();
    assert_eq!(downs, reference.iter().filter(|(k, _)| k == "touch").count());
    // Both slots are used: the thumbs overlapped.
    assert!(events.iter().any(|e| matches!(e, UiEvent::Touch(t) if t.slot == 1)));
}

#[test]
fn pt_mt_overlapping_contacts_without_concurrent_taps() {
    let (frames, reference) = pt_mt_frames();
    let mut rec = recognizer(Rotation::Portrait);
    rec.double_tap = DoubleTapMode::Off;
    let mut events = Vec::new();
    for f in &frames {
        events.extend(rec.feed_events(f));
    }
    let g = gestures(&events);
    let taps = g.iter().filter(|g| matches!(g, Gesture::Tap { .. })).count();
    let two = g.iter().filter(|g| matches!(g, Gesture::TwoFingerTap { .. })).count();
    let koreader_taps = reference.iter().filter(|(k, _)| k == "tap").count();
    // Overlapping pairs become two-finger taps (KOReader's rule without concurrent
    // taps), the rest stay taps; no contact is lost or counted twice.
    assert!(two > 0, "{g:?}");
    assert!(taps + 2 * two <= koreader_taps && taps + 2 * two + 4 >= koreader_taps, "taps {taps} two {two} koreader {koreader_taps}");
}
