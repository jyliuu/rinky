//! OPDS catalogs: a catalog list in the settings, feed browsing, search and downloads
//! into the library.
//!
//! - Settings page "OPDS catalogs": the saved catalogs (setting `catalogs`, a list of
//!   `{title, url[, user, password]}`), "Add a catalog" (URL, then a title) and "Remove a
//!   catalog".
//! - A catalog opens as a menu: Back, Search (when the feed has an OpenSearch
//!   description or a template), the entries (navigation entries open the sub-feed, books
//!   open a menu of their download formats) and Next page.
//! - A download streams into `<download_dir>/<title>.<ext>.part` and is renamed when
//!   complete (never over an existing file); then the library is asked to rescan
//!   ([`UiHost::rescan_library`]). `download_dir` defaults to `/mnt/us/Books`
//!   (`[plugins.opds] download_dir` in `settings.toml` moves it).
//! - All requests run on the plugin network worker ([`HostCtx::start_request`]): the UI
//!   loop and the watchdog heartbeat keep going while feeds and books load.

use ink_net::opds::{self, OpdsEntry, OpdsFeed, SearchLink};
use ink_net::Request;
use ink_plugin::serde_json::{json, Value};
use ink_plugin::kit::{squeeze, Answer, Pending, Requests};
use ink_plugin::{HostCtx, HttpEvent, Permission, Plugin, PluginError, PluginManifest, Registrar, Result, StreamMode};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Plugin name.
pub const NAME: &str = "opds";
/// Default download folder.
pub const DEFAULT_DIR: &str = "/mnt/us/Books";

const CATALOGS: u32 = 1;
const IN_URL: u32 = 2;
const IN_TITLE: u32 = 3;
const FEED: u32 = 4;
const BOOK: u32 = 5;
const IN_SEARCH: u32 = 6;
const REMOVE: u32 = 7;

/// A saved catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    /// Name shown in the list.
    pub title: String,
    /// Feed URL.
    pub url: String,
    /// HTTP basic auth (optional, from the settings file).
    pub auth: Option<(String, String)>,
}

/// File extension for a book MIME type, or the extension of the URL path.
pub fn extension(mime: &str, href: &str) -> String {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let known = match m.as_str() {
        "application/epub+zip" => Some("epub"),
        "application/pdf" => Some("pdf"),
        "application/x-mobipocket-ebook" => Some("mobi"),
        "application/vnd.amazon.ebook" | "application/x-mobi8-ebook" => Some("azw3"),
        "application/x-fictionbook+xml" | "application/fb2" => Some("fb2"),
        "application/fb2+zip" | "application/x-zip-compressed-fb2" => Some("fb2.zip"),
        "application/vnd.comicbook+zip" | "application/x-cbz" => Some("cbz"),
        "text/plain" => Some("txt"),
        "text/html" | "application/xhtml+xml" => Some("html"),
        "application/rtf" => Some("rtf"),
        "image/vnd.djvu" => Some("djvu"),
        _ => None,
    };
    if let Some(k) = known {
        return k.into();
    }
    let path = href.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => ext.to_ascii_lowercase(),
        _ => "bin".into(),
    }
}

/// A file name from a book title: letters, digits and a few marks kept, the rest
/// replaced by `_`, at most 80 characters, no dot or space at either end (also where
/// the 80 characters end).
pub fn safe_name(title: &str) -> String {
    let s: String = title.chars().map(|c| if c.is_alphanumeric() || " -_.,()'&".contains(c) { c } else { '_' }).collect();
    let s: String = squeeze(&s).trim_matches(['.', ' ']).chars().take(80).collect();
    match s.trim_end_matches(['.', ' ']) {
        "" => "book".into(),
        s => s.into(),
    }
}

/// `dir/name.ext`, or `dir/name (2).ext` and so on when the file exists.
pub fn unique_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{name}.{ext}"));
    let mut n = 2;
    while p.exists() || p.with_extension(format!("{}.part", p.extension().and_then(|e| e.to_str()).unwrap_or(""))).exists() {
        p = dir.join(format!("{name} ({n}).{ext}"));
        n += 1;
    }
    p
}

enum Job {
    /// A feed; `push` adds it to the browse stack, else it replaces the top (next page).
    Feed { url: String, push: bool },
    /// An OpenSearch description, then a search for `terms`.
    OpenSearch { url: String, terms: String },
    /// A book download.
    Download { part: PathBuf, target: PathBuf, file: Option<std::fs::File>, bytes: u64 },
}

/// The OPDS plugin.
pub struct OpdsPlugin {
    manifest: PluginManifest,
    inflight: Requests<Job>,
    /// Open catalog: its auth and the feeds browsed (top = shown).
    auth: Option<(String, String)>,
    stack: Vec<OpdsFeed>,
    /// Book whose formats menu is shown.
    book: Option<OpdsEntry>,
    /// URL typed in "Add a catalog", waiting for its title.
    new_url: Option<String>,
}

