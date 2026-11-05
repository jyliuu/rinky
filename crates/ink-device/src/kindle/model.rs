//! Kindle models: which one this is and what differs between them.
//!
//! The model comes from the serial number in `/proc/usid`, decoded as KOReader does
//! (`frontend/device/kindle/device.lua`): serials starting with `B` or `9` carry a
//! 2-character code at characters 3-4, newer ones (`G...`) a 3-character code at
//! characters 4-6, compared as text. `RINKY_MODEL` overrides it with a code or a
//! model name (`RINKY_MODEL=PW4`).
//!
//! What differs, all from KOReader and koreader-base (`ffi/framebuffer_mxcfb.lua`,
//! `ffi/mxcfb_kindle_h.lua`):
//! - the e-ink controller's refresh ioctl ([`Epdc`]): four struct layouts;
//! - natural light (warmth) or not, page keys and their direction, the hall sensor's
//!   sysfs folder, an accelerometer.
//!
//! Screen size is not here: it is read from the framebuffer. rinky's UI is laid out
//! for 300 ppi, so the 212 ppi and 167 ppi models (PW1, PW2, the 7th, 8th and 10th
//! generation Basic) are [`Tier::Unsupported`]. A Kindle whose code is not in the table
//! gets [`Model::unknown`]: the refresh ioctl is then found by trying them in turn.

use crate::sysfs::SysRoot;

/// The e-ink controller interface: struct layout and ioctl of a refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Epdc {
    /// `mxcfb_update_data` (72 bytes), i.MX6 "Wario"/"Duet"/"Heisenberg": Voyage, PW3,
    /// Oasis 1 (and the unsupported PW2, Basic 7th and 8th gen).
    Legacy,
    /// `mxcfb_update_data_zelda` (88 bytes), i.MX7: Oasis 2 and 3.
    Zelda,
    /// `mxcfb_update_data_rex` (80 bytes), i.MX7: PW4 (and Basic 10th gen).
    Rex,
    /// `mxcfb_update_data_mtk` (96 bytes), MediaTek: PW5 and newer.
    Mtk,
}

impl Epdc {
    /// The order in which an unknown Kindle's controller is probed (newest first).
    pub const PROBE_ORDER: [Epdc; 4] = [Epdc::Mtk, Epdc::Rex, Epdc::Zelda, Epdc::Legacy];

    /// Short name for logs.
    pub fn name(self) -> &'static str {
        match self {
            Epdc::Legacy => "mxcfb",
            Epdc::Zelda => "mxcfb-zelda",
            Epdc::Rex => "mxcfb-rex",
            Epdc::Mtk => "mtk",
        }
    }
}

/// How well rinky knows a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Tested on the device.
    Tested,
    /// Should work (same controller family as a tested model) but nobody tried yet.
    Untested,
    /// Known not to fit; rinky starts anyway and logs why.
    Unsupported(&'static str),
}

/// What rinky needs to know about one Kindle model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    /// Short name (`PW4`), as KOReader's class without the `Kindle` prefix.
    pub id: &'static str,
    /// Marketing name.
    pub name: &'static str,
    /// Device codes as the serial carries them.
    pub codes: &'static [&'static str],
    /// Refresh interface.
    pub epdc: Epdc,
    /// Has a warmth (amber) light.
    pub warmth: bool,
    /// Page keys: (next, previous) evdev key codes.
    pub page_keys: Option<(u16, u16)>,
    /// The Voyage's pressure keys, which must be switched on while the framework is
    /// stopped (`com.lab126.deviced fsrkeypad*Enable`).
    pub pressure_keys: bool,
    /// Has an accelerometer (the framework may leave the framebuffer rotated).
    pub gsensor: bool,
    /// The MTK driver takes the page-turn sweep (`MTK_EPDC_FLAG_ENABLE_SWIPE`).
    /// KOReader's `canDoSwipeAnimation`: PW5 and PW12 only. The Kindle 2024's driver
    /// voids such an update ("swipe update void is true") and the page body stays as
    /// it was (emulator, 5.20.2, 2026-11-05).
    pub swipe: bool,
    /// sysfs folder with `hall_enable` (read for logs only).
    pub hall_dir: Option<&'static str>,
    /// Support level.
    pub tier: Tier,
}

