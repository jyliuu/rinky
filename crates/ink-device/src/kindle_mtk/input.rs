//! The Kindle input loop: touch, power key, lipc events, kernel uevents and the USB
//! gadget state, multiplexed with `poll(2)`, plus the device-side policies that run
//! inside it (sleep and wake, USB drive mode, low battery, warmth schedule, NTP on
//! Wi-Fi connect) and the heartbeat the watchdog reads.
//!
//! The loop never sleeps longer than [`MAX_POLL`], so the heartbeat
//! (`/tmp/rinky/heartbeat`, a counter) moves while the app waits for input; a
//! stuck app stops calling `next_event` and the heartbeat stops.

use super::fb::MtkScreen;
use super::system;
use super::Shared;
use crate::battery::{self, LowBatteryMonitor};
use crate::canvas::Canvas;
use crate::clock::NtpSample;
use crate::events::{self, CoverSource, HallInfo, UeventSignal, Uevent};
use crate::evdev::{
    choose_touch_device, AppContacts, DevInfo, RawEvent, SlotSeed, TouchDecoder, TouchMapping, TouchOverride, TouchProfile,
    ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_X, ABS_Y, EVENT_SIZE, EV_KEY,
    KEY_POWER, SLOTS,
};
use crate::input::{Input, InputEvent, Key, KeyEvent, PowerEvent, Timestamp, TouchFrame};
use crate::journal::Entry;
use crate::light::AmberScheduler;
use crate::lipc::{EventHub, Lipc, LipcValue};
use crate::screen::RefreshClass;
use crate::sleep::{Resume, SleepController, SleepPlatform, SleepState};
use crate::sysfs::SysRoot;
use crate::usb::{self, UsbAction, UsbCable, UsbManager, Volumd};
use crate::{DeviceError, Result};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const EVIOCGRAB: u32 = 0x4004_4590;
/// Longest single `poll` (heartbeat and periodic checks).
pub const MAX_POLL: Duration = Duration::from_secs(5);
const BATTERY_EVERY: Duration = Duration::from_secs(300);
const AMBER_EVERY: Duration = Duration::from_secs(60);

const fn eviocgabs(abs: u16) -> u32 {
    // _IOR('E', 0x40 + abs, struct input_absinfo) with sizeof == 24
    0x8018_4500 | (0x40 + abs as u32)
}

const fn eviocgmtslots(len: u32) -> u32 {
    // _IOC(_IOC_READ, 'E', 0x0a, len)
    (2 << 30) | (len << 16) | (0x45 << 8) | 0x0a
}

/// Parses `/proc/bus/input/devices`.
pub fn list_devices() -> Vec<DevInfo> {
    let text = std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default();
    crate::evdev::parse_devices(&text)
}

/// Parses the text of `/proc/bus/input/devices` (kept for callers of phase 0).
pub fn parse_devices(text: &str) -> Vec<DevInfo> {
    crate::evdev::parse_devices(text)
}

fn abs_info(f: &File, code: u16) -> Option<[i32; 6]> {
    let mut info = [0i32; 6];
    // SAFETY: EVIOCGABS writes one struct input_absinfo (6 x i32).
    let r = unsafe { libc::ioctl(f.as_raw_fd(), eviocgabs(code) as _, info.as_mut_ptr()) };
    (r >= 0).then_some(info)
}

fn abs_range(f: &File, code: u16) -> Option<(i32, i32)> {
    abs_info(f, code).filter(|i| i[2] > i[1]).map(|i| (i[1], i[2]))
}

fn mt_slots(f: &File, code: u16) -> Option<[i32; SLOTS]> {
    let mut buf = [-1i32; SLOTS + 1];
    buf[0] = code as i32;
    let len = (std::mem::size_of::<i32>() * (SLOTS + 1)) as u32;
    // SAFETY: EVIOCGMTSLOTS fills `len` bytes: the code followed by one value per slot.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), eviocgmtslots(len) as _, buf.as_mut_ptr()) };
    if r < 0 {
        return None;
    }
    let mut out = [0i32; SLOTS];
    out.copy_from_slice(&buf[1..]);
    Some(out)
}

/// Writes key press + release records into the evdev node of the device whose name
/// contains `device_name` (for example `pwrkey`). Every reader of that node receives
/// them as if the key was pressed (powerd ignores injected events).
pub fn inject_key(device_name: &str, code: u16) -> Result<String> {
    let dev = list_devices()
        .into_iter()
        .find(|d| d.name.contains(device_name))
        .ok_or_else(|| DeviceError::Backend(format!("no input device named *{device_name}*")))?;
    let path = format!("/dev/input/{}", dev.event);
    let mut f = OpenOptions::new().write(true).open(&path)?;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let time = Timestamp(now);
    let mut buf = Vec::new();
    for value in [1, 0] {
        buf.extend_from_slice(&RawEvent { time, ty: EV_KEY, code, value }.to_bytes());
        buf.extend_from_slice(&RawEvent { time, ty: 0, code: 0, value: 0 }.to_bytes());
    }
    f.write_all(&buf)?;
    Ok(format!("{} ({path})", dev.name))
}

fn read_raw(f: &mut File) -> std::io::Result<Vec<RawEvent>> {
    let mut buf = [0u8; EVENT_SIZE * 64];
    let n = f.read(&mut buf)?;
    Ok(buf[..n].as_chunks::<EVENT_SIZE>().0.iter().map(|e| RawEvent::from_bytes(e)).collect())
}

/// The touchscreen: device, decoder and grab state.
pub struct TouchDev {
    file: File,
    /// `/dev/input/eventN`.
    pub path: String,
    /// Device name (`pt_mt`, `fts_ts`).
    pub name: String,
    /// Profile in use.
    pub profile: TouchProfile,
    decoder: TouchDecoder,
    grab: bool,
}