impl Default for OpdsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

fn failed(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
}

impl OpdsPlugin {
    /// The plugin.
    pub fn new() -> Self {
        OpdsPlugin {
            manifest: PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Ui, Permission::Storage, Permission::Network]),
            inflight: Requests::default(),
            auth: None,
            stack: Vec::new(),
            book: None,
            new_url: None,
        }
    }

    /// The saved catalogs.
    pub fn catalogs(cx: &mut HostCtx) -> Vec<Catalog> {
        let Some(Value::Array(list)) = cx.setting("catalogs") else { return Vec::new() };
        let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        list.iter()
            .filter_map(|v| {
                let url = s(v, "url")?;
                let auth = match (s(v, "user"), s(v, "password")) {
                    (Some(u), Some(p)) if !u.is_empty() => Some((u, p)),
                    _ => None,
                };
                Some(Catalog { title: s(v, "title").unwrap_or_else(|| url.clone()), url, auth })
            })
            .collect()
    }

    fn save_catalogs(cx: &mut HostCtx, list: &[Catalog]) -> Result<()> {
        let v: Vec<Value> = list
            .iter()
            .map(|c| {
                let mut o = json!({"title": c.title, "url": c.url});
                if let Some((u, p)) = &c.auth {
                    o["user"] = json!(u);
                    o["password"] = json!(p);
                }
                o
            })
            .collect();
        cx.storage()?.set("catalogs", Value::Array(v))
    }

    fn download_dir(cx: &mut HostCtx) -> PathBuf {
        PathBuf::from(cx.setting_str("download_dir").unwrap_or_else(|| DEFAULT_DIR.into()))
    }

    fn show_catalogs(&mut self, cx: &mut HostCtx) -> Result<()> {
        let list = Self::catalogs(cx);
        let mut items: Vec<(String, String)> = list.iter().enumerate().map(|(i, c)| (c.title.clone(), format!("c{i}"))).collect();
        items.push(("Add a catalog".into(), "add".into()));
        if !list.is_empty() {
            items.push(("Remove a catalog".into(), "remove".into()));
        }
        cx.menu(CATALOGS, "OPDS catalogs", items)
    }

    fn request(&self, url: &str, accept: &str) -> Request {
        let r = Request::get(url).header("Accept", accept);
        match &self.auth {
            Some((u, p)) => r.basic_auth(u, p),
            None => r,
        }
    }

    fn start(&mut self, cx: &mut HostCtx, req: Request, mode: StreamMode, job: Job) -> Result<()> {
        log::info!("opds: GET {}", req.url);
        self.inflight.start(cx, req, mode, job)?;
        Ok(())
    }

    fn fetch_feed(&mut self, cx: &mut HostCtx, url: &str, push: bool) -> Result<()> {
        let req = self.request(url, opds::OPDS_ACCEPT);
        // No "Loading" toast: a toast still showing would hold back the feed menu.
        self.start(cx, req, StreamMode::Whole { limit: 8 << 20 }, Job::Feed { url: url.to_string(), push })
    }

    fn show_feed(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some(feed) = self.stack.last() else { return self.show_catalogs(cx) };
        let mut items: Vec<(String, String)> = vec![("< Back".into(), "back".into())];
        if feed.search().is_some() {
            items.push(("Search".into(), "search".into()));
        }
        for (i, e) in feed.entries.iter().enumerate() {
            // "title\tauthors": the host shows the authors right-aligned in muted type.
            let label = if e.is_book() && !e.authors.is_empty() { format!("{}\t{}", e.title, e.authors.join(", ")) } else { e.title.clone() };
            items.push((label, format!("e{i}")));
        }
        if feed.next_url().is_some() {
            items.push(("Next page >".into(), "next".into()));
        }
        let title = if feed.title.is_empty() { "Catalog".to_string() } else { feed.title.clone() };
        cx.menu(FEED, &title, items)
    }

    fn show_book(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some(e) = self.book.as_ref() else { return self.show_feed(cx) };
        let mut items: Vec<(String, String)> = Vec::new();
        for (j, l) in e.acquisitions().iter().enumerate() {
            let ext = extension(l.final_mime(), &l.href).to_ascii_uppercase();
            let size = l.length.map(|n| format!(" ({} KB)", n.div_ceil(1024))).unwrap_or_default();
            let price = if l.is_free_acquisition() || l.price.is_none() { String::new() } else { " (not free)".into() };
            items.push((format!("Download {ext}{size}{price}"), format!("d{j}")));
        }
        if items.is_empty() {
            cx.ui()?.toast("This entry has no download");
        }
        items.push(("< Back".into(), "back".into()));
        let title = match &e.authors[..] {
            [] => e.title.clone(),
            a => format!("{} - {}", e.title, a.join(", ")),
        };
        cx.menu(BOOK, &title, items)
    }

    fn download(&mut self, cx: &mut HostCtx, j: usize) -> Result<()> {
        let Some(e) = self.book.clone() else { return Ok(()) };
        let acq = e.acquisitions();
        let Some(link) = acq.get(j) else { return Ok(()) };
        let dir = Self::download_dir(cx);
        std::fs::create_dir_all(&dir).map_err(|err| failed(format!("cannot create {}: {err}", dir.display())))?;
        let target = unique_path(&dir, &safe_name(&e.title), &extension(link.final_mime(), &link.href));
        let part = PathBuf::from(format!("{}.part", target.display()));
        let file = std::fs::File::create(&part).map_err(|err| failed(format!("cannot write {}: {err}", part.display())))?;
        let req = self.request(&link.href, "*/*");
        if let Err(err) = self.start(cx, req, StreamMode::Chunks, Job::Download { part: part.clone(), target: target.clone(), file: Some(file), bytes: 0 }) {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
        let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        cx.ui()?.toast(&format!("Downloading {name}..."));
        Ok(())
    }

    fn search(&mut self, cx: &mut HostCtx, terms: &str) -> Result<()> {
        let Some(link) = self.stack.last().and_then(|f| f.search()) else { return Ok(()) };
        match link {
            SearchLink::Template(t) => {
                let url = opds::fill_template(&t, terms);
                self.fetch_feed(cx, &url, true)
            }
            SearchLink::Description(url) => {
                let req = self.request(&url, "application/opensearchdescription+xml, */*;q=0.5");
                self.start(cx, req, StreamMode::Whole { limit: 1 << 20 }, Job::OpenSearch { url, terms: terms.to_string() })
            }
        }
    }

    fn feed_pick(&mut self, cx: &mut HostCtx, pick: &str) -> Result<()> {
        match pick {
            "" => {
                self.stack.clear();
                Ok(())
            }
            "back" => {
                self.stack.pop();
                self.show_feed(cx)
            }
            "search" => {
                cx.ui()?.input(IN_SEARCH, "Search the catalog", "");
                Ok(())
            }
            "next" => match self.stack.last().and_then(|f| f.next_url()).map(str::to_string) {
                Some(u) => self.fetch_feed(cx, &u, false),
                None => self.show_feed(cx),
            },
            p => {
                let Some(i) = p.strip_prefix('e').and_then(|n| n.parse::<usize>().ok()) else { return Ok(()) };
                let Some(e) = self.stack.last().and_then(|f| f.entries.get(i)).cloned() else { return Ok(()) };
                if e.is_book() {
                    self.book = Some(e);
                    self.show_book(cx)
                } else if let Some(nav) = e.navigation() {
                    let href = nav.href.clone();
                    self.fetch_feed(cx, &href, true)
                } else {
                    cx.ui()?.toast("Nothing to open here");
                    self.show_feed(cx)
                }
            }
        }
    }

    fn finish(&mut self, cx: &mut HostCtx, f: Pending<Job>, body: Answer) -> Result<()> {
        // Only a 2xx answer has a usable body.
        let body = match body {
            Ok(b) if f.success() => Ok(b),
            Ok(_) => Err(format!("server answered {}", f.status)),
            Err(e) => Err(e),
        };
        match f.job {
            Job::Feed { url, push } => {
                match body.and_then(|b| opds::parse_any(&b, &f.content_type, &url).map_err(|e| e.to_string())) {
                    Ok(feed) => {
                        if !push {
                            self.stack.pop();
                        }
                        self.stack.push(feed);
                        self.show_feed(cx)
                    }
                    Err(e) => {
                        log::warn!("opds: {url}: {e}");
                        cx.ui()?.toast(&format!("Cannot load the catalog: {e}"));
                        self.show_feed(cx)
                    }
                }
            }
            Job::OpenSearch { url, terms } => {
                let template = body.ok().and_then(|b| {
                    let urls = opds::parse_opensearch(&String::from_utf8_lossy(&b), &url).unwrap_or_default();
                    opds::pick_search_template(&urls).map(str::to_string)
                });
                match template {
                    Some(t) => {
                        let u = opds::fill_template(&t, &terms);
                        self.fetch_feed(cx, &u, true)
                    }
                    None => {
                        cx.ui()?.toast("This catalog has no usable search");
                        self.show_feed(cx)
                    }
                }
            }
            Job::Download { part, target, file, bytes } => {
                drop(file);
                if let Err(why) = body {
                    let _ = std::fs::remove_file(&part);
                    log::warn!("opds: download {} failed: {why}", target.display());
                    cx.ui()?.toast(&format!("Download failed: {why}"));
                    return Ok(());
                }
                // Never replace a book that appeared meanwhile.
                let target = if target.exists() {
                    let stem = target.file_stem().and_then(|s| s.to_str()).unwrap_or("book").to_string();
                    let ext = target.extension().and_then(|s| s.to_str()).unwrap_or("bin").to_string();
                    unique_path(target.parent().unwrap_or(Path::new(".")), &stem, &ext)
                } else {
                    target
                };
                std::fs::rename(&part, &target).map_err(|e| failed(format!("cannot rename {}: {e}", part.display())))?;
                log::info!("opds: downloaded {} ({bytes} bytes)", target.display());
                let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                let ui = cx.ui()?;
                ui.rescan_library();
                ui.toast(&format!("Downloaded {name} to the library"));
                Ok(())
            }
        }
    }
}

