//! Date and time: an SNTP client (RFC 4330) over UDP, civil date conversion, and local
//! time from a TZif file (`/etc/localtime`) or a POSIX `TZ` string.
//!
//! What the 5.19.5 guest provides itself (`/usr/sbin/updatetime`, run by lipc-daemon on
//! `com.lab126.cmd triggerTimeChange`): `sntp -S -t 10 $NTP_SERVER` with
//! `NTP_SERVER=ntp-g7g.amazon.com` from `/etc/sysconfig/ntp`, then `hwclock -u -w` and
//! `lipc-send-event com.lab126.time timeChanged -i DELTA`; an HTTP `Date:` header from
//! `kindle-time.amazon.com` as fallback. `/etc/localtime` links to
//! `/var/local/system/tz`, which does not exist on a fresh emulator image (UTC).
//! rinky queries the server itself ([`sntp_query`]) so the server is configurable
//! (tests use a responder on the host, reached from the guest at 10.0.2.2).

use crate::{DeviceError, Result};
use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds between 1900-01-01 (NTP era 0) and 1970-01-01.
pub const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// Result of one SNTP exchange.
#[derive(Clone, Debug, PartialEq)]
pub struct NtpSample {
    /// Server address used.
    pub server: String,
    /// Clock offset: add this to the local clock (milliseconds).
    pub offset_ms: i64,
    /// Round-trip delay (milliseconds).
    pub delay_ms: i64,
    /// Server stratum.
    pub stratum: u8,
    /// Corrected time (local time at reception + offset).
    pub time: SystemTime,
}

fn to_ntp(t: SystemTime) -> u64 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() + NTP_UNIX_OFFSET;
    let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

fn ntp_to_millis(ts: u64) -> i128 {
    let secs = (ts >> 32) as i128 - NTP_UNIX_OFFSET as i128;
    let frac = (ts & 0xffff_ffff) as i128;
    secs * 1000 + frac * 1000 / (1i128 << 32)
}

fn millis(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i128,
        Err(e) => -(e.duration().as_millis() as i128),
    }
}

/// Builds a client request carrying `transmit` as its transmit timestamp.
pub fn sntp_request(transmit: SystemTime) -> [u8; 48] {
    let mut p = [0u8; 48];
    p[0] = 0x23; // LI 0, version 4, mode 3 (client)
    p[40..48].copy_from_slice(&to_ntp(transmit).to_be_bytes());
    p
}

/// Checks a server reply to `request` received at `t4` and computes the offset.
pub fn sntp_parse(request: &[u8; 48], reply: &[u8], t1: SystemTime, t4: SystemTime) -> Result<(i64, i64, u8)> {
    if reply.len() < 48 {
        return Err(DeviceError::Backend(format!("sntp: short reply ({} bytes)", reply.len())));
    }
    let li = reply[0] >> 6;
    let mode = reply[0] & 7;
    let stratum = reply[1];
    if mode != 4 && mode != 5 {
        return Err(DeviceError::Backend(format!("sntp: reply mode {mode}")));
    }
    if li == 3 || stratum == 0 || stratum > 15 {
        return Err(DeviceError::Backend(format!("sntp: server unsynchronised (LI {li}, stratum {stratum})")));
    }
    if reply[24..32] != request[40..48] {
        return Err(DeviceError::Backend("sntp: reply does not match the request".into()));
    }
    let rd = |i: usize| u64::from_be_bytes(reply[i..i + 8].try_into().unwrap_or([0; 8]));
    let (t2, t3) = (ntp_to_millis(rd(32)), ntp_to_millis(rd(40)));
    if rd(40) == 0 {
        return Err(DeviceError::Backend("sntp: zero transmit time".into()));
    }
    let (t1, t4) = (millis(t1), millis(t4));
    let offset = ((t2 - t1) + (t3 - t4)) / 2;
    let delay = (t4 - t1) - (t3 - t2);
    Ok((offset as i64, delay.max(0) as i64, stratum))
}

