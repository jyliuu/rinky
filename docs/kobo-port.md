# Kobo port notes

Background for the Kobo backend: which Kobos exist, how their displays are driven, and
what is still missing. How the backend works today is in `docs/ARCHITECTURE.md`
("Running on the Kobo"); what users see is in `docs/kobo.md`.

The notes come from KOReader and koreader-base (`frontend/device/kobo/`,
`platform/kobo/`, `ffi/framebuffer_mxcfb.lua`, `ffi/mxcfb_kobo_h.lua`), FBInk
(`fbink_device_id.c`, `fbink_rota_quirks.c`, `eink/*-kobo.h`), koxtoolchain,
NickelMenu and KFMon, and Kobo's GPL tree (`kobolabs/Kobo-Reader/hw`). The first tests
ran in the eink-emulator's Tolino Shine 5 instance, which is Kobo Clara BW hardware
(MT8113, E60T00 board) with Kobo firmware 5.12.

## How the port is shaped

- **One binary.** The Kindle build (ARMv7 musl, hard float, Cortex-A9 with NEON,
  dynamically linked against the musl loader the package ships in `lib/`) runs
  unchanged on Kobo firmware; only the loader path written into it differs. It needs
  no libc from the device, and every
  Kobo since the Glo HD (2015) has a Cortex-A7, A9 or A53 CPU with NEON.
  `ink_platform::Platform::detect` tells a Kobo from a Kindle at start.
- **A separate package.** Kobo apps live in `/mnt/onboard/.adds/rinky/`, start from
  NickelMenu (or KFMon) instead of KUAL, and the launcher stops and restarts Nickel
  instead of the Kindle framework. Releases ship `rinky-<version>-kobo.zip` and
  `.tar.gz` next to the Kindle packages, and the in-app update picks the asset by
  platform (`Platform::package_suffix`).
- **The work is in the device layer.** On a Kindle, light, Wi-Fi, sleep and USB go
  through Amazon's `lipc` services. A Kobo has nothing like them, so the Kobo backend
  drives the hardware directly, as KOReader does (`crates/ink-device/src/ereader/kobo/`).

## Status

Done: platform detection and the Kobo model table (`KOBO_MODELS` in
`ereader/model.rs`); the `mxcfb` v2 and hwtcon refresh interfaces; switching the
framebuffer to 8 bpp gray and to its upright rotation, and back on exit; per-model
touch transforms and the snow protocol; page keys and the sleep cover; frontlight and
warmth; battery; suspend; Wi-Fi with Nickel's networks; the launcher, the NickelMenu
entry and the package; the update by platform.

Not done:

- the i.MX6SL models (Glo HD, Aura ONE, the first Aura H2O² revision): `mxcfb` v1 and
  touch protocol A;
- the sunxi models (Elipsa, Sage): a separate display backend;
- colour on the Kaleido models, which show grayscale at 8 bpp;
- a warmth schedule;
- USB storage from inside rinky (users exit to Nickel first, as with KOReader).

## Kobo models

Kernel versions are from Kobo's GPL tree. ppi is from FBInk.

