//! One module per feature. Each implements [`World`](super::world::World) and owns its
//! own verb vocabulary.

pub mod ai;
pub mod cli;
pub mod config;
pub mod flow;
pub mod gate;
pub mod guard;
mod gate_step;
pub mod jobs;
pub mod keymap;
pub mod loops;
pub mod markdown;
pub mod memory;
mod plugin_step;
pub mod plugins;
pub mod shell;
pub mod terminal;
pub mod theme;