/// Queries `server` (`host` or `host:port`, default port 123) once.
pub fn sntp_query(server: &str, timeout: Duration) -> Result<NtpSample> {
    let target = if server.contains(':') { server.to_string() } else { format!("{server}:123") };
    let addr = target
        .to_socket_addrs()
        .map_err(|e| DeviceError::Backend(format!("sntp: {target}: {e}")))?
        .next()
        .ok_or_else(|| DeviceError::Backend(format!("sntp: {target}: no address")))?;
    let sock = UdpSocket::bind(if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    sock.set_read_timeout(Some(timeout))?;
    sock.connect(addr)?;
    let t1 = SystemTime::now();
    let req = sntp_request(t1);
    sock.send(&req)?;
    let mut buf = [0u8; 128];
    let n = sock.recv(&mut buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
            DeviceError::Timeout(format!("sntp {target} after {timeout:?}"))
        }
        _ => DeviceError::Io(e),
    })?;
    let t4 = SystemTime::now();
    let (offset_ms, delay_ms, stratum) = sntp_parse(&req, &buf[..n], t1, t4)?;
    let time = if offset_ms >= 0 {
        t4 + Duration::from_millis(offset_ms as u64)
    } else {
        t4 - Duration::from_millis(offset_ms.unsigned_abs())
    };
    Ok(NtpSample { server: addr.to_string(), offset_ms, delay_ms, stratum, time })
}

/// Builds a server reply for `request` (for test responders): receive and transmit
/// time `now`, stratum 2.
pub fn sntp_reply(request: &[u8], now: SystemTime) -> [u8; 48] {
    let mut p = [0u8; 48];
    p[0] = 0x24; // LI 0, version 4, mode 4 (server)
    p[1] = 2;
    p[2] = 6;
    p[3] = 0xEC;
    p[12..16].copy_from_slice(b"INKT");
    let ts = to_ntp(now).to_be_bytes();
    p[16..24].copy_from_slice(&ts);
    if request.len() >= 48 {
        p[24..32].copy_from_slice(&request[40..48]);
    }
    p[32..40].copy_from_slice(&ts);
    p[40..48].copy_from_slice(&ts);
    p
}

/// `NTP_SERVER=` from a shell config such as `/etc/sysconfig/ntp`.
pub fn ntp_server_from_config(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("NTP_SERVER="))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|v| !v.is_empty())
}

/// A civil date and time with its UTC offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    /// Year.
    pub year: i32,
    /// 1..=12.
    pub month: u8,
    /// 1..=31.
    pub day: u8,
    /// 0..=23.
    pub hour: u8,
    /// 0..=59.
    pub minute: u8,
    /// 0..=59.
    pub second: u8,
    /// 0 = Sunday.
    pub weekday: u8,
    /// Seconds east of UTC.
    pub offset_s: i32,
}

/// Days since 1970-01-01 to (year, month, day) (H. Hinnant's algorithm).
pub fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// (year, month, day) to days since 1970-01-01.
pub fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The kernel's `struct rtc_time` for `unix` seconds in UTC (the Kindle RTC keeps UTC):
/// `[tm_sec, tm_min, tm_hour, tm_mday, tm_mon (0-11), tm_year (- 1900), tm_wday,
/// tm_yday, tm_isdst = 0]`, as `RTC_SET_TIME` takes it.
pub fn rtc_time_fields(unix: i64) -> [i32; 9] {
    let d = DateTime::from_unix(unix, 0);
    let yday = unix.div_euclid(86_400) - days_from_civil(d.year, 1, 1);
    [
        d.second as i32,
        d.minute as i32,
        d.hour as i32,
        d.day as i32,
        d.month as i32 - 1,
        d.year - 1900,
        d.weekday as i32,
        yday as i32,
        0,
    ]
}

impl DateTime {
    /// The civil time of `unix` seconds at `offset_s` east of UTC.
    pub fn from_unix(unix: i64, offset_s: i32) -> DateTime {
        let local = unix + offset_s as i64;
        let days = local.div_euclid(86_400);
        let secs = local.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        DateTime {
            year,
            month,
            day,
            hour: (secs / 3600) as u8,
            minute: (secs / 60 % 60) as u8,
            second: (secs % 60) as u8,
            weekday: (days + 4).rem_euclid(7) as u8,
            offset_s,
        }
    }

    /// Unix seconds.
    pub fn to_unix(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
            - self.offset_s as i64
    }

