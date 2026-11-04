use super::*;
use crate::testutil::{CannedClient, FakeBook};
use ink_plugin::{MemoryStorage, NetWorker, UiQueue, UiRequest};
use std::sync::Arc;

const TOML: &str = r#"
provider = "openai"
[providers.openai]
base_url = "https://api.openai.com/v1/chat/completions"
model = "gpt-test"
api_key = "sk-fake-test"
"#;

fn sse(deltas: &[Value]) -> String {
    deltas.iter().map(|d| format!("data: {d}\n\n")).collect::<String>() + "data: [DONE]\n\n"
}

fn tool_turn() -> String {
    sse(&[
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_a", "function": {"name": "search_book", "arguments": "{\"query\":"}}]}}]}),
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"Verona\"}"}}]}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}}),
    ])
}

const DELTAS: usize = 60;

fn answer_turn() -> String {
    let mut d: Vec<Value> = (0..DELTAS).map(|i| json!({"choices": [{"delta": {"content": format!("word{i} ")}}]})).collect();
    d.push(json!({"choices": [{"delta": {"content": "\n\n### Follow-up questions\n- <question>Who is Tybalt?</question>\n- <question>Where is Verona?</question>"}}]}));
    d.push(json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 200, "completion_tokens": 80, "total_tokens": 280}}));
    sse(&d)
}

struct Env {
    plugin: InsightfulPlugin,
    ui: UiQueue,
    store: MemoryStorage,
    book: FakeBook,
    worker: NetWorker,
    client: Arc<CannedClient>,
    dir: PathBuf,
}

impl Env {
    fn new(test: &str, toml: &str, answers: &[(u16, &str)]) -> Env {
        let dir = std::env::temp_dir().join(format!("insightful-plugin-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/insightful.toml"), toml).unwrap();
        let client = Arc::new(CannedClient::seq(answers));
        let mut plugin = InsightfulPlugin::new();
        plugin.koreader_config = dir.join("no-configuration.lua");
        plugin.batch_interval = Duration::from_secs(3600);
        let mut registry = ink_plugin::Registry::default();
        plugin.on_load(&mut Registrar { plugin: NAME, registry: &mut registry }).unwrap();
        assert_eq!(registry.selection_actions.len(), 1, "one popup button (the popup shows 3 for all plugins)");
        Env { plugin, ui: UiQueue::default(), store: MemoryStorage { dir: dir.join("data"), ..Default::default() }, book: FakeBook::new(40), worker: NetWorker::new(client.clone()), client, dir }
    }

    fn with<R>(&mut self, f: impl FnOnce(&mut InsightfulPlugin, &mut HostCtx) -> R) -> R {
        let perms = self.plugin.manifest().permissions.clone();
        let mut jobs = self.worker.for_plugin(NAME);
        let mut cx = HostCtx::new(&mut self.ui, &mut self.store, &perms).with_doc(Some(&mut self.book)).with_jobs(Some(&mut jobs));
        f(&mut self.plugin, &mut cx)
    }

    fn select(&mut self, text: &str) {
        let sel = Selection { start: FakeBook::pos(2), end: FakeBook::pos(2), text: text.into(), page: 2, rects: vec![] };
        self.with(|p, cx| p.on_selection(cx, &sel)).unwrap();
    }

    fn action(&mut self, id: &str) {
        self.with(|p, cx| p.on_action(cx, id)).unwrap();
    }

    fn answer(&mut self, id: u32, r: &str) {
        self.with(|p, cx| p.on_ui_result(cx, id, r)).unwrap();
    }

    /// Delivers network events until no request is in flight.
    fn pump(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let evs = self.worker.wait(Duration::from_millis(50));
            for (_, id, ev) in evs {
                self.with(|p, cx| p.on_http(cx, id, &ev)).unwrap();
            }
            if self.worker.in_flight() == 0 && !self.plugin.running() {
                return;
            }
        }
        panic!("requests did not finish");
    }

    fn viewers(&self) -> Vec<String> {
        self.ui.0.iter().filter_map(|r| if let UiRequest::TextViewer { id: VIEWER, markdown, .. } = r { Some(markdown.clone()) } else { None }).collect()
    }

    fn chats_file(&self) -> Value {
        let p = self.dir.join("data").join(chat::file_name("0123456789abcdef0123456789abcdef"));
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }
}

