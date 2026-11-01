//! The efference crate: every module group in dependency order (docs/README.md,
//! *Dependency direction*).

// The `Send` check on the session loop's spawned future (`host::runtime`) walks
// through the whole async call chain, `Toolbox::run` included; its auto-trait
// recursion reaches the default depth of 128 on that chain.
#![recursion_limit = "256"]

pub mod agent;
pub mod clients;
pub mod config;
pub mod curation;
pub mod host;
pub mod inference;
pub mod listeners;
pub mod platform;
pub mod protocol;
pub mod record;
pub mod routes;
pub mod session;
pub mod setup;
pub mod tools;

#[cfg(test)]
mod test;
