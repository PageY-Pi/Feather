//! feather library surface: the agent wiring the binary uses, exposed so
//! integration tests (and embedders) can drive the exact same tool set.

pub mod auth;
pub mod bash;
pub mod command_policy;
mod shell;
pub mod config;
pub mod config_store;
pub mod capabilities;
pub mod mentions;
pub mod models;
pub mod preferences;
pub mod resources;
pub mod rpc;
pub mod subagent;
pub mod oauth;
pub mod proxy;
pub mod tasks;
// The interactive TUI module was removed. The TUI now ships as the separate
// `feather-gui` binary; this crate no longer references `tui_loop`.
// Obsolete file left for the parent to delete: src/tui_loop.rs
pub mod wiring;
pub mod web;
// Terminal theme/editor preferences retired with the TUI frontend.

#[cfg(test)]
mod http_test;