    /// Minutes after local midnight (for schedules).
    pub fn day_time(&self) -> crate::light::DayTime {
        crate::light::DayTime::hm(self.hour as u16, self.minute as u16)
    }

    /// `2026-10-24 18:43:09 +0200`.
    pub fn format(&self) -> String {
        let (sign, off) = if self.offset_s < 0 { ('-', -self.offset_s) } else { ('+', self.offset_s) };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {sign}{:02}{:02}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            off / 3600,
            off / 60 % 60
        )
    }

    /// `18:43` (24 h) or `6:43 PM`.
    pub fn clock(&self, twelve_hour: bool) -> String {
        if twelve_hour {
            let h = match self.hour % 12 {
                0 => 12,
                h => h,
            };
            format!("{h}:{:02} {}", self.minute, if self.hour < 12 { "AM" } else { "PM" })
        } else {
            format!("{:02}:{:02}", self.hour, self.minute)
        }
    }
}

/// A DST rule `Mm.w.d/time` (month, week 1..5 where 5 = last, weekday 0 = Sunday,
/// seconds after local midnight).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DstRule {
    month: u8,
    week: u8,
    weekday: u8,
    time_s: i32,
}

impl DstRule {
    /// Local (wall clock of the zone in effect before the change) unix seconds of the
    /// transition in `year`, as if that wall time were UTC.
    fn local_at(&self, year: i32) -> i64 {
        let first = days_from_civil(year, self.month, 1);
        let wd_first = (first + 4).rem_euclid(7);
        let mut day = first + (self.weekday as i64 - wd_first).rem_euclid(7) + (self.week as i64 - 1) * 7;
        if self.week == 5 {
            let next_month = if self.month == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, self.month + 1, 1) };
            while day >= next_month {
                day -= 7;
            }
        }
        day * 86_400 + self.time_s as i64
    }
}

/// A POSIX `TZ` value (`CET-1CEST,M3.5.0,M10.5.0/3`, `UTC0`, `EST5EDT,M3.2.0,M11.1.0`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PosixTz {
    /// Standard-time name.
    pub std_name: String,
    /// Standard offset, seconds east of UTC.
    pub std_offset: i32,
    /// Daylight name, offset and start/end rules.
    pub dst: Option<(String, i32, DstRule, DstRule)>,
}

fn parse_tz_name(s: &str) -> Option<(String, &str)> {
    if let Some(rest) = s.strip_prefix('<') {
        let end = rest.find('>')?;
        return Some((rest[..end].to_string(), &rest[end + 1..]));
    }
    let n = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
    (n >= 3).then(|| (s[..n].to_string(), &s[n..]))
}

/// Parses `[+-]hh[:mm[:ss]]` to seconds; returns the rest. Hours up to 167 (POSIX
/// allows that much for rule times), minutes and seconds up to 59; larger values (a
/// hostile `TZ`) are rejected instead of overflowing.
fn parse_hms(s: &str) -> Option<(i32, &str)> {
    let (sign, s) = match s.as_bytes().first()? {
        b'-' => (-1, &s[1..]),
        b'+' => (1, &s[1..]),
        _ => (1, s),
    };
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(s.len());
    let mut total = 0i32;
    for (i, part) in s[..end].split(':').enumerate() {
        let (limit, unit) = *[(167u32, 3600i32), (59, 60), (59, 1)].get(i)?;
        let v: u32 = part.parse().ok()?;
        if v > limit {
            return None;
        }
        total += v as i32 * unit;
    }
    (end > 0).then_some((sign * total, &s[end..]))
}

