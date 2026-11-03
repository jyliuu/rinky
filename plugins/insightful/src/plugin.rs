//! The plugin: selection actions, the chat view and the agent loop (`main.lua`,
//! `chat.lua`, `agent.lua`).
//!
//! The agent loop is a state machine driven by `on_http`: each model turn is one
//! streamed request; when the turn ends with tool calls, the tools run on the open book
//! (shown as AGENT ACTION chips) and the next request starts. The chat view is the
//! host's Markdown viewer (one id, its text replaced as the answer arrives in batches);
//! the same viewer shows the chat list. The action menus are viewer sheets with icons.

use crate::chat::{self, BookChats, Live, Message, Totals, LINK_ASK, LINK_CHATS, LINK_DEL, LINK_DELETE, LINK_OPEN, LINK_PROMPT, LINK_QUESTION, LINK_RETRY};
use crate::config::{self, Config};
use crate::models;
use crate::prompts::{PromptSet, SelectionInfo};
use crate::provider::{self, ChatRequest, Msg, Profile, StreamAccumulator, ToolCall};
use crate::tools::{self, BookTools};
use ink_doc::Selection;
use ink_net::sse::SseParser;
use ink_plugin::net::WHOLE_LIMIT;
use ink_plugin::{HostCtx, HttpEvent, Permission, Plugin, PluginError, PluginManifest, Registrar, Result, StreamMode};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Plugin name.
pub const NAME: &str = "insightful";
/// Tool rounds per answer before the run stops.
pub const MAX_TOOL_TURNS: usize = 12;

const VIEWER: u32 = 1;
const INPUT_ASK: u32 = 2;
const INPUT_FOLLOWUP: u32 = 3;
const DIALOG_DELETE: u32 = 6;
const MENU_PROVIDER: u32 = 7;
const MENU_ACTIONS: u32 = 8;
const MENU_BOOK: u32 = 9;
const MENU_SETTINGS: u32 = 10;
const MENU_MODEL: u32 = 11;
const MENU_MODELS: u32 = 12;
const INPUT_MODEL: u32 = 13;
const INPUT_SEARCH: u32 = 14;
const DIALOG_MODEL: u32 = 15;
const DIALOG_PROMPT: u32 = 16;

/// Longest passage shown in the "what was sent" dialog of a preset prompt.
const PROMPT_PASSAGE_CHARS: usize = 240;

/// Icons of the selection actions in the action sheet (`QUICK_ACTIONS` order).
const ACTION_ICONS: [&str; 4] = ["info", "listview", "clock", "star"];

/// Selection actions: `(id, label, prompt name)`.
pub const QUICK_ACTIONS: [(&str, &str, &str); 4] =
    [("explain", "Explain", "explain"), ("examples", "Give examples", "explain_terms"), ("context", "Context/history", "context_history"), ("people", "People/characters", "people_characters")];

struct BookState {
    id: String,
    title: String,
    author: String,
    chats: BookChats,
    path: PathBuf,
}

struct Run {
    profile: Profile,
    request: Option<u32>,
    system: String,
    msgs: Vec<Msg>,
    acc: StreamAccumulator,
    /// ink-net's SSE parser over the raw chunks (streaming uses `StreamMode::Chunks` so
    /// that a non-2xx JSON error body arrives as bytes, not as dropped SSE lines).
    sse: SseParser,
    tools: BookTools,
    turns: usize,
    usage: Totals,
    live: Live,
    status: u16,
    err_body: Vec<u8>,
    flushed: usize,
    last_flush: Instant,
}

/// A model list request in flight.
struct ModelFetch {
    request: u32,
    provider: String,
    query: Option<String>,
    status: u16,
}

/// Insightful.
pub struct InsightfulPlugin {
    manifest: PluginManifest,
    config: Option<Config>,
    prompts: Option<PromptSet>,
    /// Path of KOReader's `configuration.lua` (read once to make `insightful.toml`).
    pub koreader_config: PathBuf,
    last_sel: Option<SelectionInfo>,
    pending_sel: Option<SelectionInfo>,
    delete_target: Option<String>,
    book: Option<BookState>,
    chat: Option<String>,
    questions: Vec<String>,
    run: Option<Run>,
    models_req: Option<ModelFetch>,
    /// Provider of the open model menu (the choice is saved for it).
    model_provider: Option<String>,
    error: Option<String>,
    viewer_open: bool,
    /// The viewer shows the chat list (`Some(true)`: in delete mode) instead of a chat.
    list: Option<bool>,
    /// The viewer shows a new, empty chat waiting for its first question.
    fresh: bool,
    /// Update the chat view after this many new characters...
    pub batch_chars: usize,
    /// ...or after this much time with new text.
    pub batch_interval: Duration,
}

