//! Conversations: stored per book, and rendered as Markdown for the chat view.
//!
//! Storage: `conversations/<book-id>.json` in the plugin's data folder, one file per book
//! with all its chats (newest last). Rendering follows `conversation_renderer.lua` in
//! the host viewer's `:::` blocks: the user's turns as filled cards (the selected
//! passage as a quote), the answers as cards with the model and a token/cost line, tool
//! calls as AGENT ACTION chips while the agent runs, the follow-up `<question>` items as
//! chips that send the question, and an input bar for a new question.

use crate::prompts::SelectionInfo;
use crate::provider::Usage;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Link scheme of the chat view.
pub const LINK_QUESTION: &str = "insightful:q/";
/// Link that opens the keyboard for a follow-up question.
pub const LINK_ASK: &str = "insightful:ask";
/// Link that retries the last question.
pub const LINK_RETRY: &str = "insightful:retry";
/// Link of a chat in the chat list (`insightful:open/c3`).
pub const LINK_OPEN: &str = "insightful:open/";
/// Link that shows the chat list in delete mode.
pub const LINK_DELETE: &str = "insightful:delete";
/// Link of a chat in the delete list (`insightful:del/c3`).
pub const LINK_DEL: &str = "insightful:del/";
/// Link back to the chat list.
pub const LINK_CHATS: &str = "insightful:chats";
/// Link of a preset question's bubble (`insightful:prompt/0`, the message index): shows
/// the whole prompt that was sent for the short label.
pub const LINK_PROMPT: &str = "insightful:prompt/";
/// Caption under a preset question's label.
pub const PROMPT_CAPTION: &str = "Preset prompt · tap to see what was sent";
/// Placeholder of the input bar.
pub const ASK_PLACEHOLDER: &str = "Ask about this book…";

/// One stored message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// `user` or `assistant`.
    pub role: String,
    /// Text (for the user: the question or action prompt, without the passage).
    pub content: String,
    /// Selected passage (user turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionInfo>,
    /// Model that answered (assistant turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Short label shown instead of a long action prompt (`Explain`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Seconds since 1970.
    #[serde(default)]
    pub timestamp: u64,
    /// Tokens and cost of this answer (assistant turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Totals>,
}

/// Token and cost totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    /// Requests made.
    pub requests: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Total tokens.
    pub total_tokens: u64,
    /// Cost in USD (only providers that report it).
    pub cost_usd: f64,
}

impl Totals {
    /// Adds one response.
    pub fn add(&mut self, u: Option<Usage>) {
        self.requests += 1;
        if let Some(u) = u {
            self.input_tokens += u.input_tokens;
            self.output_tokens += u.output_tokens;
            self.total_tokens += u.total_tokens;
            self.cost_usd += u.cost_usd.unwrap_or(0.0);
        }
    }
    /// Adds totals.
    pub fn merge(&mut self, o: &Totals) {
        self.requests += o.requests;
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.total_tokens += o.total_tokens;
        self.cost_usd += o.cost_usd;
    }
}

/// One chat.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    /// Id, unique within the book (`c1`, `c2`, ...).
    pub id: String,
    /// Title (the first question or action and passage, shortened).
    pub title: String,
    /// Created (seconds since 1970).
    pub created: u64,
    /// Last change.
    pub updated: u64,
    /// Messages.
    pub messages: Vec<Message>,
    /// Token and cost totals.
    #[serde(default)]
    pub usage: Totals,
}

/// All chats of one book.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BookChats {
    /// Book id (partial MD5).
    pub book_id: String,
    /// Book title.
    #[serde(default)]
    pub title: String,
    /// Chats, oldest first.
    pub chats: Vec<Conversation>,
    /// Last id number handed out.
    #[serde(default)]
    pub next_id: u64,
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// File name of a book's chats (in the data folder).
pub fn file_name(book_id: &str) -> String {
    let safe: String = book_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(64).collect();
    format!("conversations/{}.json", if safe.is_empty() { "unknown" } else { &safe })
}