fn parse_rule(s: &str) -> Option<(DstRule, &str)> {
    let s = s.strip_prefix('M')?;
    let end = s.find([',', '/']).unwrap_or(s.len());
    let mut it = s[..end].split('.');
    let (month, week, weekday) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    let mut rest = &s[end..];
    let mut time_s = 7200;
    if let Some(r) = rest.strip_prefix('/') {
        let (t, r2) = parse_hms(r)?;
        time_s = t;
        rest = r2;
    }
    ((1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6)
        .then_some((DstRule { month, week, weekday, time_s }, rest))
}

impl PosixTz {
    /// UTC.
    pub fn utc() -> Self {
        PosixTz { std_name: "UTC".into(), std_offset: 0, dst: None }
    }

    /// Parses a POSIX TZ string; `None` for forms it does not support (`Jn` rules).
    pub fn parse(s: &str) -> Option<PosixTz> {
        let s = s.trim().trim_start_matches(':');
        let (std_name, rest) = parse_tz_name(s)?;
        let (off, rest) = parse_hms(rest)?;
        let std_offset = -off;
        if rest.is_empty() {
            return Some(PosixTz { std_name, std_offset, dst: None });
        }
        let (dst_name, mut rest) = parse_tz_name(rest)?;
        let mut dst_offset = std_offset + 3600;
        if !rest.starts_with(',') && !rest.is_empty() {
            let (o, r) = parse_hms(rest)?;
            dst_offset = -o;
            rest = r;
        }
        let rest = rest.strip_prefix(',')?;
        let (start, rest) = parse_rule(rest)?;
        let (end, rest) = parse_rule(rest.strip_prefix(',')?)?;
        rest.is_empty().then_some(PosixTz { std_name, std_offset, dst: Some((dst_name, dst_offset, start, end)) })
    }

    /// Offset east of UTC at `unix` seconds.
    pub fn offset_at(&self, unix: i64) -> i32 {
        let Some((_, dst_off, start, end)) = &self.dst else { return self.std_offset };
        let year = DateTime::from_unix(unix, self.std_offset).year;
        // Transitions in UTC: start is given in standard time, end in daylight time.
        let s = start.local_at(year) - self.std_offset as i64;
        let e = end.local_at(year) - *dst_off as i64;
        let in_dst = if s < e { unix >= s && unix < e } else { unix >= s || unix < e };
        if in_dst {
            *dst_off
        } else {
            self.std_offset
        }
    }
}

/// A timezone: TZif transitions plus the POSIX footer for later times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeZone {
    transitions: Vec<(i64, i32)>,
    initial: i32,
    footer: Option<PosixTz>,
}

impl Default for TimeZone {
    fn default() -> Self {
        TimeZone::utc()
    }
}

impl TimeZone {
    /// UTC.
    pub fn utc() -> Self {
        TimeZone { transitions: Vec::new(), initial: 0, footer: Some(PosixTz::utc()) }
    }

    /// From a POSIX TZ string.
    pub fn from_posix(s: &str) -> Option<Self> {
        Some(TimeZone { transitions: Vec::new(), initial: 0, footer: Some(PosixTz::parse(s)?) })
    }

    /// Parses TZif data (versions 1-3; the 64-bit block and footer when present).
    /// Truncated or corrupted data (a header whose counts do not fit the data) gives
    /// `None`, never a panic or a large allocation.
    pub fn from_tzif(data: &[u8]) -> Option<Self> {
        fn be32(b: &[u8], i: usize) -> Option<i32> {
            Some(i32::from_be_bytes(b.get(i..i.checked_add(4)?)?.try_into().ok()?))
        }
        fn be64(b: &[u8], i: usize) -> Option<i64> {
            Some(i64::from_be_bytes(b.get(i..i.checked_add(8)?)?.try_into().ok()?))
        }
        /// Length of one data block (after its 44-byte header) with `tsize`-byte times;
        /// computed in u64 so a hostile header cannot overflow `usize` on 32-bit.
        fn block_len(c: &[u64; 6], tsize: u64) -> u64 {
            let [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt] = *c;
            timecnt * (tsize + 1) + typecnt * 6 + charcnt + leapcnt * (tsize + 4) + isstdcnt + isutcnt
        }
        if data.get(0..4)? != b"TZif" {
            return None;
        }
        let version = *data.get(4)?;
        let total = data.len() as u64;
        let header = |base: usize| -> Option<[u64; 6]> {
            let mut c = [0u64; 6];
            for (i, v) in c.iter_mut().enumerate() {
                *v = be32(data, base.checked_add(20 + 4 * i)?)? as u32 as u64;
            }
            Some(c) // isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
        };
        let c1 = header(0)?;
        // Every count is at most 2^32, so these sums fit in u64; the block must fit the data.
        let v1_end = 44 + block_len(&c1, 4);
        if v1_end > total {
            return None;
        }
        let (base, tsize, c) = if version >= b'2' {
            let c2 = header(v1_end as usize)?;
            if v1_end + 44 + block_len(&c2, 8) > total {
                return None;
            }
            (v1_end as usize, 8usize, c2)
        } else {
            (0, 4usize, c1)
        };
        // All counts are bounded by the data length now.
        let [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt] = c.map(|v| v as usize);
        let mut p = base + 44;
        let mut times = Vec::with_capacity(timecnt);
        for i in 0..timecnt {
            times.push(if tsize == 8 { be64(data, p + 8 * i)? } else { be32(data, p + 4 * i)? as i64 });
        }
        p += timecnt * tsize;
        let idx: Vec<u8> = data.get(p..p + timecnt)?.to_vec();
        p += timecnt;
        let mut types = Vec::with_capacity(typecnt);
        for i in 0..typecnt {
            types.push((be32(data, p + 6 * i)?, *data.get(p + 6 * i + 4)? != 0));
        }
        p += typecnt * 6 + charcnt + leapcnt * (tsize + 4) + isstdcnt + isutcnt;
        let transitions = times
            .iter()
            .zip(idx.iter())
            .filter_map(|(t, i)| types.get(*i as usize).map(|(off, _)| (*t, *off)))
            .collect();
        let initial = types.iter().find(|(_, dst)| !dst).or(types.first()).map(|(o, _)| *o).unwrap_or(0);
        let footer = if version >= b'2' {
            let rest = data.get(p..)?;
            let text = String::from_utf8_lossy(rest);
            text.trim_matches('\n').lines().next().and_then(PosixTz::parse)
        } else {
            None
        };
        Some(TimeZone { transitions, initial, footer })
    }

