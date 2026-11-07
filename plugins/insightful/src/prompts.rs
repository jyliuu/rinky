//! Prompts from the same Markdown files as insightful.koplugin (`prompts/*.md`), with the
//! same `<tag>` substitution (`prompt_loader.lua`, `agent.lua`).
//!
//! The files are compiled in; a file of the same name in the plugin's data folder
//! (`prompts/<file>.md`) replaces one.

use crate::provider::Msg;
use std::collections::BTreeMap;
use std::path::Path;

/// Largest prompt file.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// `(name, file, compiled-in text)`. Names as in `agent.lua` (`explain_terms` is the
/// "Give examples" action).
pub const FILES: [(&str, &str, &str); 7] = [
    ("system", "system.md", include_str!("../prompts/system.md")),
    ("highlighted_question", "highlighted_question.md", include_str!("../prompts/highlighted_question.md")),
    ("explain", "explain.md", include_str!("../prompts/explain.md")),
    ("explain_terms", "give_examples.md", include_str!("../prompts/give_examples.md")),
    ("context_history", "context_history.md", include_str!("../prompts/context_history.md")),
    ("people_characters", "people_characters.md", include_str!("../prompts/people_characters.md")),
    ("follow_up_reminder", "follow_up_reminder.md", include_str!("../prompts/follow_up_reminder.md")),
];

/// Loaded templates.
#[derive(Clone, Debug)]
pub struct PromptSet {
    templates: BTreeMap<&'static str, String>,
}

/// CRLF/CR to LF, trailing whitespace removed; errors for empty or too large text.
fn clean(text: &str) -> Result<String, String> {
    if text.len() > MAX_PROMPT_BYTES {
        return Err(format!("prompt exceeds {MAX_PROMPT_BYTES} bytes"));
    }
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let t = t.trim_end().to_string();
    if t.is_empty() {
        return Err("prompt is empty".into());
    }
    Ok(t)
}

impl PromptSet {
    /// The compiled-in prompts.
    pub fn builtin() -> PromptSet {
        let templates = FILES.iter().map(|(n, _, t)| (*n, clean(t).expect("builtin prompt"))).collect();
        PromptSet { templates }
    }

    /// The compiled-in prompts, each replaced by `dir/<file>` when that exists.
    pub fn load(dir: Option<&Path>) -> Result<PromptSet, String> {
        let mut set = PromptSet::builtin();
        let Some(dir) = dir else { return Ok(set) };
        for (name, file, _) in FILES {
            let path = dir.join(file);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let t = clean(&text).map_err(|e| format!("could not load {name} ({file}): {e}"))?;
                    set.templates.insert(name, t);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("could not load {name} ({file}): {e}")),
            }
        }
        Ok(set)
    }

    /// The raw template.
    pub fn get(&self, name: &str) -> Result<&str, String> {
        self.templates.get(name).map(String::as_str).ok_or_else(|| format!("unknown prompt: {name}"))
    }

    /// Replaces every `<tag>` (`[a-z][a-z0-9_]*`) with its value; a tag without a value
    /// is an error (`missing prompt value: <tag>`).
    pub fn render(&self, name: &str, values: &[(&str, &str)]) -> Result<String, String> {
        render(self.get(name)?, values)
    }

    /// The system prompt (`system.md`) for a book.
    pub fn system(&self, title: &str, author: &str, position: &str) -> Result<String, String> {
        let or = |s: &str, d: &'static str| if s.trim().is_empty() { d.to_string() } else { s.to_string() };
        let (t, a, p) = (or(title, "Unknown"), or(author, "Unknown"), or(position, "unknown"));
        // system.md shows the model the literal <question> tag: it is replaced by itself.
        self.render("system", &[("title", &t), ("author", &a), ("position", &p), ("question", "<question>")])
    }

    /// The text sent for a user turn: plain, or `highlighted_question.md` with the
    /// selected passage.
    pub fn user_message(&self, content: &str, sel: Option<&SelectionInfo>) -> Result<String, String> {
        let content = content.trim();
        let Some(sel) = sel.filter(|s| !s.text.trim().is_empty()) else { return Ok(content.to_string()) };
        let mut loc = Vec::new();
        if let Some(s) = sel.section.as_deref().filter(|s| !s.is_empty()) {
            loc.push(format!("Section: {s}"));
        }
        if let Some(p) = sel.page {
            loc.push(format!("Page/location: {p}"));
        } else if let Some(l) = sel.locator.as_deref() {
            loc.push(format!("Location: {l}"));
        }
        let location = if loc.is_empty() { String::new() } else { loc.join("\n") + "\n" };
        self.render("highlighted_question", &[("selection_location", &location), ("passage", &sel.text), ("question", content)])
    }

    /// Appends the follow-up reminder (read raw, not rendered) to the last user message
    /// of a request copy.
    pub fn add_reminder(&self, messages: &mut [Msg]) {
        let reminder = self.get("follow_up_reminder").unwrap_or_default().to_string();
        if let Some(Msg::User(t)) = messages.iter_mut().rev().find(|m| matches!(m, Msg::User(_))) {
            t.push_str("\n\n");
            t.push_str(&reminder);
        }
    }
}

