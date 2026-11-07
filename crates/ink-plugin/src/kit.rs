//! Small helpers the compiled-in plugins share: typed reads of plugin settings, menus
//! built from owned labels, a table of HTTP requests in flight ([`Requests`]), and text
//! for the Markdown viewer and cards.
//!
//! The setting and menu helpers are inherent methods of [`HostCtx`] and go through its
//! checked accessors, so a plugin without the `storage` permission reads every setting
//! as unset.

use crate::host::{HostCtx, UiRequest};
use crate::net::{HttpEvent, StreamMode};
use crate::Result;
use ink_net::Request;
use serde_json::Value;
use std::collections::HashMap;

impl HostCtx<'_> {
    /// The plugin setting `key`; `None` when it is unset or the plugin has no `storage`
    /// permission.
    pub fn setting(&mut self, key: &str) -> Option<Value> {
        self.storage().ok()?.get(key)
    }

    /// The setting `key` as a boolean, `default` when it is unset or not a boolean.
    pub fn setting_bool(&mut self, key: &str, default: bool) -> bool {
        self.setting(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    /// The setting `key` as a string (as stored, not trimmed); `None` when it is unset or
    /// not a string.
    pub fn setting_str(&mut self, key: &str) -> Option<String> {
        match self.setting(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The setting `key` as an unsigned integer; `None` when it is unset or not one.
    pub fn setting_u64(&mut self, key: &str) -> Option<u64> {
        self.setting(key)?.as_u64()
    }

    /// Opens a menu of `(label, action)` items (owned or borrowed strings), see
    /// [`UiRequest::Menu`]. Needs the `ui` permission.
    pub fn menu<L: Into<String>, A: Into<String>>(&mut self, id: u32, title: &str, items: impl IntoIterator<Item = (L, A)>) -> Result<()> {
        let items = items.into_iter().map(|(l, a)| (l.into(), a.into())).collect();
        self.ui()?.request(UiRequest::Menu { id, title: title.into(), items });
        Ok(())
    }
}

/// A request in flight: what it is for, and its status and content type once the
/// headers are in.
#[derive(Debug)]
pub struct Pending<J> {
    /// What the request is for.
    pub job: J,
    /// HTTP status (0 until the headers are in).
    pub status: u16,
    /// The `Content-Type` header, trimmed (empty until the headers are in).
    pub content_type: String,
}

impl<J> Pending<J> {
    /// The media type of [`Pending::content_type`]: lowercase, without parameters.
    pub fn mime(&self) -> String {
        self.content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
    }

    /// True for a 2xx status.
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// A body (empty when it was streamed in chunks) or the error that ended the request.
pub type Answer = std::result::Result<Vec<u8>, String>;

/// The HTTP requests of a plugin in flight, by request id, each with its job `J`.
/// [`Requests::event`] keeps the status and content type and hands back the finished
/// request, so `Plugin::on_http` only has to act on the answer.
#[derive(Debug)]
pub struct Requests<J>(HashMap<u32, Pending<J>>);

impl<J> Default for Requests<J> {
    fn default() -> Self {
        Requests(HashMap::new())
    }
}

impl<J> Requests<J> {
    /// Starts `req` with [`HostCtx::start_request`] and remembers `job` for it.
    pub fn start(&mut self, cx: &mut HostCtx, req: Request, mode: StreamMode, job: J) -> Result<u32> {
        let id = cx.start_request(req, mode)?;
        self.0.insert(id, Pending { job, status: 0, content_type: String::new() });
        Ok(id)
    }

    /// The request `id`, while it runs.
    pub fn get_mut(&mut self, id: u32) -> Option<&mut Pending<J>> {
        self.0.get_mut(&id)
    }

    /// Forgets request `id` (after cancelling it).
    pub fn remove(&mut self, id: u32) -> Option<Pending<J>> {
        self.0.remove(&id)
    }

    /// The jobs in flight.
    pub fn jobs(&self) -> impl Iterator<Item = &J> {
        self.0.values().map(|p| &p.job)
    }

    /// Takes event `ev` of request `id`: `Response` records the status and content type;
    /// `Done` and `Error` end the request and return it with its [`Answer`]. Other events
    /// and unknown ids return `None`.
    pub fn event(&mut self, id: u32, ev: &HttpEvent) -> Option<(Pending<J>, Answer)> {
        match ev {
            HttpEvent::Response { status, headers } => {
                let p = self.0.get_mut(&id)?;
                p.status = *status;
                p.content_type = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
                None
            }
            HttpEvent::Done { body } => Some((self.0.remove(&id)?, Ok(body.clone().unwrap_or_default()))),
            HttpEvent::Error(m) => Some((self.0.remove(&id)?, Err(m.clone()))),
            HttpEvent::Chunk(_) | HttpEvent::Sse(_) => None,
        }
    }
}

/// Escapes Markdown punctuation so text (a definition, an article) shows as written in
/// the text viewer; a leading `1. ` does not become a numbered list.
pub fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|' | '-' | '+' | '!') {
            out.push('\\');
        }
        out.push(c);
    }
    if let Some(dot) = out.find(". ") {
        if dot > 0 && out[..dot].chars().all(|c| c.is_ascii_digit()) {
            out.insert(dot, '\\');
        }
    }
    out
}

/// `text` cut at `max` characters on a word boundary (when one is in the second half),
/// without trailing separators, with an ellipsis; `text` itself when it is short enough.
pub fn cut_at(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut body: String = text.chars().take(max).collect();
    if let Some(sp) = body.rfind(' ').filter(|n| *n > max / 2) {
        body.truncate(sp);
    }
    let keep = body.trim_end_matches([' ', '•', ',', ';', ':']).len();
    body.truncate(keep);
    body.push('…');
    body
}

#[cfg(test)]
mod tests {
    use super::{cut_at, md_escape, Requests};
    use crate::{HostCtx, HttpEvent, MemoryStorage, NetHost, Permission, PluginError, StreamMode, UiQueue, UiRequest};
    use ink_net::Request;
    use serde_json::json;

    #[test]
    fn typed_settings_and_menus() {
        let mut ui = UiQueue::default();
        let mut st = MemoryStorage::default();
        for (k, v) in [("on", json!(true)), ("name", json!("x ")), ("n", json!(7)), ("neg", json!(-1))] {
            st.settings.insert(k.into(), v);
        }
        let perms = [Permission::Ui, Permission::Storage];
        let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
        assert!(cx.setting_bool("on", false) && cx.setting_bool("missing", true) && !cx.setting_bool("name", false));
        assert_eq!(cx.setting_str("name").as_deref(), Some("x "));
        assert_eq!((cx.setting_str("n"), cx.setting_u64("n"), cx.setting_u64("neg"), cx.setting_u64("name")), (None, Some(7), None, None));
        let label = String::from("Owned");
        cx.menu(3, "T", [(label.as_str(), "a"), ("Borrowed", "b")]).unwrap();
        cx.menu(4, "U", vec![(label, String::from("c"))]).unwrap();
        assert_eq!(ui.0[0], UiRequest::Menu { id: 3, title: "T".into(), items: vec![("Owned".into(), "a".into()), ("Borrowed".into(), "b".into())] });
        assert_eq!(ui.0[1].id(), Some(4));

        // Without the permissions: settings read as unset, menus are refused.
        let mut cx = HostCtx::new(&mut ui, &mut st, &[]);
        assert_eq!((cx.setting("on"), cx.setting_bool("on", false)), (None, false));
        assert!(matches!(cx.menu(5, "V", [("a", "a")]), Err(PluginError::Permission(_))));
    }

    #[test]
    fn markdown_escape_and_cut() {
        assert_eq!(md_escape("a*b_[c]"), "a\\*b\\_\\[c\\]");
        assert_eq!(md_escape("1. first"), "1\\. first");
        assert_eq!(md_escape("x. y"), "x. y");
        assert_eq!(cut_at("short", 10), "short");
        assert_eq!(cut_at("one two three four", 12), "one two…");
        assert_eq!(cut_at("one two, • three", 10), "one two…");
        assert_eq!(cut_at("abcdefghijkl", 5), "abcde…");
    }

    struct Jobs(u32);
    impl NetHost for Jobs {
        fn start(&mut self, _req: Request, _mode: StreamMode) -> crate::Result<u32> {
            self.0 += 1;
            Ok(self.0)
        }
        fn cancel(&mut self, _id: u32) {}
    }

    #[test]
    fn requests_keep_status_and_hand_back_the_answer() {
        let (mut ui, mut st, mut jobs) = (UiQueue::default(), MemoryStorage::default(), Jobs(0));
        let perms = [Permission::Network];
        let mut cx = HostCtx::new(&mut ui, &mut st, &perms).with_jobs(Some(&mut jobs));
        let mut r: Requests<&str> = Requests::default();
        let a = r.start(&mut cx, Request::get("https://x.org/a"), StreamMode::Chunks, "a").unwrap();
        let b = r.start(&mut cx, Request::get("https://x.org/b"), StreamMode::Chunks, "b").unwrap();
        let headers = vec![("Content-Type".to_string(), " Application/Atom+XML; charset=utf-8 ".to_string())];
        assert!(r.event(a, &HttpEvent::Response { status: 200, headers }).is_none());
        assert!(r.event(a, &HttpEvent::Chunk(b"x".to_vec())).is_none());
        assert_eq!(r.jobs().count(), 2);
        let (p, answer) = r.event(a, &HttpEvent::Done { body: Some(b"feed".to_vec()) }).unwrap();
        assert_eq!((p.job, p.status, p.success(), p.content_type.as_str(), p.mime(), answer), ("a", 200, true, "Application/Atom+XML; charset=utf-8", "application/atom+xml".into(), Ok(b"feed".to_vec())));
        assert!(r.event(a, &HttpEvent::Done { body: None }).is_none(), "ended requests are forgotten");
        let (p, answer) = r.event(b, &HttpEvent::Error("timed out".into())).unwrap();
        assert_eq!((p.job, p.status, p.success(), answer), ("b", 0, false, Err("timed out".into())));
        assert_eq!(r.jobs().count(), 0);
    }
}