impl Plugin for OpdsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.settings_page("catalogs", "OPDS catalogs");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            "settings:catalogs" | "catalogs" => {
                self.stack.clear();
                self.show_catalogs(cx)
            }
            _ => Ok(()),
        }
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match id {
            CATALOGS => match result {
                "" => Ok(()),
                "add" => {
                    cx.ui()?.input(IN_URL, "Catalog URL", "https://");
                    Ok(())
                }
                "remove" => {
                    let list = Self::catalogs(cx);
                    cx.menu(REMOVE, "Remove which catalog?", list.into_iter().enumerate().map(|(i, c)| (c.title, format!("r{i}"))))
                }
                c => {
                    let Some(i) = c.strip_prefix('c').and_then(|n| n.parse::<usize>().ok()) else { return Ok(()) };
                    let Some(cat) = Self::catalogs(cx).into_iter().nth(i) else { return Ok(()) };
                    self.auth = cat.auth.clone();
                    self.stack.clear();
                    self.fetch_feed(cx, &cat.url, true)
                }
            },
            REMOVE => {
                if let Some(i) = result.strip_prefix('r').and_then(|n| n.parse::<usize>().ok()) {
                    let mut list = Self::catalogs(cx);
                    if i < list.len() {
                        let c = list.remove(i);
                        Self::save_catalogs(cx, &list)?;
                        cx.ui()?.toast(&format!("Removed {}", c.title));
                    }
                }
                self.show_catalogs(cx)
            }
            IN_URL => {
                let url = result.trim();
                let valid = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")).is_some_and(|rest| !rest.is_empty());
                if valid {
                    self.new_url = Some(url.to_string());
                    cx.ui()?.input(IN_TITLE, "Catalog name", "");
                    Ok(())
                } else {
                    if !url.is_empty() && url != "https://" {
                        cx.ui()?.toast("The URL must start with http:// or https://");
                    }
                    self.show_catalogs(cx)
                }
            }
            IN_TITLE => {
                if let Some(url) = self.new_url.take() {
                    let title = if result.trim().is_empty() { url.clone() } else { result.trim().to_string() };
                    let mut list = Self::catalogs(cx);
                    list.push(Catalog { title, url, auth: None });
                    Self::save_catalogs(cx, &list)?;
                }
                self.show_catalogs(cx)
            }
            FEED => self.feed_pick(cx, result),
            BOOK => match result {
                p if p.starts_with('d') => {
                    let j = p[1..].parse::<usize>().unwrap_or(usize::MAX);
                    self.download(cx, j)?;
                    self.show_book(cx)
                }
                "back" => {
                    self.book = None;
                    self.show_feed(cx)
                }
                // Cancelled (back key, tap outside): leave the catalog.
                _ => {
                    self.book = None;
                    self.stack.clear();
                    Ok(())
                }
            },
            IN_SEARCH => {
                if result.trim().is_empty() {
                    self.show_feed(cx)
                } else {
                    self.search(cx, result.trim())
                }
            }
            _ => Ok(()),
        }
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        if let HttpEvent::Chunk(data) = ev {
            let Some(Job::Download { file: Some(file), bytes, part, .. }) = self.inflight.get_mut(id).map(|f| &mut f.job) else { return Ok(()) };
            if let Err(e) = file.write_all(data) {
                let msg = format!("cannot write {}: {e}", part.display());
                let _ = std::fs::remove_file(&*part);
                cx.cancel_request(id);
                self.inflight.remove(id);
                cx.ui()?.toast(&format!("Download failed: {msg}"));
                return Ok(());
            }
            *bytes += data.len() as u64;
            return Ok(());
        }
        let Some((mut f, body)) = self.inflight.event(id, ev) else { return Ok(()) };
        if let (Job::Download { file: Some(file), .. }, Ok(_)) = (&mut f.job, &body) {
            file.flush().map_err(failed)?;
            file.sync_all().ok();
        }
        self.finish(cx, f, body)
    }
}

#[cfg(test)]
mod tests;