| Model | Year | SoC | Display interface | ppi | rinky |
|---|---|---|---|---|---|
| Aura HD, Aura H2O | 2013-14 | i.MX50 (Cortex-A8), kernel 2.6.35 | mxcfb v1 | 265 | no (see below) |
| Aura, Aura SE, Nia, Touch 2 | 2013-21 | i.MX50 / 6SL / 6ULL | mxcfb | 212 or less | no: too few pixels |
| Glo HD | 2015 | i.MX6SL (A9), kernel 3.0.35 | mxcfb v1 | 300 | not yet: mxcfb v1 |
| Aura ONE | 2016 | i.MX6SL | mxcfb v1 | 300 | not yet: mxcfb v1 |
| Aura H2O² | 2017 | 6SL / 6SLL | mxcfb v1 / v2 | 265 | not yet (mxcfb v1 on the first revision) |
| Clara HD | 2018 | i.MX6SLL (A9) | mxcfb v2 | 300 | supported |
| Forma | 2018 | i.MX6SLL | mxcfb v2 | 300 | supported, page keys |
| Libra H2O | 2019 | i.MX6SLL | mxcfb v2 | 300 | supported, page keys |
| Libra 2, Clara 2E | 2021-22 | i.MX6SLL | mxcfb v2 | 300 | supported |
| Elipsa, Sage | 2021 | Allwinner B300 (A7) | sunxi disp2 | 227 / 300 | not yet: sunxi backend (and the Elipsa is 227 ppi) |
| Elipsa 2E | 2023 | MT8113 (A53) | hwtcon | 227 | no: too few pixels |
| Clara BW, Clara Colour | 2024 | MT8113 | hwtcon | 300 | supported (grayscale on the Colour) |
| Libra Colour | 2024 | MT8113 | hwtcon | 300 | supported, page keys, grayscale |
| Tolino Shine 5, Shine Color, Vision Color | 2024 | MT8113 | hwtcon | 300 | supported: same hardware, Kobo-made firmware |

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
| mxcfb v1 | Glo HD, Aura ONE, H2O² r1 | `MXCFB_SEND_UPDATE_V1_NTX` 0x4044462E (68 bytes) | not implemented; a struct close to the Kindle's legacy one |
| mxcfb v2 | Clara HD, Forma, Libra H2O, Libra 2, Clara 2E | `MXCFB_SEND_UPDATE_V2` 0x4048462E (72 bytes), wait 0xC008462F | `Epdc::KoboMxcfb`. Same request number as the Kindle's legacy struct but another layout, so it comes from the model table, never from probing; some models' wait call hangs, so they do not wait (`reliable_wait`) |
| hwtcon | Clara BW, Colour, Libra Colour, Elipsa 2E, Tolinos | 0x4024462E (36 bytes) | `Epdc::KoboMtk`; the framebuffer is switched to 8 bpp through the hwtcon `mdp_src_format` first; Colour models need 32 bpp to show colour |
| sunxi disp2 | Elipsa, Sage | `DISP_EINK_UPDATE2` on `/dev/disp` | not implemented; see below |

Every Kobo panel is mounted rotated, and each family maps rotation differently
(FBInk's `rotationMap`). As KOReader does, rinky sets the upright `rotate` value from
the model table at start (`Model::upright`) and restores the old one on exit.

## Notes for the missing pieces

- **mxcfb v1** (Glo HD, Aura ONE): `MXCFB_SEND_UPDATE_V1_NTX` 0x4044462E, a 68-byte
  struct close to the Kindle's legacy one. Their touch panels use protocol A
  ("phoenix"): a contact ends with `TOUCH_MAJOR 0`.
- **sunxi** (Elipsa, Sage): `DISP_EINK_UPDATE2` on `/dev/disp`, with a buffer from
  `/dev/ion` submitted as a display layer and rotation by the G2D engine. There is no
  hardware inversion. It is the largest single piece; KOReader's sunxi framebuffer
  code is the reference.
- **Colour**: the panel needs 32 bpp and BGRA writes, a second pixel path next to
  `copy_to_panel`. On hwtcon, night mode is global
  (`echo "night_mode 4" > /proc/hwtcon/cmd`).
- **Three-channel frontlights**: the Aura ONE and H2O² mix three LED channels instead
  of a white level and a warmth mixer.
- **Wi-Fi drivers**: there are three driver families (sdio, NXP `moal`, MTK `wmt`);
  KOReader's `enable-wifi.sh` is the reference when a model's module does not load.
- **Suspend on MediaTek**: never while charging, the kernel hangs
  (`Model::suspend_on_charger`).

## Risks

- **Nickel coexistence.** Every exit has to bring Nickel back cleanly; the launcher
  reboots instead when Nickel was not running before or KFMon asks for it, as KOReader
  does. Kobo firmware 5.x changed the start-up scripts, and NickelMenu needs 4.x, so
  the Tolinos on 5.x start rinky over SSH (`docs/install.md`, "Tolino").
- **Real hardware differs from the emulator.** The emulator checks the kernel's
  answers, not ghosting, refresh speed or touch noise. The snow protocol and the
  swap and mirror values came from the emulator's touch model; KOReader's model
  table agrees for the Clara BW.
