//! Settings screen runs headless with fake device services and scripted taps.
//!
//! Set `INK_SETTINGS_OUT=DIR` to keep the screenshots (`shot` steps).
//! Taps are computed from the same layout functions the screen uses (`main_rows`,
//! `wifi_rows`, `plugin_rows`, `content_rect`, ...): [`tap_row`] finds a row by its
//! action, scrolling the list with swipes first when the row is further down. The back button
//! is the app bar's (`AppBar::back_rect`, centre (60, 68) in portrait).

use super::fakes::{self, FakeState};
use ink_ui::Slider;
use super::*;
use crate::app::App;
use crate::router::Route;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::screensaver::Screensaver;
use ink_device::script::parse;
use ink_store::Paths;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

struct Run {
    probe: HeadlessProbe,
    fake: Rc<RefCell<FakeState>>,
    root: PathBuf,
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-settings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn out_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os("INK_SETTINGS_OUT").map(|d| PathBuf::from(d).join(name))
}

fn run_with(name: &str, script: &str, settings: Settings, with_fakes: bool, setup: impl FnOnce(&mut FakeState)) -> Run {
    let (mut device, probe) =
        headless::open(HeadlessOptions { script: parse(script).unwrap(), out_dir: out_dir(name), ..Default::default() });
    let fake = if with_fakes { fakes::install(&mut device) } else { Rc::default() };
    setup(&mut fake.borrow_mut());
    let root = tmp(name);
    App::new(device, settings, Paths { root: root.clone() }).run(Route::Settings).expect("settings run");
    Run { probe, fake, root }
}

fn run(name: &str, script: &str) -> Run {
    run_with(name, script, Settings::default(), true, |_| {})
}

fn saved(r: &Run) -> Settings {
    Settings::load(&r.root.join("settings.toml")).expect("settings.toml written")
}

const PORTRAIT: Size = Size { w: 1272, h: 1696 };

/// Tap on the app bar's back button.
const BACK: &str = "tap 60 68\nwait 50\n";

/// Length of a short swipe (half a page jump, see [`SettingsList::swipe_step`]).
const SWIPE: i32 = 300;

/// Script lines that tap the row with `action` in a list of `rows` (portrait): long
/// swipes back to the top of a list that scrolls, then half-page jumps down until the
/// row shows.
fn tap_row(rows: Vec<Row>, action: &str) -> String {
    let area = content_rect(PORTRAIT);
    let mut l = SettingsList::new(rows);
    let (need, r) = l.locate(area, action).unwrap_or_else(|| panic!("no row {action}"));
    let top = r.y + need; // the row's y at scroll 0
    let max = l.max_scroll(area);
    let half = SettingsList::swipe_step(area.h, SWIPE);
    let mut s = String::new();
    let y0 = area.y + 1000;
    if max > 0 {
        for _ in 0..=(max / area.h as i32) {
            s.push_str(&format!("swipe 600 {} 600 {}\nwait 50\n", area.y + 50, area.bottom() - 50));
        }
    }
    // The same jumps as the list makes (snapped to row tops).
    let mut scroll = 0;
    while top - scroll < area.y || top - scroll + r.h as i32 > area.bottom() {
        assert!(scroll < max, "{action}: no jump shows the row");
        s.push_str(&format!("swipe 600 {y0} 600 {}\nwait 50\n", y0 - SWIPE));
        l.scroll_by(area, half);
        scroll = l.scroll;
    }
    s.push_str(&format!("tap 600 {}\nwait 50\n", top - scroll + r.h as i32 / 2));
    s
}

/// A row of the main page.
fn main_tap(action: &str) -> String {
    tap_row(main_rows(&main_values()), action)
}

/// Main-page values with the Plugins section the app shows: the compiled-in plugins
/// (each a switch row with a subtitle) and their settings pages, so rows below the
/// Plugins section are found where the screen draws them.
fn main_values() -> MainValues {
    thread_local! {
        static PLUGINS: Vec<Row> = {
            let root = tmp("main-values");
            let host = crate::plugin_host::PluginHost::load(crate::plugins::compiled_in(), &Settings::default(), &Paths { root });
            let pages: Vec<(String, String)> = host.settings_pages().iter().map(|p| (p.title.clone(), p.plugin.clone())).collect();
            let plugins: Vec<(ink_plugin::PluginInfo, bool)> = host.plugins().into_iter().map(|p| (p, true)).collect();
            plugin_section(&pages, &plugins)
        };
    }
    MainValues { plugins: PLUGINS.with(|p| p.clone()), ..MainValues::default() }
}

/// Rows of a choice page: a header, then one choice per action, then `notes` notes.
fn choices(actions: &[String], notes: usize) -> Vec<Row> {
    let mut v = vec![Row::header("")];
    v.extend(actions.iter().map(|a| Row::choice("", false, a.clone())));
    v.extend((0..notes).map(|_| Row::note("")));
    v
}

/// The Wi-Fi page with the fake's networks (radio on, not connected).
fn wifi_page() -> Vec<Row> {
    let f = FakeState::default();
    let nets: Vec<WifiNetwork> =
        f.networks.iter().map(|n| WifiNetwork { known: f.profiles.iter().any(|p| p.essid == n.essid), ..n.clone() }).collect();
    wifi_rows(&ink_device::NetStatus::NoNetwork, false, &nets)
}