impl TouchDev {
    /// Opens the touchscreen chosen by name (`RINKY_TOUCH` may override), builds
    /// its mapping for a `w` x `h` panel and seeds the decoder from the kernel.
    pub fn open(w: u32, h: u32, grab: bool, ov: &TouchOverride) -> Result<TouchDev> {
        let devs = list_devices();
        let dev = choose_touch_device(&devs, ov)
            .ok_or_else(|| DeviceError::Backend(format!("no touch device in /proc/bus/input/devices: {devs:?}")))?;
        let file = File::open(format!("/dev/input/{}", dev.event))?;
        let profile = match &ov.profile {
            Some(p) => TouchProfile::by_name(p).unwrap_or_else(|| TouchProfile::for_device(&dev.name)),
            None => TouchProfile::for_device(&dev.name),
        };
        let (w, h) = (w as i32, h as i32);
        let (code_x, code_y) = if dev.has_mt { (ABS_MT_POSITION_X, ABS_MT_POSITION_Y) } else { (ABS_X, ABS_Y) };
        let rx = abs_range(&file, code_x).unwrap_or((0, w - 1));
        let ry = abs_range(&file, code_y).unwrap_or((0, h - 1));
        let mapping = TouchMapping::for_profile(&profile, rx, ry, w, h).with_override(ov);
        if !ov.unknown.is_empty() {
            log::warn!("RINKY_TOUCH: ignored {:?}", ov.unknown);
        }
        log::info!("touch: {} ({}) profile {} {}", dev.name, dev.event, profile.name, mapping.describe());
        let mut t = TouchDev {
            file,
            path: format!("/dev/input/{}", dev.event),
            name: dev.name.clone(),
            profile,
            decoder: TouchDecoder::new(mapping),
            grab,
        };
        if grab {
            t.set_grab(true);
        }
        let seeded = t.seed();
        log::info!("touch: kernel slot cache at open {}", describe_seed(&seeded));
        Ok(t)
    }

    /// The mapping in use.
    pub fn mapping(&self) -> TouchMapping {
        *self.decoder.mapping()
    }

    fn set_grab(&self, on: bool) -> bool {
        // SAFETY: EVIOCGRAB takes an int flag by value.
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, on as libc::c_int) };
        if r < 0 {
            log::warn!("EVIOCGRAB {on}: {}", std::io::Error::last_os_error());
        }
        r >= 0
    }

    /// Copies the kernel's cached slot values into the decoder (fixes the "(0, 0)
    /// first touch": see `evdev` module docs). Returns the seeded slots for logs.
    pub fn seed(&mut self) -> Vec<(usize, SlotSeed)> {
        let mut seeded = Vec::new();
        if let (Some(ids), Some(xs), Some(ys)) = (
            mt_slots(&self.file, ABS_MT_TRACKING_ID),
            mt_slots(&self.file, ABS_MT_POSITION_X),
            mt_slots(&self.file, ABS_MT_POSITION_Y),
        ) {
            // Slots the device has (ABS_MT_SLOT maximum + 1); the rest of the buffer
            // keeps its -1 filler.
            let mut nslots = SLOTS;
            if let Some(info) = abs_info(&self.file, ABS_MT_SLOT) {
                self.decoder.set_current_slot(info[0]);
                nslots = ((info[2].max(0) + 1) as usize).min(SLOTS);
            }
            for i in 0..nslots {
                let s = SlotSeed { tracking_id: ids[i], x: xs[i], y: ys[i] };
                self.decoder.seed_slot(i, s);
                if s.x != 0 || s.y != 0 || s.tracking_id >= 0 {
                    seeded.push((i, s));
                }
            }
        } else if let (Some(x), Some(y)) = (abs_info(&self.file, ABS_X), abs_info(&self.file, ABS_Y)) {
            self.decoder.seed_single(x[0], y[0]);
            seeded.push((0, SlotSeed { tracking_id: -1, x: x[0], y: y[0] }));
        }
        log::debug!("touch seed: {seeded:?}");
        seeded
    }

    /// Releases and takes the grab again and re-seeds (after a resume). Reopens the
    /// device when its node went away.
    pub fn regrab(&mut self) -> Result<()> {
        if self.grab {
            self.set_grab(false);
            if !self.set_grab(true) {
                // ENODEV after a resume: reopen the node.
                let f = File::open(&self.path)?;
                self.file = f;
                self.set_grab(true);
            }
        }
        let seeded = self.seed();
        log::info!("touch: re-grabbed, kernel slot cache {}", describe_seed(&seeded));
        Ok(())
    }
}

