//! Battery and charger state from sysfs (no child process) and the low-battery
//! monitor.
//!
//! PW12 / emulator (5.19.5): `/sys/class/power_supply/bd71827_bat/{capacity, status,
//! charging, temp, voltage_now}` (KOReader reads the same files), chargers
//! `bd71827_ac` (type `Mains`) and `Wireless`. Without a battery supply the Kindle
//! services fall back to powerd's `battLevel` and `isCharging`.

use crate::sysfs::SysRoot;

const PSY: &str = "/sys/class/power_supply";

/// Battery details.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatteryInfo {
    /// Charge in percent.
    pub percent: u8,
    /// Charging now.
    pub charging: bool,
    /// Kernel status text (`Charging`, `Discharging`, `Full`, ...).
    pub status: String,
    /// Temperature in degrees Celsius.
    pub temperature_c: Option<f32>,
    /// Voltage in millivolts.
    pub voltage_mv: Option<u32>,
    /// A charger (any non-battery supply) reports `online`.
    pub external_power: bool,
    /// Where the values came from (`sysfs:bd71827_bat`, `powerd`).
    pub source: String,
}

/// Power supplies of a type, by name (`Battery`, `Mains`, `USB`, `Wireless`).
pub fn supplies(root: &SysRoot) -> Vec<(String, String)> {
    root.list(PSY)
        .into_iter()
        .filter_map(|n| root.read(&format!("{PSY}/{n}/type")).ok().map(|t| (n, t)))
        .collect()
}

/// True when any charger supply (not a battery, not the moisture sensor) is online.
pub fn external_power(root: &SysRoot) -> bool {
    supplies(root)
        .iter()
        .filter(|(_, t)| matches!(t.as_str(), "Mains" | "USB" | "USB_DCP" | "USB_CDP" | "USB_ACA" | "Wireless"))
        .any(|(n, _)| root.read_int(&format!("{PSY}/{n}/online")).unwrap_or(0) == 1)
}

/// The battery reports charging.
pub fn charging(root: &SysRoot) -> bool {
    read_sysfs(root).is_some_and(|b| b.charging)
}

/// Reads the first `Battery` supply.
pub fn read_sysfs(root: &SysRoot) -> Option<BatteryInfo> {
    let (name, _) = supplies(root).into_iter().find(|(_, t)| t == "Battery")?;
    let base = format!("{PSY}/{name}");
    let percent = root.read_int(&format!("{base}/capacity")).ok()?.clamp(0, 100) as u8;
    let status = root.read(&format!("{base}/status")).unwrap_or_default();
    let charging = match root.read_int(&format!("{base}/charging")) {
        Ok(v) => v != 0,
        Err(_) => status == "Charging",
    } || status == "Charging";
    Some(BatteryInfo {
        percent,
        charging,
        status,
        // Tenths of a degree, as power_supply reports.
        temperature_c: root.read_int(&format!("{base}/temp")).ok().map(|t| t as f32 / 10.0),
        voltage_mv: root.read_int(&format!("{base}/voltage_now")).ok().map(|uv| (uv / 1000).max(0) as u32),
        external_power: external_power(root),
        source: format!("sysfs:{name}"),
    })
}

/// Emits a low-battery warning once when the charge falls to `threshold` while not
/// charging, and re-arms only after it rose above `threshold + hysteresis` (or a
/// charger was connected).
#[derive(Clone, Debug)]
pub struct LowBatteryMonitor {
    /// Warning level in percent.
    pub threshold: u8,
    /// Re-arm margin in percent.
    pub hysteresis: u8,
    armed: bool,
}

impl Default for LowBatteryMonitor {
    fn default() -> Self {
        LowBatteryMonitor::new(10, 3)
    }
}

impl LowBatteryMonitor {
    /// A monitor that warns at `threshold` percent.
    pub fn new(threshold: u8, hysteresis: u8) -> Self {
        LowBatteryMonitor { threshold, hysteresis, armed: true }
    }

    /// Feeds a reading; returns the percent when the warning fires.
    pub fn update(&mut self, percent: u8, charging: bool) -> Option<u8> {
        if charging || percent > self.threshold.saturating_add(self.hysteresis) {
            self.armed = true;
            return None;
        }
        if self.armed && percent <= self.threshold {
            self.armed = false;
            return Some(percent);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::{test_dir, write_tree};

    fn emulator_tree() -> std::path::PathBuf {
        let d = test_dir("battery");
        write_tree(
            &d,
            &[
                ("/sys/class/power_supply/bd71827_bat/type", "Battery\n"),
                ("/sys/class/power_supply/bd71827_bat/capacity", "59\n"),
                ("/sys/class/power_supply/bd71827_bat/status", "Discharging\n"),
                ("/sys/class/power_supply/bd71827_bat/charging", "0\n"),
                ("/sys/class/power_supply/bd71827_bat/temp", "250\n"),
                ("/sys/class/power_supply/bd71827_bat/voltage_now", "4000000\n"),
                ("/sys/class/power_supply/bd71827_ac/type", "Mains\n"),
                ("/sys/class/power_supply/bd71827_ac/online", "0\n"),
                ("/sys/class/power_supply/Wireless/type", "Wireless\n"),
                ("/sys/class/power_supply/Wireless/online", "0\n"),
                ("/sys/class/power_supply/max20342_moisture/type", "USB_WET\n"),
                ("/sys/class/power_supply/max20342_moisture/online", "1\n"),
            ],
        )
        .unwrap();
        d
    }

    #[test]
    fn reads_the_emulator_battery() {
        let d = emulator_tree();
        let root = SysRoot::at(&d);
        let b = read_sysfs(&root).unwrap();
        assert_eq!(b.percent, 59);
        assert!(!b.charging && !b.external_power, "the moisture sensor is not a charger");
        assert_eq!(b.temperature_c, Some(25.0));
        assert_eq!(b.voltage_mv, Some(4000));
        assert_eq!(b.source, "sysfs:bd71827_bat");
        write_tree(
            &d,
            &[
                ("/sys/class/power_supply/bd71827_ac/online", "1\n"),
                ("/sys/class/power_supply/bd71827_bat/status", "Charging\n"),
            ],
        )
        .unwrap();
        let b = read_sysfs(&root).unwrap();
        assert!(b.charging && b.external_power);
        std::fs::remove_dir_all(d).unwrap();
        assert!(read_sysfs(&SysRoot::at("/nonexistent-ink")).is_none());
    }

    #[test]
    fn low_battery_fires_once_per_crossing() {
        let mut m = LowBatteryMonitor::new(10, 3);
        assert_eq!(m.update(50, false), None);
        assert_eq!(m.update(11, false), None);
        assert_eq!(m.update(10, false), Some(10));
        assert_eq!(m.update(9, false), None, "only once");
        assert_eq!(m.update(12, false), None, "inside the hysteresis band: still disarmed");
        assert_eq!(m.update(9, false), None);
        assert_eq!(m.update(9, true), None, "charging re-arms");
        assert_eq!(m.update(8, false), Some(8));
        assert_eq!(m.update(14, false), None);
        assert_eq!(m.update(5, false), Some(5));
    }
}