/// Centre of a rectangle as a script tap.
fn tap_at(r: Rect) -> String {
    format!("tap {} {}\nwait 50\n", r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
}

/// The Light page's sliders (brightness, warmth): (minus, plus) rectangles.
fn light_buttons(i: i32) -> (Rect, Rect) {
    let c = content_rect(PORTRAIT);
    let action = if i == 0 { "bright" } else { "warm" };
    let q = QuickLight { brightness: Some(Slider::new("Brightness", 0.0, 24.0, 1.0, 12.0, quick_light::BRIGHTNESS)), warmth: Some(Slider::new("Warmth", 0.0, 24.0, 1.0, 4.0, quick_light::WARMTH)) };
    let (_, r) = SettingsList::new(light_rows(&q)).locate(c, action).expect("slider row");
    let (minus, _, plus) = Slider::parts(SettingsList::slider_rect(r));
    (minus, plus)
}

#[test]
fn main_page_lists_every_page_and_back_pops() {
    // The main list, dragged down, then the app bar's back.
    let script = format!("shot main\nswipe 600 1400 600 400\nshot main-2\n{BACK}wait 100\nkey power\n");
    let r = run("main", &script);
    assert!(!r.probe.updates().is_empty());
    assert!(r.probe.panel().pixels.iter().filter(|v| **v < 64).count() > 5_000);
    // Back popped the only screen: nothing saved, nothing changed.
    assert!(!r.root.join("settings.toml").exists());
    assert_eq!(r.fake.borrow().brightness, 12);
}

#[test]
fn without_device_services_every_page_opens() {
    // Unsupported services everywhere (headless as is): no page may fail.
    let pages: Vec<String> =
        main_rows(&MainValues::default()).into_iter().map(|r| r.action).filter(|a| a.starts_with("page:")).collect();
    assert_eq!(pages.len(), 21);
    let mut script = String::new();
    for (i, a) in pages.iter().enumerate() {
        script.push_str(&format!("{}shot page{i}\n{BACK}", main_tap(a)));
    }
    script.push_str(BACK);
    let r = run_with("unsupported", &script, Settings::default(), false, |_| {});
    assert!(r.probe.updates().len() >= 40);
}

#[test]
fn light_sliders_set_brightness_and_warmth() {
    let (_, bright_plus) = light_buttons(0);
    let (warm_minus, _) = light_buttons(1);
    let script = format!(
        "{}shot light\n{p}{p}{}shot light-set\n",
        main_tap("page:light"),
        tap_at(warm_minus),
        p = tap_at(bright_plus)
    );
    let r = run("light", &script);
    let f = r.fake.borrow();
    assert_eq!((f.brightness, f.warmth), (14, 3));
}

#[test]
fn quick_light_panel_opens_with_a_swipe_down_and_sets_the_light() {
    // Control center (quick_light::layout): brightness plus at 1144..1240 x 216..312;
    // the panel ends at y 1216, a tap below closes it.
    // After Done the settings page works again: Light page, brightness plus.
    let script = format!(
        "swipe 600 20 600 700\nshot quick\ntap 1174 260\ntap 600 1500\nshot quick-closed\n{}{}",
        main_tap("page:light"),
        tap_at(light_buttons(0).1)
    );
    let r = run("quick", &script);
    assert_eq!(r.fake.borrow().brightness, 14);
}

#[test]
fn quick_light_opens_over_other_screens_and_a_tap_below_closes_it() {
    let (mut device, probe) = headless::open(HeadlessOptions {
        script: parse("swipe 600 20 600 700\ntap 192 264\ntap 600 1400\nswipe 600 20 600 700\nkey back\nquit\n").unwrap(),
        out_dir: out_dir("quick-demo"),
        ..Default::default()
    });
    let fake = fakes::install(&mut device);
    App::new(device, Settings::default(), Paths { root: tmp("quick-demo") }).run(Route::Demo).unwrap();
    // Minus once on brightness; the demo survived two openings and closings.
    assert_eq!(fake.borrow().brightness, 11);
    // Each opening: one GL16 over the panel (coalesced to the whole screen), no DU frames.
    let gl16_panel = probe.updates().iter().filter(|u| u.class == RefreshClass::Gl16 && u.rect.w == 1272 && u.rect.h >= 1216).count();
    assert!(gl16_panel >= 2, "{:?}", probe.updates());
    assert!(probe.updates().iter().all(|u| u.class != RefreshClass::Du || u.rect.h < 400), "{:?}", probe.updates());
}

#[test]
fn sleep_choice_is_saved_and_given_to_the_device() {
    let sleep = choices(&SLEEP_CHOICES.map(|m| format!("sleep:{m}")), 0);
    let r = run("sleep", &format!("{}{}shot sleep\n", main_tap("page:sleep"), tap_row(sleep, "sleep:5")));
    assert_eq!(saved(&r).device.sleep_minutes, 5);
    assert_eq!(r.fake.borrow().auto_sleep, Some(Duration::from_secs(300)));
}

#[test]
fn screensaver_choice_is_saved_and_given_to_the_device() {
    let savers = choices(&["page", "cover", "blank"].map(|v| format!("saver:{v}")), 1);
    let r = run("saver", &format!("{}{}shot saver\n", main_tap("page:screensaver"), tap_row(savers, "saver:blank")));
    assert_eq!(screensaver_setting(&saved(&r)), "blank");
    assert!(matches!(r.fake.borrow().screensaver, Some(Screensaver::Message(_))));
}

#[test]
fn display_rotation_is_saved_and_the_canvas_turns() {
    let rots = choices(&[0, 1, 2, 3].map(|i| format!("rot:{i}")), 0);
    let r = run("rotate", &format!("{}shot orientation\n{}wait 50\nshot landscape\n", main_tap("page:display"), tap_row(rots, "rot:1")));
    assert_eq!(saved(&r).device.rotation, 1);
    // The panel received updates after the rotation: the last one covers the panel.
    let last = r.probe.updates().last().copied().unwrap();
    assert_eq!((last.rect.w, last.rect.h), (1272, 1696), "{last:?}");
}

#[test]
fn rotation_from_the_settings_is_used_at_start() {
    let mut s = Settings::default();
    s.device.rotation = 1;
    // Landscape: logical 1696 x 1272. Back is at logical (8..112, 16..120); in panel
    // coordinates (clockwise) that is x = 1272 - y, y = x.
    let r = run_with("start-rotated", "shot start\ntap 1204 60\n", s, true, |_| {});
    let first = r.probe.updates().first().copied().unwrap();
    assert_eq!((first.rect.w, first.rect.h), (1272, 1696));
}

/// Script taps that type `text` and press Enter on the join page's keyboard
/// (portrait; the shared keyboard rectangle at the bottom).
fn type_and_enter(text: &str) -> String {
    let kb = Keyboard::new();
    let area = kb.keys_area(Keyboard::bottom_rect(ink_device::Size::new(1272, 1696)));
    let keys = kb.layout.key_rects(area);
    let centre = |r: Rect| (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
    let mut out = String::new();
    for c in text.chars() {
        let (r, _) = keys.iter().find(|(_, k)| k.action == KeyAction::Char(c.to_string())).expect("letter key");
        let (x, y) = centre(*r);
        out.push_str(&format!("tap {x} {y}\nwait 100\n"));
    }
    let (r, _) = keys.iter().find(|(_, k)| k.action == KeyAction::Enter).expect("enter key");
    let (x, y) = centre(*r);
    out.push_str(&format!("tap {x} {y}\nwait 100\n"));
    out
}

#[test]
fn wifi_join_with_password_from_the_keyboard() {
    // Office is secured and not saved: the join page asks for the password. A wrong
    // password keeps the page; the right one connects and saves the network.
    let script = format!(
        "{}shot wifi\n{}shot join\n{}shot wrong\n{}{}shot joined\n",
        main_tap("page:wifi"),
        tap_row(wifi_page(), "net:2"),
        type_and_enter("xyz"),
        tap_at(join_button_rect(PORTRAIT)),
        // Then the Join button (field still "xyz", refused again), then "abc" more and
        // Enter: the fake accepts "xyzabc".
        type_and_enter("abc")
    );
    let r = run_with("wifi", &script, Settings::default(), true, |f| f.password = Some("xyzabc".into()));
    let f = r.fake.borrow();
    let tries: Vec<_> = f.connects.iter().map(|(e, p)| (e.as_str(), p.as_deref())).collect();
    assert_eq!(tries, vec![("Office", Some("xyz")), ("Office", Some("xyz")), ("Office", Some("xyzabc"))]);
    assert_eq!(f.connected.as_deref(), Some("Office"));
    assert!(f.profiles.iter().any(|p| p.essid == "Office"));
}

/// The hide key and Enter close the password keyboard on the key's lift; the tap that
/// lift makes is the same press and must not join the network listed under the key.
#[test]
fn password_keyboard_keys_do_not_tap_the_network_under_them() {
    // A long list of open networks under the keyboard: a stray tap would join one.
    let nets: Vec<WifiNetwork> = std::iter::once(WifiNetwork { essid: "Office".into(), signal: 2, secured: true, known: false })
        .chain((0..14).map(|i| WifiNetwork { essid: format!("Open{i:02}"), signal: 3, secured: false, known: false }))
        .collect();
    let rows = wifi_rows(&ink_device::NetStatus::NoNetwork, false, &nets);
    let kb = Keyboard::new();
    let keys = kb.layout.key_rects(kb.keys_area(Keyboard::bottom_rect(PORTRAIT)));
    let key = |a: KeyAction| tap_at(keys.iter().find(|(_, k)| k.action == a).unwrap().0);
    let open = format!("{}{}", main_tap("page:wifi"), tap_row(rows, "net:0"));
    let setup = |f: &mut FakeState| {
        f.networks = nets.clone();
        f.password = Some("xyz".into());
    };
    // Hide: back to the list, nothing joined.
    let r = run_with("wifi-hide", &format!("{open}{}shot hidden\n", key(KeyAction::Close)), Settings::default(), true, setup);
    assert_eq!(r.fake.borrow().connects, vec![], "the hide key joins nothing");
    // Enter: only the network the password is for.
    let r = run_with("wifi-enter", &format!("{open}{}shot joined\n", type_and_enter("xyz")), Settings::default(), true, setup);
    let f = r.fake.borrow();
    let tries: Vec<_> = f.connects.iter().map(|(e, p)| (e.as_str(), p.as_deref())).collect();
    assert_eq!(tries, vec![("Office", Some("xyz"))]);
}

#[test]
fn saved_network_with_a_wrong_password_asks_for_it_again() {
    // Home is saved but its saved password is wrong (b-verify finding): the failed
    // connect opens the join page, and the typed password connects.
    let script =
        format!("{}{}shot join-again\n{}shot joined\n", main_tap("page:wifi"), tap_row(wifi_page(), "net:0"), type_and_enter("homepass"));
    let r = run_with("wifi-stale", &script, Settings::default(), true, |f| {
        f.stale = vec!["Home".into()];
        f.password = Some("homepass".into());
    });
    let f = r.fake.borrow();
    let tries: Vec<_> = f.connects.iter().map(|(e, p)| (e.as_str(), p.as_deref())).collect();
    assert_eq!(tries, vec![("Home", None), ("Home", Some("homepass"))]);
    assert_eq!(f.connected.as_deref(), Some("Home"));
    assert!(f.stale.is_empty());
}

#[test]
fn wifi_open_network_connects_and_is_saved() {
    let r = run("wifi-open", &format!("{}{}shot connected\n", main_tap("page:wifi"), tap_row(wifi_page(), "net:1")));
    let f = r.fake.borrow();
    assert_eq!(f.connected.as_deref(), Some("Cafe"));
    assert!(f.profiles.iter().any(|p| p.essid == "Cafe"));
}

/// A join that runs in the background: the page says "Connecting…" and changes when
/// the network worker reports the connection (a `net` step), without a tap.
#[test]
fn wifi_page_follows_the_network_events() {
    let open = format!("{}{}shot connecting\n", main_tap("page:wifi"), tap_row(wifi_page(), "net:1"));
    let quiet = run_with("wifi-bg", &open, Settings::default(), true, |f| f.background = true);
    let told = run_with("wifi-bg-event", &format!("{open}net connected Cafe\nwait 50\nshot connected\n"), Settings::default(), true, |f| f.background = true);
    assert_eq!(told.fake.borrow().connected.as_deref(), Some("Cafe"));
    assert!(
        told.probe.updates().len() > quiet.probe.updates().len(),
        "the event repainted the page: {} vs {} updates",
        told.probe.updates().len(),
        quiet.probe.updates().len()
    );
}

#[test]
fn wifi_toggle_turns_the_radio_off() {
    let r = run("wifi-off", &format!("{}{}shot off\n", main_tap("page:wifi"), tap_row(wifi_page(), "wifi")));
    assert!(!r.fake.borrow().wifi_on);
}

#[test]
fn saved_network_is_forgotten_after_confirmation() {
    // The dialog's OK button is the right one at the dialog bottom.
    let d = Dialog::frame(PORTRAIT);
    let ok = (d.x + d.w as i32 * 3 / 4, d.bottom() - 24 - 55);
    let saved = vec![Row::header(""), Row::item("", "forget:Home").subtitle("")];
    let script = format!(
        "{}{}shot saved\n{}shot forget\ntap {} {}\nshot forgotten\n",
        main_tap("page:wifi"),
        tap_row(wifi_page(), "page:saved"),
        tap_row(saved, "forget:Home"),
        ok.0,
        ok.1
    );
    let r = run("forget", &script);
    assert!(r.fake.borrow().profiles.is_empty());
}

#[test]
fn exit_to_kindle_asks_then_exits() {
    let d = Dialog::frame(PORTRAIT);
    let cancel = (d.x + d.w as i32 / 4, d.bottom() - 24 - 55);
    let ok = (d.x + d.w as i32 * 3 / 4, d.bottom() - 24 - 55);
    // Cancel first (the screen stays), then exit; the script would go on to tap Light.
    let script = format!(
        "{exit}shot exit-dialog\ntap {} {}\n{exit}tap {} {}\n{BACK}{}",
        cancel.0,
        cancel.1,
        ok.0,
        ok.1,
        main_tap("page:light"),
        exit = main_tap("exit"),
    );
    let r = run("exit", &script);
    assert_eq!(r.fake.borrow().brightness, 12, "the app ended at Exit");
}

#[test]
fn device_page_syncs_the_time() {
    // Device page: four cards, then the NTP button.
    let (_, ntp) = device_layout(content_rect(PORTRAIT), vec![DEVICE_CARD_H, DEVICE_CARD_H, CLOCK_CARD_H, DEVICE_CARD_H]);
    let r = run("device", &format!("{}shot device\n{}shot ntp\n", main_tap("page:device"), tap_at(ntp)));
    assert_eq!(r.fake.borrow().ntp_syncs, 1);
}

#[test]
fn texts_for_networks_and_update_checks() {
    let n = WifiNetwork { essid: "A".into(), signal: 3, secured: true, known: true };
    assert_eq!(network_detail(&n), "signal 3/5 · secured · saved");
    let err: update::Result<(update::Release, bool)> = Err(update::UpdateError::Config("x".into()));
    assert!(update_check_text(&err).starts_with("Update check failed"));
    assert_eq!(auto_sleep(0), None);
    assert_eq!(screensaver_name("image:/a/b/c.png"), "c.png");
}

#[test]
fn night_mode_toggle_is_saved_and_used_at_start() {
    // The night mode switch is a row of the main page.
    let r = run("night", &format!("{}shot night\n", main_tap("night")));
    let s = saved(&r);
    assert!(s.device.night_mode);
    let dark = |p: &HeadlessProbe| p.panel().pixels.iter().filter(|v| **v < 64).count();
    let night = run_with("night-start", "shot start\n", s, true, |_| {});
    // White on black: most of the panel is dark.
    assert!(dark(&night.probe) > 1272 * 1696 / 2, "{}", dark(&night.probe));
}

#[test]
fn update_jobs_show_progress_then_the_result() {
    // A worker thread that waits: the status line shows the label and progress.
    worker::set_inline(false);
    let mut s = SettingsScreen::default();
    let (go, rx) = std::sync::mpsc::channel::<()>();
    s.start_job(Job::spawn("Checking for an update", move |r: &Reporter<UpdateJob>| {
        r.progress("asking the release server");
        rx.recv().unwrap();
        UpdateJob::Check(Err(update::UpdateError::Config("no server".into())))
    }));
    let start = std::time::Instant::now();
    while !s.status_text.contains("asking") && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
        s.poll_job();
    }
    assert_eq!(s.status_text, "Checking for an update… asking the release server");
    assert!(s.timeout().is_some(), "a running job asks for timer events");
    go.send(()).unwrap();
    while s.job.is_some() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
        s.poll_job();
    }
    worker::set_inline(true);
    assert!(s.job.is_none() && s.timeout().is_none());
    assert!(s.status_text.starts_with("Update check failed"), "{}", s.status_text);
    assert_eq!(s.rebuild, Some(RefreshClass::Gl16));
}

