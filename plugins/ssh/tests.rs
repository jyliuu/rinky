//! Arguments, the key copy, and the server's life with a shell script as dropbear and a
//! fake iptables and interface list.

use super::*;
use ink_plugin::testing::TempDir;
use ink_plugin::{MemoryStorage, Registrar, UiQueue, UiRequest};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

const PERMS: [Permission; 2] = [Permission::Ui, Permission::Storage];

fn tmp(tag: &str) -> TempDir {
    TempDir::new(&format!("ssh-{tag}"))
}

/// Two key folder candidates, `root/a/keys` and `root/b/keys` (parents exist); no
/// iptables path exists, so `iptables` goes to the fake.
fn paths(root: &Path) -> Paths {
    for d in ["a", "b"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    Paths { dropbear: root.join("bin/dropbearmulti"), ssh_dir: root.join("ssh"), key_dirs: vec![root.join("a/keys"), root.join("b/keys")], iptables: vec![root.join("no/iptables")] }
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap()
}

/// A fake dropbear: logs its arguments, then `body` (shell).
fn script(p: &Paths, body: &str) {
    fs::create_dir_all(p.dropbear.parent().unwrap()).unwrap();
    fs::write(&p.dropbear, format!("#!/bin/sh\necho \"args: $*\" >&2\n{body}\n")).unwrap();
    fs::set_permissions(&p.dropbear, fs::Permissions::from_mode(0o755)).unwrap();
}

/// What the fake system saw and holds.
#[derive(Default)]
struct World {
    rules: BTreeSet<u16>,
    iptables_calls: Vec<Vec<String>>,
    spawns: Vec<Vec<String>>,
    addrs: Vec<(String, Ipv4Addr)>,
    no_iptables: bool,
    /// `dropbearkey` argument lists; each run makes the `-f` file unless `keygen_fails`.
    keygens: Vec<Vec<String>>,
    keygen_fails: bool,
    /// Folders (and everything below them) that are not root-only.
    untrusted: Vec<PathBuf>,
}

struct Fake(Arc<Mutex<World>>);

impl System for Fake {
    fn spawn(&mut self, program: &Path, args: &[String], log: File) -> io::Result<Child> {
        self.0.lock().unwrap().spawns.push(args.to_vec());
        // A script written a moment ago can be busy (ETXTBSY) while another test thread
        // forks; try again.
        let mut tries = 0;
        loop {
            match spawn_logged(program, args, log.try_clone()?) {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 20 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                r => return r,
            }
        }
    }
    fn run(&mut self, program: &str, args: &[String]) -> io::Result<bool> {
        let mut w = self.0.lock().unwrap();
        if args.first().map(String::as_str) == Some("dropbearkey") {
            assert!(program.ends_with("dropbearmulti"), "{program}");
            w.keygens.push(args.to_vec());
            if w.keygen_fails {
                return Ok(false);
            }
            fs::write(args.last().unwrap(), b"key").unwrap();
            return Ok(true);
        }
        assert_eq!(program, "iptables");
        if w.no_iptables {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no iptables"));
        }
        w.iptables_calls.push(args.to_vec());
        let port: u16 = args[5].parse().unwrap();
        Ok(match args[0].as_str() {
            "-C" => w.rules.contains(&port),
            "-A" => {
                w.rules.insert(port);
                true
            }
            "-D" => w.rules.remove(&port),
            _ => false,
        })
    }
    fn addresses(&mut self) -> Vec<(String, Ipv4Addr)> {
        self.0.lock().unwrap().addrs.clone()
    }
    fn trusted(&mut self, path: &Path) -> bool {
        !self.0.lock().unwrap().untrusted.iter().any(|u| path.starts_with(u))
    }
}

struct Rig {
    root: TempDir,
    paths: Paths,
    world: Arc<Mutex<World>>,
    plugin: SshPlugin,
    ui: UiQueue,
    store: MemoryStorage,
}

impl Rig {
    fn new(tag: &str, body: &str) -> Rig {
        let root = tmp(tag);
        let paths = paths(&root);
        script(&paths, body);
        let world = Arc::new(Mutex::new(World::default()));
        world.lock().unwrap().addrs = vec![("lo".into(), Ipv4Addr::LOCALHOST), ("wlan0".into(), Ipv4Addr::new(10, 0, 0, 7)), ("usb0".into(), Ipv4Addr::new(192, 168, 15, 244))];
        let plugin = SshPlugin::with_system(paths.clone(), Box::new(Fake(world.clone())));
        Rig { root, paths, world, plugin, ui: UiQueue::default(), store: MemoryStorage::default() }
    }
    fn action(&mut self, id: &str) {
        self.plugin.on_action(&mut HostCtx::new(&mut self.ui, &mut self.store, &PERMS), id).unwrap();
    }
    fn answer(&mut self, id: u32, result: &str) {
        self.plugin.on_ui_result(&mut HostCtx::new(&mut self.ui, &mut self.store, &PERMS), id, result).unwrap();
    }
    fn tick(&mut self) {
        self.plugin.on_tick(&mut HostCtx::new(&mut self.ui, &mut self.store, &PERMS)).unwrap();
    }
    fn exit(&mut self) {
        self.plugin.on_exit(&mut HostCtx::new(&mut self.ui, &mut self.store, &PERMS), false).unwrap();
    }
    fn exit_for_restart(&mut self) {
        self.plugin.on_exit(&mut HostCtx::new(&mut self.ui, &mut self.store, &PERMS), true).unwrap();
    }
    fn toasts(&self) -> Vec<String> {
        self.ui.0.iter().filter_map(|r| if let UiRequest::Toast { text } = r { Some(text.clone()) } else { None }).collect()
    }
    fn last_menu(&self) -> Vec<(String, String)> {
        match self.ui.0.iter().rev().find(|r| matches!(r, UiRequest::Menu { .. })) {
            Some(UiRequest::Menu { items, .. }) => items.clone(),
            _ => panic!("no menu: {:?}", self.ui.0),
        }
    }
    fn rules(&self) -> BTreeSet<u16> {
        self.world.lock().unwrap().rules.clone()
    }
}


/// A process that is still alive (not a zombie we already reaped).
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks the pid.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

// -- Pieces --------------------------------------------------------------------------------

#[test]
fn argument_list_with_and_without_no_password() {
    let p = Paths { dropbear: "/b/dropbearmulti".into(), ssh_dir: "/u/ssh".into(), key_dirs: vec![], iptables: vec![] };
    let dir = Path::new("/var/run/rinky-ssh");
    let args = dropbear_args(&p, dir, 22, false);
    assert_eq!(
        args.join(" "),
        "dropbear -F -E -p 22 -r /u/ssh/dropbear_ed25519_host_key -r /u/ssh/dropbear_ecdsa_host_key -D /var/run/rinky-ssh -P /var/run/rinky-ssh/dropbear.pid -s"
    );
    let args = dropbear_args(&p, dir, 2200, true);
    assert_eq!(args.last().map(String::as_str), Some("-n"));
    assert!(!args.iter().any(|a| a == "-s" || a == "-w" || a == "-R"));
    assert!(args.windows(2).any(|w| w == ["-p", "2200"]));
    assert_eq!(firewall_args("-C", 22).join(" "), "-C INPUT -p tcp --dport 22 -j ACCEPT");
    assert_eq!(keygen_args(&p.ecdsa_key(), &["-t", "ecdsa", "-s", "256"]).join(" "), "dropbearkey -t ecdsa -s 256 -f /u/ssh/dropbear_ecdsa_host_key");
    assert_eq!(p.iptables_program(), "iptables", "nothing listed exists: PATH");
    let k = Paths::kindle();
    assert_eq!(k.key_dirs[0], Path::new("/var/run/rinky-ssh"));
    if std::env::var_os("RINKY_HOME").is_none() {
        assert_eq!(k.dropbear, Path::new("/mnt/us/rinky/bin/dropbearmulti"));
        assert_eq!(k.host_key(), Path::new("/mnt/us/rinky/ssh/dropbear_ed25519_host_key"));
    }
}

#[test]
fn keys_are_copied_root_only_and_a_template_is_made() {
    let root = tmp("keys");
    let p = paths(&root);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let dir = choose_key_dir(&p, &mut |_| true).unwrap();
    assert_eq!(dir, canon(&root.join("a/keys")));
    assert_eq!(mode(&dir), 0o700);
    // Missing source: the folder and a commented file are created; no keys.
    assert_eq!(install_keys(&p, &dir).unwrap(), 0);
    assert_eq!(fs::read_to_string(p.user_keys()).unwrap(), KEYS_TEMPLATE);
    assert_eq!(mode(&dir.join("authorized_keys")), 0o600);
    // A loose folder and file (as on vfat) are tightened; the keys are copied.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
    fs::set_permissions(dir.join("authorized_keys"), fs::Permissions::from_mode(0o666)).unwrap();
    let text = "# mine\nssh-ed25519 AAAA one@mac\n\n  ssh-rsa BBBB two@pc\n";
    fs::write(p.user_keys(), text).unwrap();
    let dir = choose_key_dir(&p, &mut |_| true).unwrap();
    assert_eq!(install_keys(&p, &dir).unwrap(), 2);
    assert_eq!(fs::read_to_string(dir.join("authorized_keys")).unwrap(), text);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("authorized_keys")), 0o600);
    assert!(!dir.join("authorized_keys.new").exists());
    // A file in the folder's place is replaced by the folder.
    fs::remove_dir_all(&dir).unwrap();
    fs::write(&dir, b"x").unwrap();
    assert_eq!(choose_key_dir(&p, &mut |_| true), Some(dir.clone()));
    assert!(dir.is_dir());
    // Removing it leaves the parent.
    install_keys(&p, &dir).unwrap();
    fs::write(dir.join("dropbear.pid"), b"1").unwrap();
    remove_key_dir(&dir);
    assert!(!dir.exists() && root.join("a").is_dir());
}