/// `slot 0 idle at 636,700; slot 1 down (id 7) at 10,20` for logs.
fn describe_seed(seeded: &[(usize, SlotSeed)]) -> String {
    if seeded.is_empty() {
        return "empty".into();
    }
    seeded
        .iter()
        .map(|(i, s)| {
            if s.tracking_id >= 0 {
                format!("slot {i} down (id {}) at {},{}", s.tracking_id, s.x, s.y)
            } else {
                format!("slot {i} idle at {},{}", s.x, s.y)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// volumd through lipc.
pub struct LipcVolumd(pub Lipc);

const VOLUMD: &str = "com.lab126.volumd";

impl Volumd for LipcVolumd {
    fn set_disable_mounting(&mut self, on: bool) -> Result<()> {
        self.0.set_int(VOLUMD, "disableMounting", on as i64)
    }
    fn disable_mounting(&self) -> Result<bool> {
        Ok(self.0.get_int(VOLUMD, "disableMounting")? != 0)
    }
    fn drive_mode_state(&self) -> Result<i64> {
        self.0.get_int(VOLUMD, "driveModeState")
    }
    fn userstore_available(&self) -> Result<bool> {
        Ok(self.0.get_int(VOLUMD, "userstoreIsAvailable")? != 0)
    }
    fn ready_to_unmount(&mut self) -> Result<()> {
        self.0.set_int(VOLUMD, "userstoreReadyToUnMount", 1)
    }
}

/// How the userstore would be exported here.
pub fn detect_transport(lipc: &Lipc) -> usb::UsbTransport {
    if lipc.get_int(VOLUMD, "useUsbForNetwork").unwrap_or(0) == 1 {
        usb::UsbTransport::Network
    } else if std::path::Path::new("/usr/bin/tizen-mtp").exists() {
        usb::UsbTransport::Mtp
    } else {
        usb::UsbTransport::MassStorage
    }
}

/// Opens a `NETLINK_KOBJECT_UEVENT` socket (kernel broadcast group 1).
fn open_uevent_socket() -> Option<OwnedFd> {
    // SAFETY: plain socket/bind calls with a zeroed sockaddr_nl.
    unsafe {
        let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 15);
        if fd < 0 {
            log::warn!("uevent socket: {}", std::io::Error::last_os_error());
            return None;
        }
        let owned = OwnedFd::from_raw_fd(fd);
        let mut addr: libc::sockaddr_nl = std::mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as _;
        addr.nl_groups = 1;
        if libc::bind(fd, (&addr as *const libc::sockaddr_nl).cast(), std::mem::size_of::<libc::sockaddr_nl>() as _) < 0 {
            log::warn!("uevent bind: {}", std::io::Error::last_os_error());
            return None;
        }
        Some(owned)
    }
}

fn make_pipe() -> Option<(OwnedFd, File)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe2() fills two descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
        return None;
    }
    // SAFETY: both descriptors were just created and are owned here.
    let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Some((rd, File::from(wr)))
}

fn drain_fd(fd: i32) {
    let mut buf = [0u8; 256];
    // SAFETY: reading into a local buffer from our own non-blocking pipe.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

/// Requests from the service objects, executed in the input loop.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Sleep now.
    Sleep,
    /// Defer suspend for a while (`ZERO` cancels).
    Defer(Duration),
    /// New inactivity limit.
    AutoSleep(Option<Duration>),
    /// Start or end USB drive mode.
    DriveMode(bool),
    /// Charge only.
    ChargeOnly(bool),
    /// Re-read the warmth schedule now.
    AmberChanged,
    /// The user set the warmth by hand (new base warmth outside the schedule).
    Warmth(u32),
}

/// Options of the input loop (see [`super::KindleOptions`]).
#[derive(Clone, Debug)]
pub struct LoopOptions {
    /// Grab the touchscreen.
    pub grab_touch: bool,
    /// Subscribe to lipc publishers.
    pub lipc_events: bool,
    /// Sleep policy (`None` = deliver the power key to the app).
    pub sleep: Option<crate::sleep::SleepConfig>,
    /// USB policy (`None` = leave volumd alone).
    pub usb: Option<usb::UsbPolicy>,
    /// Low-battery warning level.
    pub low_battery: u8,
    /// NTP server for a sync when Wi-Fi connects (`None` = no sync).
    pub ntp_server: Option<String>,
    /// Apply powerd's warmth schedule.
    pub amber: bool,
    /// Heartbeat file.
    pub heartbeat: Option<PathBuf>,
}

/// Kindle input and the device policies.
pub struct KindleInput {
    shared: Rc<RefCell<Shared>>,
    touch: TouchDev,
    power: Option<File>,
    lipc: Lipc,
    sys: SysRoot,
    opts: LoopOptions,
    hub: Option<(EventHub, OwnedFd)>,
    uevent: Option<OwnedFd>,
    udc: Option<File>,
    queue: VecDeque<InputEvent>,
    sleep: Option<SleepController>,
    usb: Option<UsbManager<LipcVolumd>>,
    usb_next_tick: Instant,
    battery: LowBatteryMonitor,
    battery_next: Instant,
    amber: AmberScheduler,
    amber_next: Instant,
    amber_base: Option<u32>,
    ntp_rx: Option<mpsc::Receiver<Result<NtpSample>>>,
    wifi_state: String,
    heartbeat: u64,
    heartbeat_at: Option<Instant>,
    light_saved: Vec<(String, i64)>,
    hal_plugged: Option<bool>,
    /// The contacts the app has down.
    app_touch: AppContacts,
}

struct Plat<'a> {
    shared: &'a RefCell<Shared>,
    touch: &'a mut TouchDev,
    sys: &'a SysRoot,
    lipc: &'a Lipc,
    light_saved: &'a mut Vec<(String, i64)>,
}

fn invert(c: &mut Canvas) {
    for p in &mut c.pixels {
        *p = 255 - *p;
    }
}

/// Shows a panel-orientation picture over whatever is there, keeping the old content
/// as the shadow (app updates go to the shadow until [`hide_overlay`]).
fn show_overlay(sh: &mut Shared, mut picture: Canvas) -> Result<()> {
    if sh.shadow.is_none() {
        sh.shadow = Some(sh.screen.snapshot());
    }
    if sh.screen.hw_night() {
        // The driver inverts; store the negative so the picture shows as drawn.
        invert(&mut picture);
    }
    sh.screen.write_panel(&picture, RefreshClass::Gc16)
}

fn hide_overlay(sh: &mut Shared) -> Result<()> {
    if sh.asleep || sh.locked {
        return Ok(());
    }
    if let Some(shadow) = sh.shadow.take() {
        sh.screen.write_panel(&shadow, RefreshClass::Gc16)?;
    }
    Ok(())
}

