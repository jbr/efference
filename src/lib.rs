//! efference — context as a deterministic projection over an append-only log.
//!
//! See docs/README.md for the full architecture. This crate root exposes the Phase 0
//! core: the append-only [`log`] (source of truth) and the deterministic
//! [`project`]ion over it (the view sent to the API). The binary (`main.rs`) is
//! currently the transport spike and will grow into the agent loop.

pub mod agent;
pub mod api;
pub mod bash;
pub mod client;
pub mod config;
pub mod consolidate;
pub mod context;
pub mod daemon;
pub mod distill;
pub mod env;
pub mod frontend;
pub mod inbox;
pub mod log;
pub mod persist;
pub mod policy;
pub mod pretty;
pub mod project;
pub mod spotter;
pub mod supervisor;
pub mod tools;
pub mod tui;