/// Install has no confirmation: the button says "Install X and restart". Without a
/// launcher that can restart (tests, a run by hand) the status says to start rinky
/// again; with one, "Restarting rinky…" shows and the next timer ends rinky.
#[test]
fn install_restarts_into_the_new_version() {
    let mut s = SettingsScreen::default();
    let package = update::Asset { name: "rinky-9.9.9-kindlehf.tar.gz".into(), url: String::new(), size: None, sha256: None };
    let rel = update::Release { tag: "v9.9.9".into(), version: update::Version::parse("9.9.9").unwrap(), package, sums: None };
    s.release = Some(rel.clone());
    s.start_job(Job::spawn("Installing 9.9.9", move |_: &Reporter<UpdateJob>| UpdateJob::Install(rel, Ok("log".into()))));
    s.poll_job();
    assert!(s.dialog.is_none(), "no confirmation dialog");
    if std::env::var_os("RINKY_RUN").is_none() {
        assert_eq!(s.status_text, "Installed 9.9.9. Exit and start rinky again to use it.");
        assert!(s.restarting.is_none() && s.timeout().is_none());
    }
    // With the launcher's restart request written: a short timer, then the exit.
    s.restarting = Some("9.9.9".into());
    assert_eq!(s.timeout(), Some(RESTART_DELAY));
}