#[test]
fn explain_with_tool_call_streams_in_batches_and_saves() {
    let (t1, t2) = (tool_turn(), answer_turn());
    let mut env = Env::new("explain", TOML, &[(200, &t1), (200, &t2), (200, &answer_turn())]);
    env.select("Two households, both alike in dignity");
    env.action("explain");
    assert!(env.plugin.running());
    env.pump();
    assert!(!env.plugin.running());

    let views = env.viewers();
    assert!(views.len() < DELTAS / 4, "text arrives in batches: {} viewer updates for {DELTAS} deltas", views.len());
    assert!(views.len() >= 3, "several updates while streaming: {}", views.len());
    assert!(
        views.iter().any(|v| v.contains(":::action\nFinished `search_book` — query \"Verona\"")),
        "AGENT ACTION block shown: {views:#?}"
    );
    let last = views.last().unwrap();
    assert!(last.starts_with(":::back insightful:chats\n:::you\n**Selected passage"), "{last}");
    assert!(last.contains("> \"Two households, both alike in dignity\"\n\nExplain the selected passage clearly"), "{last}");
    assert!(last.contains(":::answer AI — gpt-test | 300 tokens in · 90 out · 2 requests\nword0 word1"), "{last}");
    assert!(last.contains(":::chips\n- [Who is Tybalt?](insightful:q/0)"));
    assert!(!last.contains(":::action"), "actions only while the agent runs");

    let sent = env.client.bodies_sent();
    assert_eq!(sent.len(), 2);
    let first = &sent[0]["messages"];
    assert!(first[0]["content"].as_str().unwrap().contains("Title: Romeo and Juliet\nAuthor: William Shakespeare"));
    assert!(first[0]["content"].as_str().unwrap().contains("Current reading position: Act 1"));
    let user = first[1]["content"].as_str().unwrap();
    assert!(user.starts_with("[Selected passage]\nSection: Act 1\nPage/location: 3\n\n\"Two households, both alike in dignity\"\n\n[Question or action]\nExplain the selected passage"), "{user}");
    assert!(user.ends_with("- <question>question text</question>"), "follow-up reminder on the last user turn");
    assert_eq!(sent[0]["tools"].as_array().unwrap().len(), 5);
    let second = sent[1]["messages"].as_array().unwrap();
    assert_eq!(second[2]["tool_calls"][0]["function"]["name"], "search_book");
    assert_eq!(second[3]["role"], "tool");
    assert_eq!(second[3]["tool_call_id"], "call_a");
    let result: Value = serde_json::from_str(second[3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["hits"][0]["id"], "q1:h1");
    assert_eq!(result["hits"].as_array().unwrap().len(), 15);

    let saved = env.chats_file();
    assert_eq!(saved["chats"][0]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(saved["chats"][0]["messages"][1]["model"], "gpt-test");
    assert_eq!(saved["chats"][0]["usage"]["total_tokens"], 390);

    // A follow-up link sends that question in the same chat.
    env.answer(VIEWER, "insightful:q/0");
    env.pump();
    let sent = env.client.bodies_sent();
    let msgs = sent[2]["messages"].as_array().unwrap();
    assert!(msgs[msgs.len() - 1]["content"].as_str().unwrap().starts_with("Who is Tybalt?\n\n[Reply format]"));
    assert!(msgs[msgs.len() - 2]["content"].as_str().unwrap().starts_with("word0"), "earlier answer is sent back");
    assert_eq!(env.chats_file()["chats"][0]["messages"].as_array().unwrap().len(), 4);
}

#[test]
fn chats_list_open_delete_and_close_keeps_running() {
    let t2 = answer_turn();
    let mut env = Env::new("list", TOML, &[(200, &t2), (200, &t2)]);
    env.select("In fair Verona");
    env.action("people");
    env.pump();
    env.ui.0.clear();
    env.action("chats");
    let list = env.viewers().last().unwrap().clone();
    assert!(list.starts_with(":::chips\n- [Start new chat](insightful:new)\n\n:::card insightful:open/c1 | Current · "), "{list}");
    assert!(list.contains(" · 2 messages · 280 tokens\n**People / characters**\n\n*\"In fair Verona\"*"), "{list}");
    env.answer(VIEWER, "insightful:open/c1");
    assert!(env.viewers().last().unwrap().contains("Who is Tybalt?"));

    // The one-time checkbox of the highlight menu: this action starts a new chat.
    // Closing the viewer while its request runs leaves it running: the viewer stays
    // closed, the answer is saved to its chat and a toast says it came.
    env.select("Two households");
    env.action("menu");
    env.answer(MENU_ACTIONS, "stay:override");
    let UiRequest::TextViewer { id: MENU_ACTIONS, markdown, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert!(markdown.ends_with(":::row stay:override | check\nStart a new chat\n"), "the checkbox is ticked in place: {markdown}");
    env.answer(MENU_ACTIONS, "context");
    assert!(env.plugin.running());
    env.answer(VIEWER, "closed");
    assert!(env.plugin.running(), "the request goes on");
    env.ui.0.clear();
    env.pump();
    assert!(env.viewers().is_empty(), "a closed viewer is not opened again: {:?}", env.ui.0);
    assert!(env.ui.0.iter().any(|r| matches!(r, UiRequest::Toast { text } if text == "Insightful answered")));
    let saved = env.chats_file();
    assert_eq!(saved["chats"][1]["messages"].as_array().unwrap().len(), 2, "answer saved to c2");
    assert_eq!(saved["current"], "c2");
    env.action("last_chat");
    assert!(env.viewers().last().unwrap().contains("word0 word1"), "the answer is there when the chat opens again");

    // Hold a chat in the list to delete it.
    env.action("chats");
    env.answer(VIEWER, "insightful:open/c1");
    env.answer(VIEWER, "insightful:chats");
    env.answer(VIEWER, "hold:insightful:open/c1");
    let Some(UiRequest::Dialog { id: DIALOG_DELETE, title, body, .. }) = env.ui.0.last().cloned() else { panic!("{:?}", env.ui.0.last()) };
    assert_eq!(title, "Delete this chat?");
    assert!(body.ends_with("\n\nThe deleted chat cannot be recovered."), "{body}");
    env.answer(DIALOG_DELETE, "delete");
    assert!(env.ui.0.iter().any(|r| matches!(r, UiRequest::Toast { text } if text == "Chat deleted")));
    assert!(env.viewers().last().unwrap().contains("insightful:open/c2"), "the list again");
    let saved = env.chats_file();
    assert_eq!(saved["chats"].as_array().unwrap().len(), 1);
    assert_eq!(saved["chats"][0]["id"], "c2");
    assert!(saved.get("current").is_none(), "the deleted chat was the current one");
}

#[test]
fn a_chat_has_a_back_link_to_the_list_and_last_chat_reopens_it() {
    let t2 = answer_turn();
    let mut env = Env::new("last-chat", TOML, &[(200, &t2)]);
    // No chat yet: "Continue current chat" opens an empty one.
    env.action("last_chat");
    let empty = env.viewers().last().unwrap().clone();
    assert!(empty.contains("*No conversation yet.*") && empty.ends_with(":::input insightful:ask | Message Insightful…"), "{empty}");
    assert!(!env.ui.0.iter().any(|r| matches!(r, UiRequest::TextInput { .. })), "the keyboard stays closed");
    env.select("In fair Verona");
    env.action("people");
    env.pump();
    assert!(env.viewers().last().unwrap().starts_with(":::back insightful:chats\n"), "a chat goes back to the list");
    env.answer(VIEWER, "insightful:chats");
    assert!(env.viewers().last().unwrap().starts_with(":::chips\n- [Start new chat]"), "the list has no back link");
    env.answer(VIEWER, "closed");
    env.ui.0.clear();
    env.action("last_chat");
    assert!(env.viewers().last().unwrap().contains("Who is Tybalt?"), "the last chat is open again");
}

#[test]
fn ask_ai_errors_and_provider_menu() {
    let mut env = Env::new("errors", TOML, &[(401, "{\"error\":{\"message\":\"Incorrect API key\"}}")]);
    env.select("dignity");
    env.action("ask");
    // Ask AI… opens the chat with the keyboard closed; the passage waits for the message.
    let view = env.viewers().last().unwrap().clone();
    assert!(view.contains(":::card | Sent with your next message\n**Selected passage — Act 1**\n\n> \"dignity\""), "{view}");
    assert!(!matches!(env.ui.0.last(), Some(UiRequest::TextInput { .. })));
    env.answer(VIEWER, "insightful:ask");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_ASK, prompt, .. }) if prompt == "Message Insightful…"));
    env.answer(INPUT_ASK, "What does dignity mean here?");
    env.pump();
    let last = env.viewers().last().unwrap().clone();
    assert!(last.contains("**Error:** AI service HTTP 401: Incorrect API key"), "provider message kept: {last}");
    assert!(!last.contains("[Retry]"), "a rejected key needs a change first: {last}");
    env.answer(VIEWER, "insightful:retry");
    assert!(!env.plugin.running());
    let saved = env.chats_file();
    assert_eq!(saved["chats"][0]["messages"][0]["content"], "What does dignity mean here?");
    assert_eq!(saved["chats"][0]["messages"][0]["selection"]["text"], "dignity");

    env.action("settings:provider");
    env.answer(MENU_SETTINGS, "provider");
    let UiRequest::Menu { id: MENU_PROVIDER, items, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert_eq!(items, [("✓ openai · gpt-test".to_string(), "openai".to_string())]);
    env.answer(MENU_PROVIDER, "openai");
    env.action("book_menu");
    let UiRequest::TextViewer { id: MENU_BOOK, markdown, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert!(
        markdown.starts_with(":::sheet\n:::row last_chat | chat\nContinue current chat\n:::row chats | folder\nChats\n:::row new_chat | plus\nStart new chat\n:::row stay:new_on_highlight | minus\nNew chat for highlighted actions   ★\n:::row settings:provider | settings\nProvider: OpenAI\n:::row stats | chart\nStatistics\n"),
        "{markdown}"
    );
    env.answer(MENU_BOOK, "new_chat");
    assert!(env.viewers().last().unwrap().contains("*No conversation yet.*"));
    assert_eq!(env.store.settings["provider"], "openai");

    let mut env = Env::new("nokey", "provider = \"openai\"\n", &[(200, "")]);
    env.action("ask_book");
    env.answer(VIEWER, "insightful:ask");
    env.answer(INPUT_ASK, "Who wrote this?");
    assert!(!env.plugin.running());
    assert!(env.viewers().last().unwrap().contains("**Error:** Insightful is not configured: no provider has an API key (insightful.toml)."));
    assert!(env.client.bodies_sent().is_empty());
}

/// Highlighted actions continue the current chat unless the book's setting (or the
/// menu's one-time checkbox) starts a new one; holding the setting makes it the
/// default for other books.
#[test]
fn highlighted_actions_continue_the_current_chat_or_start_new_ones() {
    let t = answer_turn();
    let mut env = Env::new("continue", TOML, &[(200, &t), (200, &t), (200, &t), (200, &t), (200, &t)]);
    let chats = |env: &Env| env.chats_file()["chats"].as_array().unwrap().iter().map(|c| c["messages"].as_array().unwrap().len()).collect::<Vec<_>>();
    env.select("In fair Verona");
    env.action("people");
    env.pump();
    env.select("Two households");
    env.action("context");
    env.pump();
    assert_eq!(chats(&env), [4], "the second action continues the chat");
    let sent = env.client.bodies_sent();
    let msgs = sent[1]["messages"].as_array().unwrap();
    assert!(msgs[msgs.len() - 1]["content"].as_str().unwrap().contains("\"Two households\""), "the new passage goes with it");
    // The checkbox: this one action starts a new chat; the next continues it.
    env.action("menu");
    env.answer(MENU_ACTIONS, "stay:override");
    env.answer(MENU_ACTIONS, "explain");
    env.pump();
    assert_eq!(chats(&env), [4, 2]);
    // The book's setting: every highlighted action starts a new chat...
    env.action("book_menu");
    env.answer(MENU_BOOK, "stay:new_on_highlight");
    let UiRequest::TextViewer { id: MENU_BOOK, markdown, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert!(markdown.contains(":::row stay:new_on_highlight | check\nNew chat for highlighted actions\n"), "ticked, not the default: {markdown}");
    env.answer(MENU_BOOK, "closed");
    env.action("explain");
    env.pump();
    assert_eq!(chats(&env), [4, 2, 2]);
    // ...and the checkbox then says "Start in existing chat".
    env.action("menu");
    let UiRequest::TextViewer { id: MENU_ACTIONS, markdown, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert!(markdown.ends_with(":::row stay:override | minus\nStart in existing chat\n"), "{markdown}");
    env.answer(MENU_ACTIONS, "stay:override");
    env.answer(MENU_ACTIONS, "people");
    env.pump();
    assert_eq!(chats(&env), [4, 2, 4], "continued the current chat (c3)");
    assert_eq!(env.chats_file()["new_chat_on_highlight"], true);
    // Hold: the book's value becomes the default for other books.
    env.action("book_menu");
    env.answer(MENU_BOOK, "hold:stay:new_on_highlight");
    let Some(UiRequest::Dialog { id: DIALOG_DEFAULT, body, .. }) = env.ui.0.last().cloned() else { panic!("{:?}", env.ui.0.last()) };
    assert_eq!(body, "Use a new chat for highlighted actions by default in other books?");
    env.answer(DIALOG_DEFAULT, "ok");
    assert_eq!(env.store.settings[NEW_CHAT_DEFAULT], true);
    let UiRequest::TextViewer { id: MENU_BOOK, markdown, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert!(markdown.contains("New chat for highlighted actions   ★"), "{markdown}");
}

/// Stop ends the answer being written; Retry sends the question again. Statistics show
/// the requests of the book and of all books.
#[test]
fn stop_retry_and_statistics() {
    let t = answer_turn();
    let mut env = Env::new("stop", TOML, &[(200, &t), (200, &t)]);
    env.select("In fair Verona");
    env.action("people");
    assert!(env.viewers().last().unwrap().contains("- [Stop](insightful:stop)"));
    env.answer(VIEWER, "insightful:stop");
    assert!(!env.plugin.running());
    let view = env.viewers().last().unwrap().clone();
    assert!(view.contains("**Error:** Request canceled.") && view.contains("[Retry](insightful:retry)"), "{view}");
    env.answer(VIEWER, "insightful:retry");
    assert!(env.plugin.running());
    env.pump();
    env.action("stats");
    env.answer(MENU_STATS, "book");
    let Some(UiRequest::Dialog { title, body, .. }) = env.ui.0.last().cloned() else { panic!() };
    assert_eq!(title, "Current book\nRomeo and Juliet");
    // The stopped request had no response: only the retry counts.
    assert!(body.starts_with("Model requests: 1\nInput tokens: 200\nOutput tokens: 80\nTotal tokens: 280\nProvider reported cost: Not available"), "{body}");
    env.answer(MENU_STATS, "all");
    let Some(UiRequest::Dialog { title, body, .. }) = env.ui.0.last().cloned() else { panic!() };
    assert_eq!(title, "All books");
    assert!(body.starts_with("Model requests: 1\n"), "{body}");
    env.answer(MENU_STATS, "general");
    let Some(UiRequest::Dialog { title, body, .. }) = env.ui.0.last().cloned() else { panic!() };
    assert_eq!(title, "General");
    assert_eq!(body, "Provider: OpenAI\nConfigured model: gpt-test\nStreaming: On\nOutput token limit: Provider default");
}

#[test]
fn converts_koreader_configuration_once() {
    let mut env = Env::new("convert", TOML, &[(200, "")]);
    std::fs::remove_file(env.dir.join("data/insightful.toml")).unwrap();
    let lua = env.dir.join("configuration.lua");
    std::fs::write(&lua, "return { provider = 'anthropic', providers = { anthropic = { base_url = 'https://api.anthropic.com/v1/messages', model = 'claude-fake', api_key = 'sk-ant-fake' } } }").unwrap();
    env.plugin.koreader_config = lua;
    env.action("settings:provider");
    assert!(matches!(&env.ui.0[0], UiRequest::Notification { text } if text.contains("imported")));
    env.answer(MENU_SETTINGS, "provider");
    let UiRequest::Menu { items, .. } = env.ui.0.last().unwrap().clone() else { panic!() };
    assert_eq!(items[0].1, "anthropic");
    assert!(std::fs::read_to_string(env.dir.join("data/insightful.toml")).unwrap().contains("claude-fake"));
}

#[test]
fn popup_entry_opens_the_action_menu_and_keeps_the_selection() {
    let t2 = answer_turn();
    let mut env = Env::new("menu", TOML, &[(200, &t2), (200, &t2)]);
    env.select("Two households");
    env.action("menu");
    let UiRequest::TextViewer { id: MENU_ACTIONS, markdown, .. } = env.ui.0.last().unwrap().clone() else { panic!("{:?}", env.ui.0) };
    let ids: Vec<&str> = markdown.lines().filter_map(|l| l.strip_prefix(":::row ")).map(|l| l.split(" | ").next().unwrap()).collect();
    assert_eq!(ids, ["explain", "examples", "context", "people", "ask", "stay:override"]);
    assert!(markdown.contains(":::row examples | listview\nGive examples\n"));
    // The reader cleared its selection meanwhile; the menu keeps the one from the popup.
    env.plugin.last_sel = None;
    env.answer(MENU_ACTIONS, "examples");
    assert!(env.plugin.running());
    env.pump();
    let sent = env.client.bodies_sent();
    let user = sent[0]["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("\"Two households\"") && user.contains("Give clear, concrete examples"), "{user}");
    // Ask AI… on another passage: the current chat, the passage sent with the message.
    env.select("dignity");
    env.action("menu");
    env.answer(MENU_ACTIONS, "ask");
    let view = env.viewers().last().unwrap().clone();
    assert!(view.contains("Give clear, concrete examples") && view.contains("> \"dignity\""), "the current chat with the passage: {view}");
    env.answer(VIEWER, "insightful:ask");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_FOLLOWUP, .. })));
    env.answer(INPUT_FOLLOWUP, "What does it mean?");
    env.pump();
    let saved = env.chats_file();
    assert_eq!(saved["chats"].as_array().unwrap().len(), 1);
    assert_eq!(saved["chats"][0]["messages"][2]["selection"]["text"], "dignity");
    assert!(!env.viewers().last().unwrap().contains("Sent with your next message"), "the passage went with it");
}

