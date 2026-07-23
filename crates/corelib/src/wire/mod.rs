//! `wire` — std-only data interchange: JSON + a TOML subset (parsers/serializers)
//! and a `frontmatter` splitter (TOML head + Markdown body) for agent/skill/app
//! files. JSON-RPC framing and an SSE decoder are lifted here in the AI phase.
//! One hand-written, fuzzable codec surface shared by `bridge`, `browser`,
//! `ai`, and the plugin protocol in `dx`.
#![forbid(unsafe_code)]

pub mod frontmatter;
pub mod json;
pub mod toml;

pub use frontmatter::Frontmatter;
pub use json::Json;
pub use toml::{json_to_toml, toml_to_json, Toml};