#[test]
fn update_check_from_the_page_runs_as_a_job_and_reports_a_bad_url() {
    let mut settings = Settings::default();
    let url = std::collections::BTreeMap::from([("url".to_string(), "ftp://example.invalid/release.json".to_string())]);
    settings.set_plugin_value("update", &url).unwrap();
    // "Check for update" under the card.
    let c = content_rect(PORTRAIT);
    let check = Rect::new(c.x + 32, c.y + 16 + UPDATE_CARD_H as i32 + 32, c.w - 64, 104);
    let script = format!("{}shot update\n{}shot checked\n", main_tap("page:update"), tap_at(check));
    let r = run_with("update-job", &script, settings, true, |_| {});
    assert!(!r.probe.updates().is_empty());
}

#[test]
fn plugins_page_opens_a_plugin_page_toggles_a_plugin_and_lists_notifications() {
    let root = tmp("plugins");
    let paths = Paths { root: root.clone() };
    let dir = paths.plugins().join("tpage");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), "name = \"tpage\"\nversion = \"0.1\"\ndescription = \"Test page\"\npermissions = [\"ui\"]\n").unwrap();
    let lua = "local p = {}\nfunction p.on_load(reg)\n  reg.settings_page(\"opts\", \"Test options\", function() ink.ui.notify(\"page opened\") end)\nend\nreturn p\n";
    std::fs::write(dir.join("main.lua"), lua).unwrap();
    // Plugins page: "Test options", the tpage switch, Notifications (the host below has
    // no compiled-in plugins).
    let manifest = ink_plugin::manifest::PluginManifest::parse("name = \"tpage\"\nversion = \"0.1\"\n").unwrap();
    let info = ink_plugin::PluginInfo { manifest, kind: "lua", enabled: true, errors: 0 };
    let rows = || {
        let plugins = plugin_section(&[("Test options".into(), "tpage".into())], &[(info.clone(), true)]);
        main_rows(&MainValues { plugins, ..MainValues::default() })
    };
    let script = format!(
        "shot plugins\n{}wait 50\nshot page-opened\nkey back\nwait 50\n{}wait 50\nshot toggled\n{}shot notes\n",
        tap_row(rows(), "ppage:0"),
        tap_row(rows(), "ptoggle:tpage"),
        tap_row(rows(), "page:notifications")
    );
    let (mut device, probe) =
        headless::open(HeadlessOptions { script: parse(&script).unwrap(), out_dir: out_dir("plugins"), ..Default::default() });
    let _fake = fakes::install(&mut device);
    let mut app = App::new(device, Settings::default(), paths.clone());
    // Only the Lua test plugin: compiled-in plugins would add rows to the page.
    app.plugins = crate::plugin_host::PluginHost::load(Vec::new(), &Settings::default(), &paths);
    assert_eq!(app.plugins.plugins().len(), 1, "{:?}", app.plugins.manager.errors);
    app.run(Route::Settings).expect("settings run");
    assert!(!probe.updates().is_empty());
    // The page entry ran the plugin's function; the Notifications page marked it read.
    let notes = app.plugins.notifications();
    assert!(notes.iter().any(|n| n.text == "page opened" && n.plugin == "tpage"), "{notes:?}");
    assert_eq!(app.plugins.unread_notifications(), 0);
    // The toggle wrote plugins/disabled.txt; the plugin still runs until the next start.
    assert!(plugin_host::user_disabled(&paths).contains("tpage"));
    assert!(app.plugins.plugins().iter().any(|p| p.manifest.name == "tpage" && p.enabled));
}