const TOML_MODELS: &str = r#"
provider = "deepseek"
[providers.deepseek]
base_url = "https://api.deepseek.com/chat/completions"
model = "deepseek-v4-flash"
api_key = "sk-fake-ds"
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1/chat/completions"
model = "openrouter/auto"
api_key = "sk-fake-or"
"#;

impl Env {
    fn menu(&self) -> (u32, String, Vec<(String, String)>) {
        let Some(UiRequest::Menu { id, title, items }) = self.ui.0.last().cloned() else { panic!("no menu: {:?}", self.ui.0.last()) };
        (id, title, items)
    }

    fn last_get(&self) -> ink_net::Request {
        self.client.sent.lock().unwrap().iter().rfind(|r| r.body.is_empty()).cloned().unwrap()
    }

    fn saved_config(&self) -> Config {
        Config::from_toml(&std::fs::read_to_string(self.dir.join("data/insightful.toml")).unwrap()).unwrap()
    }

    fn toasted(&self, t: &str) -> bool {
        self.ui.0.iter().any(|r| matches!(r, UiRequest::Toast { text } if text == t))
    }
}

#[test]
fn model_list_pick_saves_and_next_chat_uses_it() {
    let list = r#"{"object":"list","data":[{"id":"deepseek-v4-pro","object":"model"},{"id":"deepseek-v4-flash"},{"id":"deepseek-v4-pro"}]}"#;
    let mut env = Env::new("models", TOML_MODELS, &[(200, list), (200, &answer_turn())]);
    env.action("settings:provider");
    let (id, _, items) = env.menu();
    assert_eq!(id, MENU_SETTINGS);
    assert_eq!(items, [("Provider\tdeepseek".to_string(), "provider".to_string()), ("Model\tdeepseek-v4-flash".into(), "model".into())]);
    env.answer(MENU_SETTINGS, "model");
    let (id, title, items) = env.menu();
    assert_eq!((id, title.as_str()), (MENU_MODEL, "deepseek model: deepseek-v4-flash"));
    let acts: Vec<&str> = items.iter().map(|(_, a)| a.as_str()).collect();
    assert_eq!(acts, ["list", "search", "enter"], "no \"Use default\" without an override");

    env.answer(MENU_MODEL, "list");
    assert!(env.toasted("Loading models…"));
    env.pump();
    let get = env.last_get();
    assert_eq!((get.method, get.url.as_str()), (ink_net::Method::Get, "https://api.deepseek.com/models"));
    assert_eq!(get.header_value("Authorization"), Some("Bearer sk-fake-ds"));
    assert_eq!(get.header_value("Accept"), Some("application/json"));
    let (id, title, items) = env.menu();
    assert_eq!((id, title.as_str()), (MENU_MODELS, "deepseek models"));
    assert_eq!(items, [("✓ deepseek-v4-flash".to_string(), "deepseek-v4-flash".to_string()), ("deepseek-v4-pro".into(), "deepseek-v4-pro".into())], "current first, no duplicates");

    env.answer(MENU_MODELS, "deepseek-v4-pro");
    assert!(env.toasted("Insightful uses deepseek-v4-pro"));
    let saved = env.saved_config();
    assert_eq!(saved.models["deepseek"], "deepseek-v4-pro");
    assert_eq!((saved.providers["deepseek"].api_key.as_str(), saved.providers["openrouter"].api_key.as_str()), ("sk-fake-ds", "sk-fake-or"), "keys kept");
    assert!(!env.dir.join("data/insightful.toml.tmp").exists());

    // The next chat request uses the chosen model.
    env.action("ask_book");
    env.answer(INPUT_ASK, "Who wrote this?");
    env.pump();
    assert_eq!(env.client.bodies_sent().last().unwrap()["model"], "deepseek-v4-pro");
    // A new plugin reads it from the file.
    let mut env2 = Env::new("models-reload", &std::fs::read_to_string(env.dir.join("data/insightful.toml")).unwrap(), &[(200, "")]);
    env2.action("settings:provider");
    assert_eq!(env2.menu().2[1].0, "Model\tdeepseek-v4-pro");

    // Enter model ID: prefilled with the current model.
    env.action("settings:model");
    let (id, _, items) = env.menu();
    assert_eq!(id, MENU_MODEL);
    assert_eq!(items.last().unwrap(), &("Use default (deepseek-v4-flash)".to_string(), "default".to_string()));
    env.answer(MENU_MODEL, "enter");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_MODEL, initial, .. }) if initial == "deepseek-v4-pro"));
    env.answer(INPUT_MODEL, "");
    assert_eq!(env.saved_config().models["deepseek"], "deepseek-v4-pro", "cancel changes nothing");
    env.answer(INPUT_MODEL, " deepseek-reasoner ");
    assert_eq!(env.saved_config().models["deepseek"], "deepseek-reasoner");
    // Use default removes the override.
    env.answer(MENU_MODEL, "default");
    assert!(env.toasted("Insightful uses deepseek-v4-flash"));
    assert!(env.saved_config().models.is_empty());
}