#[test]
fn key_dir_skips_untrusted_chains() {
    let root = tmp("trust");
    let mut p = paths(&root);
    let a = canon(&root.join("a"));
    // An untrusted parent: the next candidate; the first is not even created.
    let got = choose_key_dir(&p, &mut |d| !d.starts_with(&a));
    assert_eq!(got, Some(canon(&root.join("b/keys"))));
    assert!(!root.join("a/keys").exists());
    // A symlinked parent is judged by where it points.
    std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();
    p.key_dirs = vec![root.join("link/keys"), root.join("b/keys")];
    assert_eq!(choose_key_dir(&p, &mut |d| !d.starts_with(&a)), Some(canon(&root.join("b/keys"))));
    assert_eq!(choose_key_dir(&p, &mut |_| true), Some(canon(&root.join("a/keys"))));
    // An untrusted folder far up the chain rules out everything; a missing parent too.
    let top = canon(&root);
    assert_eq!(choose_key_dir(&p, &mut |d| !top.starts_with(d) || d != top.parent().unwrap()), None);
    p.key_dirs = vec![root.join("none/keys")];
    assert_eq!(choose_key_dir(&p, &mut |_| true), None);
    // The real check: a temp folder is not owned by root here (tests do not run as root).
    // SAFETY: getuid has no preconditions.
    if unsafe { libc::getuid() } != 0 {
        assert!(!root_only(&root));
    }
    assert!(root_only(Path::new("/")));
}