#[test]
fn plugin_rows_say_when_a_change_applies() {
    let manifest = ink_plugin::manifest::PluginManifest::parse("name = \"x\"\nversion = \"1\"\ndescription = \"Does x\"\n").unwrap();
    let mut p = ink_plugin::PluginInfo { manifest, kind: "lua", enabled: true, errors: 0 };
    assert_eq!(plugin_detail(&p, true), "lua · on · Does x");
    assert_eq!(plugin_detail(&p, false), "lua · off at the next start · Does x");
    p.enabled = false;
    assert_eq!(plugin_detail(&p, true), "lua · on at the next start · Does x");
    p.errors = 5;
    assert_eq!(plugin_detail(&p, true), "lua · stopped after 5 errors · Does x");
    assert_eq!(plugin_detail(&p, false), "lua · off · Does x");
}

#[test]
fn main_list_plugin_subtitle_has_no_jargon() {
    let manifest = ink_plugin::manifest::PluginManifest::parse("name = \"x\"\nversion = \"1\"\ndescription = \"Does x\"\n").unwrap();
    let mut p = ink_plugin::PluginInfo { manifest, kind: "rust", enabled: true, errors: 0 };
    // The switch shows on/off; a built-in plugin's kind means nothing to the reader.
    assert_eq!(plugin_summary(&p, true), "Does x");
    assert_eq!(plugin_summary(&p, false), "off at the next start · Does x");
    p.kind = "lua";
    assert_eq!(plugin_summary(&p, true), "Lua plugin · Does x");
    p.enabled = false;
    p.errors = 2;
    assert_eq!(plugin_summary(&p, true), "stopped after 2 errors · Lua plugin · Does x");
    p.manifest.description.clear();
    p.kind = "rust";
    assert_eq!(plugin_summary(&p, false), "");
}

#[test]
fn page_turn_wave_switch_is_saved() {
    assert!(page_wave(&Settings::default()));
    let r = run("wave", &format!("{}shot wave-off\n", main_tap("anim")));
    assert!(!page_wave(&saved(&r)));
}

#[test]
fn navigation_bar_items_move_and_keep_their_limits() {
    let nav = |items: &[&str]| {
        let chosen: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        let mut rows = vec![order::order_note(NAV_MIN, NAV_MAX), Row::header("")];
        rows.extend(order::order_rows(ink_store::prefs::NAV_ITEMS, &chosen, "nav"));
        rows
    };
    // Settings moves up one place (its up button), Statistics is added.
    let area = content_rect(PORTRAIT);
    let l = SettingsList::new(nav(&["home", "library", "collections", "settings"]));
    let (_, r) = l.locate(area, "nav:settings").unwrap();
    let (up, _) = rows::order_buttons(r);
    let script = format!("{}{}{}shot nav\n", main_tap("page:nav"), tap_at(up), tap_row(nav(&["home", "library", "settings", "collections"]), "nav:stats"));
    let r = run("nav", &script);
    assert_eq!(saved(&r).ui.nav_items, ["home", "library", "settings", "collections", "stats"]);
}

#[test]
fn export_format_choice_is_saved_and_names_the_clippings_file() {
    let rows = koreader_rows(ExportFormat::Markdown);
    let r = run("export-format", &format!("{}{}shot export-format\n", main_tap("page:koreader"), tap_row(rows, "exportfmt:clippings")));
    assert_eq!(saved(&r).export_format(), ExportFormat::Clippings);
    assert!(r.probe.frames() > 0);
    // The page lists the three formats with the chosen one checked.
    let rows = koreader_rows(ExportFormat::Clippings);
    let checked: Vec<&str> = rows.iter().filter(|r| r.trail == Trail::Check(true)).map(|r| r.action.as_str()).collect();
    assert_eq!(checked, vec!["exportfmt:clippings"]);
    assert_eq!(rows.iter().filter(|r| r.action.starts_with("exportfmt:")).count(), 3);
    let all = rows.iter().find(|r| r.action == "export-all").unwrap();
    assert_eq!(all.subtitle.as_deref(), Some("All books in rinky/exports/My Clippings.txt"));
}

#[test]
fn full_refresh_choice_is_saved() {
    let refresh = choices(&REFRESH_CHOICES.map(|n| format!("refresh:{n}")), 1);
    let r = run("refresh", &format!("{}shot refresh\n{}", main_tap("page:refresh"), tap_row(refresh, "refresh:16")));
    assert_eq!(saved(&r).reader.flash_every, 16);
}

#[test]
fn airplane_switch_on_the_main_page_keeps_the_page() {
    let r = run("airplane", &format!("{}shot airplane\n", main_tap("airplane")));
    assert!(r.fake.borrow().airplane);
}

#[test]
fn wifi_rows_say_what_the_network_is_doing() {
    use ink_device::NetStatus;
    let nets = FakeState::default().networks;
    let line = |s: &NetStatus| wifi_rows(s, false, &nets).into_iter().find(|r| r.action == "wifi").and_then(|r| r.subtitle).unwrap();
    assert_eq!(line(&NetStatus::Off), "Off");
    assert_eq!(line(&NetStatus::NoNetwork), "Not connected");
    assert_eq!(line(&NetStatus::Connecting { essid: None }), "Looking for saved networks…");
    assert_eq!(line(&NetStatus::Connecting { essid: Some("Home".into()) }), "Connecting to Home…");
    assert_eq!(line(&NetStatus::Connected { essid: "Home".into(), signal: None }), "Connected to Home");
    // The network being joined says so; none is checked before it is joined.
    let rows = wifi_rows(&NetStatus::Connecting { essid: Some("Home".into()) }, false, &nets);
    let home = rows.iter().find(|r| r.action == "net:0").unwrap();
    assert_eq!((home.subtitle.as_deref(), home.trail), (Some("Connecting…"), Trail::Check(false)));
    let toggle = |s: &NetStatus| wifi_rows(s, false, &nets).into_iter().find(|r| r.action == "wifi").unwrap().trail;
    assert_ne!(toggle(&NetStatus::Off), toggle(&NetStatus::NoNetwork), "the switch shows the radio");
}

#[test]
fn wifi_rows_show_signal_lock_and_the_connection() {
    let nets = FakeState::default().networks;
    let state = ink_device::NetStatus::Connected { essid: "Cafe".into(), signal: Some(3) };
    let rows = wifi_rows(&state, false, &nets);
    let home = rows.iter().find(|r| r.action == "net:0").unwrap();
    assert_eq!((home.lead, home.lock, home.trail), (rows::Lead::Signal(5), true, Trail::Check(false)));
    let cafe = rows.iter().find(|r| r.action == "net:1").unwrap();
    assert_eq!((cafe.lock, cafe.trail, cafe.subtitle.as_deref()), (false, Trail::Check(true), Some("Connected")));
    let wifi = rows.iter().find(|r| r.action == "wifi").unwrap();
    assert_eq!(wifi.subtitle.as_deref(), Some("Connected to Cafe (signal 3/5)"));
    // Every section of the main page is there, in order.
    let heads: Vec<String> = main_rows(&MainValues::default()).into_iter().filter(|r| r.kind == rows::Kind::Header).map(|r| r.title).collect();
    assert_eq!(heads, SECTIONS);
    // A panel without the page-turn sweep (i.MX Kindles, the Kindle 2024) has no toggle.
    let has_wave = |v: &MainValues| main_rows(v).iter().any(|r| r.action == "anim");
    assert!(has_wave(&MainValues::default()));
    assert!(!has_wave(&MainValues { wave_hidden: true, ..MainValues::default() }));
}

