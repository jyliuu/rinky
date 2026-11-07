# plugins/

First-party plugins, one Rust crate per directory (`plugins/<name>/Cargo.toml`), built
against `ink-plugin`. The `plugins/*` glob in the root `Cargo.toml` makes each one a
workspace member, and one line in `crates/ink-app/src/plugins.rs` compiles it in.

| Directory | Plugin |
| --- | --- |
| `calibre` | Calibre wireless: receive books from calibre over Wi-Fi |
| `dictionary` | StarDict look-up from a text selection |
| `insightful` | AI questions about the open book ([README](insightful/README.md)) |
| `kosync` | KOReader progress sync |
| `opds` | OPDS catalogs: browse and download books |
| `stats` | crate `ink-stats`: reading statistics and the vocabulary list |
| `wikipedia` | Wikipedia summaries from a text selection |
| `usbnet` | USB network (USB Ethernet gadget, Kindle) |
| `ssh` | SSH server (bundled dropbear, Kindle) |
| `sample-lua` | not a plugin: embeds and tests the sample Lua plugin |

User plugins written in Lua live on the device in `/mnt/us/rinky/plugins/<name>/`
(`plugin.toml` and `main.lua`), not here. [`docs/plugins.md`](../docs/plugins.md)
explains how to write either kind, and `docs/ARCHITECTURE.md` ("How plugins register")
how they are wired in.
