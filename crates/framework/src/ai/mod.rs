//! `framework-ai` — the AI capability of the Framework layer.
//!
//! A streaming chat client built on the generic Platform transport, with the
//! agentic loop, multi-agent orchestration, terminal-context capture, and
//! on-disk agent/skill loading. The wire protocol is pluggable
//! behind a provider Strategy ([`provider`]), so the engine is **not** locked to
//! any single vendor. Tests run fully offline against a mock transport; no
//! third-party crates, no `unsafe`.
#![forbid(unsafe_code)]

mod agent;
mod client;
mod context;
pub mod defs;
pub mod diff;
mod mcp;
pub mod memory;
mod model;
mod orchestrate;
pub mod pool;
pub mod provider;
mod request;
mod setup;
mod stream;
mod tools;

// Curated flat surface — callers write `framework::ai::Client`, `…::run_agent`, …
pub use agent::{run_agent, AgentObserver, AgentRun, AgentSpec, NoopObserver, RunOutcome, ToolRunner, ToolSpec, ToolStep};
pub use client::Client;
pub use context::{capture_context, TermContext};
pub use mcp::{load_servers, McpHub, McpServer};
pub use model::AiSettings;
pub use orchestrate::{run_orchestration, Orchestration, OrchestrationStep, StepResult};
pub use memory::{MemoryEntry, MemoryService};
pub use pool::{ModelOverrides, ModelPool, PoolEntry, Strategy};
pub use request::{command_request, first_command_line, qa_request, ChatRequest, ImageData, Message, Role};
pub use setup::{setup_hint, setup_hint_short};
pub use stream::StreamEvent;
pub use tools::{DEFAULT_CODER_TOOLS, DEFAULT_SAFE_TOOLS};

// The provider seam: the Strategy + Adapters + Factory + self-describing model
// catalog. Adding a backend is one adapter + one `provider_for` arm; adding a
// model is one `[models.<id>]` table in an `ai/models/<provider>.toml` file.
pub use provider::{
    builtin_default, decode_stream, load_models, parse_models_doc, provider_for, text_sse,
    text_sse_openai, AnthropicAdapter, ModelCaps, ModelCatalog, ModelDef, ModelPricing, OpenAiAdapter,
    Provider, ProviderKind, StreamDecoder,
};

// The streaming transport types callers construct (`framework::ai::CurlTransport`, …).
pub use platform::transport::{CancelToken, CurlTransport, MockTransport, ScriptedTransport, StreamHandle, Transport};