    /// Reads `/etc/localtime` (TZif) or uses `$TZ`; UTC when neither works.
    pub fn local() -> Self {
        if let Ok(tz) = std::env::var("TZ") {
            if let Some(z) = TimeZone::from_posix(&tz) {
                return z;
            }
            if let Ok(d) = std::fs::read(format!("/usr/share/zoneinfo/{}", tz.trim_start_matches(':'))) {
                if let Some(z) = TimeZone::from_tzif(&d) {
                    return z;
                }
            }
        }
        std::fs::read("/etc/localtime").ok().and_then(|d| TimeZone::from_tzif(&d)).unwrap_or_default()
    }

    /// Offset east of UTC at `unix` seconds.
    pub fn offset_at(&self, unix: i64) -> i32 {
        match self.transitions.iter().rposition(|(t, _)| *t <= unix) {
            Some(i) if i + 1 < self.transitions.len() || self.footer.is_none() => self.transitions[i].1,
            Some(i) => self.footer.as_ref().map(|f| f.offset_at(unix)).unwrap_or(self.transitions[i].1),
            None if self.transitions.is_empty() => self.footer.as_ref().map(|f| f.offset_at(unix)).unwrap_or(self.initial),
            None => self.initial,
        }
    }

    /// Local civil time at `t`.
    pub fn at(&self, t: SystemTime) -> DateTime {
        let unix = millis(t).div_euclid(1000) as i64;
        DateTime::from_unix(unix, self.offset_at(unix))
    }
}

/// Unix seconds of `t`.
pub fn unix_secs(t: SystemTime) -> i64 {
    millis(t).div_euclid(1000) as i64
}