const WARIO_HALL: Option<&str> = Some("/sys/devices/system/wario_hall/wario_hall0");
const REX_HALL: Option<&str> = Some("/sys/bus/platform/drivers/hall_sensor/rex_hall");
const EINK_HALL: Option<&str> = Some("/sys/devices/platform/eink_hall");
/// Voyage: KEY_PAGEDOWN turns forward, KEY_PAGEUP back.
const VOYAGE_KEYS: Option<(u16, u16)> = Some((109, 104));
/// Oasis 1-3: the other way round (KOReader: 104 forward, 109 back).
const OASIS_KEYS: Option<(u16, u16)> = Some((104, 109));
const LOW_DPI: Tier = Tier::Unsupported("212 ppi or lower screen; rinky's layout is made for 300 ppi");

const fn m(id: &'static str, name: &'static str, codes: &'static [&'static str], epdc: Epdc) -> Model {
    Model {
        id,
        name,
        codes,
        epdc,
        warmth: false,
        page_keys: None,
        pressure_keys: false,
        gsensor: false,
        swipe: false,
        hall_dir: None,
        tier: Tier::Untested,
    }
}

/// Every 5.x Kindle KOReader knows, in its match order.
pub const MODELS: &[Model] = &[
    Model { tier: LOW_DPI, ..m("PaperWhite", "Kindle Paperwhite (5th gen)", &["24", "1B", "1D", "1F", "1C", "20"], Epdc::Legacy) },
    Model {
        tier: LOW_DPI,
        hall_dir: WARIO_HALL,
        ..m(
            "PaperWhite2",
            "Kindle Paperwhite 2 (6th gen)",
            &["D4", "5A", "D5", "D6", "D7", "D8", "F2", "17", "60", "F4", "F9", "62", "61", "5F"],
            Epdc::Legacy,
        )
    },
    Model { tier: LOW_DPI, hall_dir: WARIO_HALL, ..m("Basic", "Kindle (7th gen)", &["C6", "DD"], Epdc::Legacy) },
    Model {
        page_keys: VOYAGE_KEYS,
        pressure_keys: true,
        hall_dir: WARIO_HALL,
        ..m("Voyage", "Kindle Voyage", &["13", "54", "2A", "4F", "52", "53"], Epdc::Legacy)
    },
    Model {
        hall_dir: WARIO_HALL,
        ..m(
            "PaperWhite3",
            "Kindle Paperwhite 3 (7th gen)",
            &["0G1", "0G2", "0G4", "0G5", "0G6", "0G7", "0KB", "0KC", "0KD", "0KE", "0KF", "0KG", "0LK", "0LL"],
            Epdc::Legacy,
        )
    },
    Model {
        page_keys: OASIS_KEYS,
        gsensor: true,
        hall_dir: WARIO_HALL,
        ..m("Oasis", "Kindle Oasis (8th gen)", &["0GC", "0GD", "0GR", "0GS", "0GT", "0GU"], Epdc::Legacy)
    },
    Model {
        tier: Tier::Unsupported("167 ppi screen and no frontlight"),
        ..m("Basic2", "Kindle (8th gen)", &["0DU", "0K9", "0KA"], Epdc::Legacy)
    },
    Model {
        page_keys: OASIS_KEYS,
        gsensor: true,
        ..m(
            "Oasis2",
            "Kindle Oasis 2 (9th gen)",
            &["0LM", "0LN", "0LP", "0LQ", "0P1", "0P2", "0P6", "0P7", "0P8", "0S1", "0S2", "0S3", "0S4", "0S7", "0SA"],
            Epdc::Zelda,
        )
    },
    Model {
        hall_dir: REX_HALL,
        ..m(
            "PaperWhite4",
            "Kindle Paperwhite 4 (10th gen)",
            &[
                "0PP", "0T1", "0T2", "0T3", "0T4", "0T5", "0T6", "0T7", "0TJ", "0TK", "0TL", "0TM", "0TN", "102", "103", "16Q", "16R",
                "16S", "16T", "16U", "16V",
            ],
            Epdc::Rex,
        )
    },
    Model {
        tier: Tier::Unsupported("167 ppi screen; its touch controller also needs another protocol"),
        hall_dir: REX_HALL,
        ..m("Basic3", "Kindle (10th gen)", &["10L", "0WF", "0WG", "0WH", "0WJ", "0VB"], Epdc::Rex)
    },
    Model {
        warmth: true,
        page_keys: OASIS_KEYS,
        gsensor: true,
        ..m("Oasis3", "Kindle Oasis 3 (10th gen)", &["11L", "0WQ", "0WP", "0WN", "0WM", "0WL"], Epdc::Zelda)
    },
    Model {
        warmth: true,
        swipe: true,
        hall_dir: EINK_HALL,
        ..m("PaperWhite5", "Kindle Paperwhite 5 (11th gen)", &["1Q0", "1PX", "1VD", "21A", "2BJ", "2DK"], Epdc::Mtk)
    },
    Model {
        warmth: true,
        swipe: true,
        hall_dir: EINK_HALL,
        ..m("PaperWhite5SE", "Kindle Paperwhite 5 Signature Edition", &["1LG", "219", "2BH"], Epdc::Mtk)
    },
    Model { hall_dir: EINK_HALL, ..m("Basic4", "Kindle (11th gen)", &["22D", "25T", "23A", "2AQ", "2AP", "1XH", "22C"], Epdc::Mtk) },
    Model {
        warmth: true,
        gsensor: true,
        hall_dir: EINK_HALL,
        ..m(
            "Scribe",
            "Kindle Scribe",
            &["27J", "2BL", "263", "227", "2BM", "23L", "23M", "270", "3V0", "3V1", "3X5", "3UV", "3X4", "3X3", "41E", "410"],
            Epdc::Mtk,
        )
    },
    Model {
        warmth: true,
        hall_dir: EINK_HALL,
        ..m(
            "ColorSoft",
            "Kindle Colorsoft",
            &["3H2", "3H4", "3H6", "3H7", "3H9", "3JT", "3J6", "455", "456", "4EP", "34X", "3HB"],
            Epdc::Mtk,
        )
    },
    Model { hall_dir: EINK_HALL, ..m("Basic5", "Kindle (12th gen)", &["A89", "3L2", "3L3", "3L4", "3L5", "3L6", "3KM"], Epdc::Mtk) },
    Model {
        warmth: true,
        swipe: true,
        hall_dir: EINK_HALL,
        tier: Tier::Tested,
        ..m(
            "PaperWhite6",
            "Kindle Paperwhite (12th gen)",
            &["33W", "33X", "346", "349", "3H3", "3H5", "3H8", "3HA", "3J5", "3JS"],
            Epdc::Mtk,
        )
    },
    Model {
        warmth: true,
        gsensor: true,
        hall_dir: EINK_HALL,
        ..m("Scribe3", "Kindle Scribe (3rd gen)", &["4PG", "4PE", "4PL", "4F8", "4FA", "454"], Epdc::Mtk)
    },
    Model {
        warmth: true,
        gsensor: true,
        hall_dir: EINK_HALL,
        ..m("ScribeColorSoft", "Kindle Scribe Colorsoft", &["4VX", "4PF", "4PH", "4F9", "4FB", "46P"], Epdc::Mtk)
    },
];