impl Default for InsightfulPlugin {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms() -> Instant {
    Instant::now()
}

impl InsightfulPlugin {
    /// The plugin with default settings.
    pub fn new() -> InsightfulPlugin {
        let mut manifest = PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Document, Permission::Ui, Permission::Storage, Permission::Network]);
        manifest.description = "AI reading companion: explain, examples, context, people, questions about the book".into();
        InsightfulPlugin {
            manifest,
            config: None,
            prompts: None,
            koreader_config: PathBuf::from(config::KOREADER_CONFIGURATION),
            last_sel: None,
            pending_sel: None,
            delete_target: None,
            book: None,
            chat: None,
            questions: Vec::new(),
            run: None,
            models_req: None,
            model_provider: None,
            error: None,
            viewer_open: false,
            list: None,
            fresh: false,
            batch_chars: 160,
            batch_interval: Duration::from_millis(1200),
        }
    }

    /// True while a request runs.
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    fn ensure_config(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.config.is_none() {
            let toml_path = cx.data_file(config::CONFIG_FILE)?;
            let (c, origin) = config::load_or_migrate(&toml_path, &self.koreader_config).map_err(PluginError::Failed)?;
            if origin == config::Origin::Converted {
                cx.ui()?.notify("Insightful: settings imported from KOReader's configuration.lua");
            }
            self.config = Some(c);
        }
        if self.prompts.is_none() {
            let dir = cx.data_file("prompts")?;
            self.prompts = Some(PromptSet::load(Some(&dir)).map_err(PluginError::Failed)?);
        }
        Ok(())
    }

    fn ensure_book(&mut self, cx: &mut HostCtx) -> Result<()> {
        let doc = cx.document()?;
        let id = doc.book_id().unwrap_or_else(|| "unknown".into());
        if self.book.as_ref().is_some_and(|b| b.id == id) {
            return Ok(());
        }
        let meta = doc.metadata();
        let path = cx.data_file(&chat::file_name(&id))?;
        let mut chats = BookChats::load(&path, &id).map_err(PluginError::Failed)?;
        chats.title = meta.title.clone();
        self.book = Some(BookState { id, title: meta.title, author: meta.authors.join(", "), chats, path });
        self.chat = None;
        Ok(())
    }

    fn selection_info(cx: &mut HostCtx, sel: &Selection) -> SelectionInfo {
        let section = cx.document().ok().and_then(|d| d.toc().iter().rfind(|e| e.page.is_some_and(|p| p <= sel.page)).map(|e| e.title.clone()));
        SelectionInfo { text: sel.text.clone(), section, page: Some(sel.page + 1), locator: Some(sel.start.to_string()) }
    }

    fn current_selection(&mut self, cx: &mut HostCtx) -> Option<SelectionInfo> {
        if let Some(s) = cx.document().ok().and_then(|d| d.selection()) {
            return Some(Self::selection_info(cx, &s));
        }
        self.last_sel.clone()
    }

    fn save(&mut self) {
        if let Some(b) = &self.book {
            if let Err(e) = b.chats.save(&b.path) {
                log::warn!("insightful: saving chats: {e}");
            }
        }
    }

    fn conv(&self) -> Option<&chat::Conversation> {
        let b = self.book.as_ref()?;
        b.chats.get(self.chat.as_deref()?)
    }

    fn conv_mut(&mut self) -> Option<&mut chat::Conversation> {
        let id = self.chat.clone()?;
        self.book.as_mut()?.chats.get_mut(&id)
    }

    /// Updates the chat view after a streamed change, unless the viewer shows the chat
    /// list now (the user went back while the answer was still coming).
    fn refresh(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.viewer_open && self.list.is_some() {
            return Ok(());
        }
        self.show(cx)
    }

    fn show(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some(conv) = self.conv() else { return Ok(()) };
        let (md, q) = chat::render(conv, self.run.as_ref().map(|r| &r.live), self.error.as_deref());
        // Back goes to the list of chats.
        let md = format!(":::back {}\n{md}", chat::LINK_CHATS);
        let title = format!("Insightful\n{}", conv.title);
        self.questions = q;
        cx.ui()?.view_markdown(VIEWER, &title, &md);
        self.viewer_open = true;
        self.list = None;
        self.fresh = false;
        if let Some(r) = &mut self.run {
            r.flushed = r.live.text.len();
            r.last_flush = now_ms();
        }
        Ok(())
    }

    /// Starts a new chat with one user message and runs it.
    fn start_chat(&mut self, cx: &mut HostCtx, content: &str, label: Option<&str>, sel: Option<SelectionInfo>) -> Result<()> {
        self.ensure_config(cx)?;
        self.ensure_book(cx)?;
        if self.run.is_some() {
            cx.ui()?.toast("Insightful is still answering");
            return Ok(());
        }
        let head = label.unwrap_or(content);
        let title = match &sel {
            Some(s) if !s.text.trim().is_empty() => format!("{head}: {}", s.text),
            _ => head.to_string(),
        };
        let book = self.book.as_mut().expect("book");
        let id = book.chats.create(&title);
        let msg = Message { role: "user".into(), content: content.into(), selection: sel, model: None, label: label.map(str::to_string), timestamp: chat::now(), usage: None };
        book.chats.get_mut(&id).expect("chat").messages.push(msg);
        self.chat = Some(id);
        self.begin_run(cx)
    }

    fn ask_followup(&mut self, cx: &mut HostCtx, q: &str) -> Result<()> {
        if self.run.is_some() || q.trim().is_empty() {
            return Ok(());
        }
        let Some(conv) = self.conv_mut() else { return Ok(()) };
        conv.messages.push(Message { role: "user".into(), content: q.trim().into(), selection: None, model: None, label: None, timestamp: chat::now(), usage: None });
        self.begin_run(cx)
    }

    fn begin_run(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.error = None;
        let override_id = cx.storage().ok().and_then(|s| s.get("provider")).and_then(|v| v.as_str().map(str::to_string));
        let Some(profile) = self.config.as_ref().and_then(|c| c.resolve(override_id.as_deref())) else {
            self.error = Some(format!("Insightful is not configured: no provider has an API key ({}).", config::CONFIG_FILE));
            return self.show(cx);
        };
        let prompts = self.prompts.as_ref().expect("prompts");
        let book = self.book.as_ref().expect("book");
        let position = cx
            .document()
            .ok()
            .and_then(|d| {
                let page = d.current_page()?;
                let section = d.toc().iter().rfind(|e| e.page.is_some_and(|p| p <= page)).map(|e| e.title.clone());
                Some(section.unwrap_or_else(|| format!("page {}", page + 1)))
            })
            .unwrap_or_default();
        let system = prompts.system(&book.title, &book.author, &position).map_err(PluginError::Failed)?;
        let conv = self.conv().expect("chat");
        let mut msgs = Vec::new();
        for m in &conv.messages {
            if m.role == "user" {
                msgs.push(Msg::User(prompts.user_message(&m.content, m.selection.as_ref()).map_err(PluginError::Failed)?));
            } else {
                msgs.push(Msg::Assistant { text: m.content.clone(), tool_calls: Vec::new(), state: None });
            }
        }
        prompts.add_reminder(&mut msgs);
        let live = Live { model: profile.model.clone(), status: "Thinking…".into(), ..Default::default() };
        self.run = Some(Run {
            acc: StreamAccumulator::new(profile.kind),
            sse: SseParser::new(),
            profile,
            request: None,
            system,
            msgs,
            tools: BookTools::new(),
            turns: 0,
            usage: Totals::default(),
            live,
            status: 0,
            err_body: Vec::new(),
            flushed: 0,
            last_flush: now_ms(),
        });
        self.send_turn(cx)
    }

    fn send_turn(&mut self, cx: &mut HostCtx) -> Result<()> {
        let r = self.run.as_mut().expect("run");
        let req = ChatRequest { system: r.system.clone(), messages: r.msgs.clone(), tools: tools::schemas() };
        let http = match provider::build_request(&r.profile, &req, r.profile.stream) {
            Ok(h) => h,
            Err(e) => return self.fail(cx, e),
        };
        let mode = if r.profile.stream { StreamMode::Chunks } else { StreamMode::Whole { limit: WHOLE_LIMIT } };
        r.acc = StreamAccumulator::new(r.profile.kind);
        r.status = 0;
        r.err_body.clear();
        r.sse = SseParser::new();
        r.live.text.clear();
        r.live.status = "Thinking…".into();
        match cx.start_request(http, mode) {
            Ok(id) => {
                self.run.as_mut().expect("run").request = Some(id);
                self.show(cx)
            }
            Err(e) => self.fail(cx, format!("Couldn't reach the AI service: {e}")),
        }
    }

    fn fail(&mut self, cx: &mut HostCtx, msg: String) -> Result<()> {
        if let Some(r) = self.run.take() {
            if let Some(id) = r.request {
                cx.cancel_request(id);
            }
            let usage = r.usage;
            if let Some(c) = self.conv_mut() {
                c.usage.merge(&usage);
                c.updated = chat::now();
            }
        }
        self.error = Some(msg);
        self.save();
        self.refresh(cx)
    }

    fn finish_answer(&mut self, cx: &mut HostCtx, text: String) -> Result<()> {
        let r = self.run.take().expect("run");
        if let Some(c) = self.conv_mut() {
            c.messages.push(Message { role: "assistant".into(), content: text, selection: None, model: Some(r.profile.model.clone()), label: None, timestamp: chat::now(), usage: Some(r.usage) });
            c.usage.merge(&r.usage);
            c.updated = chat::now();
        }
        self.save();
        self.refresh(cx)
    }

    fn run_tools(&mut self, cx: &mut HostCtx, reply: provider::Reply) -> Result<()> {
        let r = self.run.as_mut().expect("run");
        r.turns += 1;
        if r.turns > MAX_TOOL_TURNS {
            return self.fail(cx, format!("The AI service kept calling book tools; stopped after {MAX_TOOL_TURNS} rounds."));
        }
        let calls: Vec<ToolCall> = reply
            .tool_calls
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                if c.id.is_empty() {
                    c.id = format!("insightful-{}-{}", r.turns, i + 1);
                }
                if !c.arguments.is_object() {
                    c.arguments = json!({});
                }
                c
            })
            .collect();
        r.msgs.push(Msg::Assistant { text: reply.text.unwrap_or_default(), tool_calls: calls.clone(), state: reply.provider_state });
        for c in &calls {
            let detail = tools::describe(&c.name, &c.arguments);
            let line = |verb: &str| if detail.is_empty() { format!("{verb} `{}`", c.name) } else { format!("{verb} `{}` — {detail}", c.name) };
            r.live.actions.push(line("Calling"));
            let result: Value = match cx.doc.as_deref_mut() {
                Some(doc) if cx.permissions.contains(&Permission::Document) => r.tools.execute(doc, &c.name, &c.arguments),
                _ => json!({"ok": false, "error": "The open document changed."}),
            };
            *r.live.actions.last_mut().expect("action") = line("Finished");
            r.msgs.push(Msg::Tool { call_id: c.id.clone(), name: c.name.clone(), content: result });
        }
        self.send_turn(cx)
    }

    fn on_turn_done(&mut self, cx: &mut HostCtx, body: Option<Vec<u8>>) -> Result<()> {
        let r = self.run.as_mut().expect("run");
        r.request = None;
        if !(200..300).contains(&r.status) && r.status != 0 {
            let b = body.unwrap_or_else(|| std::mem::take(&mut r.err_body));
            let m = provider::http_error(r.profile.kind, r.status, &b);
            return self.fail(cx, m);
        }
        let reply = if r.profile.stream {
            std::mem::replace(&mut r.acc, StreamAccumulator::new(r.profile.kind)).finish()
        } else {
            provider::parse_whole(r.profile.kind, body.as_deref().unwrap_or(b""))
        };
        match reply {
            Err(f) => {
                r.usage.add(f.usage);
                self.fail(cx, f.message)
            }
            Ok(reply) => {
                r.usage.add(reply.usage);
                if !reply.tool_calls.is_empty() {
                    return self.run_tools(cx, reply);
                }
                match reply.text.filter(|t| !t.trim().is_empty()) {
                    Some(t) => self.finish_answer(cx, t),
                    None => self.fail(cx, "The AI service returned neither text nor tool calls.".into()),
                }
            }
        }
    }

    /// Explain / Give examples / Context / People on `sel`.
    fn quick_action(&mut self, cx: &mut HostCtx, id: &str, sel: Option<SelectionInfo>) -> Result<()> {
        let Some((_, label, prompt)) = QUICK_ACTIONS.iter().find(|(a, _, _)| *a == id) else { return Ok(()) };
        self.ensure_config(cx)?;
        let content = self.prompts.as_ref().expect("prompts").get(prompt).map_err(PluginError::Failed)?.to_string();
        self.start_chat(cx, &content, Some(label), sel)
    }

    /// Feeds the `data` of server-sent events to the accumulator and updates the chat
    /// view when enough new text arrived (a batch).
    fn on_data(&mut self, cx: &mut HostCtx, data: Vec<String>) -> Result<()> {
        let r = self.run.as_mut().expect("run");
        for d in &data {
            if let Err(m) = r.acc.feed(d) {
                return self.fail(cx, m);
            }
        }
        r.live.text = r.acc.text().to_string();
        r.live.status = if !r.live.text.is_empty() {
            String::new()
        } else if !r.acc.tool_names().is_empty() {
            "Choosing book tools…".into()
        } else if r.acc.reasoning() {
            "Reasoning…".into()
        } else {
            "Thinking…".into()
        };
        let grown = r.live.text.len().saturating_sub(r.flushed);
        if grown >= self.batch_chars || (grown > 0 && r.last_flush.elapsed() >= self.batch_interval) {
            return self.refresh(cx);
        }
        Ok(())
    }

    fn open_chats(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.ensure_config(cx)?;
        self.ensure_book(cx)?;
        if self.book.as_ref().expect("book").chats.chats.is_empty() {
            cx.ui()?.toast("No Insightful chats for this book yet");
            return Ok(());
        }
        self.show_list(cx, false)
    }

    /// Opens the chat last used in this book (the one open before, else the most recently
    /// changed); a toast when the book has no chats.
    fn open_last_chat(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.ensure_config(cx)?;
        self.ensure_book(cx)?;
        let b = self.book.as_ref().expect("book");
        let id = self.chat.clone().filter(|c| b.chats.get(c).is_some()).or_else(|| b.chats.chats.iter().max_by_key(|c| c.updated.max(c.created)).map(|c| c.id.clone()));
        let Some(id) = id else {
            cx.ui()?.toast("No Insightful chats for this book yet");
            return Ok(());
        };
        self.chat = Some(id);
        self.error = None;
        self.show(cx)
    }

    /// The chat list in the viewer: book title, a card per chat.
    fn show_list(&mut self, cx: &mut HostCtx, delete: bool) -> Result<()> {
        let Some(b) = self.book.as_ref() else { return Ok(()) };
        let t = b.chats.totals();
        let book = if b.title.is_empty() { "This book".to_string() } else { b.title.clone() };
        let title = format!("Chats\n{book} · {} tokens", t.total_tokens);
        let md = chat::render_list(&b.chats, delete);
        cx.ui()?.view_markdown(VIEWER, &title, &md);
        self.viewer_open = true;
        self.list = Some(delete);
        self.fresh = false;
        Ok(())
    }

    /// A new chat: the chat view with the selected passage (if any) and its input bar,
    /// and the keyboard docked under it (typing happens in the view's own input bar).
    fn ask(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.ensure_config(cx)?;
        self.ensure_book(cx)?;
        let book = self.book.as_ref().map(|b| b.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "This book".into());
        let mut md = match self.pending_sel.as_ref().filter(|s| !s.text.trim().is_empty()) {
            Some(s) => format!(":::you\n{}\n\n:::\n*Ask about the selected passage.*", chat::quote_passage(&s.text)),
            None => "*Ask anything about this book.*".to_string(),
        };
        md.push_str(&format!("\n\n:::\n:::input {LINK_ASK} | {}", chat::ASK_PLACEHOLDER));
        if self.book.as_ref().is_some_and(|b| !b.chats.chats.is_empty()) {
            md = format!(":::back {}\n{md}", chat::LINK_CHATS);
        }
        cx.ui()?.view_markdown(VIEWER, &format!("Insightful\n{book}"), &md);
        self.viewer_open = true;
        self.list = None;
        self.fresh = true;
        self.ask_input(cx)
    }

    fn ask_input(&mut self, cx: &mut HostCtx) -> Result<()> {
        let prompt = if self.pending_sel.is_some() { "Ask about the selection" } else { "Ask about the book" };
        cx.ui()?.input(INPUT_ASK, prompt, "");
        Ok(())
    }

    /// The dialog behind a preset bubble: the label and the text that was sent for it.
    fn show_prompt(&mut self, cx: &mut HostCtx, index: usize) -> Result<()> {
        let Some(m) = self.conv().and_then(|c| c.messages.get(index)).filter(|m| m.role == "user").cloned() else { return Ok(()) };
        let Some(prompts) = self.prompts.as_ref() else { return Ok(()) };
        let sel = m.selection.clone().map(|mut s| {
            if s.text.chars().count() > PROMPT_PASSAGE_CHARS {
                s.text = s.text.chars().take(PROMPT_PASSAGE_CHARS).collect::<String>() + "…";
            }
            s
        });
        let sent = prompts.user_message(&m.content, sel.as_ref()).map_err(PluginError::Failed)?;
        let body = format!("{sent}\n\nSent with the system prompt (book title, author, reading position).");
        let title = m.label.as_deref().unwrap_or("Prompt");
        cx.ui()?.dialog(DIALOG_PROMPT, &format!("{title}: the prompt sent"), &body, &[("OK", "ok")]);
        Ok(())
    }

    /// A sheet of `(label, action, icon)` rows.
    fn sheet(cx: &mut HostCtx, id: u32, title: &str, rows: &[(&str, &str, &str)]) -> Result<()> {
        let mut md = String::from(":::sheet\n");
        for (label, action, icon) in rows {
            md.push_str(&format!(":::row {action} | {icon}\n{label}\n"));
        }
        cx.ui()?.view_markdown(id, title, &md);
        Ok(())
    }

    fn provider_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.ensure_config(cx)?;
        let c = self.config.as_ref().expect("config");
        let current = cx.storage()?.get("provider").and_then(|v| v.as_str().map(str::to_string));
        let chosen = c.resolve(current.as_deref()).map(|p| p.kind);
        let items: Vec<(String, String)> = c
            .available()
            .into_iter()
            .map(|id| {
                let p = c.resolve(Some(id)).expect("available");
                let mark = if Some(p.kind) == chosen { "✓ " } else { "" };
                (format!("{mark}{id} · {}", p.model), id.to_string())
            })
            .collect();
        if items.is_empty() {
            cx.ui()?.dialog(0, "Insightful", &format!("No provider has an API key. Add one to {} in the plugin's data folder.", config::CONFIG_FILE), &[("OK", "ok")]);
            return Ok(());
        }
        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        cx.ui()?.menu(MENU_PROVIDER, "Insightful provider", &refs);
        Ok(())
    }

    /// The profile a chat would use now (the provider chosen in the menu, resolved).
    fn current_profile(&mut self, cx: &mut HostCtx) -> Result<Option<Profile>> {
        self.ensure_config(cx)?;
        let chosen = cx.storage()?.get("provider").and_then(|v| v.as_str().map(str::to_string));
        Ok(self.config.as_ref().expect("config").resolve(chosen.as_deref()))
    }

    fn no_provider(cx: &mut HostCtx) -> Result<()> {
        cx.ui()?.dialog(0, "Insightful", &format!("No provider has an API key. Add one to {} in the plugin's data folder.", config::CONFIG_FILE), &[("OK", "ok")]);
        Ok(())
    }

    fn model_error(cx: &mut HostCtx, msg: &str) -> Result<()> {
        cx.ui()?.dialog(DIALOG_MODEL, "Insightful", msg, &[("OK", "ok")]);
        Ok(())
    }

    /// Settings: `Provider <id>` and `Model <model>`.
    fn settings_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some(p) = self.current_profile(cx)? else { return Self::no_provider(cx) };
        let items = [(format!("Provider\t{}", p.kind.id()), "provider"), (format!("Model\t{}", p.model), "model")];
        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        cx.ui()?.menu(MENU_SETTINGS, "Insightful settings", &refs);
        Ok(())
    }

    /// Model menu for the current provider: list, search, enter an id, back to default.
    fn model_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some(p) = self.current_profile(cx)? else { return Self::no_provider(cx) };
        let id = p.kind.id();
        let c = self.config.as_ref().expect("config");
        let default = c.providers.get(id).map(|pc| pc.model.trim().to_string()).unwrap_or_default();
        let mut items: Vec<(String, &str)> = Vec::new();
        if models::supports(id) {
            items.push(("Show available models".into(), "list"));
            items.push(("Search models…".into(), "search"));
        }
        items.push(("Enter model ID…".into(), "enter"));
        if c.models.get(id).is_some_and(|m| !m.trim().is_empty()) {
            items.push((format!("Use default ({default})"), "default"));
        }
        self.model_provider = Some(id.to_string());
        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        cx.ui()?.menu(MENU_MODEL, &format!("{id} model: {}", p.model), &refs);
        Ok(())
    }

    /// The profile of the provider in the model menu.
    fn model_profile(&self) -> Option<Profile> {
        let id = self.model_provider.as_deref()?;
        self.config.as_ref()?.resolve(Some(id)).filter(|p| p.kind.id() == id)
    }

    /// Starts the model list request (`query`: search text).
    fn fetch_models(&mut self, cx: &mut HostCtx, query: Option<String>) -> Result<()> {
        let Some(p) = self.model_profile() else { return Ok(()) };
        let provider = p.kind.id().to_string();
        let Some(url) = models::url(&provider, &p.base_url, query.as_deref()) else {
            return Self::model_error(cx, &format!("{provider} has no model list. Use \"Enter model ID…\"."));
        };
        if let Some(old) = self.models_req.take() {
            cx.cancel_request(old.request);
        }
        match cx.start_request(models::request(&url, &p.api_key, p.timeout), StreamMode::Whole { limit: models::BODY_LIMIT }) {
            Ok(request) => {
                self.models_req = Some(ModelFetch { request, provider, query, status: 0 });
                cx.ui()?.toast("Loading models…");
                Ok(())
            }
            Err(e) => {
                log::warn!("insightful: model list for {provider}: {e}");
                Self::model_error(cx, &format!("Couldn't load models: {e}"))
            }
        }
    }

    /// Events of the model list request: a menu of models, or an error dialog.
    fn on_models_http(&mut self, cx: &mut HostCtx, ev: &HttpEvent) -> Result<()> {
        let f = self.models_req.as_mut().expect("model request");
        let body = match ev {
            HttpEvent::Response { status, .. } => {
                f.status = *status;
                return Ok(());
            }
            HttpEvent::Done { body } => body.as_deref().unwrap_or(b""),
            HttpEvent::Error(m) => {
                let f = self.models_req.take().expect("model request");
                log::warn!("insightful: model list for {}: {m}", f.provider);
                return Self::model_error(cx, &format!("Couldn't load models: {m}. Check the Wi-Fi connection."));
            }
            _ => return Ok(()),
        };
        let f = self.models_req.take().expect("model request");
        let kind = provider::Kind::from_id(&f.provider).unwrap_or(provider::Kind::OpenAi);
        if !(200..300).contains(&f.status) {
            let m = provider::http_error(kind, f.status, body).replacen("AI service", "Model service", 1);
            log::warn!("insightful: model list for {}: HTTP {}", f.provider, f.status);
            return Self::model_error(cx, &format!("Couldn't load models: {m}"));
        }
        let current = self.config.as_ref().and_then(|c| c.resolve(Some(&f.provider))).map(|p| p.model).unwrap_or_default();
        let (ids, more) = match models::parse(body, &current, f.query.as_deref()) {
            Ok(r) => r,
            Err(m) => return Self::model_error(cx, &m),
        };
        if ids.is_empty() {
            let m = match &f.query {
                Some(q) => format!("No models match \"{q}\"."),
                None => "The provider returned no models.".into(),
            };
            return Self::model_error(cx, &m);
        }
        let items: Vec<(String, String)> = ids.into_iter().map(|id| (if id == current { format!("✓ {id}") } else { id.clone() }, id)).collect();
        let mut title = match &f.query {
            Some(q) => format!("{} models matching \"{q}\"", f.provider),
            None => format!("{} models", f.provider),
        };
        if more {
            title.push_str(&format!(" (first {})", models::LIMIT));
        }
        self.model_provider = Some(f.provider);
        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        cx.ui()?.menu(MENU_MODELS, &title, &refs);
        Ok(())
    }

    /// Saves `model` (`None`: the profile's model) for `provider` in `insightful.toml`.
    fn set_model(&mut self, cx: &mut HostCtx, provider: &str, model: Option<&str>) -> Result<()> {
        let Some(mut c) = self.config.clone() else { return Ok(()) };
        let default = c.providers.get(provider).map(|p| p.model.trim().to_string()).unwrap_or_default();
        // Choosing the profile's own model removes the override.
        let model = model.map(str::trim).filter(|m| !m.is_empty() && *m != default);
        match model {
            Some(m) => c.models.insert(provider.to_string(), m.to_string()),
            None => c.models.remove(provider),
        };
        let path = cx.data_file(config::CONFIG_FILE)?;
        if let Err(e) = c.save(&path) {
            log::warn!("insightful: saving the model choice: {e}");
            return Self::model_error(cx, &format!("Couldn't save the model: {e}"));
        }
        self.config = Some(c);
        let used = model.unwrap_or(&default);
        log::info!("insightful: {provider} model set to {used}");
        cx.ui()?.toast(&format!("Insightful uses {used}"));
        Ok(())
    }
}

