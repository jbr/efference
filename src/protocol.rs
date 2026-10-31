//! The contract between a session and whatever drives it: the names and shapes that cross
//! the control channel or the tool wire (docs/control-api.md, docs/tools.md).
//!
//! Each submodule holds one surface's types and names, and the readers of them that need
//! nothing else, such as which decision a call's input names. The code that serves a
//! surface imports its contract from here, and so does the code that only reads it: the
//! clients, the loop, the toolbox, the config validator. Behaviour that needs a subsystem
//! stays with that subsystem, as `impl` blocks on the types defined here. Every value the
//! channel carries goes through [`channel_value`].
//!
//! The module imports the vocabulary its shapes are written in — `log`, `policy`,
//! `config`, `inference`, `api`, `project`, `catalog`, `devices`, `sign_ins` — and nothing
//! from a module that hosts sessions or is a client of them (docs/README.md, *Dependency
//! direction*).

use serde::Serialize;
use serde_json::Value;

pub mod agents;
pub mod answers;
pub mod attention;
pub mod files;
pub mod inventory;
pub mod projects;
pub mod sessions;
pub mod sign_in;
pub mod state;
pub mod status;
pub mod stream;
pub mod transcript;
pub mod turns;

/// A value as the control channel carries it: a [`Value`], whose objects sort their keys
/// (serde_json without `preserve_order`), so a type's field order never reaches the
/// bytes. Every topic, the composite snapshot, and the loop resource go through here; a
/// value that cannot be serialized is `null`.
pub(crate) fn channel_value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}