#[test]
fn model_search_errors_offline_and_close() {
    let list = r#"{"data":[{"id":"openai/gpt-x","name":"OpenAI: GPT X"},{"id":"anthropic/claude-z","name":"Anthropic: Claude Z"},{"id":"x/y","name":"Claude-like"}]}"#;
    let mut env = Env::new("model-search", TOML_MODELS, &[(200, list), (401, "{\"error\":{\"message\":\"No auth credentials found\"}}"), (0, "")]);
    env.action("settings:provider");
    env.answer(MENU_SETTINGS, "provider");
    env.answer(MENU_PROVIDER, "openrouter");
    env.action("settings:model");
    env.answer(MENU_MODEL, "search");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_SEARCH, initial, .. }) if initial.is_empty()));
    env.answer(INPUT_SEARCH, " claude ");
    env.pump();
    let get = env.last_get();
    assert_eq!(get.url, "https://openrouter.ai/api/v1/models?output_modalities=text&supported_parameters=tools&sort=most-popular&q=claude");
    assert_eq!(get.header_value("Authorization"), Some("Bearer sk-fake-or"));
    let (id, title, items) = env.menu();
    assert_eq!((id, title.as_str()), (MENU_MODELS, "openrouter models matching \"claude\""));
    let ids: Vec<&str> = items.iter().map(|(_, a)| a.as_str()).collect();
    assert_eq!(ids, ["anthropic/claude-z", "x/y"], "filtered here too (the server may ignore q)");

    // Error status: a dialog with the provider's message, never the key.
    env.answer(MENU_MODEL, "list");
    env.pump();
    let Some(UiRequest::Dialog { id: DIALOG_MODEL, body, .. }) = env.ui.0.last().cloned() else { panic!("{:?}", env.ui.0.last()) };
    assert_eq!(body, "Couldn't load models: Model service HTTP 401: No auth credentials found");
    // No network.
    env.answer(MENU_MODEL, "list");
    env.pump();
    let Some(UiRequest::Dialog { id: DIALOG_MODEL, body, .. }) = env.ui.0.last().cloned() else { panic!("{:?}", env.ui.0.last()) };
    assert!(body.starts_with("Couldn't load models: ") && body.contains("network is unreachable") && body.ends_with("Check the Wi-Fi connection."), "{body}");
    assert!(!body.contains("sk-fake"));
    assert!(env.saved_config().models.is_empty(), "errors save nothing");

    // Closing the book cancels a pending list; a stale id is ignored.
    env.answer(MENU_MODEL, "list");
    env.with(|p, cx| p.on_close(cx)).unwrap();
    assert!(env.plugin.models_req.is_none());
    let n = env.ui.0.len();
    env.with(|p, cx| p.on_http(cx, 999, &HttpEvent::Done { body: Some(b"{}".to_vec()) })).unwrap();
    env.pump();
    assert_eq!(env.ui.0.len(), n, "no dialog or menu after the cancel");
}