impl SleepPlatform for Plat<'_> {
    fn show_screensaver(&mut self) -> Result<()> {
        let mut sh = self.shared.borrow_mut();
        let size = crate::Screen::panel_size(&sh.screen);
        // Under the USB lock screen the page is in the shadow (raw values).
        let visible = match &sh.shadow {
            Some(shadow) => {
                let mut c = shadow.clone();
                if sh.screen.hw_night() {
                    invert(&mut c);
                }
                c
            }
            None => crate::Screen::screenshot(&sh.screen)?,
        };
        let picture = crate::screensaver::render(&sh.screensaver, Some(&visible), size);
        sh.asleep = true;
        show_overlay(&mut sh, picture)
    }

    fn restore_screen(&mut self) -> Result<()> {
        let mut sh = self.shared.borrow_mut();
        sh.asleep = false;
        if sh.locked {
            // Woken in USB drive mode: the lock screen again; the page stays in the
            // shadow until drive mode ends.
            let size = crate::Screen::panel_size(&sh.screen);
            return show_overlay(&mut sh, crate::screensaver::usb_lock_screen(size));
        }
        hide_overlay(&mut sh)
    }

    fn set_light(&mut self, on: bool) -> Result<()> {
        if on {
            system::backlight_restore(self.sys, self.light_saved);
            self.light_saved.clear();
            let sh = self.shared.borrow();
            let _ = sh.journal.forget(&Entry::LipcInt {
                publisher: "com.lab126.powerd".into(),
                prop: "flIntensity".into(),
                value: 0,
            });
        } else {
            // A crash while asleep must not leave the light off: the launcher re-sets
            // powerd's level, which makes powerd drive the LEDs again.
            if let Ok(v) = self.lipc.get_int("com.lab126.powerd", "flIntensity") {
                let sh = self.shared.borrow();
                let _ = sh.journal.record(Entry::LipcInt {
                    publisher: "com.lab126.powerd".into(),
                    prop: "flIntensity".into(),
                    value: v,
                });
            }
            *self.light_saved = system::backlight_off(self.sys);
        }
        Ok(())
    }

    fn suspend(&mut self, rtc_wake: Option<Duration>) -> Result<Resume> {
        let mut lines = Vec::new();
        let r = system::kernel_suspend(self.sys, rtc_wake, &mut |s| lines.push(s));
        for l in lines {
            log::info!("suspend: {l}");
        }
        r
    }

    fn after_resume(&mut self) -> Result<()> {
        self.touch.regrab()
    }

    fn defer_powerd(&mut self, d: Duration) {
        // Best effort: with lab126_gui stopped powerd refuses (lipcPropErrInvalidState);
        // the deferral is kept by rinky's own controller either way.
        if let Err(e) = self.lipc.set_int("com.lab126.powerd", "deferSuspend", d.as_secs() as i64) {
            log::info!("sleep: powerd deferSuspend {} not set ({e}); deferred by rinky only", d.as_secs());
        }
    }
}

impl KindleInput {
    /// Opens touch, power key and the event sources.
    pub(crate) fn open(shared: Rc<RefCell<Shared>>, lipc: Lipc, sys: SysRoot, opts: LoopOptions) -> Result<Self> {
        let size = crate::Screen::panel_size(&shared.borrow().screen);
        let ov = TouchOverride::parse(&std::env::var("RINKY_TOUCH").unwrap_or_default());
        let touch = TouchDev::open(size.w, size.h, opts.grab_touch, &ov)?;
        let devs = list_devices();
        let power =
            devs.iter().find(|d| d.name.contains("pwrkey")).and_then(|d| File::open(format!("/dev/input/{}", d.event)).ok());
        let now = Instant::now();
        let mut me = KindleInput {
            shared,
            touch,
            power,
            lipc: lipc.clone(),
            sys,
            hub: None,
            uevent: open_uevent_socket(),
            udc: None,
            queue: VecDeque::new(),
            sleep: opts.sleep.clone().map(|c| SleepController::new(c, now)),
            usb: None,
            usb_next_tick: now,
            battery: LowBatteryMonitor::new(opts.low_battery, 3),
            battery_next: now,
            amber: AmberScheduler::default(),
            amber_next: now + AMBER_EVERY,
            amber_base: None,
            ntp_rx: None,
            wifi_state: String::new(),
            heartbeat: 0,
            heartbeat_at: None,
            light_saved: Vec::new(),
            hal_plugged: None,
            app_touch: AppContacts::default(),
            opts,
        };
        if me.opts.lipc_events {
            if let Some((rd, wr)) = make_pipe() {
                let mut hub = EventHub::new(lipc, move || {
                    let _ = (&wr).write_all(&[1]);
                });
                for p in events::PUBLISHERS {
                    hub.subscribe(p, &[]);
                }
                me.hub = Some((hub, rd));
            }
        }
        if let Some((name, _)) = usb::read_udc(&me.sys) {
            if let Ok(mut f) = File::open(me.sys.path(&format!("/sys/class/udc/{name}/state"))) {
                let mut s = String::new();
                let _ = f.read_to_string(&mut s); // sysfs_notify needs one read before poll
                me.udc = Some(f);
            }
        }
        let cable = me.read_cable();
        me.shared.borrow_mut().cable = cable;
        if let Some(policy) = me.opts.usb {
            let transport = detect_transport(&me.lipc);
            let (m, acts) = UsbManager::new(LipcVolumd(me.lipc.clone()), policy, transport, cable);
            me.usb = Some(m);
            me.run_usb_actions(acts);
        }
        if let Some(s) = &me.sleep {
            let mut sh = me.shared.borrow_mut();
            sh.auto_sleep = s.config.auto_sleep;
        }
        log::info!(
            "input: power key {}, lipc events {}, uevents {}, udc {}, cable {cable:?}, sleep {:?}, sleep cover {:?}",
            me.power.is_some(),
            me.hub.is_some(),
            me.uevent.is_some(),
            me.udc.is_some(),
            me.opts.sleep.as_ref().map(|c| (c.auto_sleep, c.suspend)),
            me.opts.sleep.as_ref().map(|c| c.cover)
        );
        // Read only: KOReader's hall toggle writes hall_enable; rinky never does.
        let hall = HallInfo::read(&me.sys);
        if hall.disabled() {
            log::warn!("{}", hall.describe());
        } else {
            log::info!("{}", hall.describe());
        }
        Ok(me)
    }

