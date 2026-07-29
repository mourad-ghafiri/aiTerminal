//! One module per feature. Each implements [`World`](super::world::World) and owns its
//! own verb vocabulary.

pub mod ai;
pub mod config;
pub mod gate;
mod gate_step;
pub mod jobs;
pub mod keymap;
pub mod loops;
pub mod markdown;
mod plugin_step;
pub mod plugins;
pub mod security;
pub mod shell;
pub mod terminal;
pub mod theme;