/// The selected passage attached to a user message.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SelectionInfo {
    /// Selected text.
    pub text: String,
    /// Section title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Page (1-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    /// Position string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

/// `<tag>` substitution of `prompt_loader.lua`.
pub fn render(template: &str, values: &[(&str, &str)]) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut missing: Option<String> = None;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let tag_len = after
            .char_indices()
            .take_while(|(j, c)| if *j == 0 { c.is_ascii_lowercase() } else { c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' })
            .count();
        if tag_len > 0 && after[tag_len..].starts_with('>') {
            let tag = &after[..tag_len];
            match values.iter().find(|(k, _)| *k == tag) {
                Some((_, v)) => out.push_str(v),
                None => {
                    missing.get_or_insert_with(|| tag.to_string());
                    out.push_str(&rest[i..i + tag_len + 2]);
                }
            }
            rest = &after[tag_len + 1..];
        } else {
            out.push('<');
            rest = after;
        }
    }
    out.push_str(rest);
    match missing {
        Some(t) => Err(format!("missing prompt value: <{t}>")),
        None => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_like_the_lua_loader() {
        assert_eq!(render("a <x> b <y_2>.", &[("x", "1"), ("y_2", "<z>")]).unwrap(), "a 1 b <z>.");
        assert_eq!(render("<x> <Y> <1a> < x> <", &[("x", "v")]).unwrap(), "v <Y> <1a> < x> <");
        assert_eq!(render("<a> <b>", &[]).unwrap_err(), "missing prompt value: <a>");
    }

    #[test]
    fn builtin_prompts() {
        let p = PromptSet::builtin();
        let s = p.system("Romeo and Juliet", "", "Act 1").unwrap();
        assert!(s.contains("Title: Romeo and Juliet\nAuthor: Unknown\n"));
        assert!(s.contains("Current reading position: Act 1"));
        assert!(s.contains("- <question>Why does the narrator"), "literal <question> kept");
        assert!(p.get("explain_terms").unwrap().starts_with("Give clear, concrete examples"));
        let sel = SelectionInfo { text: "Two households".into(), section: Some("Prologue".into()), page: Some(3), locator: None };
        let u = p.user_message(" Explain this ", Some(&sel)).unwrap();
        assert_eq!(u, "[Selected passage]\nSection: Prologue\nPage/location: 3\n\n\"Two households\"\n\n[Question or action]\nExplain this");
        assert_eq!(p.user_message("Hi", None).unwrap(), "Hi");
        let mut msgs = vec![Msg::User("a".into()), Msg::User("b".into())];
        p.add_reminder(&mut msgs);
        assert_eq!(msgs[0], Msg::User("a".into()));
        let Msg::User(b) = &msgs[1] else { panic!() };
        assert!(b.starts_with("b\n\n[Reply format]") && b.contains("<question>question text</question>"));
    }

    #[test]
    fn override_from_folder() {
        let dir = ink_plugin::testing::TempDir::new("insightful-prompts");
        std::fs::write(dir.join("explain.md"), "Say it simply.\r\n\r\n  ").unwrap();
        let p = PromptSet::load(Some(&dir)).unwrap();
        assert_eq!(p.get("explain").unwrap(), "Say it simply.");
        std::fs::write(dir.join("explain.md"), " \n").unwrap();
        assert!(PromptSet::load(Some(&dir)).unwrap_err().contains("prompt is empty"));
    }
}
