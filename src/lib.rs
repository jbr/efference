// The `Send` check on the session loop's spawned future (`daemon::runtime`) walks
// through the whole async call chain, `Toolbox::run` included; its auto-trait
// recursion reaches the default depth of 128 on that chain.
#![recursion_limit = "256"]

pub mod agent;
pub mod api;
pub mod attachments;
pub mod bash;
pub mod boot;
pub mod catalog;
pub mod client;
pub mod compact;
pub mod config;
pub mod config_schemas;
pub mod consolidate;
pub mod context;
pub mod control_tools;
pub mod daemon;
pub mod digest;
pub mod discard;
pub mod env;
pub mod env_table;
pub mod expand;
pub mod frontend;
pub mod guide;
pub mod history;
pub mod inbox;
pub mod inference;
pub mod inventory;
pub mod mcp;
pub mod platform;
pub mod policy;
pub mod process_monitor;
pub mod protocol;
pub mod record;
pub mod remembered;
pub mod session_ids;
pub mod setup;
pub mod spotter;
pub mod tools;
pub mod triage;
pub mod tui;
pub mod user_config;
pub mod vision;
pub mod worktree;

#[cfg(test)]
mod test;
#[cfg(test)]
mod testing;
