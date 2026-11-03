# plugins/

First-party plugins, one Rust crate per directory (`plugins/<name>/Cargo.toml`), built
against `ink-plugin`. Each plugin crate is a workspace member through the `plugins/*`
glob in the root `Cargo.toml`, and is compiled in by one line in
`crates/ink-app/src/plugins.rs`.

Planned (wave C): `insightful`, `dictionary`, `stats`, `vocab`, `opds`, `kosync`,
`calibre`, `wikipedia`. Device access: `usbnet` (USB network), `ssh` (SSH server).

User plugins written in Lua live on the device in `/mnt/us/rinky/plugins/<name>/`
(`plugin.toml` + `main.lua`), not here. See `docs/ARCHITECTURE.md`, "Plugins".