#[test]
fn log_trim_and_last_line() {
    let root = tmp("log");
    let log = root.join("dropbear.log");
    assert_eq!(last_log_line(&log), None);
    let mut text = String::new();
    while text.len() as u64 <= LOG_MAX {
        text.push_str("[1] Nov 02 10:00:00 Child connection from 192.168.15.201:50000\n");
    }
    text.push_str("[1] Nov 02 10:00:01 Early exit: Error listening: Address already in use\n\n");
    fs::write(&log, &text).unwrap();
    trim_log(&log);
    let after = fs::read_to_string(&log).unwrap();
    assert!(after.len() as u64 <= LOG_KEEP && after.starts_with("[1]"), "cut at a line start");
    assert_eq!(last_log_line(&log).as_deref(), Some("[1] Nov 02 10:00:01 Early exit: Error listening: Address already in use"));
}

#[test]
fn addresses_usb_first() {
    let all = vec![("lo".to_string(), Ipv4Addr::LOCALHOST), ("wlan0".into(), Ipv4Addr::new(10, 0, 0, 7)), ("usb0".into(), Ipv4Addr::new(192, 168, 15, 244))];
    assert_eq!(usable_addrs(&all), vec![("USB", Ipv4Addr::new(192, 168, 15, 244)), ("Wi-Fi", Ipv4Addr::new(10, 0, 0, 7))]);
    assert!(usable_addrs(&all[..1]).is_empty());
    // The real call works on this machine (at least lo).
    assert!(!interface_addrs().is_empty());
}