fn short(text: &str, n: usize) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= n {
        t
    } else {
        format!("{}…", t.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

impl BookChats {
    /// Reads `path`; a missing file gives an empty list. A file that does not parse is
    /// renamed to `<name>.corrupt-<secs>-<n>` (the bytes are kept) and an empty list is
    /// returned, so the next save does not overwrite it and the chats keep working.
    pub fn load(path: &Path, book_id: &str) -> Result<BookChats, String> {
        match std::fs::read(path) {
            Ok(b) => match serde_json::from_slice(&b) {
                Ok(c) => Ok(c),
                Err(e) => {
                    let kept = ink_store::set_aside(path);
                    log::warn!("insightful: {}: {e}; kept as {}, starting an empty chat list", path.display(), kept.display());
                    Ok(BookChats { book_id: book_id.into(), ..Default::default() })
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BookChats { book_id: book_id.into(), ..Default::default() }),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes `path` atomically.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        // Temporary file, fsync, rename, directory fsync.
        ink_store::write_atomic(path, &data).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A new empty chat; returns its id.
    pub fn create(&mut self, title: &str) -> String {
        self.next_id += 1;
        let id = format!("c{}", self.next_id);
        let t = now();
        self.chats.push(Conversation { id: id.clone(), title: short(title, 60), created: t, updated: t, ..Default::default() });
        id
    }

    /// The chat with `id`.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Conversation> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    /// The chat with `id`.
    pub fn get(&self, id: &str) -> Option<&Conversation> {
        self.chats.iter().find(|c| c.id == id)
    }

    /// Removes a chat; true when it existed.
    pub fn delete(&mut self, id: &str) -> bool {
        let n = self.chats.len();
        self.chats.retain(|c| c.id != id);
        self.chats.len() != n
    }

    /// `(label, id)` for a menu, newest first.
    pub fn list(&self) -> Vec<(String, String)> {
        self.chats.iter().rev().map(|c| (format!("{} ({} messages)", c.title, c.messages.len()), c.id.clone())).collect()
    }

    /// Totals over all chats.
    pub fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for c in &self.chats {
            t.merge(&c.usage);
        }
        t
    }
}


/// What the chat view shows below the stored messages while a request runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// AGENT ACTION lines of this run (`Calling search_book — query "x"`).
    pub actions: Vec<String>,
    /// Text streamed so far.
    pub text: String,
    /// Status line (`Thinking…`, an error).
    pub status: String,
    /// Model label.
    pub model: String,
}

/// A selected passage as the chat view shows it: a quote in quotation marks.
pub fn quote_passage(text: &str) -> String {
    quote(&guard(&format!("\"{}\"", text.trim())))
}

fn quote(text: &str) -> String {
    text.lines().map(|l| if l.trim().is_empty() { ">".to_string() } else { format!("> {l}") }).collect::<Vec<_>>().join("\n")
}

/// Text that cannot open or close a viewer block (a line starting with `:::`).
fn guard(text: &str) -> String {
    text.lines().map(|l| if l.trim_start().starts_with(":::") { format!("\\{}", l.trim_start()) } else { l.to_string() }).collect::<Vec<_>>().join("\n")
}

/// One line for a block argument (no `|` separator, no line breaks).
fn arg(text: &str) -> String {
    text.replace('|', "/").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replaces `<question>q</question>` items with links; returns the new text and adds
/// the questions to `questions` (link `insightful:q/N` is `questions[N]`).
pub fn link_questions(text: &str, questions: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<question>") {
        let after = &rest[i + 10..];
        let Some(j) = after.find("</question>") else { break };
        out.push_str(&rest[..i]);
        let q = after[..j].trim();
        let label = q.replace(['[', ']'], "");
        out.push_str(&format!("[{label}]({LINK_QUESTION}{})", questions.len()));
        questions.push(q.to_string());
        rest = &after[j + 11..];
    }
    out.push_str(rest);
    out
}

/// Takes the follow-up questions out of an answer: lines holding only `<question>` items
/// (and a list marker) are removed and their questions returned as chip links, with a
/// "Follow-up questions" heading left at the end; questions inside prose become links.
pub fn split_questions(text: &str, questions: &mut Vec<String>) -> (String, Vec<String>) {
    let mut kept: Vec<String> = Vec::new();
    let mut chips = Vec::new();
    for line in text.lines() {
        let mut found = Vec::new();
        let _ = link_questions(line, &mut found);
        let only = !found.is_empty() && {
            let mut rest = line.to_string();
            while let (Some(i), Some(j)) = (rest.find("<question>"), rest.find("</question>")) {
                if j < i {
                    break;
                }
                rest.replace_range(i..j + 11, "");
            }
            rest.trim_matches(|c: char| c.is_whitespace() || c.is_ascii_digit() || "-*+.)".contains(c)).is_empty()
        };
        if only {
            for q in found {
                chips.push(format!("- [{}]({LINK_QUESTION}{})", q.replace(['[', ']'], ""), questions.len()));
                questions.push(q);
            }
        } else if found.is_empty() {
            kept.push(line.to_string());
        } else {
            kept.push(link_questions(line, questions));
        }
    }
    let blank = |k: &Vec<String>| k.last().is_some_and(|l| l.trim().is_empty());
    while blank(&kept) {
        kept.pop();
    }
    if !chips.is_empty() {
        let heading = kept.last().is_some_and(|l| {
            let t = l.trim().to_lowercase();
            t.len() < 80 && (t.starts_with('#') || t.starts_with("**")) && t.contains("question")
        });
        if heading {
            kept.pop();
        }
        while blank(&kept) {
            kept.pop();
        }
    }
    (kept.join("\n"), chips)
}

/// The token/cost line of an answer.
pub fn usage_line(u: &Totals) -> String {
    let mut s = format!("{} tokens in · {} out", u.input_tokens, u.output_tokens);
    if u.cost_usd > 0.0 {
        s.push_str(&format!(" · ${:.4}", u.cost_usd));
    }
    if u.requests > 1 {
        s.push_str(&format!(" · {} requests", u.requests));
    }
    s
}

/// The chat as viewer Markdown, and the follow-up questions its links refer to.
/// `error` is shown after the messages.
pub fn render(conv: &Conversation, live: Option<&Live>, error: Option<&str>) -> (String, Vec<String>) {
    let mut md: Vec<String> = Vec::new();
    let mut questions = Vec::new();
    let last_answer = conv.messages.iter().rposition(|m| m.role != "user");
    for (i, m) in conv.messages.iter().enumerate() {
        if m.role == "user" {
            // A preset action shows its short label; the bubble opens the full prompt.
            let mut s = match &m.label {
                Some(_) => format!(":::you {LINK_PROMPT}{i} | {PROMPT_CAPTION}\n"),
                None => String::from(":::you\n"),
            };
            if let Some(sel) = m.selection.as_ref().filter(|s| !s.text.trim().is_empty()) {
                s.push_str(&quote(&guard(&format!("\"{}\"", sel.text.trim()))));
                s.push_str("\n\n");
            }
            s.push_str(&guard(m.label.as_deref().unwrap_or(&m.content)));
            md.push(s);
        } else {
            let label = arg(m.model.as_deref().unwrap_or("AI"));
            // Older chats have no per-answer usage: the last answer shows the chat's.
            let foot = match (&m.usage, Some(i) == last_answer && live.is_none() && conv.usage.requests > 0) {
                (Some(u), _) => usage_line(u),
                (None, true) => format!("Chat: {}", usage_line(&conv.usage)),
                _ => String::new(),
            };
            let (text, chips) = split_questions(&m.content, &mut questions);
            md.push(format!(":::answer {label} | {foot}\n{}", guard(&text)));
            // Only the newest answer offers its questions.
            if !chips.is_empty() && Some(i) == last_answer && live.is_none() {
                md.push(format!(":::chips\n{}", chips.join("\n")));
            }
        }
    }
    if let Some(l) = live {
        for a in &l.actions {
            md.push(format!(":::action\n{}", guard(a)));
        }
        let label = if l.model.is_empty() { "AI".to_string() } else { arg(&l.model) };
        if !l.text.is_empty() {
            // Questions in a partial answer are not links yet.
            md.push(format!(":::answer {label} |\n{}", guard(&l.text)));
        }
        if !l.status.is_empty() {
            md.push(format!(":::answer {label} |\n*{}*", l.status));
        }
    }
    if let Some(e) = error {
        md.push(format!(":::card |\n**Error:** {}", guard(e)));
    }
    if live.is_none() && conv.messages.last().is_some_and(|m| m.role == "user") {
        md.push(format!(":::chips\n- [Retry]({LINK_RETRY})"));
    }
    if md.is_empty() {
        md.push("*No conversation yet.*".into());
    }
    md.push(format!(":::\n:::input {LINK_ASK} | {ASK_PLACEHOLDER}"));
    (md.join("\n\n"), questions)
}

/// `12 Oct 2026` for seconds since 1970 (UTC).
pub fn date(secs: u64) -> String {
    // Days to civil date (Howard Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// The chat list as viewer Markdown: a card per chat (newest first) with the date, the
/// message count and the first question; `delete` makes the cards delete links.
pub fn render_list(book: &BookChats, delete: bool) -> String {
    let mut md = Vec::new();
    if delete {
        md.push("**Tap a chat to delete it.**".to_string());
    }
    for c in book.chats.iter().rev() {
        let link = if delete { LINK_DEL } else { LINK_OPEN };
        let n = c.messages.len();
        let mut caption = format!("{} · {n} message{}", date(c.updated.max(c.created)), if n == 1 { "" } else { "s" });
        if c.usage.total_tokens > 0 {
            caption.push_str(&format!(" · {} tokens", c.usage.total_tokens));
        }
        let first = c.messages.iter().find(|m| m.role == "user");
        let q = first.map(|m| m.label.as_deref().unwrap_or(&m.content)).unwrap_or(&c.title);
        let mut body = format!("**{}**", guard(&short(q, 90)).replace('*', ""));
        if let Some(sel) = first.and_then(|m| m.selection.as_ref()).filter(|s| !s.text.trim().is_empty()) {
            body.push_str(&format!("\n\n*\"{}\"*", guard(&short(&sel.text, 90)).replace('*', "")));
        }
        md.push(format!(":::card {link}{} | {}\n{body}", c.id, arg(&caption)));
    }
    if book.chats.is_empty() {
        md.push("*No chats for this book yet.*".into());
    }
    let tail = if delete { format!("- [Cancel]({LINK_CHATS})") } else if book.chats.is_empty() { String::new() } else { format!("- [Delete a chat…]({LINK_DELETE})") };
    if !tail.is_empty() {
        md.push(format!(":::chips\n{tail}"));
    }
    md.push(format!(":::\n:::input {LINK_ASK} | {ASK_PLACEHOLDER}"));
    md.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message { role: role.into(), content: content.into(), selection: None, model: None, label: None, timestamp: 0, usage: None }
    }

    #[test]
    fn questions_become_links_or_chips() {
        let mut q = Vec::new();
        let t = link_questions("A\n- <question>Who is [Tybalt]?</question>\n<question>open", &mut q);
        assert_eq!(t, "A\n- [Who is Tybalt?](insightful:q/0)\n<question>open");
        assert_eq!(q, ["Who is [Tybalt]?"]);
        let mut q = Vec::new();
        let (text, chips) = split_questions("Answer, see <question>Why?</question>.\n\n### Follow-up questions\n- <question>Who?</question>\n2. <question> When? </question>\n", &mut q);
        assert_eq!(text, "Answer, see [Why?](insightful:q/0).");
        assert_eq!(chips, ["- [Who?](insightful:q/1)", "- [When?](insightful:q/2)"]);
        assert_eq!(q, ["Why?", "Who?", "When?"]);
    }

    #[test]
    fn render_messages_live_and_tail() {
        let mut c = Conversation { id: "c1".into(), ..Default::default() };
        assert!(render(&c, None, None).0.starts_with("*No conversation yet.*"));
        let mut u = msg("user", "Explain the selected passage clearly");
        u.label = Some("Explain".into());
        u.selection = Some(SelectionInfo { text: "Two households".into(), ..Default::default() });
        c.messages.push(u);
        let live = Live { actions: vec!["Calling `toc`".into()], text: "Par\n:::input x".into(), status: String::new(), model: "m1".into() };
        let (md, _) = render(&c, Some(&live), None);
        assert!(md.starts_with(":::you insightful:prompt/0 | Preset prompt · tap to see what was sent\n> \"Two households\"\n\nExplain\n\n:::action\nCalling `toc`\n\n:::answer m1 |\nPar\n\\:::input x"), "{md}");
        assert!(md.ends_with(":::\n:::input insightful:ask | Ask about this book…") && !md.contains("Retry"));
        assert!(render(&c, None, Some("boom")).0.contains(":::card |\n**Error:** boom\n\n:::chips\n- [Retry](insightful:retry)"));
        let mut a = msg("assistant", "Answer.\n- <question>Next?</question>");
        a.model = Some("m1".into());
        a.usage = Some(Totals { requests: 1, input_tokens: 5, output_tokens: 7, total_tokens: 12, cost_usd: 0.01 });
        c.messages.push(a);
        let (md, q) = render(&c, None, None);
        assert!(md.contains(":::answer m1 | 5 tokens in · 7 out · $0.0100\nAnswer.\n\n:::chips\n- [Next?](insightful:q/0)"), "{md}");
        assert_eq!(q, ["Next?"]);
    }

    #[test]
    fn list_cards_and_date() {
        assert_eq!(date(0), "1 Jan 1970");
        assert_eq!(date(1_793_000_000), "26 Oct 2026");
        let mut b = BookChats::default();
        let id = b.create("Who is Tybalt?");
        let c = b.get_mut(&id).unwrap();
        c.created = 0;
        c.updated = 0;
        c.messages.push(msg("user", "Who is Tybalt?"));
        let md = render_list(&b, false);
        assert!(md.starts_with(":::card insightful:open/c1 | 1 Jan 1970 · 1 message\n**Who is Tybalt?**"), "{md}");
        assert!(md.contains("[Delete a chat…](insightful:delete)"));
        assert!(render_list(&b, true).contains(":::card insightful:del/c1 |"));
    }

    #[test]
    fn corrupt_chat_file_is_kept_aside_and_chats_start_empty() {
        let dir = std::env::temp_dir().join(format!("insightful-chats-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abcd.json");
        std::fs::write(&path, b"{\"book_id\": \"abcd\", \"chats\": [{\"id\"").unwrap(); // truncated
        let mut b = BookChats::load(&path, "abcd").unwrap();
        assert!(b.chats.is_empty());
        assert!(!path.exists(), "the bad file was moved");
        let kept: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(kept.iter().any(|n| n.starts_with("abcd.json.corrupt-")), "{kept:?}");
        b.create("new");
        b.save(&path).unwrap();
        assert_eq!(BookChats::load(&path, "abcd").unwrap().chats.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_list_delete() {
        let dir = std::env::temp_dir().join(format!("insightful-chats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(file_name("ab/../cd"));
        assert!(path.ends_with("conversations/abcd.json"));
        let mut b = BookChats::load(&path, "abcd").unwrap();
        assert!(b.chats.is_empty());
        let one = b.create("Explain: Two households, both alike in dignity, in fair Verona, where we lay our scene");
        let two = b.create("Who is Tybalt?");
        b.get_mut(&one).unwrap().messages.push(msg("user", "x"));
        b.get_mut(&two).unwrap().usage.add(Some(Usage { input_tokens: 1, output_tokens: 2, total_tokens: 3, cost_usd: None }));
        b.save(&path).unwrap();
        let mut back = BookChats::load(&path, "abcd").unwrap();
        assert_eq!(back, b);
        assert_eq!(back.list()[0], ("Who is Tybalt? (0 messages)".to_string(), two.clone()));
        assert!(back.list()[1].0.ends_with("… (1 messages)"));
        assert_eq!(back.totals().total_tokens, 3);
        assert!(back.delete(&one) && !back.delete(&one));
        let three = back.create("again");
        assert_eq!(three, "c3", "ids are not reused");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