#[test]
fn main_page_in_night_mode_and_landscape() {
    let mut s = Settings::default();
    s.device.night_mode = true;
    let r = run_with("main-night", "shot main-night\n", s, true, |_| {});
    assert!(!r.probe.updates().is_empty());
}

#[test]
fn main_list_scrolls_to_every_row_and_keeps_the_bar_and_back_button() {
    let area = content_rect(PORTRAIT);
    let main = SettingsList::new(main_rows(&MainValues::default()));
    assert!(main.max_scroll(area) > 0, "the main list is longer than the screen");
    for row in main.rows.iter().filter(|r| !r.action.is_empty()) {
        let (need, r) = main.locate(area, &row.action).unwrap();
        assert!(need <= main.max_scroll(area) && r.y >= area.y && r.bottom() <= area.bottom(), "{}: {r:?}", row.action);
    }
    // Exit to Kindle is the last row.
    assert_eq!(main.rows.last().unwrap().action, "exit");
    // The brightness plus button: a slider row under the "Frontlight" header.
    assert_eq!(light_buttons(0).1, Rect::new(1148, 316, 100, 100));
    let back = AppBar::back_rect(app_bar_rect(PORTRAIT));
    assert_eq!((back.x + back.w as i32 / 2, back.y + back.h as i32 / 2), (60, 68));
}

#[test]
fn path_tail_keeps_short_paths_and_cuts_long_ones() {
    assert_eq!(path_tail("/mnt/us/rinky/settings.toml", 60), "/mnt/us/rinky/settings.toml");
    let cut = path_tail("/a/very/long/directory/name/settings.toml", 20);
    assert_eq!(cut.chars().count(), 20);
    assert!(cut.starts_with('…') && cut.ends_with("settings.toml"));
}

/// A toggled switch on the main page refreshes its row and the status line (the rows
/// of pixels that changed), not the whole screen.
#[test]
fn toggling_an_option_refreshes_only_the_changed_rows() {
    let r = run("toggle-rows", &main_tap("confirm-exit"));
    let ups = r.probe.updates();
    let full = Rect::new(0, 0, 1272, 1696);
    // The first full-screen update is the page coming up, then the DU jumps to the row;
    // after them, nothing is full.
    let start = ups.iter().rposition(|u| u.rect == full || (u.class == RefreshClass::Du && u.rect.h >= 1400)).unwrap_or(0);
    let after: Vec<_> = ups[start + 1..].iter().map(|u| (u.rect, u.class)).collect();
    println!("after the last full-screen update: {after:?}");
    assert!(!after.is_empty() && after.iter().all(|(r, _)| r.h < 400), "{after:?}");
}

#[test]
fn back_from_a_sub_page_keeps_the_scroll_offset() {
    // The main list after one long swipe up, and a page row shown at that offset.
    let area = content_rect(PORTRAIT);
    let swipe = format!("swipe 600 {} 600 {}\nwait 50\n", area.bottom() - 100, area.y + 100);
    let mut l = SettingsList::new(main_rows(&main_values()));
    assert!(l.scroll_by(area, SettingsList::swipe_step(area.h, area.h as i32 - 200)));
    let (_, r) = l
        .layout(area)
        .into_iter()
        .map(|(i, r)| (i, Rect::new(r.x, r.y - l.scroll, r.w, r.h)))
        .find(|(i, r)| l.rows[*i].action.starts_with("page:") && r.y >= area.y && r.bottom() <= area.bottom())
        .expect("a page row on screen after the swipe");
    let tap = format!("tap 600 {}\nwait 50\n", r.y + r.h as i32 / 2);
    // Scrolled only; scrolled, a sub-page opened, then Back; never scrolled.
    let scrolled = run("keep-scroll-a", &format!("wait 100\n{swipe}wait 100\nkey power\n")).probe.panel();
    let back = run("keep-scroll-b", &format!("wait 100\n{swipe}{tap}wait 100\n{BACK}wait 100\nkey power\n")).probe.panel();
    let top = run("keep-scroll-c", "wait 100\nkey power\n").probe.panel();
    // The list area only (a row cut at the bottom edge may leave ink under the list), in
    // black and white: the jump is DU (two levels), Back redraws with REAGL (text edges
    // in gray).
    let rows = |c: &ink_device::Canvas| c.pixels[area.y as usize * c.width..area.bottom() as usize * c.width].iter().map(|p| *p >= 0x80).collect::<Vec<_>>();
    assert!(rows(&scrolled) != rows(&top), "the swipe scrolled the list");
    assert!(rows(&back) == rows(&scrolled), "Back shows the main list at the offset it was left at");
}

#[test]
fn reveal_scrolls_only_as_far_as_the_row_needs() {
    let area = content_rect(PORTRAIT);
    let mut l = SettingsList::new(main_rows(&main_values()));
    let last = l.rows.last().unwrap().action.clone();
    l.reveal(area, &last);
    let on_screen = |l: &SettingsList, a: &str| l.layout(area).iter().any(|(i, r)| l.rows[*i].action == a && r.y - l.scroll >= area.y && r.bottom() - l.scroll <= area.bottom());
    assert!(l.scroll > 0 && on_screen(&l, &last), "the last row: scrolled down to it");
    let first = l.rows.iter().find(|r| !r.action.is_empty()).unwrap().action.clone();
    l.reveal(area, &first);
    assert!(on_screen(&l, &first), "the first row is on screen again");
    let s = l.scroll;
    l.reveal(area, "no-such-row");
    assert_eq!(l.scroll, s);
}

#[test]
fn back_from_the_gesture_picker_keeps_the_gesture_list_offset() {
    // Reader gestures, one long swipe down the list, a trigger row opens the action
    // picker, Back: the list comes back at the same offset (it used to start at the top).
    // The scripts end on the page (no power key: in settings it acts as Back).
    let area = content_rect(PORTRAIT);
    let open = main_tap("page:mod:gestures");
    let swipe = format!("swipe 600 {} 600 {}\nwait 50\n", area.bottom() - 100, area.y + 100);
    let mut l = SettingsList::new(gestures::rows(&Settings::default().gestures, None, &[]));
    assert!(l.scroll_by(area, SettingsList::swipe_step(area.h, area.h as i32 - 200)), "the gesture list scrolls");
    let (_, r) = l
        .layout(area)
        .into_iter()
        .map(|(i, r)| (i, Rect::new(r.x, r.y - l.scroll, r.w, r.h)))
        .find(|(i, r)| l.rows[*i].action.starts_with("gest:") && r.y >= area.y && r.bottom() <= area.bottom())
        .expect("a trigger row on screen after the swipe");
    let tap = format!("tap 600 {}\nwait 50\n", r.y + r.h as i32 / 2);
    // In black and white, as in back_from_a_sub_page_keeps_the_scroll_offset.
    let rows = |c: &ink_device::Canvas| c.pixels[area.y as usize * c.width..area.bottom() as usize * c.width].iter().map(|p| *p >= 0x80).collect::<Vec<_>>();
    let scrolled = run("gest-scroll-a", &format!("{open}wait 100\n{swipe}wait 100\n")).probe.panel();
    let back = run("gest-scroll-b", &format!("{open}wait 100\n{swipe}{tap}wait 100\n{BACK}wait 100\n")).probe.panel();
    let top = run("gest-scroll-c", &format!("{open}wait 100\n")).probe.panel();
    assert!(rows(&scrolled) != rows(&top), "the swipe scrolled the list");
    assert!(rows(&back) == rows(&scrolled), "Back shows the gesture list at the offset it was left at");
}