/// A preset action sends the whole prompt file (with the passage, its section and
/// page), and the chat shows exactly that: the passage with its location, then the prompt.
#[test]
fn preset_sends_the_full_prompt_and_the_bubble_shows_it() {
    let mut env = Env::new("preset", TOML, &[(200, &answer_turn())]);
    env.select("In fair Verona, where we lay our scene");
    env.action("people");
    env.pump();
    let full = PromptSet::builtin().get("people_characters").unwrap().to_string();
    assert!(full.starts_with("Identify the important people or characters") && full.len() > 100, "{full}");
    let sent = env.client.bodies_sent();
    let user = sent[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.starts_with(&format!("[Selected passage]\nSection: Act 1\nPage/location: 3\n\n\"In fair Verona, where we lay our scene\"\n\n[Question or action]\n{full}\n\n[Reply format]")),
        "the request carries the whole preset prompt, not its label: {user}"
    );
    assert!(!user.contains("People / characters"), "the label is not sent: {user}");
    let system = sent[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Title: Romeo and Juliet\nAuthor: William Shakespeare"));

    let view = env.viewers().last().unwrap().clone();
    let bubble = format!(":::back insightful:chats\n:::you\n**Selected passage — Act 1**\n\n> \"In fair Verona, where we lay our scene\"\n\n{full}\n");
    assert!(view.starts_with(&bubble), "the passage and the whole prompt, no tap needed: {view}");
    assert!(!view.contains("tap to see") && !view.contains("People / characters"), "{view}");
}

/// "Start new chat" opens an empty chat view with the keyboard closed; a tap on the
/// input bar opens it (the host docks it under the view), and the question starts a
/// chat in the same viewer.
#[test]
fn ask_opens_the_chat_view_with_the_keyboard() {
    let mut env = Env::new("askview", TOML, &[(200, &answer_turn())]);
    env.action("ask_book");
    assert!(
        matches!(env.ui.0.last(), Some(UiRequest::TextViewer { id: VIEWER, markdown, .. }) if markdown.contains("*No conversation yet.*") && markdown.ends_with(":::input insightful:ask | Message Insightful…")),
        "{:?}",
        env.ui.0
    );
    env.answer(VIEWER, "insightful:ask");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_ASK, prompt, .. }) if prompt == "Message Insightful…"));
    // The keyboard put away and the bar tapped again: the keyboard comes back.
    env.answer(INPUT_ASK, "");
    env.answer(VIEWER, "insightful:ask");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_ASK, .. })));
    env.answer(INPUT_ASK, "Who is Tybalt?");
    env.pump();
    let last = env.viewers().last().unwrap().clone();
    assert!(last.starts_with(":::back insightful:chats\n:::you\nWho is Tybalt?"), "{last}");
    // In the chat the bar asks a follow-up in the same chat.
    env.answer(VIEWER, "insightful:ask");
    assert!(matches!(env.ui.0.last(), Some(UiRequest::TextInput { id: INPUT_FOLLOWUP, .. })));
}

