//! feather library surface: the agent wiring the binary uses, exposed so
//! integration tests (and embedders) can drive the exact same tool set.

pub mod auth;
pub mod bash;
pub mod capabilities;
pub mod command_policy;
pub mod config;
pub mod config_store;
pub mod mentions;
pub mod models;
pub mod oauth;
pub mod preferences;
pub mod proxy;
pub mod resources;
pub mod rpc;
mod shell;
pub mod subagent;
pub mod tasks;
// The interactive TUI module was removed. The TUI now ships as the separate
// `feather-gui` binary; this crate no longer references `tui_loop`.
// Obsolete file left for the parent to delete: src/tui_loop.rs
pub mod web;
pub mod wiring;
// Terminal theme/editor preferences retired with the TUI frontend.

#[cfg(test)]
mod http_test;