#[test]
fn a_jump_leaves_no_ink_outside_the_list() {
    // The main list as it opens, and after a half-page jump and a whole-page jump: every
    // pixel outside the list area (app bar, status line, margins) is the same. Before,
    // the icon, chevron or toggle of a row cut by the list's edge was drawn there.
    let area = content_rect(PORTRAIT);
    let half = format!("swipe 600 1213 600 {}\nwait 50\n", 1213 - SWIPE);
    let page = format!("swipe 600 {} 600 {}\nwait 50\n", area.bottom() - 100, area.y + 100);
    let outside = |c: &ink_device::Canvas| {
        (0..c.height as i32)
            .flat_map(|y| (0..c.width as i32).map(move |x| (x, y)))
            .filter(|(x, y)| !area.contains(ink_device::Point::new(*x, *y)))
            .map(|(x, y)| c.get(x, y).unwrap())
            .collect::<Vec<u8>>()
    };
    let opened = run("clip-a", "wait 100\n").probe.panel();
    for (name, script) in [("clip-half", half.clone()), ("clip-page", page.clone()), ("clip-both", format!("{half}{page}{page}"))] {
        let jumped = run(name, &format!("wait 100\n{script}wait 100\n")).probe.panel();
        let inside = |c: &ink_device::Canvas| c.crop(area).pixels;
        assert!(inside(&jumped) != inside(&opened), "{name}: the list jumped");
        assert!(outside(&jumped) == outside(&opened), "{name}: ink outside the list area after the jump");
    }
}

/// Half-page jumps down and back up the main list: every pixel the canvas changed is
/// inside an update rect (the headless framebuffer, written only inside update rects,
/// equals the canvas after each frame). Day and night.
#[test]
fn list_jumps_show_every_changed_pixel_day_and_night() {
    let area = content_rect(PORTRAIT);
    let y0 = area.y + 1000;
    let mut script = String::new();
    for _ in 0..4 {
        script.push_str(&format!("swipe 600 {y0} 600 {}\nwait 50\n", y0 - SWIPE));
    }
    for _ in 0..4 {
        script.push_str(&format!("swipe 600 {} 600 {}\nwait 50\n", y0 - SWIPE, y0));
    }
    for night in [false, true] {
        let mut s = Settings::default();
        s.device.night_mode = night;
        let r = run_with(if night { "jumps-night" } else { "jumps-day" }, &script, s, true, |_| {});
        assert!(r.probe.frames() > 8, "night {night}: {} frames", r.probe.frames());
        assert_eq!(r.probe.uncovered(), vec![], "night {night}: pixels drawn outside every update rect");
        // The page opens with REAGL; each of the 7 jumps that moves the list (the 8th
        // hits the top) is one DU of the list area (the rows are black and white), never
        // a flash, and no settle update follows.
        let ups = r.probe.updates();
        assert!(ups.iter().all(|u| u.class != RefreshClass::Gc16), "night {night}: no flash in {ups:?}");
        let jumps = ups.iter().filter(|u| u.class == RefreshClass::Du && u.rect.intersect(&area) == area).count();
        assert!(jumps >= 7, "night {night}: {jumps} DU jumps in {ups:?}");
        assert!(ups.iter().skip(1).all(|u| u.class == RefreshClass::Du), "night {night}: only DU after opening: {ups:?}");
    }
}

#[test]
fn control_buttons_page_sets_the_rotate_angle_and_links_wifi() {
    open_at("mod:control_center");
    let rows = control_buttons::rows_for(&Settings::default(), &crate::plugin_host::PluginHost::new(Vec::new()));
    assert!(rows.iter().any(|r| r.action == "page:wifi"), "Wi-Fi long-press options link the Wi-Fi page");
    // Long swipes up to the end of the list (offset max_scroll), then a tap on the row.
    let area = content_rect(PORTRAIT);
    let l = SettingsList::new(rows);
    let (need, r) = l.locate(area, "ccadv:rotate:2").expect("180° row");
    let y = r.y + need - l.max_scroll(area) + r.h as i32 / 2;
    let up = format!("swipe 600 {} 600 {}\nwait 50\n", area.bottom() - 50, area.y + 50).repeat(4);
    let r = run("ccbtn-angle", &format!("{up}tap 600 {y}\nwait 50\nshot angle\n"));
    let s = saved(&r);
    assert_eq!(s.ui.rotate_angle, 180);
    assert_eq!(s.device.rotation, 0, "Settings only stores the angle");
}

/// Back from a page opened with `open_at` returns to the screen below, not to the main list.
#[test]
fn back_from_a_directly_opened_page_returns_to_the_screen_below() {
    let go = format!("wait 100\ntap 600 320\nwait 100\n{BACK}wait 100\n");
    let plain = {
        let (mut device, probe) =
            headless::open(HeadlessOptions { script: parse(&go).unwrap(), out_dir: None, ..Default::default() });
        let _ = fakes::install(&mut device);
        App::new(device, Settings::default(), Paths { root: tmp("direct-a") }).run(Route::Demo).expect("run");
        probe.panel()
    };
    open_at("status_bars");
    let (mut device, probe) = headless::open(HeadlessOptions { script: parse(&go).unwrap(), out_dir: None, ..Default::default() });
    let _ = fakes::install(&mut device);
    App::new(device, Settings::default(), Paths { root: tmp("direct-b") }).run(Route::Demo).expect("run");
    assert!(plain.pixels == probe.panel().pixels, "Back shows the demo screen, as after the main list");
}

/// An App with only the probe plugin (a control tile "Probe go" and a library entry
/// "Probe lib"), running the settings screen.
fn run_probe(name: &str, script: &str, settings: Settings) -> (Run, crate::app::App, crate::plugin_entries::test_plugin::Calls) {
    let calls: crate::plugin_entries::test_plugin::Calls = Default::default();
    let (mut device, probe) = headless::open(HeadlessOptions { script: parse(script).unwrap(), out_dir: out_dir(name), ..Default::default() });
    let fake = fakes::install(&mut device);
    let root = tmp(name);
    let mut app = App::new(device, settings, Paths { root: root.clone() });
    app.plugins = crate::plugin_entries::test_plugin::Probe::host(&calls);
    app.run(Route::Settings).expect("settings run");
    (Run { probe, fake, root }, app, calls)
}