    /// The touchscreen.
    pub fn touch(&self) -> &TouchDev {
        &self.touch
    }

    /// Status of the lipc subscriptions.
    pub fn hub_status(&mut self) -> Vec<crate::lipc::SubscriptionStatus> {
        self.hub.as_mut().map(|(h, _)| h.status()).unwrap_or_default()
    }

    fn read_cable(&self) -> UsbCable {
        let udc = usb::read_udc(&self.sys).map(|(_, s)| s);
        usb::cable_from(udc.as_deref(), battery::external_power(&self.sys), self.hal_plugged)
    }

    fn plat(&mut self) -> (Option<&mut SleepController>, Plat<'_>) {
        (
            self.sleep.as_mut(),
            Plat {
                shared: &self.shared,
                touch: &mut self.touch,
                sys: &self.sys,
                lipc: &self.lipc,
                light_saved: &mut self.light_saved,
            },
        )
    }

    fn push_power(&mut self, evs: Vec<PowerEvent>) {
        if evs.contains(&PowerEvent::GoingToScreenSaver) {
            self.close_touches("going to sleep");
        }
        for e in evs {
            self.queue.push_back(InputEvent::Power(e));
        }
        if let Some(s) = self.sleep.as_mut() {
            for l in s.take_log() {
                say!("{} sleep: {l}", crate::clock::stamp());
            }
            self.shared.borrow_mut().sleep_state = s.state();
        }
    }

    fn run_usb_actions(&mut self, acts: Vec<UsbAction>) {
        for a in acts {
            match a {
                UsbAction::ShowLockScreen => {
                    self.close_touches("USB lock screen");
                    let mut sh = self.shared.borrow_mut();
                    let size = crate::Screen::panel_size(&sh.screen);
                    sh.locked = true;
                    if let Err(e) = show_overlay(&mut sh, crate::screensaver::usb_lock_screen(size)) {
                        log::warn!("usb lock screen: {e}");
                    }
                    say!("usb: lock screen shown");
                }
                UsbAction::HideLockScreen => {
                    let mut sh = self.shared.borrow_mut();
                    sh.locked = false;
                    if let Err(e) = hide_overlay(&mut sh) {
                        log::warn!("usb lock screen: {e}");
                    }
                    say!("usb: lock screen removed, page restored");
                }
                UsbAction::Emit(e) => {
                    if let PowerEvent::DriveMode(s) = &e {
                        self.shared.borrow_mut().drive_state = *s;
                    }
                    self.queue.push_back(InputEvent::Power(e));
                }
                UsbAction::Cable(b) => self.queue.push_back(InputEvent::Usb(b)),
                UsbAction::Journal(p, k, v) => {
                    let _ = self.shared.borrow().journal.record(Entry::LipcInt { publisher: p, prop: k, value: v });
                }
                UsbAction::Log(s) => say!("{s}"),
            }
        }
    }

    fn cable_check(&mut self, now: Instant) {
        let cable = self.read_cable();
        let old = self.shared.borrow().cable;
        if cable == old {
            return;
        }
        self.shared.borrow_mut().cable = cable;
        match self.usb.as_mut() {
            Some(m) => {
                let acts = m.cable_changed(cable, now);
                self.run_usb_actions(acts);
            }
            None => {
                say!("usb: cable {old:?} -> {cable:?}");
                if old.present() != cable.present() {
                    self.queue.push_back(InputEvent::Usb(cable.present()));
                }
            }
        }
    }

    fn battery_check(&mut self, now: Instant) {
        self.battery_next = now + BATTERY_EVERY;
        if let Some(b) = battery::read_sysfs(&self.sys) {
            if let Some(p) = self.battery.update(b.percent, b.charging) {
                say!("battery: low ({p}%)");
                self.queue.push_back(InputEvent::Power(PowerEvent::LowBattery(p)));
            }
        }
    }

    fn amber_check(&mut self, now: Instant) {
        self.amber_next = now + AMBER_EVERY;
        if !self.opts.amber || self.shared.borrow().asleep {
            return;
        }
        let sched = match super::services::read_amber_schedule(&self.lipc) {
            Ok(s) => s,
            Err(_) => return,
        };
        if !sched.enabled || sched.mode != crate::light::AmberMode::Manual {
            self.amber.reset();
            return;
        }
        let tz = crate::clock::TimeZone::local();
        let t = tz.at(SystemTime::now()).day_time();
        let base = match self.amber_base {
            Some(b) => b,
            None => {
                let cur = self.lipc.get_int("com.lab126.powerd", "currentAmberLevel").unwrap_or(0).max(0) as u32;
                let b = if sched.in_window(t) { 0 } else { cur };
                self.amber_base = Some(b);
                b
            }
        };
        if let Some(w) = self.amber.tick(&sched, t, base) {
            say!("amber: schedule {}..{} level {} at {t}: warmth {w}", sched.start, sched.end, sched.level);
            let _ = self.lipc.set_int("com.lab126.powerd", "currentAmberLevel", w as i64);
        }
    }

