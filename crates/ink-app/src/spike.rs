//! `rinky spike-suspend`: a test tool for suspend and resume without the framework.
//!
//! Logs every input and powerd event with the wall-clock time and marks every tap with a
//! numbered square, so screenshots show whether the screen and touch still work after a
//! wake. Sleep and wake come from two sources:
//! - powerd: `goingToScreenSaver` draws the screensaver; `outOfScreenSaver` /
//!   `wakeupFromSuspend` / `resuming` redraw the awake screen (this is what happens while
//!   the framework runs);
//! - the power key itself (evdev `KEY_POWER`): with the framework stopped powerd ignores
//!   the button, so the spike handles it: first press draws the screensaver and, with
//!   `--suspend N`, tries a kernel suspend with an RTC alarm N seconds later; next press
//!   redraws the awake screen.
//!
//! A tap in the top-right corner or `--seconds N` ends it. See "Sleep and the Kindle framework" in
//! `docs/ARCHITECTURE.md`.

use ink_device::{Canvas, Device, InputEvent, PowerEvent, RefreshClass};
use ink_text::{Fonts, Style};
use ink_ui::{Gesture, GestureRecognizer, ScreenSpace, UiEvent};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn stamp() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let s = t.as_secs() % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, s / 60 % 60, s % 60, t.subsec_millis())
}

fn awake_screen(canvas: &mut Canvas, fonts: &mut Fonts, wakes: u32, taps: u32) {
    let lines = [
        "rinky suspend spike".to_string(),
        format!("awake at {} UTC", stamp()),
        format!("wakes: {wakes}   taps: {taps}"),
        "power key: sleep / wake".to_string(),
        "tap anywhere: mark; top right: exit".to_string(),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    ink_ui::message::draw_message(canvas, fonts, &refs);
}

fn screensaver(canvas: &mut Canvas, fonts: &mut Fonts, image: Option<&Canvas>) {
    canvas.clear(255);
    match image {
        Some(img) => {
            let x = (canvas.width as i32 - img.width as i32) / 2;
            let y = (canvas.height as i32 - img.height as i32) / 2;
            canvas.blit(img, x, y);
        }
        None => {
            // A simple full-screen pattern that is easy to recognise in screenshots.
            let (w, h) = (canvas.width as i32, canvas.height as i32);
            canvas.fill_rect(0, 0, w, h, 0);
            canvas.fill_rect(60, 60, w - 120, h - 120, 255);
            for i in 0..8 {
                canvas.fill_rect(120 + i * 40, h / 2 + 120, 20, 300, (i * 32) as u8);
            }
            let px = 64.0;
            let text = "rinky screensaver";
            let tw = fonts.text_width(Style::Bold, text, px);
            fonts.draw_text(canvas, Style::Bold, px, (w as f32 - tw) / 2.0, h as f32 / 3.0, text);
            let sub = format!("since {} UTC", stamp());
            let sw = fonts.text_width(Style::Regular, &sub, 44.0);
            fonts.draw_text(canvas, Style::Regular, 44.0, (w as f32 - sw) / 2.0, h as f32 / 3.0 + 90.0, &sub);
        }
    }
}

/// Runs the spike on `device` until the top-right tap or `limit`. `suspend_secs`: on a
/// power-key sleep, try a kernel suspend that the RTC ends after that many seconds.
pub fn run(mut device: Device, image: Option<&Path>, limit: Option<Duration>, suspend_secs: Option<u64>) -> anyhow::Result<()> {
    let image = match image {
        Some(p) => Some(Canvas::load_png(p)?),
        None => None,
    };
    let size = device.screen.size();
    let mut canvas = Canvas::new(size.w as usize, size.h as usize);
    let mut fonts = Fonts::load();
    let mut rec = GestureRecognizer::new(ScreenSpace { panel: device.screen.panel_size(), rotation: device.screen.rotation() });
    let (mut wakes, mut taps, mut asleep) = (0u32, 0u32, false);
    awake_screen(&mut canvas, &mut fonts, wakes, taps);
    device.screen.update_all(&canvas, RefreshClass::Gc16)?;
    println!("{} spike: started, powerd state {:?}", stamp(), device.power.state().ok());
    let start = Instant::now();
    loop {
        if limit.is_some_and(|l| start.elapsed() >= l) {
            println!("{} spike: time limit", stamp());
            return Ok(());
        }
        let Some(ev) = device.input.next_event(Some(Duration::from_secs(1)))? else { continue };
        match &ev {
            InputEvent::Touch(_) => {}
            other => println!("{} event: {other:?}", stamp()),
        }
        match ev {
            InputEvent::Power(p) => match p {
                PowerEvent::GoingToScreenSaver => {
                    asleep = true;
                    screensaver(&mut canvas, &mut fonts, image.as_ref());
                    device.screen.update_all(&canvas, RefreshClass::Gc16)?;
                    println!("{} spike: screensaver drawn", stamp());
                }
                PowerEvent::OutOfScreenSaver | PowerEvent::WakeupFromSuspend | PowerEvent::Resuming if asleep || p != PowerEvent::Resuming => {
                    wakes += 1;
                    asleep = false;
                    awake_screen(&mut canvas, &mut fonts, wakes, taps);
                    device.screen.update_all(&canvas, RefreshClass::Gc16)?;
                    println!("{} spike: awake screen drawn (wake {wakes})", stamp());
                }
                _ => {}
            },
            InputEvent::Quit => return Ok(()),
            InputEvent::Key(k) if k.key == ink_device::Key::Power && k.pressed => {
                if asleep {
                    wakes += 1;
                    asleep = false;
                    awake_screen(&mut canvas, &mut fonts, wakes, taps);
                    device.screen.update_all(&canvas, RefreshClass::Gc16)?;
                    println!("{} spike: power key wake, awake screen drawn (wake {wakes})", stamp());
                } else {
                    asleep = true;
                    screensaver(&mut canvas, &mut fonts, image.as_ref());
                    device.screen.update_all(&canvas, RefreshClass::Gc16)?;
                    println!("{} spike: power key sleep, screensaver drawn", stamp());
                    if let Some(secs) = suspend_secs {
                        let t = Instant::now();
                        match device.power.suspend(Some(Duration::from_secs(secs))) {
                            Ok(()) => println!("{} spike: resumed after {:?}", stamp(), t.elapsed()),
                            Err(e) => println!("{} spike: suspend refused: {e}", stamp()),
                        }
                    }
                }
            }
            ev => {
                for g in ink_ui::translate(&mut rec, ev) {
                    let UiEvent::Gesture(Gesture::Tap { pos }) = g else { continue };
                    println!("{} tap at {},{}", stamp(), pos.x, pos.y);
                    if pos.y < size.h as i32 / 10 && pos.x > size.w as i32 * 3 / 4 {
                        println!("{} spike: exit", stamp());
                        return Ok(());
                    }
                    taps += 1;
                    let r = ink_device::Rect::new(pos.x - 40, pos.y - 40, 80, 80);
                    canvas.fill_rect(r.x, r.y, 80, 80, 0);
                    let label = taps.to_string();
                    fonts.draw_text(&mut canvas, Style::Bold, 40.0, (r.x + 90) as f32, (r.y + 55) as f32, &label);
                    device.screen.update(&canvas, ink_device::Rect::new(r.x, r.y, 200, 80), RefreshClass::Du)?;
                }
            }
        }
    }
}
