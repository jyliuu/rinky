//! Wi-Fi data from `com.lab126.wifid` hash-array properties, and the profile format.
//!
//! Measured on the 5.19.5 emulator (`out/evidence/device/wifi-lipc-probe-*.txt`):
//! - `scanList` hashes: `bssid`, `essid`, `secured` ("yes"/"no"), `known`, `netid`,
//!   `signal`, `signal_max`, `supported`, `wps`, `enterprise`, `key_mgmt`;
//! - `currentEssid`: one hash `netid, essid, bssid, connected ("yes"/"no"), signal,
//!   signal_max, secured`;
//! - `profileData`: one hash per saved profile `netid, essid, secured, smethod,
//!   key_mgmt, store_nw_user_pref, psk, ...`;
//! - `createProfile` takes `{ essid = "x", secured = "yes", psk = "...",
//!   store_nw_user_pref = (0) }` (KOReader passes 0 so the password is not uploaded);
//! - connecting: `lipc-set-prop com.lab126.cmd ensureConnection wifi:ESSID`, then
//!   `cmStateChange "CONNECTED"` or `cmConnectionFailed`; `signalStrength` is a string
//!   `"5/5"`; `cmState` `PENDING`/`CONNECTED`/`NA`/`READY`;
//! - airplane mode: `com.lab126.cmd wirelessEnable` 0/1 (KOReader toggles it together
//!   with `com.lab126.wifid enable`).

use crate::lipc::{LipcHash, LipcValue};
use crate::services::WifiNetwork;
use crate::{DeviceError, Result};

/// A saved Wi-Fi profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiProfile {
    /// Network name.
    pub essid: String,
    /// wifid's id (for `deleteProfile`).
    pub netid: i64,
    /// Uses a password.
    pub secured: bool,
}

/// The network wifid is connected to (or trying).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentNetwork {
    /// Name ("" when none).
    pub essid: String,
    /// Connected now.
    pub connected: bool,
    /// Signal 0..=5.
    pub signal: u8,
}

fn s(h: &LipcHash, k: &str) -> String {
    h.get(k).map(LipcValue::as_string).unwrap_or_default()
}

fn yes(h: &LipcHash, k: &str) -> bool {
    s(h, k) == "yes"
}

/// Parses `signalStrength` (`"5/5"`, `"0/5"`, `""`) to 0..=5.
pub fn parse_signal(v: &str) -> Option<u8> {
    let (a, b) = v.trim().split_once('/')?;
    let (a, b): (u32, u32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    if b == 0 {
        return None;
    }
    // In u64: a u32 numerator times 5 cannot overflow.
    Some((a as u64 * 5 / b as u64).min(5) as u8)
}

/// Networks from `scanList`: hidden (empty) names dropped, one entry per name (the
/// strongest), strongest first.
pub fn networks_from_scan(list: &[LipcHash]) -> Vec<WifiNetwork> {
    let mut out: Vec<WifiNetwork> = Vec::new();
    for h in list {
        let essid = s(h, "essid");
        if essid.is_empty() {
            continue;
        }
        let max = h.get("signal_max").and_then(LipcValue::as_int).filter(|m| *m > 0).unwrap_or(5);
        let signal = (h.get("signal").and_then(LipcValue::as_int).unwrap_or(0).max(0).saturating_mul(5) / max).min(5) as u8;
        let net = WifiNetwork { essid: essid.clone(), signal, secured: yes(h, "secured"), known: yes(h, "known") };
        match out.iter_mut().find(|n| n.essid == essid) {
            Some(n) if n.signal >= signal => {}
            Some(n) => *n = net,
            None => out.push(net),
        }
    }
    out.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.essid.cmp(&b.essid)));
    out
}

/// Profiles from `profileData` (passwords are not kept).
pub fn profiles_from(list: &[LipcHash]) -> Vec<WifiProfile> {
    list.iter()
        .filter(|h| !s(h, "essid").is_empty())
        .map(|h| WifiProfile {
            essid: s(h, "essid"),
            netid: h.get("netid").and_then(LipcValue::as_int).unwrap_or(-1),
            secured: yes(h, "secured"),
        })
        .collect()
}

/// The current network from `currentEssid`.
pub fn current_from(list: &[LipcHash]) -> Option<CurrentNetwork> {
    let h = list.first()?;
    let max = h.get("signal_max").and_then(LipcValue::as_int).filter(|m| *m > 0).unwrap_or(5);
    Some(CurrentNetwork {
        essid: s(h, "essid"),
        connected: yes(h, "connected"),
        signal: (h.get("signal").and_then(LipcValue::as_int).unwrap_or(0).max(0) * 5 / max).min(5) as u8,
    })
}

