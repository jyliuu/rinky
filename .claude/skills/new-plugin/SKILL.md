---
name: new-plugin
description: Add a rinky plugin - a compiled-in Rust plugin crate under plugins/<name> registered in crates/ink-app/src/plugins.rs, or a Lua plugin (plugin.toml + main.lua) like plugins/sample-lua - with its tests. Use when asked to add, create, write or register a plugin, a reader menu entry, selection action, gesture action, control-center tile or library action provided by a plugin.
argument-hint: "[plugin-name]"
---

# New plugin: $ARGUMENTS

The plugin API is one trait, `ink_plugin::Plugin` (`crates/ink-plugin/src/lib.rs`),
for both kinds. Read `docs/plugins.md` sections "Registries", "Rust plugins" and
"Testing" before writing code; it is the reference for every call named below.

Existing plugins: `kosync`, `opds`, `calibre`, `dictionary`, `stats` (crate
`ink-stats`), `insightful`, `wikipedia`, `usbnet`, `ssh`, and `sample-lua` (Lua).

## Rust or Lua?

- **Rust (compiled in)**: first-party features that ship in the binary, need threads,
  raw sockets, device access or speed. Lives in `plugins/<name>/`.
- **Lua (user plugin)**: small features users install themselves in
  `/mnt/us/rinky/plugins/<name>/`. In this repo only the sample lives in
  `plugins/sample-lua/` (a crate that embeds and tests it).

## Rust plugin

1. Name: `[a-z0-9_-]`, at most 64 characters, not taken (the list above).
2. Create `plugins/<name>/Cargo.toml`, copying `plugins/wikipedia/Cargo.toml`:
   `version.workspace = true` (and edition, license, rust-version, publish),
   `[lib] doctest = false, path = "lib.rs"`, `ink-plugin.workspace = true`, and only
   the other dependencies you need (`ink-net` for HTTP, `log`). Tests that fake a
   document need `ink-doc` as a dev-dependency.
3. Create `plugins/<name>/lib.rs` with a type implementing `Plugin`:
   `manifest()` returns a `PluginManifest::new("<name>", env!("CARGO_PKG_VERSION"), &[permissions])`;
   `on_load(reg)` registers entries (`reg.menu_item`, `reg.selection_action`,
   `reg.action`, `reg.settings_page`, `reg.library_action`, `reg.status_widget`, ...);
   `on_action(cx, id)` handles them; UI answers arrive in `on_ui_result`, HTTP events
   in `on_http`, worker-thread results in `on_tick` (with `tick_interval`). Use the
   `HostCtx` accessors (`cx.ui()?`, `cx.document()?`, `cx.storage()?`, `cx.http()`)
   so the manifest permissions hold.
   Patterns to copy: `plugins/wikipedia` (network, selection action, cards, retry),
   `plugins/calibre` (own threads with `on_tick`), `plugins/usbnet` and `plugins/ssh`
   (device access behind a small trait so tests run on the host).
4. Errors the user can fix are toasts or cards, not `Err`: five `Err`s disable a
   plugin. A selection action id must not be `lookup` (the dictionary owns it).
5. Put tests in `plugins/<name>/tests.rs` (`#[cfg(test)] mod tests;` in lib.rs):
   a fake `DocumentAccess`, `HostCtx::new(&mut UiQueue, &mut MemoryStorage, &perms)`,
   and for HTTP a fake `NetHost` that records requests; call `on_http` by hand. No
   threads, no network, no real device files. `plugins/wikipedia/tests.rs` shows all
   of it.
6. Register it:
   - root `Cargo.toml` `[workspace.dependencies]`: `<name> = { path = "plugins/<name>" }`
     (the crate is already a workspace member through `plugins/*`);
   - `crates/ink-app/Cargo.toml`: `<name>.workspace = true`;
   - `crates/ink-app/src/plugins.rs`: one line `Box::new(<name>::<Type>::new()),`
     in `compiled_in()`.
   `plugins::tests::list_loads` then checks that it loads without errors.
7. Docs: a section in `docs/user-guide.md` "5. Plugins" if users see it, the plugin
   table at the top of `docs/plugins.md`, and a `CHANGELOG.md` entry under the
   unreleased heading (`### Added`).

## Lua plugin

1. `plugin.toml` (`name` = folder name, `version`, `description`, `permissions` from
   `document`, `ui`, `storage`, `network`; optional `hosts`, `time_limit_ms`,
   `memory_mb`) and `main.lua` returning a table of hooks. Copy
   `plugins/sample-lua/plugin.toml` and `main.lua`.
2. Test it through the real Lua host: `ink_plugin::lua::LuaPlugin::from_source(manifest,
   dir, source)` with a fake `DocumentAccess`, as in `plugins/sample-lua/lib.rs` and
   `crates/ink-plugin/src/lua/tests.rs`. A new first-party Lua sample needs its own
   crate like `plugins/sample-lua` (every folder in `plugins/` must be a crate).
3. Simulator: copy the two files to `$TMPDIR/rinky-sim/plugins/<name>/`, then
   `cargo run -p ink-app -- sim testdata/juliet.epub --headless --script <script> --out OUT`
   (`plugins/sample-lua/sim.txt` is an example script) and look at the PNGs.

## Checks

```sh
cargo clippy -p <crate-name> --all-targets -- -D warnings
cargo test -p <crate-name>
cargo test -p ink-app --lib plugins::tests::list_loads
cargo test -p ink-app plugin_host            # host and overlay tests
scripts/test-fast.sh
```

Emulator (optional, needs a slot): `scripts/e2e/run.sh NAME OUTDIR 'plugin-*'` runs the
sample and broken-plugin scenarios; a new device-facing plugin may add its own
`tests/e2e/plugin-<name>.sh`.

## Done when

- The plugin loads (`list_loads` passes), its tests pass without network or device,
  clippy is clean, and `scripts/test-fast.sh` passes.
- Its entries appear where registered (sim screenshot or a screen test) and the user
  guide and changelog say what it does.
- If a step fails twice for a reason outside the plugin (a host API missing), stop and
  report what host change is needed instead of working around it in `ink-app`.