// -- The plugin ----------------------------------------------------------------------------

#[test]
fn registers_tile_and_settings_page() {
    let mut rig = Rig::new("reg", "exec sleep 30");
    let mut reg = ink_plugin::Registry::default();
    rig.plugin.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    let tile = reg.control_tiles.iter().find(|t| t.id == "toggle").expect("tile");
    assert_eq!((tile.label.as_str(), tile.icon.as_str()), ("SSH server", "terminal"));
    assert!(reg.settings_pages.iter().any(|s| s.id == "ssh" && s.title == "SSH server"));
    assert_eq!(rig.plugin.manifest().name, "ssh");
    assert_eq!(rig.plugin.tile_state("toggle"), Some(TileState { text: "Off".into(), on: Some(false) }));
    assert_eq!(rig.plugin.tile_state("x"), None);
}

#[test]
fn start_shows_address_and_stop_removes_the_rule() {
    let mut rig = Rig::new("start", "exec sleep 30");
    rig.action("toggle");
    assert!(rig.plugin.running(), "{:?}", rig.toasts());
    assert_eq!(rig.plugin.tile_state("toggle"), Some(TileState { text: "192.168.15.244:22".into(), on: Some(true) }));
    assert_eq!(rig.plugin.tick_interval(), Some(TICK));
    assert_eq!(rig.rules(), BTreeSet::from([22]));
    assert!(rig.toasts().iter().any(|t| t.starts_with("SSH server on 192.168.15.244:22") && t.contains("no keys")), "{:?}", rig.toasts());
    let key_dir = canon(&rig.paths.key_dirs[0]);
    assert_eq!(rig.world.lock().unwrap().spawns[0], dropbear_args(&rig.paths, &key_dir, 22, false));
    assert!(key_dir.join("authorized_keys").is_file() && rig.paths.user_keys().is_file());
    // Both host keys were made, once.
    assert_eq!(rig.world.lock().unwrap().keygens.len(), 2);
    assert!(rig.paths.host_key().is_file() && rig.paths.ecdsa_key().is_file());

    // Only wlan0: its address; no address at all: the port.
    rig.world.lock().unwrap().addrs.retain(|(n, _)| n != "usb0");
    rig.tick();
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "10.0.0.7:22");
    rig.world.lock().unwrap().addrs.clear();
    rig.tick();
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "Port 22");
    assert!(rig.plugin.running(), "still running after ticks");

    let pid = rig.plugin.pid().unwrap();
    rig.action("toggle");
    assert!(!rig.plugin.running());
    assert!(!alive(pid), "the process is gone");
    assert!(rig.rules().is_empty(), "the firewall rule is removed");
    assert!(!key_dir.exists(), "the key folder is removed");
    assert!(rig.root.join("a").is_dir(), "its parent stays");
    let calls: Vec<String> = rig.world.lock().unwrap().iptables_calls.iter().map(|c| c[0].clone()).collect();
    assert_eq!(calls, ["-C", "-A", "-D"]);
    assert_eq!(rig.plugin.tile_state("toggle"), Some(TileState { text: "Off".into(), on: Some(false) }));
    assert_eq!(rig.toasts().last().map(String::as_str), Some("SSH server stopped"));
}

