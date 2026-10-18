//! feather library surface: the agent wiring the binary uses, exposed so
//! integration tests (and embedders) can drive the exact same tool set.

pub mod auth;
pub mod bash;
pub mod command_policy;
pub mod config;
pub mod config_store;
pub mod mentions;
pub mod models;
pub mod netlimit;
pub mod oauth;
pub mod preferences;
pub mod prereq;
pub mod proxy;
pub mod resources;
pub mod rpc;
pub mod shell;
pub mod subagent;
pub mod tasks;
pub mod web;
pub mod wiring;
// The interactive TUI frontend was retired: GUI work lives in `feather-gui`, and
// this crate exposes only the agent wiring, tools and the RPC bridge.

#[cfg(test)]
mod http_test;
