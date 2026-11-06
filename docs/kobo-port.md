# Porting rinky to Kobo

> **Status:** implemented on branch `spike/kobo` (2026-11-06): the Kobo backend, the
> Kobo package and launcher, and the updater are in; see `docs/kobo.md` for users and
> `docs/ARCHITECTURE.md` (Kobo) for the design. The notes below are the investigation
> that led there; the "work to do" list is done except the sunxi models (Elipsa,
> Sage), Kaleido colour, a warmth schedule and USB storage from inside rinky.

Investigation of 2026-11-06. Sources: KOReader and
koreader-base (`frontend/device/kobo/`, `platform/kobo/`, `ffi/framebuffer_mxcfb.lua`,
`ffi/mxcfb_kobo_h.lua`), FBInk (`fbink_device_id.c`, `fbink_rota_quirks.c`,
`eink/*-kobo.h`), koxtoolchain, NickelMenu and KFMon, and Kobo's GPL tree
(`kobolabs/Kobo-Reader/hw`). Tested in the eink-emulator's Tolino Shine 5 instance,
which is Kobo Clara BW hardware (MT8113, E60T00 board) running Kobo firmware 5.12.

## Verdict

- **No new binary.** The Kindle build (static ARMv7 musl, hard float, Cortex-A9 +
  NEON) runs unchanged on Kobo firmware. It needs no libc from the device, and every
  Kobo since the Glo HD (2015) has a Cortex-A7, A9 or A53 CPU with NEON. The binary
  can tell a Kindle from a Kobo when it starts, as it already tells Kindle models
  apart.
- **A separate package.** The binary is shared, but the package around it differs:
  Kobo apps live in `/mnt/onboard/.adds/rinky/`, are started from NickelMenu or KFMon
  instead of KUAL, and the launcher has to stop and restart Nickel instead of the
  Kindle framework. So a release would ship `rinky-<v>-kobo.zip` next to the Kindle
  tar, and the in-app updater would pick the asset by platform.
- **The work is in the device layer.** On Kindle, rinky leans on Amazon's `lipc`
  services for light, Wi-Fi, sleep and USB. Kobo has nothing like that, so a Kobo
  backend has to drive the hardware directly, as KOReader does.

## What the emulator showed

The spike's changes, about 150 lines:

- **Refresh:** a `KoboMtk` controller, Kobo's 36-byte `hwtcon_update_data` struct
  with `HWTCON_SEND_UPDATE` (0x4024462E). REAGL and GLR16 are sent FULL, as KOReader
  does.
- **Pixel format:** the framebuffer boots at 32 bpp. Switching it to 8 bpp gray takes
  `Y8` written to `/sys/devices/platform/*.hwtcon/mdp_src_format` first, as FBInk
  does; otherwise the driver silently keeps 32 bpp. Both settings are restored on
  exit.
- **Touch:** the "snow" protocol. The panel never sends `TRACKING_ID -1`, so a
  `BTN_TOUCH 0` now ends the contact (`evdev.rs`, with a test).

Results:

| Check | Result |
|---|---|
| Binary starts | yes, unchanged |
| Book page drawn | yes, cover and text pages, 8 bpp |
| Page turns, both directions | yes, with `RINKY_TOUCH=swap,mirror_x` |
| Reader menu, quick settings panel | yes |
| Exit restores 32 bpp / ABGR32 | yes |
| Frontlight, warmth, Wi-Fi | "not available" (they go through `lipc`) |
| Sleep, USB, starting Nickel again | not tried |

## Kobo models

Kernel versions are from Kobo's GPL tree. ppi is from FBInk.

| Model | Year | SoC | Display interface | ppi | Fit for rinky |
|---|---|---|---|---|---|
| Aura HD, Aura H2O | 2013-14 | i.MX50 (Cortex-A8), kernel 2.6.35 | mxcfb v1 | 265 | no (see below) |
| Aura, Aura SE, Nia, Touch 2 | 2013-21 | i.MX50 / 6SL / 6ULL | mxcfb | 212 or less | no: too few pixels |
| Glo HD | 2015 | i.MX6SL (A9), kernel 3.0.35 | mxcfb v1 | 300 | yes |
| Aura ONE | 2016 | i.MX6SL | mxcfb v1 | 300 | yes |
| Aura H2O² | 2017 | 6SL / 6SLL | mxcfb v1 / v2 | 265 | probably (265 ppi, like the Oasis 1) |
| Clara HD | 2018 | i.MX6SLL (A9) | mxcfb v2 | 300 | yes |
| Forma | 2018 | i.MX6SLL | mxcfb v2 | 300 | yes, page keys |
| Libra H2O | 2019 | i.MX6SLL | mxcfb v2 | 300 | yes, page keys |
| Libra 2, Clara 2E | 2021-22 | i.MX6SLL | mxcfb v2 | 300 | yes |
| Elipsa, Sage | 2021 | Allwinner B300 (A7) | sunxi disp2 | 227 / 300 | Sage yes, Elipsa low ppi; needs the hardest display backend |
| Elipsa 2E | 2023 | MT8113 (A53) | hwtcon | 227 | low ppi |
| Clara BW, Clara Colour | 2024 | MT8113 | hwtcon | 300 | yes: what the spike ran on |
| Libra Colour | 2024 | MT8113 | hwtcon | 300 | yes, page keys |
| Tolino Shine 5, Shine Color, Vision Color | 2024 | MT8113 | hwtcon | 300 | yes: same hardware, Kobo-made firmware |