impl Model {
    /// A Kindle whose code is not in [`MODELS`]: the controller is probed, warmth is
    /// tried, nothing model-specific is assumed.
    pub const fn unknown() -> Model {
        Model { warmth: true, hall_dir: EINK_HALL, ..m("Unknown", "Kindle (unknown model)", &[], Epdc::Mtk) }
    }

    /// Not from the table: the refresh ioctl must be probed.
    pub fn is_unknown(&self) -> bool {
        self.codes.is_empty()
    }

    /// The model with this device code.
    pub fn by_code(code: &str) -> Option<Model> {
        let code = code.to_ascii_uppercase();
        MODELS.iter().find(|m| m.codes.contains(&code.as_str())).copied()
    }

    /// The model with this id (`PaperWhite4`, also without the leading `Kindle`; case
    /// does not matter) or code.
    pub fn by_name_or_code(s: &str) -> Option<Model> {
        let s = s.trim();
        let id = s.strip_prefix("Kindle").unwrap_or(s);
        MODELS.iter().find(|m| m.id.eq_ignore_ascii_case(id)).copied().or_else(|| Model::by_code(s))
    }

    /// The model this serial number belongs to; [`Model::unknown`] otherwise.
    pub fn from_serial(serial: &str) -> Model {
        device_code(serial).and_then(Model::by_code).unwrap_or_else(Model::unknown)
    }

