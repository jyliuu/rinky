# packaging/

Sources of the Kindle install package. `scripts/package.sh` assembles them with the ARM
binary into `out/package/rinky-<version>-kindlehf.tar.gz` (see its header for the
layout). Owner: the `packaging` agent.

| File | Installed as | What it does |
| --- | --- | --- |
| `install.sh` | `/mnt/us/rinky/install.sh` | copies `rinky/` and `extensions/rinky/` from an unpacked package; `--upgrade` while rinky runs (in-app update) |
| `uninstall.sh` | `/mnt/us/rinky/uninstall.sh` | stops rinky, removes everything it added (`--keep-data`: program only) |
| `rinky-ctl.sh` | `/mnt/us/rinky/rinky-ctl.sh` | `start`, `stop` (Exit to Kindle), `status` |
| `kual/rinky/` | `/mnt/us/extensions/rinky/` | KUAL menu: start, exit to Kindle, update, uninstall |
| `kpm/` | root of the `.kpkg` | KPM manifest and hooks (`install.sh`, `launch.sh`, `uninstall.sh`), plus `repo.json`, the repository manifest `scripts/package.sh` fills in for a release |
| `README.txt` | `/mnt/us/rinky/README.txt` | short install/uninstall text for users |
| `test/host-install-test.sh` | - | install/uninstall cycle on the host under a fake `/mnt/us` |
| `test/mock-release-server.py` | - | GitHub-release-shaped server for the in-app update tests (`--slow` for overlapping updates) |
| `test/verify-hostile.sh` | - | verifier's host attacks V1-V9 on install.sh and uninstall.sh |
| `test/make-variant.sh` | - | a release whose binary and launcher differ (makes a mixed install visible) |
| `test/guest-aside.sh`, `test/e2e-fb.sh`, `test/guest-list.sh`, `test/compare-lists.py` | - | helpers of the `tests/e2e/packaging-*.sh` scenarios |

Rules the scripts keep:

- All or nothing: `install.sh` first copies every file to `<target>.new<pid>`, and
  renames the copies into place only when every copy succeeded (VERSION, FILES,
  SHA256SUMS and `.install-state` last). A failed copy (no space) deletes the copies and
  leaves the old version. The renames let an update replace the running binary and
  launcher.
- One install at a time (`/mnt/us/rinky/.install.lock`) and one in-app update at a
  time (`flock` on `/mnt/us/rinky/.update.lock`); `uninstall.sh` refuses while
  either names a live process.
- `install.sh` writes only the files in the package's `FILES`; user data in
  `/mnt/us/rinky` (settings, library index, plugins, caches, logs) is kept.
- What the installer creates outside `/mnt/us/rinky` is recorded in
  `/mnt/us/rinky/.install-state` (`created=extensions`), and `uninstall.sh` removes
  exactly that.
- rinky may write outside `/mnt/us/rinky` only as `<book>.sdr/rinky*` next to
  books. `uninstall.sh` removes those files and the `.sdr` folder when it is empty
  afterwards, unless the folder is listed in `/mnt/us/rinky/.install-sdr` (the
  `.sdr` folders that existed before rinky first ran); KOReader's files there are
  never touched. `/mnt/us/koreader` is never read or written.
- `RINKY_PREFIX=DIR` moves `/mnt/us` under `DIR` and `RINKY_RUN` moves
  `/tmp/rinky`, for host tests.