#[test]
fn an_existing_rule_is_kept_and_iptables_may_be_missing() {
    let mut rig = Rig::new("rule", "exec sleep 30");
    rig.world.lock().unwrap().rules.insert(22);
    rig.action("toggle");
    rig.action("toggle");
    assert_eq!(rig.rules(), BTreeSet::from([22]), "a rule the plugin did not add stays");

    rig.world.lock().unwrap().no_iptables = true;
    rig.action("toggle");
    assert!(rig.plugin.running(), "no iptables is not fatal");
    rig.action("toggle");
    assert!(!rig.plugin.running());
}

#[test]
fn a_server_that_exits_is_noticed_on_tick() {
    let mut rig = Rig::new("exit", "echo '[9] Early exit: Error listening: Address already in use' >&2\nexit 1");
    rig.action("toggle");
    assert!(rig.plugin.running());
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.plugin.running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        rig.tick();
    }
    assert!(!rig.plugin.running());
    assert!(rig.rules().is_empty(), "the rule goes with the server");
    assert!(!rig.paths.key_dirs[0].exists(), "and the key folder");
    let last = rig.toasts().last().cloned().unwrap();
    assert!(last.starts_with("SSH server stopped:") && last.contains("Address already in use"), "{last}");
    assert_eq!(rig.plugin.tile_state("toggle"), Some(TileState { text: "Stopped".into(), on: Some(false) }));
    assert_eq!(rig.plugin.tick_interval(), None, "no ticks after the first one when stopped");
    // The log has the script's output.
    let log = fs::read_to_string(rig.paths.log()).unwrap();
    assert!(log.contains("args: dropbear -F -E -p 22 -r "), "{log}");
}

#[test]
fn a_missing_binary_is_a_toast_and_tile_text() {
    let mut rig = Rig::new("missing", "exit 0");
    fs::remove_file(&rig.paths.dropbear).unwrap();
    rig.action("toggle");
    assert!(!rig.plugin.running());
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "Not installed");
    assert!(rig.toasts()[0].contains("not installed"), "{:?}", rig.toasts());
    assert!(rig.world.lock().unwrap().spawns.is_empty());
}