/// `HH:MM:SS.mmm` UTC wall-clock stamp for log lines.
pub fn stamp() -> String {
    let ms = millis(SystemTime::now());
    let d = DateTime::from_unix(ms.div_euclid(1000) as i64, 0);
    format!("{:02}:{:02}:{:02}.{:03}", d.hour, d.minute, d.second, ms.rem_euclid(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_round_trips() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for d in [-1000i64, -1, 0, 59, 365, 11_016, 20_000, 20_750, 50_000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
        // 2026-10-24 18:43:09 UTC was a Saturday.
        let t = DateTime { year: 2026, month: 10, day: 24, hour: 18, minute: 43, second: 9, weekday: 6, offset_s: 0 };
        let u = t.to_unix();
        assert_eq!(DateTime::from_unix(u, 0), t);
        let cest = DateTime::from_unix(u, 7200);
        assert_eq!((cest.hour, cest.minute, cest.offset_s), (20, 43, 7200));
        assert_eq!(cest.format(), "2026-10-24 20:43:09 +0200");
        assert_eq!(cest.clock(false), "20:43");
        assert_eq!(cest.clock(true), "8:43 PM");
        assert_eq!(DateTime::from_unix(u, -18000).format(), "2026-10-24 13:43:09 -0500");
        assert_eq!(cest.day_time(), crate::light::DayTime::hm(20, 43));
    }

    #[test]
    fn rtc_time_for_rtc_set_time() {
        // Reference values from Python's time.gmtime (wday Sunday = 0, yday from 0).
        assert_eq!(rtc_time_fields(0), [0, 0, 0, 1, 0, 70, 4, 0, 0]);
        assert_eq!(rtc_time_fields(1_792_886_620), [40, 3, 0, 25, 9, 126, 0, 297, 0]);
        assert_eq!(rtc_time_fields(951_868_799), [59, 59, 23, 29, 1, 100, 2, 59, 0], "2000-02-29 23:59:59");
    }

    #[test]
    fn posix_tz_rules() {
        let cet = PosixTz::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        assert_eq!(cet.std_offset, 3600);
        // 2026-10-24 12:00 UTC: still summer time (ends 2026-10-25 01:00 UTC).
        let oct24 = DateTime { year: 2026, month: 10, day: 24, hour: 12, minute: 0, second: 0, weekday: 0, offset_s: 0 };
        assert_eq!(cet.offset_at(oct24.to_unix()), 7200);
        let after = DateTime { day: 25, hour: 1, ..oct24 }.to_unix();
        assert_eq!(cet.offset_at(after - 1), 7200);
        assert_eq!(cet.offset_at(after), 3600);
        // Starts 2026-03-29 01:00 UTC.
        let start = DateTime { month: 3, day: 29, hour: 1, ..oct24 }.to_unix();
        assert_eq!(cet.offset_at(start - 1), 3600);
        assert_eq!(cet.offset_at(start), 7200);
        let ny = PosixTz::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(ny.offset_at(oct24.to_unix()), -4 * 3600);
        let jan = DateTime { month: 1, day: 15, ..oct24 }.to_unix();
        assert_eq!(ny.offset_at(jan), -5 * 3600);
        // Southern hemisphere: DST across the new year.
        let syd = PosixTz::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(syd.offset_at(jan), 11 * 3600);
        assert_eq!(PosixTz::parse("UTC0").unwrap().offset_at(jan), 0);
        assert_eq!(PosixTz::parse("<+0530>-5:30").unwrap().std_offset, 5 * 3600 + 1800);
        assert!(PosixTz::parse("XYZ").is_none());
        assert!(PosixTz::parse("CET-1CEST,J60,J300").is_none());
    }

    fn tzif_v2(transitions: &[(i64, u8)], types: &[(i32, bool)], footer: &str) -> Vec<u8> {
        let mut v = Vec::new();
        // Minimal v1 block (no data), then the v2 block.
        let header = |v: &mut Vec<u8>, timecnt: usize, typecnt: usize, charcnt: usize| {
            v.extend_from_slice(b"TZif2");
            v.extend_from_slice(&[0; 15]);
            for c in [0, 0, 0, timecnt, typecnt, charcnt] {
                v.extend_from_slice(&(c as i32).to_be_bytes());
            }
        };
        header(&mut v, 0, 1, 1);
        v.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
        header(&mut v, transitions.len(), types.len(), 1);
        for (t, _) in transitions {
            v.extend_from_slice(&t.to_be_bytes());
        }
        for (_, i) in transitions {
            v.push(*i);
        }
        for (off, dst) in types {
            v.extend_from_slice(&off.to_be_bytes());
            v.push(*dst as u8);
            v.push(0);
        }
        v.push(0);
        v.extend_from_slice(format!("\n{footer}\n").as_bytes());
        v
    }

    #[test]
    fn tzif_transitions_and_footer() {
        let data = tzif_v2(&[(1_000_000, 1), (2_000_000, 0)], &[(3600, false), (7200, true)], "CET-1CEST,M3.5.0,M10.5.0/3");
        let z = TimeZone::from_tzif(&data).unwrap();
        assert_eq!(z.offset_at(0), 3600);
        assert_eq!(z.offset_at(1_500_000), 7200);
        // After the last transition the footer rule decides (October 2026: summer time).
        let oct = DateTime { year: 2026, month: 10, day: 24, hour: 12, minute: 0, second: 0, weekday: 0, offset_s: 0 };
        assert_eq!(z.offset_at(oct.to_unix()), 7200);
        assert!(TimeZone::from_tzif(b"nope").is_none());
        assert_eq!(TimeZone::utc().offset_at(oct.to_unix()), 0);
        let local = TimeZone::from_posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let t = UNIX_EPOCH + Duration::from_secs(oct.to_unix() as u64);
        assert_eq!(local.at(t).hour, 8);
    }

    #[test]
    fn truncated_and_hostile_tzif_is_rejected() {
        let data = tzif_v2(&[(1_000_000, 1), (2_000_000, 0)], &[(3600, false), (7200, true)], "CET-1CEST,M3.5.0,M10.5.0/3");
        // Every truncation inside the binary blocks is rejected (the footer is text).
        let footer_at = data.len() - "\nCET-1CEST,M3.5.0,M10.5.0/3\n".len();
        for n in 0..footer_at {
            assert!(TimeZone::from_tzif(&data[..n]).is_none(), "accepted {n} of {} bytes", data.len());
        }
        // Counts that claim far more data than there is: no allocation, no overflow.
        for (field, value) in [(3usize, 0x7fff_ffffu32), (3, u32::MAX), (4, u32::MAX), (2, u32::MAX), (5, 0x8000_0000)] {
            let mut z = data.clone();
            z[20 + 4 * field..24 + 4 * field].copy_from_slice(&value.to_be_bytes());
            assert!(TimeZone::from_tzif(&z).is_none(), "field {field} = {value:#x}");
        }
        // The same in the v2 header.
        let v2 = 44 + 7;
        let mut z = data.clone();
        z[v2 + 32..v2 + 36].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(TimeZone::from_tzif(&z).is_none());
        assert!(PosixTz::parse("UTC168").is_none() && PosixTz::parse("UTC167").is_some());
        assert!(PosixTz::parse("UTC1:60").is_none() && PosixTz::parse("UTC1:59:59").is_some());
    }

    #[test]
    fn sntp_against_a_local_responder() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        // The responder's clock is 90 s ahead.
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            let reply = sntp_reply(&buf[..n], SystemTime::now() + Duration::from_secs(90));
            server.send_to(&reply, from).unwrap();
        });
        let s = sntp_query(&addr.to_string(), Duration::from_secs(2)).unwrap();
        h.join().unwrap();
        assert!((s.offset_ms - 90_000).abs() < 1000, "offset {}", s.offset_ms);
        assert!(s.delay_ms < 1000);
        assert_eq!(s.stratum, 2);
        let drift = millis(s.time) - millis(SystemTime::now() + Duration::from_secs(90));
        assert!(drift.abs() < 1000);
    }

    #[test]
    fn sntp_rejects_bad_replies_and_times_out() {
        let t1 = SystemTime::now();
        let req = sntp_request(t1);
        let mut r = sntp_reply(&req, t1);
        assert!(sntp_parse(&req, &r, t1, t1).is_ok());
        r[1] = 0; // kiss-o'-death / unsynchronised
        assert!(sntp_parse(&req, &r, t1, t1).is_err());
        let mut r = sntp_reply(&req, t1);
        r[24] ^= 1; // not our request
        assert!(sntp_parse(&req, &r, t1, t1).is_err());
        assert!(sntp_parse(&req, &r[..20], t1, t1).is_err());
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let e = sntp_query(&silent.local_addr().unwrap().to_string(), Duration::from_millis(200)).unwrap_err();
        assert!(matches!(e, DeviceError::Timeout(_)), "{e}");
        assert_eq!(ntp_server_from_config("# x\nNTP_SERVER=ntp-g7g.amazon.com\n").as_deref(), Some("ntp-g7g.amazon.com"));
        assert_eq!(ntp_server_from_config("NTP_CHECK_TIME=1"), None);
    }
}