**Why the i.MX50 models are out.** Two risks, neither tested:
- **CPU:** a Cortex-A9 build may contain instructions the Cortex-A8 lacks (half-precision
  conversions, the MP extensions).
- **Kernel:** 2.6.35 is older than the Linux 3.2 that Rust's standard library supports.

Both could be worked around (a generic ARMv7 build, avoiding new system calls), but
those models are 2013-14 hardware, and the Aura is 212 ppi. Leaving them out keeps the
single binary.

## The four display interfaces

| Interface | Models | Refresh call | Needs |
|---|---|---|---|
| mxcfb v1 | Glo HD, Aura ONE, H2O² r1 | `MXCFB_SEND_UPDATE_V1_NTX` 0x4044462E (68 bytes) | new struct, close to Kindle's legacy one |
| mxcfb v2 | Clara HD, Forma, Libra H2O, Libra 2, Clara 2E | `MXCFB_SEND_UPDATE_V2` 0x4048462E (72 bytes), wait 0xC008462F | same request number as Kindle's legacy struct but a different layout, so detection has to come from the model, not probing; some models' wait call hangs (KOReader sleeps 2.5 ms instead) |
| hwtcon | Clara BW, Colour, Libra Colour, Elipsa 2E, Tolinos | 0x4024462E (36 bytes) | done in the spike; night mode is global (`echo "night_mode 4" > /proc/hwtcon/cmd`); Colour models need 32 bpp to show colour |
| sunxi disp2 | Elipsa, Sage | `DISP_EINK_UPDATE2` on `/dev/disp` | its own buffer from `/dev/ion`, submitted as a display layer; rotation by the G2D engine; no hardware inversion |

Every Kobo panel is mounted rotated, and each family maps rotation differently
(FBInk's `rotationMap`). KOReader sets portrait at start and restores the rotation
on exit; rinky's `make_upright` already does that for Kindles.

## Work to do

Rough sizes, for a backend at the level the Kindle one is now:

1. **Platform detection** (small). Kindle or Kobo, from `/proc/usid` against
   `/mnt/onboard/.kobo/version`. Kobo model from the version file's last three
   digits, or the NTX hardware config block (FBInk `identify_kobo`). A Kobo model
   table like `kindle/model.rs`: display interface, touch transforms (`swap`,
   `mirror_x` or `mirror_y`, snow protocol), frontlight type, page keys.
2. **Display** (medium). hwtcon is done. mxcfb v1 and v2 are struct variants of
   what exists. sunxi is a separate backend (ION buffer, layers, G2D rotation), the
   largest single piece. Leaving the Elipsa and Sage for later is reasonable.
3. **Touch and keys** (small). Transforms per model instead of `RINKY_TOUCH`. Snow
   protocol is done; protocol A ("phoenix", lift by `TOUCH_MAJOR 0`) for the Glo HD
   and Aura ONE. Page keys 193/194, sleep cover key 59 (35 on the Elipsa).
4. **Frontlight and warmth** (small). `/sys/class/backlight/mxc_msp430.0/brightness`
   or `ioctl(/dev/ntx_io, 241, level)`; warmth 0-10 written inverted to the mixer's
   `color` node. The Aura ONE and H2O² mix three LED channels.
5. **Sleep** (medium). No powerd: rinky does `echo 1 > /sys/power/state-extended`,
   waits 2 s, `echo mem > /sys/power/state`, and undoes it on wake. The screensaver,
   Wi-Fi and light off before. On MTK, never while charging (the kernel hangs).
6. **Wi-Fi** (medium). Load the driver modules for the platform, `wpa_supplicant`
   with Nickel's `/mnt/onboard/.kobo/wpa_supplicant.conf`, `udhcpc` or `dhcpcd`.
   Three driver variants (sdio, NXP `moal`, MTK `wmt`). KOReader's
   `enable-wifi.sh` is the reference.
7. **Battery and USB** (small). Battery from `/sys/class/power_supply/*`. USB mass
   storage means exiting to Nickel, as KOReader does (it has no USB export of its
   own).
8. **Launcher and package** (medium). A `rinky.sh` that copies itself to `/tmp`,
   stops Nickel and its helpers (`nickel hindenburg sickel fickel …`), turns
   Bluetooth off, runs rinky, restores bit depth and rotation, and starts Nickel
   again. A NickelMenu entry (`menu_item:main:rinky:cmd_spawn:quiet:exec …`) and a
   KFMon entry as the alternative. A zip that unpacks onto the Kobo drive.
9. **Updater and docs** (small). Asset name by platform; a Kobo section in the user
   guide (KoboRoot.tgz for NickelMenu, no jailbreak needed).

Paths in the app that assume `/mnt/us` (library folders, fonts, logs) need a
platform root as well. `rinky run` on the Tolino defaulted to `/mnt/us/Books` and
missed the bundled UI font.

## Risks

- **Nickel coexistence.** Every exit has to bring Nickel back cleanly. KOReader
  reboots when it was started from KFMon. Kobo firmware 5.x changed the start-up
  scripts, and NickelMenu does not support 5.x yet.
- **Real hardware differs from the emulator.** The emulator checks the kernel's
  answers, not ghosting, refresh speed or touch noise. The snow protocol and the
  swap and mirror values came from the emulator's touch model; KOReader's model
  table agrees for the Clara BW.
- **Colour models** at 8 bpp show grayscale. Colour needs 32 bpp and BGRA writes:
  a second pixel path in the screen code.
