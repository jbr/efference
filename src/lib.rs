// The `Send` check on the session loop's spawned future (`daemon::runtime`) walks
// through the whole async call chain, `Toolbox::run` included; its auto-trait
// recursion reaches the default depth of 128 on that chain.
#![recursion_limit = "256"]

pub mod agent;
pub mod boot;
pub mod client;
pub mod config;
pub mod control_tools;
pub mod curation;
pub mod daemon;
pub mod discard;
pub mod frontend;
pub mod inference;
pub mod inventory;
pub mod platform;
pub mod protocol;
pub mod record;
pub mod remembered;
pub mod session;
pub mod session_ids;
pub mod setup;
pub mod tools;
pub mod tui;
pub mod worktree;

#[cfg(test)]
mod test;
