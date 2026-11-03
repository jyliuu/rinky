//! Insightful: an AI reading companion, ported from insightful.koplugin v0.7.0 to a
//! first-party Rust plugin on the rinky plugin API (`docs/plugins.md`).
//!
//! | Module | Contents | Lua original |
//! | --- | --- | --- |
//! | [`provider`] | Chat Completions and Anthropic Messages, SSE streaming | `providers/*.lua` |
//! | [`config`] | `insightful.toml`, converter from `configuration.lua` | `configuration.lua`, `provider_profiles.lua` |
//! | [`models`] | the provider's model list for the model menu | `model_catalog.lua` |
//! | [`prompts`] | `prompts/*.md` with `<tag>` substitution | `prompt_loader.lua`, `agent.lua` |
//! | [`tools`] | the five book tools with the same limits | `book_tools.lua` |
//! | [`chat`] | chats per book (saved, list, delete), Markdown for the chat view | `storage.lua`, `conversation_renderer.lua` |
//! | [`plugin`] | selection actions, chat view, agent loop | `main.lua`, `chat.lua`, `agent.lua` |

#![warn(missing_docs)]

pub mod chat;
pub mod config;
pub mod models;
pub mod plugin;
pub mod prompts;
pub mod provider;
pub mod tools;

#[cfg(test)]
mod testutil;
pub use plugin::InsightfulPlugin;
