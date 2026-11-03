# Insightful (Rust)

Port of `insightful.koplugin` v0.7.0 to a compiled-in rinky plugin (PLAN phase 8).

| File | Contents |
| --- | --- |
| `src/provider.rs` | OpenAI, DeepSeek, OpenRouter (Chat Completions) and Anthropic (Messages): request bodies, headers, SSE accumulator, whole responses, the koplugin's error strings |
| `src/config.rs` | `insightful.toml`; one-time converter from KOReader's `configuration.lua` (run in a Lua state without standard library) |
| `src/prompts.rs` | `prompts/*.md` (copied from the koplugin) with the same `<tag>` substitution; a file in `data/prompts/` replaces one |
| `src/tools.rs` | `search_book`, `read_around`, `list_links`, `toc`, `current_position` with the same limits (15 hits, 200 scanned, 250-byte snippets, 8000-byte reads, 120 TOC entries, 40 links, 200/500-byte labels/targets) |
| `src/chat.rs` | chats per book in `data/conversations/<book-id>.json` (create, list, delete, totals); Markdown for the chat view with AGENT ACTION blocks and follow-up links |
| `src/plugin.rs` | the `Plugin`: selection popup entry `Insightful` (Explain, Give examples, Context/history, People/characters, Ask AI…), reader-menu entry `Insightful` (Ask about the book, Chats, Provider), settings page `Insightful` (provider), the agent loop in `on_http` |

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
`insightful:q/N` (follow-up question N), `insightful:ask` (keyboard: the host docks it
under the view and lifts the view's input bar onto it), `insightful:retry`,
`insightful:prompt/N` (the bubble of preset message N). Closing the viewer (or going to
another chat) leaves a running request running: the answer is saved to the chat that asked,
and a toast says "Insightful answered" when it ends off screen. Closing the book cancels
it. "Ask about the book…" / "Ask AI…" open the chat view first (empty, with the
selected passage) and the docked keyboard under it.

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

## Not done yet (PLAN phase 8)

Model lists per provider, importing koplugin chats (`insightful/conversations/<book-id>.lua`),
retry per answer. Registered in `crates/ink-app/src/plugins.rs`.