#[test]
fn control_center_page_lists_a_plugin_tile_saves_its_id_and_skips_unknown_ids() {
    let calls = Default::default();
    let host = crate::plugin_entries::test_plugin::Probe::host(&calls);
    let mut s = Settings::default();
    s.ui.control_buttons = ["wifi", "plugin:gone:x", "plugin:probe:go", "sync", "rotate"].map(String::from).to_vec();
    let rows = control_buttons::rows_for(&s, &host);
    // Listed on and in order; the unknown id and the legacy sync button (kosync is not
    // loaded here) are skipped.
    let order: Vec<&str> = rows.iter().filter(|r| r.action.starts_with("ccbtn:")).map(|r| r.action.as_str()).collect();
    assert_eq!(&order[..3], ["ccbtn:wifi", "ccbtn:plugin:probe:go", "ccbtn:rotate"]);
    assert!(!order.iter().any(|a| a.contains("gone") || a.contains("kosync")));
    let go = rows.iter().find(|r| r.action == "ccbtn:plugin:probe:go").unwrap();
    assert_eq!(go.title, "Probe go");
    assert_eq!(go.trail, Trail::Order { on: true, up: true, down: true });
    // A plugin that is not there offers nothing.
    let none = control_buttons::rows_for(&s, &crate::plugin_host::PluginHost::new(Vec::new()));
    assert!(!none.iter().any(|r| r.action.contains("probe")));
    // Tapping the row of a tile that is off adds it and saves its id.
    let mut s = Settings::default();
    s.ui.control_buttons = vec!["wifi".into(), "rotate".into()];
    let off = control_buttons::rows_for(&s, &host);
    assert_eq!(off.iter().find(|r| r.action == "ccbtn:plugin:probe:go").unwrap().trail, Trail::Order { on: false, up: false, down: false });
    open_at("mod:control_center");
    let script = format!("{}shot added\n", tap_row(off, "ccbtn:plugin:probe:go"));
    let (r, _app, _calls) = run_probe("ccbtn-plugin", &script, s);
    assert_eq!(saved(&r).ui.control_buttons, ["wifi", "rotate", "plugin:probe:go"]);
}

#[test]
fn navigation_page_lists_a_plugin_entry_and_saves_its_id() {
    let calls = Default::default();
    let host = crate::plugin_entries::test_plugin::Probe::host(&calls);
    let extra = crate::plugin_entries::nav_choices(&host);
    let list = crate::plugin_entries::merged(ink_store::prefs::NAV_ITEMS, &extra);
    let chosen: Vec<String> = ["home", "library", "collections", "settings"].map(String::from).to_vec();
    let mut rows = vec![order::order_note(NAV_MIN, NAV_MAX), Row::header("Bottom bar, left to right")];
    rows.extend(order::order_rows(&list, &chosen, "nav"));
    let r = rows.iter().find(|r| r.action == "nav:plugin:probe:lib").expect("the plugin entry is listed");
    assert_eq!((r.title.as_str(), &r.trail), ("Probe lib", &Trail::Order { on: false, up: false, down: false }));
    let script = format!("{}{}shot nav-plugin\n", main_tap("page:nav"), tap_row(rows, "nav:plugin:probe:lib"));
    let (r, _app, _calls) = run_probe("nav-plugin", &script, Settings::default());
    assert_eq!(saved(&r).ui.nav_items, ["home", "library", "collections", "settings", "plugin:probe:lib"]);
}

/// Settings > Plugins: a plugin's settings open as a full page (not a popup over the
/// list): the plugin's menu is the page's rows, a switch answers the menu and the
/// plugin's next menu updates the page in place, Back cancels and returns. Nothing
/// flashes after the screen is shown.
#[test]
fn plugin_settings_open_as_a_page_built_from_the_plugin_menu() {
    use crate::plugin_entries::test_plugin::Toggler;
    let calls: crate::plugin_entries::test_plugin::Calls = Default::default();
    let root = tmp("plugin-page");
    let manifest = ink_plugin::manifest::PluginManifest::parse("name = \"toggler\"\nversion = \"1\"\n").unwrap();
    let info = ink_plugin::PluginInfo { manifest, kind: "rust", enabled: true, errors: 0 };
    let main = main_rows(&MainValues { plugins: plugin_section(&[("Toggler options".into(), "toggler".into())], &[(info, true)]), ..MainValues::default() });
    let items = vec![("Ready".into(), String::new()), ("Auto start\toff".into(), "auto".into()), ("Server\tauto".into(), "server".into()), ("Done".into(), "done".into())];
    let page = PluginPage {
        entry: PluginEntry { plugin: "toggler".into(), id: "opts".into(), label: "Toggler options".into(), kind: plugin_host::EntryKind::Settings },
        menu: Some((Toggler::MENU, "Toggler".into(), items)),
        open: true,
    };
    let rows = page.rows();
    assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), [rows::Kind::Note, rows::Kind::Item, rows::Kind::Item, rows::Kind::Item]);
    assert_eq!((rows[1].trail, rows[2].value.as_deref()), (Trail::Toggle(false), Some("auto")));
    let script = format!("{}wait 50\nshot page\n{}wait 50\nshot toggled\n{BACK}shot back\n", tap_row(main, "ppage:0"), tap_row(rows, "menu:1"));
    let (mut device, probe) = headless::open(HeadlessOptions { script: parse(&script).unwrap(), out_dir: out_dir("plugin-page"), ..Default::default() });
    let _fake = fakes::install(&mut device);
    let mut app = App::new(device, Settings::default(), Paths { root: root.clone() });
    app.plugins = Toggler::host(&calls);
    app.run(Route::Settings).expect("settings run");
    assert_eq!(*calls.borrow(), ["settings:opts", "result:auto", "result:"], "opened, switched, cancelled by Back");
    assert!(app.plugins.ui.layer_names().is_empty() && app.plugins.claimed_menus().is_none(), "no popup; the menus are given back");
    let u = probe.updates();
    assert!(u.iter().skip(1).all(|d| d.class != RefreshClass::Gc16), "no flash after the screen opened: {u:?}");
    // The switch: the plugin's new menu repaints the switch row only, not the page.
    let content = content_rect(PORTRAIT);
    assert!(u.iter().any(|d| d.rect.h <= rows::ROW_H && d.rect.y >= content.y), "one row repainted: {u:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ruby_annotations_default_is_saved() {
    let d = Settings::default();
    assert!(d.reader.ruby);
    let r = run("ruby", &format!("{}{}shot ruby-off\n", main_tap("page:layout"), tap_row(layout_rows(&d.reader), "ruby")));
    assert!(!saved(&r).reader.ruby);
}
