# Insightful (Rust)

Port of [`insightful.koplugin`](https://github.com/jyliuu/insightful.koplugin) v0.7.0 to a
compiled-in rinky plugin (PLAN phase 8). The flows follow the koplugin's `main.lua`,
`chat.lua` and `chat_list.lua`; the screens use rinky's chat view.

| File | Contents |
| --- | --- |
| `src/provider.rs` | OpenAI, DeepSeek, OpenRouter (Chat Completions) and Anthropic (Messages): request bodies, headers, SSE accumulator, whole responses, the koplugin's error strings |
| `src/config.rs` | `insightful.toml`; one-time converter from KOReader's `configuration.lua` (run in a Lua state without standard library) |
| `src/prompts.rs` | `prompts/*.md` (copied from the koplugin) with the same `<tag>` substitution; a file in `data/prompts/` replaces one |
| `src/tools.rs` | `search_book`, `read_around`, `list_links`, `toc`, `current_position` with the same limits (15 hits, 200 scanned, 250-byte snippets, 8000-byte reads, 120 TOC entries, 40 links, 200/500-byte labels/targets) |
| `src/chat.rs` | chats per book in `data/conversations/<book-id>.json` (create, list, delete, totals, the current chat, the book's "new chat for highlighted actions"); Markdown for the chat view with AGENT ACTION blocks and follow-up links; the Statistics text |
| `src/plugin.rs` | the `Plugin`: selection popup entry `Insightful` (the "Insightful actions" sheet), reader-menu entry `Insightful`, gesture actions, settings page `Insightful` (provider), the agent loop in `on_http` |

## Menus

- **Insightful actions** (the selection popup's Insightful button): Explain, Give
  examples, Context / history, People / characters, Ask AI…, and a one-time checkbox.
  The actions continue the book's current chat unless **New chat for highlighted
  actions** is on; the checkbox (**Start a new chat**, or **Start in existing chat**
  when the setting is on) flips that for this highlight only. Ask AI… opens the chat
  with the keyboard closed; the passage goes with the next message typed.
- **Insightful** (reader menu): Continue current chat, Chats, Start new chat, New chat
  for highlighted actions (tap: this book; hold: make this book's value the default for
  other books, ★ when it is), Provider, Statistics (General, Current book, All books:
  requests, tokens and the provider-reported cost; all books in `data/statistics.json`),
  Gesture shortcuts.
- **Chats**: Start new chat, then a card per chat, newest first, the current one marked.
  Tap a chat to open it; hold it to delete it after a confirmation.
- Gesture actions: `last_chat` ("Insightful: show current chat"), `chats` ("Insightful:
  show chats"), `ask_book` ("Insightful: start new chat").

## Settings

`plugins/insightful/data/insightful.toml`:

```toml
provider = "openai"
timeout = 60
stream = true

[providers.openai]
base_url = "https://api.openai.com/v1/chat/completions"
model = "gpt-4.1-mini"
api_key = "sk-..."
```

When the file does not exist, the plugin reads
`/mnt/us/koreader/plugins/insightful.koplugin/configuration.lua` once (only reads it)
and writes `insightful.toml` from its `providers` table.

## Chat view

The host's Markdown viewer (`UiRequest::TextViewer`, one id), a scrolling chat view that
pages a whole window at a time. While an answer streams, the text is replaced after 160 new
characters or 1.2 s; the view does not scroll with it (a question just sent sits at the
top, its answer fills in below). Links:
`insightful:q/N` (follow-up question N), `insightful:ask` (the "Message Insightful…" bar:
keyboard, docked under the view with the bar lifted onto it), `insightful:stop` (while an
answer is written), `insightful:retry` (only when sending again can work: not after a
missing or rejected key), `insightful:prompt/N` (the bubble of preset message N),
`insightful:chats` (Back: the chat list). Answers are labelled `AI — <model>`. Closing the
viewer (or going to another chat) leaves a running request running: the answer is saved
to the chat that asked, and a toast says "Insightful answered" when it ends off screen.
Closing the book cancels it. Only one answer runs at a time.

Preset actions (Explain, Give examples, Context/history, People/characters) store the
whole prompt file as the message and its label separately: the request carries the full
prompt (wrapped in `highlighted_question.md` with the passage, section and page, plus
the follow-up reminder; the system prompt has title, author and position), the chat
shows the label with the caption "Preset prompt · tap to see what was sent", and a tap
shows the sent text in a dialog.

## Tests

`CARGO_BUILD_JOBS=2 cargo test -p insightful`: providers against canned SSE served in
7-byte reads through ink-plugin's network path (ink-net's SSE parser) and one real HTTP
request to ink-net's `TestServer` `/echo`; the full plugin with a fake book and a
`NetWorker`. Only fake keys; no request reaches an AI provider.

## Registration

Registered in `crates/ink-app/src/plugins.rs`. Chats saved by the koplugin are not
imported.