    fn start_ntp(&mut self) {
        let Some(server) = self.opts.ntp_server.clone() else { return };
        if self.ntp_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.ntp_rx = Some(rx);
        say!("ntp: Wi-Fi connected, querying {server}");
        std::thread::spawn(move || {
            let _ = tx.send(crate::clock::sntp_query(&server, Duration::from_secs(10)));
        });
    }

    fn ntp_check(&mut self) {
        let Some(rx) = &self.ntp_rx else { return };
        let r = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.ntp_rx = None;
                return;
            }
        };
        self.ntp_rx = None;
        match r {
            Ok(s) => {
                say!("ntp: {} offset {} ms, delay {} ms, stratum {}", s.server, s.offset_ms, s.delay_ms, s.stratum);
                if s.offset_ms.abs() >= 1000 {
                    match system::set_system_time(s.time) {
                        Ok(rtc) => {
                            let delta = s.offset_ms / 1000;
                            say!("ntp: clock set ({delta:+} s); {rtc}");
                            let _ = self.lipc.send_event("com.lab126.time", "timeChanged", &[LipcValue::Int(delta)]);
                            self.queue.push_back(InputEvent::Power(PowerEvent::TimeChanged(delta)));
                        }
                        Err(e) => say!("ntp: setting the clock failed: {e}"),
                    }
                }
            }
            Err(e) => say!("ntp: {e}"),
        }
    }

    fn requests(&mut self, now: Instant) {
        let reqs: Vec<Request> = self.shared.borrow_mut().requests.drain(..).collect();
        for r in reqs {
            match r {
                Request::Sleep => {
                    let (c, mut p) = self.plat();
                    let evs = c.map(|c| c.sleep(crate::sleep::Trigger::App, now, &mut p)).unwrap_or_default();
                    self.push_power(evs);
                }
                Request::Defer(d) => {
                    let (c, mut p) = self.plat();
                    if let Some(c) = c {
                        c.defer_suspend(d, now, &mut p);
                    }
                    self.push_power(Vec::new());
                }
                Request::AutoSleep(a) => {
                    if let Some(c) = self.sleep.as_mut() {
                        c.config.auto_sleep = a;
                        c.activity(now);
                    }
                    self.shared.borrow_mut().auto_sleep = a;
                }
                Request::DriveMode(on) => {
                    if let Some(m) = self.usb.as_mut() {
                        let acts = m.set_drive_mode(on, now);
                        self.run_usb_actions(acts);
                    }
                }
                Request::ChargeOnly(on) => {
                    if let Some(m) = self.usb.as_mut() {
                        let acts = m.set_charge_only(on, now);
                        self.run_usb_actions(acts);
                    }
                }
                Request::AmberChanged => {
                    self.amber.reset();
                    self.amber_next = now;
                }
                Request::Warmth(w) => {
                    self.amber_base = Some(w);
                    self.amber.reset();
                }
            }
        }
    }

    fn heartbeat(&mut self, now: Instant) {
        let Some(path) = &self.opts.heartbeat else { return };
        if self.heartbeat_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
            return;
        }
        self.heartbeat += 1;
        self.heartbeat_at = Some(now);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, format!("{}\n", self.heartbeat)).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Passes a touch frame to the app through [`AppContacts`] (no `Move`/`Up` for a
    /// contact the app does not have down).
    fn deliver_touch(&mut self, frame: TouchFrame) {
        if let Some(f) = self.app_touch.filter(frame) {
            self.queue.push_back(InputEvent::Touch(f));
        }
    }

    /// Sends an `Up` for every contact the app has down (the screensaver or the lock
    /// screen takes over the panel; a lift that comes meanwhile is not delivered).
    fn close_touches(&mut self, why: &str) {
        if let Some(f) = self.app_touch.close_all() {
            let list: Vec<String> = f.contacts.iter().map(|c| format!("slot {} at {},{}", c.slot, c.x, c.y)).collect();
            say!("{} touch: closed {} ({why})", crate::clock::stamp(), list.join("; "));
            self.queue.push_back(InputEvent::Touch(f));
        }
    }

    /// After a resume that woke us: drops power-key presses already queued on the
    /// evdev node. They were made before the wake work finished (the press that woke
    /// the device); delivered now they would put it straight back to sleep.
    fn drain_wake_presses(&mut self) {
        let Some(p) = self.power.as_mut() else { return };
        let mut presses = 0;
        for _ in 0..16 {
            let mut pfd = libc::pollfd { fd: p.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd, no wait.
            if unsafe { libc::poll(&mut pfd, 1, 0) } <= 0 || pfd.revents & libc::POLLIN == 0 {
                break;
            }
            match read_raw(p) {
                Ok(evs) if !evs.is_empty() => {
                    presses += evs.iter().filter(|e| e.ty == EV_KEY && e.code == KEY_POWER && e.value == 1).count();
                }
                _ => break,
            }
        }
        if presses > 0 {
            say!("{} power key: {presses} press(es) from before the wake ignored", crate::clock::stamp());
        }
    }

    fn handle_power_key(&mut self, ev: RawEvent, now: Instant) {
        if ev.value == 2 {
            return; // autorepeat
        }
        let pressed = ev.value == 1;
        if self.sleep.is_some() {
            if pressed {
                say!("power key pressed");
                let (c, mut p) = self.plat();
                let evs = c.map(|c| c.power_key(now, &mut p)).unwrap_or_default();
                self.push_power(evs);
            }
        } else {
            self.queue.push_back(InputEvent::Key(KeyEvent { key: Key::Power, pressed, time: ev.time }));
        }
    }

    fn handle_cover(&mut self, closed: bool, source: CoverSource, now: Instant) {
        let what = if closed { "closed" } else { "opened" };
        log::info!("cover: {what} (source {source})");
        say!("{} cover: {what} (source {source})", crate::clock::stamp());
        if self.sleep.is_none() {
            log::info!("cover: {what} not handled: the device layer does not own sleep");
            return;
        }
        let (c, mut p) = self.plat();
        let evs = c.map(|c| c.cover_from(closed, &source.to_string(), now, &mut p)).unwrap_or_default();
        self.push_power(evs);
    }

    fn handle_lipc(&mut self, now: Instant) {
        let mut evs = Vec::new();
        if let Some((hub, rd)) = self.hub.as_mut() {
            drain_fd(rd.as_raw_fd());
            while let Some(e) = hub.try_next() {
                evs.push(e);
            }
        }
        for e in evs {
            let name = e.event.name.clone();
            if matches!(e.publisher.as_str(), "com.lab126.hal" | "com.lab126.volumd" | "com.lab126.time") {
                say!("{} lipc: {} {}", crate::clock::stamp(), e.publisher, e.event.line());
            } else if e.publisher == "com.lab126.powerd" && !matches!(name.as_str(), "t1TimerReset" | "battLevelChanged") {
                // powerd's screensaver/suspend/cover events at info: they show whether
                // powerd still runs its own flow while the framework is stopped.
                log::info!("lipc event {} {}", e.publisher, e.event.line());
            } else {
                log::debug!("lipc event {} {}", e.publisher, e.event.line());
            }
            if e.publisher == "com.lab126.volumd" || (e.publisher == "com.lab126.hal" && name.starts_with("usb")) {
                if let Some(m) = self.usb.as_mut() {
                    let acts = m.lipc_event(&name, now);
                    self.run_usb_actions(acts);
                }
            }
            let cover = events::cover_from_published(&e);
            if let Some((closed, source)) = cover {
                self.handle_cover(closed, source, now);
            }
            for ie in events::map_published(&e) {
                match &ie {
                    InputEvent::Usb(b) => {
                        self.hal_plugged = Some(*b);
                        self.cable_check(now);
                        continue; // cable_check delivers Usb events itself
                    }
                    InputEvent::Power(PowerEvent::GoingToScreenSaver | PowerEvent::OutOfScreenSaver)
                        if cover.is_some() && self.sleep.is_some() =>
                    {
                        // powerd's cover screensaver event: the controller above sent its
                        // own GoingToScreenSaver/OutOfScreenSaver (or none if ignored).
                        continue;
                    }
                    InputEvent::Power(PowerEvent::Charging(_)) | InputEvent::Power(PowerEvent::BatteryLevel(_)) => {
                        self.battery_check(now);
                        self.cable_check(now);
                    }
                    InputEvent::Power(PowerEvent::WifiState(s)) => {
                        let was = std::mem::replace(&mut self.wifi_state, s.clone());
                        if s == "CONNECTED" && was != "CONNECTED" {
                            self.start_ntp();
                        }
                    }
                    InputEvent::Power(PowerEvent::Lipc { publisher, name, .. })
                        if publisher == "com.lab126.hal" && name == "powerButtonPressed" && self.power.is_none() =>
                    {
                        // No evdev power key: use the udev notification instead.
                        let (c, mut p) = self.plat();
                        let evs = c.map(|c| c.power_key(now, &mut p)).unwrap_or_default();
                        self.push_power(evs);
                    }
                    _ => {}
                }
                self.queue.push_back(ie);
            }
        }
    }

    fn handle_uevents(&mut self, now: Instant) {
        let Some(fd) = self.uevent.as_ref().map(|f| f.as_raw_fd()) else { return };
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: non-blocking recv into a local buffer.
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
            if n <= 0 {
                break;
            }
            let Some(u) = Uevent::parse(&buf[..n as usize]) else { continue };
            match events::classify(&u) {
                Some(UeventSignal::Cover(closed)) => {
                    log::info!("uevent: hall sensor {} ({})", if closed { "closed" } else { "opened" }, u.devpath);
                    self.handle_cover(closed, CoverSource::Uevent, now);
                    self.queue.push_back(InputEvent::Power(PowerEvent::Cover(closed)));
                }
                Some(UeventSignal::PowerSupply(name)) => {
                    log::debug!("uevent: power supply {name}");
                    self.cable_check(now);
                    self.battery_check(now);
                }
                Some(UeventSignal::Usb) => self.cable_check(now),
                Some(UeventSignal::PowerButton) | None => {}
            }
        }
    }

    fn timers(&mut self, now: Instant) {
        if let Some((hub, _)) = self.hub.as_mut() {
            hub.maintain();
        }
        if self.sleep.is_some() {
            let (c, mut p) = self.plat();
            let evs = c.map(|c| c.tick(now, &mut p)).unwrap_or_default();
            if evs.contains(&PowerEvent::Resuming) {
                // Time passed in suspend: re-read everything that may have changed.
                self.battery_next = Instant::now();
                self.hal_plugged = None;
                if evs.contains(&PowerEvent::OutOfScreenSaver) {
                    self.drain_wake_presses();
                }
            }
            self.push_power(evs);
        }
        if self.usb.as_ref().is_some_and(|m| m.busy()) && now >= self.usb_next_tick {
            self.usb_next_tick = now + Duration::from_secs(1);
            let acts = self.usb.as_mut().map(|m| m.tick(now)).unwrap_or_default();
            self.run_usb_actions(acts);
        }
        if now >= self.battery_next {
            self.battery_check(now);
        }
        if now >= self.amber_next {
            self.amber_check(now);
        }
        self.ntp_check();
    }

    fn next_deadline(&self, caller: Option<Instant>, now: Instant) -> Instant {
        let mut d = now + MAX_POLL;
        if let Some(c) = caller {
            d = d.min(c);
        }
        if let Some(t) = self.sleep.as_ref().and_then(|s| s.deadline()) {
            d = d.min(t);
        }
        if self.usb.as_ref().is_some_and(|m| m.busy()) {
            d = d.min(self.usb_next_tick);
        }
        if self.ntp_rx.is_some() {
            d = d.min(now + Duration::from_millis(250));
        }
        d.min(self.battery_next).min(self.amber_next).max(now)
    }
}

