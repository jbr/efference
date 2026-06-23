//! efference — context as a deterministic projection over an append-only log.
//!
//! See DESIGN.md for the full architecture. This crate root exposes the Phase 0
//! substrate: the append-only [`log`] (source of truth) and the deterministic
//! [`project`]ion over it (the view sent to the API). The binary (`main.rs`) is
//! currently the transport spike and will grow into the agent loop.

pub mod api;
pub mod log;
pub mod project;