/// The user goes back to the list and opens another chat while an answer streams: the
/// open chat is not overwritten with the live answer, and the answer is saved to the
/// chat that asked.
#[test]
fn a_run_answers_its_own_chat_while_another_is_open() {
    let t2 = answer_turn();
    let mut env = Env::new("other-chat", TOML, &[(200, &t2), (200, &t2)]);
    env.select("In fair Verona");
    env.action("people");
    env.pump();
    // New chats for highlighted actions in this book.
    env.action("book_menu");
    env.answer(MENU_BOOK, "stay:new_on_highlight");
    env.select("Two households");
    env.action("context");
    assert!(env.plugin.running());
    env.answer(VIEWER, "insightful:chats");
    env.answer(VIEWER, "insightful:open/c1");
    env.ui.0.clear();
    env.pump();
    assert!(env.viewers().is_empty(), "chat c1 stays as it was: {:?}", env.ui.0);
    assert!(env.ui.0.iter().any(|r| matches!(r, UiRequest::Toast { text } if text == "Insightful answered")));
    let saved = env.chats_file();
    assert_eq!(saved["chats"][0]["messages"].as_array().unwrap().len(), 2, "c1 untouched");
    assert_eq!(saved["chats"][1]["messages"].as_array().unwrap().len(), 2, "c2 has its answer");
}