impl Input for KindleInput {
    fn coalesce_moves(&self) -> bool {
        true
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<InputEvent>> {
        let caller = timeout.map(|t| Instant::now() + t);
        let mut polled = false;
        // SIGTERM/SIGINT/SIGHUP (the signal thread, `trace::request_stop`) write to this
        // pipe: the poll below wakes and the app loop closes its screens.
        let stop_fd = crate::trace::stop_wake_fd();
        loop {
            if crate::trace::stop_requested().is_some() {
                return Ok(Some(InputEvent::Quit));
            }
            let now = Instant::now();
            self.requests(now);
            self.timers(now);
            self.heartbeat(now);
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            let wait = match crate::input::wait_step(caller, self.next_deadline(caller, now), now, polled) {
                crate::input::WaitStep::Return => return Ok(None),
                crate::input::WaitStep::Poll(d) => d,
            };
            polled = true;
            let mut fds = vec![libc::pollfd { fd: self.touch.file.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            let mut add = |fd: i32, events: i16| {
                fds.push(libc::pollfd { fd, events, revents: 0 });
                fds.len() - 1
            };
            let power_idx = self.power.as_ref().map(|p| add(p.as_raw_fd(), libc::POLLIN));
            let lipc_idx = self.hub.as_ref().map(|(_, rd)| add(rd.as_raw_fd(), libc::POLLIN));
            let uevent_idx = self.uevent.as_ref().map(|f| add(f.as_raw_fd(), libc::POLLIN));
            let udc_idx = self.udc.as_ref().map(|f| add(f.as_raw_fd(), libc::POLLPRI | libc::POLLERR));
            if let Some(fd) = stop_fd {
                add(fd, libc::POLLIN);
            }
            let ms = wait.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: fds is a valid array of pollfd.
            let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, ms) };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            }
            let now = Instant::now();
            if r == 0 {
                self.cable_check(now);
                continue;
            }
            if let Some(i) = power_idx {
                if fds[i].revents & libc::POLLIN != 0 {
                    let evs = match self.power.as_mut() {
                        Some(p) => read_raw(p)?,
                        None => Vec::new(),
                    };
                    for ev in evs {
                        if ev.ty == EV_KEY && ev.code == KEY_POWER {
                            self.handle_power_key(ev, now);
                        }
                    }
                }
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                log::warn!("touch device error; reopening");
                let _ = self.touch.regrab();
            } else if fds[0].revents & libc::POLLIN != 0 {
                let raw = match read_raw(&mut self.touch.file) {
                    Ok(r) => r,
                    Err(e) if e.raw_os_error() == Some(libc::ENODEV) => {
                        let _ = self.touch.regrab();
                        Vec::new()
                    }
                    Err(e) => return Err(e.into()),
                };
                let (asleep, locked) = {
                    let sh = self.shared.borrow();
                    (sh.asleep, sh.locked)
                };
                for ev in raw {
                    if let Some(frame) = self.touch.decoder.feed(ev) {
                        if asleep || locked {
                            continue; // the screensaver / lock screen takes no touches
                        }
                        if let Some(s) = self.sleep.as_mut() {
                            s.activity(now);
                        }
                        self.deliver_touch(frame);
                    }
                    if self.touch.decoder.take_resync() {
                        log::warn!("touch: SYN_DROPPED; re-seeding from the kernel");
                        self.touch.seed();
                        if let Some(f) = self.touch.decoder.flush_seeded(ev.time) {
                            if !(asleep || locked) {
                                self.deliver_touch(f);
                            }
                        }
                    }
                }
            }
            if lipc_idx.is_some_and(|i| fds[i].revents & libc::POLLIN != 0) {
                self.handle_lipc(now);
            }
            if uevent_idx.is_some_and(|i| fds[i].revents & libc::POLLIN != 0) {
                self.handle_uevents(now);
            }
            if let Some(i) = udc_idx {
                if fds[i].revents & (libc::POLLPRI | libc::POLLERR) != 0 {
                    if let Some(f) = self.udc.as_mut() {
                        let _ = f.rewind();
                        let mut s = String::new();
                        let _ = f.read_to_string(&mut s);
                        log::debug!("udc state {}", s.trim());
                    }
                    self.cable_check(now);
                }
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        // Drop queued touches only; keep power and system events.
        self.queue.retain(|e| !matches!(e, InputEvent::Touch(_)));
        Ok(())
    }
}

impl Drop for KindleInput {
    fn drop(&mut self) {
        if let Some(m) = self.usb.as_mut() {
            for a in m.release() {
                if let UsbAction::Log(s) = a {
                    say!("{s}");
                }
            }
            let _ = self.shared.borrow().journal.forget(&Entry::LipcInt {
                publisher: "com.lab126.volumd".into(),
                prop: "disableMounting".into(),
                value: 0,
            });
        }
        if !self.light_saved.is_empty() {
            system::backlight_restore(&self.sys, &self.light_saved);
        }
        if let Some(s) = &self.sleep {
            if s.state() == SleepState::Asleep {
                let mut sh = self.shared.borrow_mut();
                sh.asleep = false;
                let _ = hide_overlay(&mut sh);
            }
        }
    }
}

impl MtkScreen {
    /// The panel orientation size (for callers without the trait in scope).
    pub fn size_px(&self) -> (u32, u32) {
        let s = crate::Screen::panel_size(self);
        (s.w, s.h)
    }
}