#[test]
fn settings_changes_restart_a_running_server() {
    let mut rig = Rig::new("restart", "exec sleep 30");
    rig.action("settings:ssh");
    let items = rig.last_menu();
    assert_eq!(items[0].0, "Off");
    assert!(items.iter().any(|(l, a)| l == "Port\t22" && a == "port"));
    assert!(items.iter().any(|(l, _)| l == "Login without password\toff"));
    assert!(items.iter().any(|(l, _)| l == "Start when rinky starts\toff"));
    rig.answer(MENU, "start");
    assert!(rig.plugin.running());
    let items = rig.last_menu();
    assert!(items[0].0.starts_with("Running on 192.168.15.244:22 (USB), 10.0.0.7:22 (Wi-Fi)"), "{items:?}");
    assert!(items.iter().any(|(_, a)| a == "stop"));
    let first = rig.plugin.pid().unwrap();

    // Port: a bad value is refused, a good one restarts on the new port.
    rig.answer(MENU, "port");
    assert!(matches!(rig.ui.0.last(), Some(UiRequest::TextInput { id: IN_PORT, .. })));
    rig.answer(IN_PORT, "99999");
    assert_eq!(rig.plugin.pid(), Some(first));
    rig.answer(IN_PORT, "2222");
    assert_eq!(rig.store.settings.get("port"), Some(&json!(2222)));
    assert!(rig.toasts().iter().any(|t| t == "Port changed to 2222: restarting the SSH server"), "{:?}", rig.toasts());
    let second = rig.plugin.pid().unwrap();
    assert_ne!(first, second);
    assert!(!alive(first));
    assert_eq!(rig.rules(), BTreeSet::from([2222]));
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "192.168.15.244:2222");
    // The same port again: no restart.
    rig.answer(IN_PORT, "2222");
    assert_eq!(rig.plugin.pid(), Some(second));

    // Login without password: a dialog first; Cancel changes nothing.
    rig.answer(MENU, "no_password");
    assert!(matches!(rig.ui.0.last(), Some(UiRequest::Dialog { id: DLG_NO_PASSWORD, buttons, .. }) if buttons[1].1 == "on"));
    rig.answer(DLG_NO_PASSWORD, "cancel");
    assert_eq!(rig.plugin.pid(), Some(second));
    assert_eq!(rig.store.settings.get("no_password"), None);
    rig.answer(MENU, "no_password");
    rig.answer(DLG_NO_PASSWORD, "on");
    let third = rig.plugin.pid().unwrap();
    assert_ne!(second, third);
    assert_eq!(rig.world.lock().unwrap().spawns.last().unwrap().last().map(String::as_str), Some("-n"));
    assert!(rig.world.lock().unwrap().spawns[0].last().is_some_and(|a| a == "-s"), "keys only before");
    let items = rig.last_menu();
    assert!(items.iter().any(|(l, _)| l == "Warning: login without password is on"));
    assert!(items.iter().any(|(l, _)| l == "Login without password\ton"));
    // Off again: no dialog, restart without -n.
    rig.answer(MENU, "no_password");
    assert!(rig.plugin.pid() != Some(third));
    assert_ne!(rig.world.lock().unwrap().spawns.last().unwrap().last().map(String::as_str), Some("-n"));

    // Help names the address and port.
    rig.answer(MENU, "help");
    let Some(UiRequest::TextViewer { markdown, .. }) = rig.ui.0.last() else { panic!("{:?}", rig.ui.0) };
    assert!(markdown.contains("ssh -p 2222 root@192.168.15.244"), "{markdown}");
    assert!(markdown.contains("'cat >> "), "{markdown}");
    assert!(markdown.contains("scp -O -P 2222 book.epub root@192.168.15.244:/mnt/us/"), "{markdown}");
    assert!(markdown.contains("trust"), "{markdown}");

    // on_exit stops the server and removes the rule.
    let pid = rig.plugin.pid().unwrap();
    rig.exit();
    assert!(!rig.plugin.running() && !alive(pid));
    assert!(rig.rules().is_empty());
    assert!(!rig.paths.key_dirs[0].exists());
    // Host keys were made at the first start only.
    assert_eq!(rig.world.lock().unwrap().keygens.len(), 2);
}

#[test]
fn settings_changes_while_stopped_only_store() {
    let mut rig = Rig::new("stopped", "exec sleep 30");
    rig.answer(IN_PORT, "2200");
    rig.answer(MENU, "autostart");
    assert_eq!(rig.store.settings.get("autostart"), Some(&json!(true)));
    assert!(!rig.plugin.running());
    assert!(rig.world.lock().unwrap().spawns.is_empty());
    assert!(rig.last_menu().iter().any(|(l, _)| l == "Port\t2200"));
}

#[test]
fn autostart_once_after_load() {
    // Off: the first tick looks and stops asking.
    let mut rig = Rig::new("auto-off", "exec sleep 30");
    assert_eq!(rig.plugin.tick_interval(), Some(FIRST_TICK));
    rig.tick();
    assert!(!rig.plugin.running());
    assert_eq!(rig.plugin.tick_interval(), None);

    // On: the first tick starts the server; a later stop is not undone by ticks.
    let mut rig = Rig::new("auto-on", "exec sleep 30");
    rig.store.settings.insert("autostart".into(), json!(true));
    rig.store.settings.insert("port".into(), json!(2022));
    rig.tick();
    assert!(rig.plugin.running());
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "192.168.15.244:2022");
    rig.action("toggle");
    rig.tick();
    assert!(!rig.plugin.running());
}

#[test]
fn drop_stops_the_server() {
    let mut rig = Rig::new("drop", "exec sleep 30");
    rig.action("toggle");
    let pid = rig.plugin.pid().unwrap();
    let plugin = std::mem::replace(&mut rig.plugin, SshPlugin::with_system(rig.paths.clone(), Box::new(Fake(rig.world.clone()))));
    drop(plugin);
    assert!(!alive(pid));
    assert!(rig.rules().is_empty());
    assert!(!rig.paths.key_dirs[0].exists(), "Drop removes the key folder");
}