    /// The running Kindle: `RINKY_MODEL`, else `/proc/usid`.
    pub fn detect(sys: &SysRoot) -> Model {
        if let Some(m) = std::env::var("RINKY_MODEL").ok().and_then(|v| Model::by_name_or_code(&v)) {
            return m;
        }
        let serial = sys.read("/proc/usid").unwrap_or_default();
        Model::from_serial(&serial)
    }

    /// One log line: name, id, controller and support level.
    pub fn describe(&self) -> String {
        let tier = match self.tier {
            Tier::Tested => "tested".to_string(),
            Tier::Untested => "untested".to_string(),
            Tier::Unsupported(why) => format!("unsupported: {why}"),
        };
        format!("{} ({}, {}, {tier})", self.name, self.id, self.epdc.name())
    }
}

/// The device code in a serial number (KOReader's rule), upper case.
pub fn device_code(serial: &str) -> Option<&str> {
    let sn = serial.lines().next().unwrap_or("").trim();
    let range = match sn.as_bytes().first()? {
        b'B' | b'9' => 2..4,
        _ => 3..6,
    };
    sn.get(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials_decode_like_koreader() {
        assert_eq!(device_code("B013123456789"), Some("13"));
        assert_eq!(device_code("G000PP0000000000\n"), Some("0PP"));
        assert_eq!(Model::from_serial("B0131234").id, "Voyage");
        assert_eq!(Model::from_serial("G0000PP000000000").id, "Unknown", "code at 4-6, not 5-7");
        assert_eq!(Model::from_serial("G0010PP00000").id, "Unknown", "10P is no code");
        assert_eq!(Model::from_serial("G0033W12345678").id, "PaperWhite6");
        assert_eq!(Model::from_serial("G000T1abcdef").id, "PaperWhite4");
        assert_eq!(Model::from_serial("G0011L000").id, "Oasis3");
        assert_eq!(Model::from_serial("").id, "Unknown");
        assert_eq!(Model::from_serial("G0").id, "Unknown");
        assert_eq!(Model::from_serial("G00zz1").id, "Unknown");
    }

    #[test]
    fn every_code_names_one_model() {
        let mut seen = std::collections::HashSet::new();
        for m in MODELS {
            assert!(!m.codes.is_empty(), "{}", m.id);
            for c in m.codes {
                assert!(seen.insert(*c), "{c} twice");
                assert_eq!(Model::by_code(c).unwrap().id, m.id);
            }
        }
    }

    #[test]
    fn names_and_overrides() {
        assert_eq!(Model::by_name_or_code("PaperWhite4").unwrap().epdc, Epdc::Rex);
        assert_eq!(Model::by_name_or_code("KindleOasis2").unwrap().epdc, Epdc::Zelda);
        assert_eq!(Model::by_name_or_code("voyage").unwrap().page_keys, Some((109, 104)));
        assert_eq!(Model::by_name_or_code("0t1").unwrap().id, "PaperWhite4");
        assert!(Model::by_name_or_code("Kobo").is_none());
        assert!(Model::unknown().is_unknown());
        assert!(!Model::by_code("33W").unwrap().is_unknown());
        assert!(matches!(Model::by_code("0DU").unwrap().tier, Tier::Unsupported(_)));
        assert!(Model::by_code("3H3").unwrap().describe().contains("tested"));
        assert!(Model::by_code("3H3").unwrap().swipe && !Model::by_code("3KM").unwrap().swipe && !Model::unknown().swipe);
    }
}