/// The `createProfile` hash for a network; `password` `None` = open network.
/// WPA passphrases must be 8..=63 characters.
pub fn profile_hash(essid: &str, password: Option<&str>) -> Result<LipcHash> {
    if essid.is_empty() || essid.len() > 32 {
        return Err(DeviceError::Backend(format!("Wi-Fi name must be 1..32 bytes, got {}", essid.len())));
    }
    let mut h = LipcHash::new();
    h.insert("essid".into(), LipcValue::Str(essid.to_string()));
    match password {
        Some(p) => {
            if !(8..=63).contains(&p.len()) {
                return Err(DeviceError::Backend(format!("WPA password must be 8..63 characters, got {}", p.len())));
            }
            h.insert("secured".into(), LipcValue::Str("yes".into()));
            h.insert("psk".into(), LipcValue::Str(p.to_string()));
            h.insert("store_nw_user_pref".into(), LipcValue::Int(0));
        }
        None => {
            h.insert("secured".into(), LipcValue::Str("no".into()));
        }
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lipc::{format_hash_input, parse_hash_output};

    const SCAN: &str = "Hash Index: 0\n\tbssid = \"02:00:00:81:13:01\"\n\tessid = \"Kindle-QEMU\"\n\tsecured = \"no\"\n\tknown = \"no\"\n\tnetid = (-1)\n\tsignal = (5)\n\tsignal_max = (5)\n\tsupported = \"yes\"\n\twps = \"no\"\n\tenterprise = \"no\"\n\tkey_mgmt = \"\"\n\nHash Index: 1\n\tessid = \"\"\n\tsignal = (4)\n\nHash Index: 2\n\tessid = \"Home\"\n\tsecured = \"yes\"\n\tknown = \"yes\"\n\tsignal = (2)\n\tsignal_max = (5)\nHash Index: 3\n\tessid = \"Home\"\n\tsecured = \"yes\"\n\tknown = \"yes\"\n\tsignal = (3)\n\tsignal_max = (5)\n";

    #[test]
    fn scan_list_from_the_emulator_format() {
        let nets = networks_from_scan(&parse_hash_output(SCAN));
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0], WifiNetwork { essid: "Kindle-QEMU".into(), signal: 5, secured: false, known: false });
        assert_eq!(nets[1], WifiNetwork { essid: "Home".into(), signal: 3, secured: true, known: true });
    }

    #[test]
    fn current_profiles_signal() {
        let cur = "Hash Index: 0\n\tnetid = (5)\n\tessid = \"Kindle-QEMU\"\n\tbssid = \"02:00:00:81:13:01\"\n\tconnected = \"yes\"\n\tsignal = (5)\n\tsignal_max = (5)\n\tsecured = \"no\"\n";
        assert_eq!(
            current_from(&parse_hash_output(cur)),
            Some(CurrentNetwork { essid: "Kindle-QEMU".into(), connected: true, signal: 5 })
        );
        let prof = "Hash Index: 0\n\tnetid = (1)\n\tessid = \"inktest1\"\n\tsecured = \"no\"\n\tpsk = \"\"\nHash Index: 1\n\tnetid = (2)\n\tessid = \"inktest2\"\n\tsecured = \"yes\"\n\tpsk = \"x\"\n";
        let p = profiles_from(&parse_hash_output(prof));
        assert_eq!(p[1], WifiProfile { essid: "inktest2".into(), netid: 2, secured: true });
        assert_eq!(parse_signal("5/5"), Some(5));
        assert_eq!(parse_signal("1/5"), Some(1));
        assert_eq!(parse_signal("3/4"), Some(3));
        assert_eq!(parse_signal(""), None);
        assert_eq!(parse_signal("x/0"), None);
    }

    #[test]
    fn profile_hash_formats_like_the_tested_input() {
        let h = profile_hash("inktest5", Some("fakepass123")).unwrap();
        assert_eq!(
            format_hash_input(&h).unwrap(),
            "{ essid = \"inktest5\", psk = \"fakepass123\", secured = \"yes\", store_nw_user_pref = (0) }"
        );
        let open = profile_hash("Kindle-QEMU", None).unwrap();
        assert_eq!(format_hash_input(&open).unwrap(), "{ essid = \"Kindle-QEMU\", secured = \"no\" }");
        assert!(profile_hash("x", Some("short")).is_err());
        assert!(profile_hash("", None).is_err());
        assert!(format_hash_input(&profile_hash("a\"b", None).unwrap()).is_err());
    }
}