#[test]
fn only_missing_host_keys_are_made_and_a_failure_stops_the_start() {
    let mut rig = Rig::new("hostkeys", "exec sleep 30");
    fs::create_dir_all(&rig.paths.ssh_dir).unwrap();
    fs::write(rig.paths.host_key(), b"old").unwrap();
    rig.action("toggle");
    assert!(rig.plugin.running());
    let gens = rig.world.lock().unwrap().keygens.clone();
    assert_eq!(gens.len(), 1, "{gens:?}");
    assert_eq!(gens[0][..5].join(" "), "dropbearkey -t ecdsa -s 256");
    assert_eq!(fs::read(rig.paths.host_key()).unwrap(), b"old", "an existing key is kept");
    rig.action("toggle");

    fs::remove_file(rig.paths.ecdsa_key()).unwrap();
    rig.world.lock().unwrap().keygen_fails = true;
    rig.action("toggle");
    assert!(!rig.plugin.running());
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "No host key");
    assert!(rig.toasts().last().unwrap().starts_with("Cannot make the SSH host key"), "{:?}", rig.toasts());
    assert_eq!(rig.world.lock().unwrap().spawns.len(), 1);
}

#[test]
fn no_safe_key_folder_fails_the_start() {
    let mut rig = Rig::new("nosafe", "exec sleep 30");
    let top = canon(&rig.root);
    rig.world.lock().unwrap().untrusted = vec![top];
    rig.action("toggle");
    assert!(!rig.plugin.running());
    assert_eq!(rig.plugin.tile_state("toggle").unwrap().text, "No safe folder for keys");
    assert!(rig.toasts().last().unwrap().starts_with("No safe folder for keys"), "{:?}", rig.toasts());
    assert!(rig.world.lock().unwrap().spawns.is_empty());
    // Only the first is untrusted: the second is used.
    let a = canon(&rig.root.join("a"));
    rig.world.lock().unwrap().untrusted = vec![a];
    rig.action("toggle");
    assert!(rig.plugin.running());
    let args = rig.world.lock().unwrap().spawns[0].clone();
    let b = canon(&rig.root.join("b/keys"));
    assert!(args.windows(2).any(|w| w[0] == "-D" && Path::new(&w[1]) == b), "{args:?}");
}

#[test]
fn the_server_gets_default_signals_although_the_parent_blocks_sigterm() {
    // Like rinky's main thread (ink-device trace.rs): SIGTERM blocked. A spawned
    // child must still die from SIGTERM, or a stop could never end dropbear.
    let d = tmp("signals");
    let log = d.join("log");
    let child = std::thread::spawn(move || {
        // SAFETY: a local signal set, this thread's mask only.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let args = ["-c".to_string(), "kill -TERM $$; sleep 5; echo survived".to_string()];
        spawn_logged(Path::new("/bin/sh"), &args, File::create(&log).unwrap()).unwrap().wait().unwrap()
    })
    .join()
    .unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(child.signal(), Some(libc::SIGTERM));
    assert!(!fs::read_to_string(d.join("log")).unwrap().contains("survived"));
}

#[test]
fn a_restart_resumes_the_server_once() {
    let mut rig = Rig::new("resume", "exec sleep 30");
    rig.store.settings.insert("port".into(), json!(2022));
    rig.tick(); // the start tick (autostart off)
    rig.action("toggle");
    assert!(rig.plugin.running());
    rig.exit_for_restart();
    assert!(!rig.plugin.running());
    assert_eq!(rig.store.settings.get("resume"), Some(&json!(true)));

    // The next rinky: its first tick starts the server and clears the note.
    rig.plugin = SshPlugin::with_system(rig.paths.clone(), Box::new(Fake(rig.world.clone())));
    rig.tick();
    assert!(rig.plugin.running());
    assert_eq!(rig.store.settings.get("resume"), None);

    // A plain exit leaves no note: the start after it does not start the server.
    rig.exit();
    assert_eq!(rig.store.settings.get("resume"), None);
    rig.plugin = SshPlugin::with_system(rig.paths.clone(), Box::new(Fake(rig.world.clone())));
    rig.tick();
    assert!(!rig.plugin.running());
}