impl Plugin for InsightfulPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        // The reader's selection popup shows at most 3 plugin buttons for all plugins
        // together, so Insightful takes one: it opens a menu with the five actions
        // (`explain`, `examples`, `context`, `people`, `ask` stay action ids too).
        reg.selection_action("menu", "Insightful");
        // One reader-menu entry: a menu with "Ask about the book" and "Chats".
        reg.menu_item("book_menu", "Insightful", 60);
        // For the reader's gesture map (the menu item above is offered there too).
        reg.action("ask_book", "Insightful: Ask about the book");
        reg.action("chats", "Insightful: Chats for this book");
        reg.action("last_chat", "Insightful: Open the last chat");
        reg.settings_page("provider", "Insightful");
        Ok(())
    }

    fn on_selection(&mut self, cx: &mut HostCtx, sel: &Selection) -> Result<()> {
        self.last_sel = Some(Self::selection_info(cx, sel));
        Ok(())
    }

    fn on_close(&mut self, cx: &mut HostCtx) -> Result<()> {
        if let Some(id) = self.run.as_ref().and_then(|r| r.request) {
            cx.cancel_request(id);
        }
        self.run = None;
        if let Some(f) = self.models_req.take() {
            cx.cancel_request(f.request);
        }
        self.save();
        self.book = None;
        self.chat = None;
        self.last_sel = None;
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        if QUICK_ACTIONS.iter().any(|(a, _, _)| *a == id) {
            let sel = self.current_selection(cx);
            return self.quick_action(cx, id, sel);
        }
        match id {
            "menu" => {
                // The reader clears its selection when the popup closes: keep it now.
                self.pending_sel = self.current_selection(cx);
                let mut rows: Vec<(&str, &str, &str)> = QUICK_ACTIONS.iter().zip(ACTION_ICONS).map(|((a, l, _), i)| (*l, *a, i)).collect();
                rows.push(("Ask AI…", "ask", "chat"));
                Self::sheet(cx, MENU_ACTIONS, "Insightful", &rows)
            }
            "ask" | "ask_book" => {
                self.pending_sel = if id == "ask" { self.current_selection(cx) } else { None };
                self.ask(cx)
            }
            "book_menu" => {
                Self::sheet(cx, MENU_BOOK, "Insightful", &[("Ask about the book…", "ask_book", "chat"), ("Chats for this book", "chats", "folder"), ("Provider", "settings:provider", "settings"), ("Model", "settings:model", "settings")])
            }
            "chats" => self.open_chats(cx),
            "last_chat" => self.open_last_chat(cx),
            "settings:provider" => self.settings_menu(cx),
            "settings:model" => self.model_menu(cx),
            _ => Ok(()),
        }
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match id {
            VIEWER => {
                if result == "closed" {
                    self.viewer_open = false;
                    self.list = None;
                    self.fresh = false;
                    if let Some(id) = self.run.take().and_then(|r| r.request) {
                        cx.cancel_request(id);
                    }
                    self.save();
                } else if let Some(n) = result.strip_prefix(LINK_QUESTION) {
                    if let Some(q) = n.parse::<usize>().ok().and_then(|n| self.questions.get(n)).cloned() {
                        self.ask_followup(cx, &q)?;
                    }
                } else if let Some(n) = result.strip_prefix(LINK_PROMPT) {
                    if let Ok(n) = n.parse::<usize>() {
                        self.show_prompt(cx, n)?;
                    }
                } else if result == LINK_ASK {
                    if self.fresh {
                        self.ask_input(cx)?;
                    } else if self.list.is_some() {
                        self.pending_sel = None;
                        cx.ui()?.input(INPUT_ASK, "Ask about the book", "");
                    } else if self.run.is_none() {
                        cx.ui()?.input(INPUT_FOLLOWUP, "Ask a follow-up question", "");
                    }
                } else if let Some(cid) = result.strip_prefix(LINK_OPEN) {
                    self.chat = Some(cid.to_string());
                    self.error = None;
                    self.show(cx)?;
                } else if result == LINK_DELETE || result == LINK_CHATS {
                    self.show_list(cx, result == LINK_DELETE)?;
                } else if let Some(cid) = result.strip_prefix(LINK_DEL) {
                    let title = self.book.as_ref().and_then(|b| b.chats.get(cid)).map(|c| c.title.clone()).unwrap_or_default();
                    self.delete_target = Some(cid.to_string());
                    cx.ui()?.dialog(DIALOG_DELETE, "Delete chat?", &title, &[("Cancel", "cancel"), ("Delete", "delete")]);
                } else if result == LINK_RETRY {
                    if self.run.is_none() && self.conv().and_then(|c| c.messages.last()).is_some_and(|m| m.role == "user") {
                        self.begin_run(cx)?;
                    }
                } else if !result.is_empty() {
                    cx.ui()?.toast(&format!("Link: {result}"));
                }
                Ok(())
            }
            INPUT_ASK if !result.trim().is_empty() => {
                let sel = self.pending_sel.take();
                self.start_chat(cx, result.trim(), None, sel)
            }
            INPUT_FOLLOWUP if !result.trim().is_empty() => self.ask_followup(cx, result),
            DIALOG_DELETE => {
                if let (Some(cid), "delete") = (self.delete_target.take(), result) {
                    if self.run.is_some() && self.chat.as_deref() == Some(cid.as_str()) {
                        cx.ui()?.toast("That chat is still answering");
                        return Ok(());
                    }
                    let ok = self.book.as_mut().is_some_and(|b| b.chats.delete(&cid));
                    if self.chat.as_deref() == Some(cid.as_str()) {
                        self.chat = None;
                    }
                    self.save();
                    cx.ui()?.toast(if ok { "Chat deleted" } else { "Chat not found" });
                    if self.viewer_open && self.list.is_some() {
                        self.show_list(cx, false)?;
                    }
                }
                Ok(())
            }
            MENU_BOOK if !result.is_empty() && result != "closed" => self.on_action(cx, result),
            MENU_ACTIONS if result == "ask" => self.ask(cx),
            MENU_ACTIONS if !result.is_empty() && result != "closed" => {
                let sel = self.pending_sel.take();
                self.quick_action(cx, result, sel)
            }
            MENU_SETTINGS if result == "provider" => self.provider_menu(cx),
            MENU_SETTINGS if result == "model" => self.model_menu(cx),
            MENU_MODEL if result == "list" => self.fetch_models(cx, None),
            MENU_MODEL if result == "search" => {
                cx.ui()?.input(INPUT_SEARCH, "Search models (name or ID)", "");
                Ok(())
            }
            MENU_MODEL if result == "enter" => {
                let current = self.model_profile().map(|p| p.model).unwrap_or_default();
                cx.ui()?.input(INPUT_MODEL, "Exact model ID", &current);
                Ok(())
            }
            MENU_MODEL if result == "default" => {
                let Some(provider) = self.model_provider.clone() else { return Ok(()) };
                self.set_model(cx, &provider, None)
            }
            INPUT_MODEL | MENU_MODELS if !result.trim().is_empty() => {
                let Some(provider) = self.model_provider.clone() else { return Ok(()) };
                self.set_model(cx, &provider, Some(result))
            }
            INPUT_SEARCH if !result.trim().is_empty() => self.fetch_models(cx, Some(result.trim().to_string())),
            MENU_PROVIDER if !result.is_empty() => {
                cx.storage()?.set("provider", json!(result))?;
                cx.ui()?.toast(&format!("Insightful uses {result}"));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        if self.models_req.as_ref().is_some_and(|f| f.request == id) {
            return self.on_models_http(cx, ev);
        }
        let Some(r) = self.run.as_mut().filter(|r| r.request == Some(id)) else { return Ok(()) };
        match ev {
            HttpEvent::Response { status, .. } => r.status = *status,
            HttpEvent::Sse(e) => return self.on_data(cx, vec![e.data.clone()]),
            HttpEvent::Chunk(c) => {
                if !(200..300).contains(&r.status) && r.status != 0 {
                    // An error body (JSON, not an event stream): kept for the message.
                    r.err_body.extend_from_slice(c);
                    return Ok(());
                }
                let data: Vec<String> = r.sse.feed(c).into_iter().map(|e| e.data).collect();
                return self.on_data(cx, data);
            }
            HttpEvent::Done { body } => return self.on_turn_done(cx, body.clone()),
            HttpEvent::Error(m) => return self.fail(cx, format!("Couldn't reach the AI service: {m}")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
